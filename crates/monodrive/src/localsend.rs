//! LocalSend in the window: receiving while Settings says so, the offers a
//! person answers, the sends and receives under way, and the devices the
//! LocalSend page lists while one is open. One for the app, so every window
//! shows the same; `monodrive_core::localsend` does the work.

use crate::feedback::{self, Sound};
use crate::frame;
use crate::preferences::Preferences;
use gpui::{
    App, AppContext, ClipboardItem, Context, Entity, EventEmitter, Global, SystemNotification,
    SystemNotificationAction,
};
use monodrive_core::localsend::{
    self as core, Cancel, Device, Ended, Event, Identity, Message, Messages, Nearby, Offer,
    Progress, Receiver, SendError, Update,
};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Receiving {
    Off,
    Starting,
    On { port: u16 },
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

/// A send or a receive under way.
pub struct Transfer {
    pub key: u64,
    pub direction: Direction,
    pub peer: String,
    pub done: u64,
    pub total: u64,
    /// Offered, and the receiver hasn't answered.
    pub waiting: bool,
    stop: Stop,
}

enum Stop {
    Send(Cancel),
    Receive(String),
}

/// Something for the windows to say in a toast, with what Show would show.
pub struct Said {
    pub text: String,
    pub failed: bool,
    pub show: Option<PathBuf>,
}

/// A device that wants a PIN before it takes these.
pub struct PinWanted {
    pub device: Device,
    pub paths: Vec<PathBuf>,
    pub wrong: bool,
}

type Then = Box<dyn FnOnce(&mut LocalSend, Arc<Identity>, &mut Context<LocalSend>)>;

pub struct LocalSend {
    identity: Option<Arc<Identity>>,
    /// What waits for the identity to be read.
    waiting: Vec<Then>,
    receiver: Option<Receiver>,
    /// Receivers being stopped. Stopping one and starting the next both hold
    /// it, so a new one never starts while an old one keeps the port.
    retiring: Arc<Mutex<Vec<Receiver>>>,
    /// The PIN and folder receiving was last started with.
    applied: Option<(Option<String>, PathBuf)>,
    /// Counts each start, so what an older one says is dropped.
    generation: u64,
    pub receiving: Receiving,
    pub offers: Vec<Offer>,
    pub transfers: Vec<Transfer>,
    next_key: u64,
    nearby: Option<Nearby>,
    /// Panes showing the LocalSend page.
    lookers: usize,
    pub devices: Vec<Device>,
    pub scanning: bool,
    /// For the active window to ask for.
    pub pin_wanted: Option<PinWanted>,
    /// The text devices sent, newest first, once read.
    pub messages: Vec<Message>,
    messages_read: bool,
}

impl EventEmitter<Said> for LocalSend {}

struct Shared(Entity<LocalSend>);
impl Global for Shared {}

/// The app's one.
pub fn get(cx: &mut App) -> Entity<LocalSend> {
    if let Some(shared) = cx.try_global::<Shared>() {
        return shared.0.clone();
    }
    let localsend = cx.new(|_| LocalSend {
        identity: None,
        waiting: Vec::new(),
        receiver: None,
        retiring: Arc::default(),
        applied: None,
        generation: 0,
        receiving: Receiving::Off,
        offers: Vec::new(),
        transfers: Vec::new(),
        next_key: 0,
        nearby: None,
        lookers: 0,
        devices: Vec::new(),
        scanning: false,
        pin_wanted: None,
        messages: Vec::new(),
        messages_read: false,
    });
    cx.set_global(Shared(localsend.clone()));
    localsend
}

/// The tag of offer `id`'s notification, and its buttons.
const OFFER_TAG: &str = "localsend-offer:";
const ACCEPT: &str = "accept";
const DECLINE: &str = "decline";

/// Receives from launch while Settings says to, and follows Settings.
/// Answers offers from their notifications, the app's only ones.
pub fn start(cx: &mut App) {
    let localsend = get(cx);
    localsend.update(cx, |this, cx| this.follow_settings(cx));
    cx.observe_global::<Preferences>(move |cx| {
        localsend.update(cx, |this, cx| this.follow_settings(cx));
    })
    .detach();
    cx.on_system_notification_response(|response, cx| {
        let Some(id) = response.tag.strip_prefix(OFFER_TAG) else {
            return;
        };
        let accept = match response.action_id.as_deref() {
            Some(ACCEPT) => true,
            Some(DECLINE) => false,
            _ => return frame::bring_forward(cx),
        };
        let id = id.to_owned();
        get(cx).update(cx, |localsend, cx| localsend.answer(&id, accept, cx));
    });
}

impl LocalSend {
    /// The name other devices see, once it's been read.
    pub fn alias(&self) -> Option<&str> {
        self.identity
            .as_ref()
            .map(|identity| identity.alias.as_str())
    }

    fn with_identity(
        &mut self,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Arc<Identity>, &mut Context<Self>) + 'static,
    ) {
        if let Some(identity) = self.identity.clone() {
            return then(self, identity, cx);
        }
        self.waiting.push(Box::new(then));
        if self.waiting.len() > 1 {
            return;
        }
        // Reads the key, or makes one the first time, which takes a moment.
        let read = cx.background_spawn(async { Identity::user() });
        cx.spawn(async move |this, cx| {
            let identity = read.await;
            let _ = this.update(cx, |this, cx| {
                let waiting = std::mem::take(&mut this.waiting);
                match identity {
                    Ok(identity) => {
                        let identity = Arc::new(identity);
                        this.identity = Some(identity.clone());
                        for then in waiting {
                            then(this, identity.clone(), cx);
                        }
                    }
                    Err(why) => {
                        this.scanning = false;
                        if this.receiving == Receiving::Starting {
                            this.receiving = Receiving::Failed(why.clone());
                        }
                        this.say(why, true, None, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn follow_settings(&mut self, cx: &mut Context<Self>) {
        let prefs = cx.global::<Preferences>();
        let on = prefs.localsend;
        let wanted = (on && prefs.localsend_receive)
            .then(|| (prefs.localsend_pin.clone(), prefs.localsend_save_to()));
        if !on {
            self.stop_sending(cx);
        } else if !self.messages_read {
            self.messages_read = true;
            self.change_messages(Messages::list, cx);
        }
        match (wanted, self.applied.clone()) {
            (None, None) => {}
            (None, Some(_)) => {
                self.stop_receiving(cx);
                let retiring = self.retiring.clone();
                cx.background_spawn(async move { stop_retired(&retiring) })
                    .detach();
            }
            (Some((pin, folder)), Some((had_pin, _))) if pin == had_pin => {
                if let Some(receiver) = &self.receiver {
                    receiver.save_to(folder.clone());
                }
                self.applied = Some((pin, folder));
            }
            (Some((pin, folder)), _) => {
                self.stop_receiving(cx);
                self.start_receiving(pin, folder, cx);
            }
        }
    }

    /// Settings turned LocalSend off: sends under way stop, and a PIN
    /// waiting to be asked for isn't.
    fn stop_sending(&mut self, cx: &mut Context<Self>) {
        self.pin_wanted = None;
        for transfer in &self.transfers {
            if let Stop::Send(cancel) = &transfer.stop {
                cancel.cancel();
            }
        }
        cx.notify();
    }

    /// Starts receiving once the receivers being stopped have let go of
    /// their ports.
    fn start_receiving(&mut self, pin: Option<String>, folder: PathBuf, cx: &mut Context<Self>) {
        let retiring = self.retiring.clone();
        self.applied = Some((pin.clone(), folder.clone()));
        self.receiving = Receiving::Starting;
        self.generation += 1;
        let generation = self.generation;
        cx.notify();
        self.with_identity(cx, move |this, identity, cx| {
            if this.generation != generation {
                return;
            }
            let (tell, heard) = async_channel::unbounded();
            let start = cx.background_spawn(async move {
                let mut retired = retiring.lock().unwrap_or_else(PoisonError::into_inner);
                for old in retired.drain(..) {
                    old.stop();
                }
                Receiver::start(identity, folder, pin, move |event| {
                    let _ = tell.try_send(event);
                })
            });
            cx.spawn(async move |this, cx| {
                let started = start.await;
                let current = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        match started {
                            Ok(receiver) => {
                                // Settings may have moved the folder meanwhile.
                                receiver.save_to(cx.global::<Preferences>().localsend_save_to());
                                this.receiving = Receiving::On {
                                    port: receiver.port(),
                                };
                                this.receiver = Some(receiver);
                                if this.lookers > 0
                                    && let Some(receiver) = &this.receiver
                                {
                                    receiver.announce();
                                }
                            }
                            Err(why) => this.receiving = Receiving::Failed(why),
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    return;
                }
                while let Ok(event) = heard.recv().await {
                    let heard = this.update(cx, |this, cx| {
                        if this.generation == generation {
                            this.heard(event, cx);
                        }
                    });
                    if heard.is_err() {
                        break;
                    }
                }
            })
            .detach();
        });
    }

    /// Stops receiving: offers waiting are declined and receives under way
    /// end where they are. The receiver joins `retiring`, since waiting on
    /// its ports belongs off this thread.
    fn stop_receiving(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.applied = None;
        if let Some(receiver) = self.receiver.take() {
            for offer in self.offers.drain(..) {
                receiver.answer(&offer.id, false);
                cx.dismiss_system_notification(&offer_tag(&offer.id));
            }
            self.retiring
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(receiver);
        }
        self.offers.clear();
        self.transfers
            .retain(|transfer| transfer.direction == Direction::Out);
        self.receiving = Receiving::Off;
        cx.notify();
    }

    fn heard(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Offer(offer) => {
                call_over(&offer, cx);
                self.offers.push(offer);
            }
            Event::Withdrawn(id) => {
                if let Some(at) = self.offers.iter().position(|offer| offer.id == id) {
                    let offer = self.offers.remove(at);
                    cx.dismiss_system_notification(&offer_tag(&id));
                    self.say(format!("{} stopped asking.", offer.from), false, None, cx);
                }
            }
            Event::Started {
                id,
                from,
                files: _,
                bytes,
            } => {
                let key = self.key();
                self.transfers.push(Transfer {
                    key,
                    direction: Direction::In,
                    peer: from,
                    done: 0,
                    total: bytes,
                    waiting: false,
                    stop: Stop::Receive(id),
                });
            }
            Event::Progress { id, done, total } => {
                if let Some(transfer) = self.receiving_transfer(&id) {
                    transfer.done = done;
                    transfer.total = total;
                }
            }
            Event::Ended { id, ended } => {
                self.transfers
                    .retain(|transfer| !matches!(&transfer.stop, Stop::Receive(had) if *had == id));
                self.received(ended, cx);
            }
            Event::Seen(device) => {
                if let Some(nearby) = &self.nearby {
                    nearby.add(device);
                }
            }
            Event::Stopped(why) => {
                self.receiver = None;
                self.receiving = Receiving::Failed(why);
            }
        }
        cx.notify();
    }

    fn receiving_transfer(&mut self, id: &str) -> Option<&mut Transfer> {
        self.transfers
            .iter_mut()
            .find(|transfer| matches!(&transfer.stop, Stop::Receive(had) if had == id))
    }

    fn received(&mut self, ended: Ended, cx: &mut Context<Self>) {
        let what = match ended.received.as_slice() {
            [one] => one
                .file_name()
                .map(|name| format!("\"{}\"", name.to_string_lossy()))
                .unwrap_or_default(),
            items => count(items.len(), "item"),
        };
        let show = ended.received.first().cloned();
        let from = ended.from;
        let (text, failed) = match (ended.cancelled, ended.failed.first()) {
            (true, _) if ended.received.is_empty() => {
                (format!("Receiving from {from} stopped"), false)
            }
            (true, _) => (format!("Receiving from {from} stopped after {what}"), false),
            (false, Some((name, why))) => (
                format!(
                    "{} from {from} didn't arrive: \"{name}\" {}",
                    count(ended.failed.len(), "file"),
                    why.trim_end_matches('.')
                ),
                true,
            ),
            (false, None) => (format!("Received {what} from {from}"), false),
        };
        self.say(text, failed, show, cx);
    }

    /// Says it in the windows' toasts, and in a notification while another
    /// app is in front.
    fn say(&self, text: String, failed: bool, show: Option<PathBuf>, cx: &mut Context<Self>) {
        if !frame::in_front() && !frame::unattended() {
            cx.show_system_notification(SystemNotification {
                tag: "localsend".into(),
                title: "LocalSend".into(),
                body: text.clone().into(),
                actions: Vec::new(),
            });
        }
        cx.emit(Said { text, failed, show });
    }

    fn key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }

    /// Takes every file of the offer, or none. A message goes to the
    /// clipboard and the messages instead.
    pub fn answer(&mut self, id: &str, accept: bool, cx: &mut Context<Self>) {
        let Some(at) = self.offers.iter().position(|offer| offer.id == id) else {
            return;
        };
        let offer = self.offers.remove(at);
        cx.dismiss_system_notification(&offer_tag(id));
        if let Some(receiver) = &self.receiver {
            receiver.answer(id, accept);
        }
        if accept && let Some(text) = offer.text {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            self.say(format!("Copied text from {}", offer.from), false, None, cx);
            let at = chrono::Utc::now().timestamp_millis().unsigned_abs();
            self.change_messages(move |messages| messages.add(offer.from, text, at), cx);
        }
        cx.notify();
    }

    /// Puts a message on the clipboard again.
    pub fn copy_message(&mut self, at: u64, cx: &mut Context<Self>) {
        let Some(message) = self.messages.iter().find(|message| message.at == at) else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(message.text.clone()));
        let said = format!("Copied text from {}", message.from);
        self.say(said, false, None, cx);
    }

    pub fn forget_message(&mut self, at: u64, cx: &mut Context<Self>) {
        self.change_messages(move |messages| messages.forget(at), cx);
    }

    pub fn clear_messages(&mut self, cx: &mut Context<Self>) {
        self.change_messages(Messages::clear, cx);
    }

    /// Reads or changes the messages off this thread, then shows them.
    fn change_messages(
        &mut self,
        change: impl FnOnce(&Messages) -> Result<Vec<Message>, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(messages) = Messages::user() else {
            return;
        };
        let changed = cx.background_spawn(async move { change(&messages) });
        cx.spawn(async move |this, cx| {
            let changed = changed.await;
            let _ = this.update(cx, |this, cx| match changed {
                Ok(messages) => {
                    this.messages = messages;
                    cx.notify();
                }
                Err(why) => this.say(why, true, None, cx),
            });
        })
        .detach();
    }

    pub fn cancel(&mut self, key: u64, cx: &mut Context<Self>) {
        let Some(transfer) = self.transfers.iter().find(|t| t.key == key) else {
            return;
        };
        match &transfer.stop {
            Stop::Send(cancel) => cancel.cancel(),
            Stop::Receive(id) => {
                if let Some(receiver) = &self.receiver {
                    receiver.cancel(id);
                }
            }
        }
        cx.notify();
    }

    /// Sends `paths` to `device`, saying how it went.
    pub fn send(
        &mut self,
        device: Device,
        paths: Vec<PathBuf>,
        pin: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if !cx.global::<Preferences>().localsend {
            return;
        }
        let key = self.key();
        let cancel = Cancel::new();
        self.transfers.push(Transfer {
            key,
            direction: Direction::Out,
            peer: device.alias.clone(),
            done: 0,
            total: 0,
            waiting: true,
            stop: Stop::Send(cancel.clone()),
        });
        cx.notify();
        self.with_identity(cx, move |_, identity, cx| {
            let (tell, heard) = async_channel::unbounded();
            core::spawn_send(
                identity,
                device.clone(),
                paths.clone(),
                pin.clone(),
                cancel,
                move |update| {
                    let _ = tell.try_send(update);
                },
            );
            cx.spawn(async move |this, cx| {
                while let Ok(update) = heard.recv().await {
                    let sent = this.update(cx, |this, cx| {
                        this.sending(key, &device, &paths, pin.is_some(), update, cx)
                    });
                    if sent.is_err() {
                        break;
                    }
                }
            })
            .detach();
        });
    }

    fn sending(
        &mut self,
        key: u64,
        device: &Device,
        paths: &[PathBuf],
        gave_pin: bool,
        update: Update,
        cx: &mut Context<Self>,
    ) {
        let transfer = self.transfers.iter_mut().find(|t| t.key == key);
        match (update, transfer) {
            (Update::Progress(Progress::Waiting), Some(transfer)) => transfer.waiting = true,
            (Update::Progress(Progress::Sending { sent, total }), Some(transfer)) => {
                transfer.waiting = false;
                transfer.done = sent;
                transfer.total = total;
            }
            (Update::Progress(_), None) => {}
            (Update::Done(result), _) => {
                self.transfers.retain(|t| t.key != key);
                let to = &device.alias;
                match result {
                    Ok(sent) => {
                        let what = match paths {
                            [one] => one
                                .file_name()
                                .map(|name| format!("\"{}\"", name.to_string_lossy()))
                                .unwrap_or_default(),
                            items => count(items.len(), "item"),
                        };
                        let declined = match sent.declined {
                            0 => String::new(),
                            n => format!("; it declined {}", count(n, "file")),
                        };
                        self.say(format!("Sent {what} to {to}{declined}"), false, None, cx);
                    }
                    // Settings turned LocalSend off since: nobody asks.
                    Err(SendError::Pin { .. }) if !cx.global::<Preferences>().localsend => {}
                    Err(SendError::Pin { wrong }) => {
                        self.pin_wanted = Some(PinWanted {
                            device: device.clone(),
                            paths: paths.to_vec(),
                            wrong: wrong || gave_pin,
                        })
                    }
                    Err(SendError::Cancelled) => {
                        self.say(format!("Stopped sending to {to}"), false, None, cx);
                    }
                    Err(e) => self.say(e.says(to), true, None, cx),
                }
            }
        }
        cx.notify();
    }

    /// Lists devices nearby while any pane shows the LocalSend page. Each
    /// call is matched by one `stop_looking`.
    pub fn look(&mut self, cx: &mut Context<Self>) {
        self.lookers += 1;
        if self.lookers > 1 {
            return;
        }
        self.scanning = true;
        if let Some(receiver) = &self.receiver {
            receiver.announce();
        }
        self.with_identity(cx, |this, identity, cx| {
            if this.lookers == 0 || this.nearby.is_some() {
                return;
            }
            let (tell, heard) = async_channel::unbounded();
            this.nearby = Some(Nearby::start(identity, move |devices, scanning| {
                let _ = tell.try_send((devices.to_vec(), scanning));
            }));
            cx.spawn(async move |this, cx| {
                while let Ok((devices, scanning)) = heard.recv().await {
                    let shown = this.update(cx, |this, cx| {
                        if this.nearby.is_some() {
                            this.devices = devices;
                            this.scanning = scanning;
                            cx.notify();
                        }
                    });
                    if shown.is_err() {
                        break;
                    }
                }
            })
            .detach();
        });
    }

    pub fn stop_looking(&mut self) {
        self.lookers = self.lookers.saturating_sub(1);
        if self.lookers == 0 {
            self.nearby = None;
            self.devices.clear();
            self.scanning = false;
        }
    }

    /// Looks again: asks the subnets, and announces this Mac if it's
    /// receiving.
    pub fn scan(&mut self, cx: &mut Context<Self>) {
        if let Some(nearby) = &mut self.nearby {
            nearby.scan();
        }
        if let Some(receiver) = &self.receiver {
            receiver.announce();
        }
        cx.notify();
    }
}

fn offer_tag(id: &str) -> String {
    format!("{OFFER_TAG}{id}")
}

/// Gets a person to an offer: AirDrop's chime, the app in front, and a
/// notification that answers it, since macOS may keep another app in front.
fn call_over(offer: &Offer, cx: &mut App) {
    if frame::unattended() {
        return;
    }
    feedback::sound(Sound::Invite, cx);
    if !frame::in_front() {
        let (what, take) = match offer.text {
            Some(_) => ("text".to_string(), "Copy to Clipboard"),
            None => (count(offer.files.len(), "file"), "Accept"),
        };
        let action = |id: &str, label: &str| SystemNotificationAction {
            id: id.to_owned().into(),
            label: label.to_owned().into(),
        };
        cx.show_system_notification(SystemNotification {
            tag: offer_tag(&offer.id).into(),
            title: "LocalSend".into(),
            body: format!("{} wants to send you {what}", offer.from).into(),
            actions: vec![action(DECLINE, "Decline"), action(ACCEPT, take)],
        });
    }
    frame::bring_forward(cx);
}

/// Stops the receivers waiting in `retiring`, and waits for their ports.
fn stop_retired(retiring: &Mutex<Vec<Receiver>>) {
    let mut retired = retiring.lock().unwrap_or_else(PoisonError::into_inner);
    for old in retired.drain(..) {
        old.stop();
    }
}

/// "1 item", "3 items".
pub fn count(n: usize, what: &str) -> String {
    match n {
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
}
