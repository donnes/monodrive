//! Receiving: LocalSend's server and announcements, and what becomes of
//! each offer and file. A person answers every offer; nothing is accepted on
//! its own. Files land under a hidden name in the save folder and take a
//! free name only once whole, so a partial file never sits under a real one.

use super::{Device, Identity, Kind, PORT, RECEIVING_ON, runtime, tidy};
use crate::disk;
use localsend::discovery::{self, DiscoveredDevice, DiscoveryConfig, DiscoveryEvent};
use localsend::http::dto_v2::RegisterDtoV2;
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2, SessionEndReasonV2};
use localsend::http::server::web::WebConfig;
use localsend::http::server::{ServerConfigV2, ServerHandle, start_with_port};
use localsend::model::discovery::ProtocolType;
use localsend::model::transfer::FileDto;
use localsend::util::filename::{Rules, sanitize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// How often a transfer says how far it's got.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Someone asking to send this Mac files.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub id: String,
    pub from: String,
    pub kind: Kind,
    pub model: Option<String>,
    /// Names as the sender gave them, `Photos/a.jpg` for a folder's, with
    /// their sizes, by name.
    pub files: Vec<(String, u64)>,
    pub bytes: u64,
    /// What a message says. Accepting one writes no file.
    pub text: Option<String>,
}

/// How a transfer ended.
#[derive(Clone, Debug, PartialEq)]
pub struct Ended {
    pub from: String,
    /// What arrived, each folder once, in the order it arrived.
    pub received: Vec<PathBuf>,
    /// Files that didn't, with why.
    pub failed: Vec<(String, String)>,
    /// The sender or this Mac stopped it.
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A person must answer it with `Receiver::answer`.
    Offer(Offer),
    /// The sender gave up before anyone answered.
    Withdrawn(String),
    Started {
        id: String,
        from: String,
        files: usize,
        bytes: u64,
    },
    Progress {
        id: String,
        done: u64,
        total: u64,
    },
    Ended {
        id: String,
        ended: Ended,
    },
    /// A device that reached this Mac.
    Seen(Device),
    /// The server stopped by itself, for this reason.
    Stopped(String),
}

type Tell = Box<dyn Fn(Event) + Send + Sync>;

/// Where a sender listens, to tell it this Mac cancelled.
#[derive(Clone)]
struct Peer {
    host: String,
    port: u16,
    https: bool,
    fingerprint: String,
}

struct Pending {
    decision: oneshot::Sender<PrepareUploadDecisionV2>,
    from: String,
    peer: Peer,
    files: HashMap<String, FileDto>,
}

/// Where one accepted file goes.
#[derive(Clone)]
struct Planned {
    /// As the sender named it.
    name: String,
    temp: PathBuf,
    to: PathBuf,
    /// What the folder shows for it: `to`, or the folder it's in.
    top: PathBuf,
    size: u64,
}

struct Transfer {
    from: String,
    peer: Peer,
    planned: HashMap<String, Planned>,
    total: u64,
    done: HashMap<String, u64>,
    /// Uploads under way.
    open: usize,
    received: Vec<PathBuf>,
    /// Why each file that didn't arrive didn't, by id: a retry that lands
    /// takes its file off.
    failed: BTreeMap<String, (String, String)>,
    /// Uploads started, which name each one's own hidden file.
    attempts: u32,
    /// Files that arrived, by id.
    landed: HashSet<String>,
    /// Set once the session is over, to whether it was cancelled.
    ended: Option<bool>,
    /// This Mac cancelled it: what still arrives goes to the Trash.
    stopped: bool,
    told: Option<Instant>,
}

#[derive(Default)]
struct State {
    offers: HashMap<String, Pending>,
    transfers: HashMap<String, Transfer>,
}

/// What becomes of the server's events. Separate from the server, so tests
/// drive it with the events the server sends.
pub(super) struct Inbox {
    fingerprint: String,
    save_to: Mutex<PathBuf>,
    tell: Tell,
    state: Mutex<State>,
}

impl Inbox {
    fn save_to(&self, folder: PathBuf) {
        if let Ok(mut save_to) = self.save_to.lock() {
            *save_to = folder;
        }
    }

    pub(super) fn new(
        fingerprint: String,
        save_to: PathBuf,
        tell: impl Fn(Event) + Send + Sync + 'static,
    ) -> Arc<Inbox> {
        Arc::new(Inbox {
            fingerprint,
            save_to: Mutex::new(save_to),
            tell: Box::new(tell),
            state: Mutex::default(),
        })
    }

    pub(super) fn handle(self: &Arc<Self>, event: ServerEventV2) {
        match event {
            ServerEventV2::Register { ip, info } => {
                if let Some(device) = self.device(ip.to_string(), &info) {
                    (self.tell)(Event::Seen(device));
                }
            }
            ServerEventV2::PrepareUpload {
                session_id,
                ip,
                info,
                cert_fingerprint,
                files,
                decision_tx,
            } => self.offered(
                session_id,
                ip.to_string(),
                info,
                cert_fingerprint,
                files,
                decision_tx,
            ),
            ServerEventV2::FileUpload {
                session_id,
                file_id,
                target_tx,
                ..
            } => self.upload(session_id, file_id, target_tx),
            ServerEventV2::SessionEnd { session_id, reason } => {
                if let Ok(mut state) = self.state.lock()
                    && let Some(transfer) = state.transfers.get_mut(&session_id)
                {
                    transfer.ended = Some(reason == SessionEndReasonV2::Cancelled);
                }
                self.end_if_done(&session_id);
            }
            ServerEventV2::PrepareUploadAborted { session_id } => {
                let withdrawn = self
                    .state
                    .lock()
                    .is_ok_and(|mut state| state.offers.remove(&session_id).is_some());
                if withdrawn {
                    (self.tell)(Event::Withdrawn(session_id));
                } else {
                    self.never_started(&session_id);
                }
            }
            // A receiver stopping what this Mac sends: the upload fails
            // and says so.
            ServerEventV2::CancelReceived { .. } => {}
            ServerEventV2::ListenerFailed { error } => {
                (self.tell)(Event::Stopped(error));
            }
        }
    }

    fn device(&self, host: String, info: &RegisterDtoV2) -> Option<Device> {
        (info.fingerprint != self.fingerprint).then(|| Device {
            alias: tidy(&info.alias),
            fingerprint: info.fingerprint.clone(),
            kind: Kind::of(info.device_type.as_ref()),
            model: info.device_model.as_deref().map(tidy),
            host,
            port: info.port,
            https: info.protocol == ProtocolType::Https,
        })
    }

    fn offered(
        &self,
        id: String,
        host: String,
        info: RegisterDtoV2,
        cert_fingerprint: Option<String>,
        files: HashMap<String, FileDto>,
        decision: oneshot::Sender<PrepareUploadDecisionV2>,
    ) {
        let from = tidy(&info.alias);
        if let Some(device) = self.device(host.clone(), &info) {
            (self.tell)(Event::Seen(device));
        }
        let mut listed: Vec<(String, u64)> = files
            .values()
            .map(|file| (tidy_name(&file.file_name), file.size))
            .collect();
        listed.sort();
        let offer = Offer {
            id: id.clone(),
            from: from.clone(),
            kind: Kind::of(info.device_type.as_ref()),
            model: info.device_model.as_deref().map(tidy),
            bytes: files.values().map(|file| file.size).sum(),
            files: listed,
            text: message(&files),
        };
        let peer = Peer {
            host,
            port: info.port,
            https: info.protocol == ProtocolType::Https,
            fingerprint: cert_fingerprint.unwrap_or(info.fingerprint),
        };
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.offers.insert(
            id,
            Pending {
                decision,
                from,
                peer,
                files,
            },
        );
        drop(state);
        (self.tell)(Event::Offer(offer));
    }

    /// Takes every file of offer `id`, or none.
    pub(super) fn answer(&self, id: &str, accept: bool) {
        let Some(pending) = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.offers.remove(id))
        else {
            return;
        };
        if !accept {
            let _ = pending.decision.send(PrepareUploadDecisionV2::Decline);
            return;
        }
        if message(&pending.files).is_some() {
            // Taking none answers 204, as the LocalSend app does for one.
            let _ = pending
                .decision
                .send(PrepareUploadDecisionV2::Accept(HashSet::new()));
            return;
        }
        let folder = self.save_to.lock().map(|f| f.clone()).unwrap_or_default();
        if let Err(e) = std::fs::create_dir_all(&folder) {
            let _ = pending.decision.send(PrepareUploadDecisionV2::Decline);
            let why = format!("Couldn't make {}: {e}", folder.display());
            return (self.tell)(Event::Ended {
                id: id.to_owned(),
                ended: Ended {
                    from: pending.from,
                    received: Vec::new(),
                    failed: pending
                        .files
                        .values()
                        .map(|file| (tidy_name(&file.file_name), why.clone()))
                        .collect(),
                    cancelled: false,
                },
            });
        }
        let planned = plan(&folder, &pending.files, id);
        let total = planned.values().map(|file| file.size).sum();
        let count = planned.len();
        let ids = planned.keys().cloned().collect();
        // In place, and said, before the answer, which uploads and the
        // sender's leaving follow; the lock keeps the leaving from being
        // said first.
        if let Ok(mut state) = self.state.lock() {
            state.transfers.insert(
                id.to_owned(),
                Transfer {
                    from: pending.from.clone(),
                    peer: pending.peer,
                    planned,
                    total,
                    done: HashMap::new(),
                    open: 0,
                    received: Vec::new(),
                    failed: BTreeMap::new(),
                    attempts: 0,
                    landed: HashSet::new(),
                    ended: None,
                    stopped: false,
                    told: None,
                },
            );
            (self.tell)(Event::Started {
                id: id.to_owned(),
                from: pending.from,
                files: count,
                bytes: total,
            });
        }
        if pending
            .decision
            .send(PrepareUploadDecisionV2::Accept(ids))
            .is_err()
        {
            self.never_started(id);
        }
    }

    /// Ends accepted transfer `id`, which the sender gave up on before any
    /// upload.
    fn never_started(&self, id: &str) {
        if let Ok(mut state) = self.state.lock()
            && let Some(transfer) = state.transfers.get_mut(id)
        {
            transfer.ended = Some(true);
        }
        self.end_if_done(id);
    }

    fn upload(
        self: &Arc<Self>,
        id: String,
        file: String,
        target: oneshot::Sender<FileUploadTarget>,
    ) {
        let temp = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(transfer) = state.transfers.get_mut(&id).filter(|t| !t.stopped) else {
                // Dropping the target fails the upload.
                return;
            };
            let Some(planned) = transfer.planned.get(&file) else {
                return;
            };
            let mut temp = planned.temp.clone().into_os_string();
            temp.push(format!("-{}", transfer.attempts));
            transfer.attempts += 1;
            transfer.open += 1;
            PathBuf::from(temp)
        };
        let (progress_tx, mut progress) = mpsc::channel(16);
        let (result_tx, result) = oneshot::channel();
        let sent = target.send(FileUploadTarget::Path {
            path: temp.clone(),
            result_tx,
            progress_tx: Some(progress_tx),
        });
        let inbox = self.clone();
        let (id_, file_) = (id.clone(), file.clone());
        runtime().spawn(async move {
            while let Some(bytes) = progress.recv().await {
                inbox.progress(&id_, &file_, bytes);
            }
        });
        let inbox = self.clone();
        runtime().spawn(async move {
            let result = match (sent, result.await) {
                (Ok(()), Ok(result)) => result,
                _ => Err("The upload stopped.".to_owned()),
            };
            inbox.arrived(&id, &file, &temp, result);
        });
    }

    fn progress(&self, id: &str, file: &str, bytes: u64) {
        let said = {
            let Ok(mut state) = self.state.lock() else {
                return;
            };
            let Some(transfer) = state.transfers.get_mut(id) else {
                return;
            };
            transfer.done.insert(file.to_owned(), bytes);
            let done: u64 = transfer.done.values().sum();
            let due = transfer
                .told
                .is_none_or(|told| told.elapsed() >= PROGRESS_EVERY);
            if !due && done < transfer.total {
                return;
            }
            transfer.told = Some(Instant::now());
            Event::Progress {
                id: id.to_owned(),
                done,
                total: transfer.total,
            }
        };
        (self.tell)(said);
    }

    fn arrived(&self, id: &str, file: &str, temp: &Path, result: Result<(), String>) {
        let Some((planned, stopped)) = self.state.lock().ok().and_then(|state| {
            let transfer = state.transfers.get(id)?;
            Some((transfer.planned.get(file)?.clone(), transfer.stopped))
        }) else {
            return;
        };
        let outcome = match result {
            Ok(()) if stopped => Err("Cancelled.".to_owned()),
            Ok(()) => place(&planned, temp),
            Err(why) => Err(why),
        };
        if outcome.is_err() && temp.symlink_metadata().is_ok() {
            let _ = disk::trash(temp);
        }
        if let Ok(mut state) = self.state.lock()
            && let Some(transfer) = state.transfers.get_mut(id)
        {
            // Only now, with what it came to recorded, is it no longer open.
            transfer.open = transfer.open.saturating_sub(1);
            match outcome {
                Ok(top) => {
                    transfer.failed.remove(file);
                    transfer.landed.insert(file.to_owned());
                    transfer.done.insert(file.to_owned(), planned.size);
                    if !transfer.received.contains(&top) {
                        transfer.received.push(top);
                    }
                }
                // An older attempt that failed after a retry landed changes
                // nothing.
                Err(why) if !stopped && !transfer.landed.contains(file) => {
                    transfer
                        .failed
                        .insert(file.to_owned(), (tidy_name(&planned.name), why));
                }
                Err(_) => {}
            }
        }
        self.end_if_done(id);
    }

    /// Stops transfer `id` from this side, and says where its sender is.
    fn stop(&self, id: &str) -> Option<Peer> {
        let peer = {
            let mut state = self.state.lock().ok()?;
            let transfer = state.transfers.get_mut(id)?;
            transfer.stopped = true;
            transfer.ended = Some(true);
            transfer.peer.clone()
        };
        self.end_if_done(id);
        Some(peer)
    }

    fn end_if_done(&self, id: &str) {
        let Some(transfer) = self.state.lock().ok().and_then(|mut state| {
            let transfer = state.transfers.get(id)?;
            if transfer.ended.is_none() || transfer.open > 0 {
                return None;
            }
            state.transfers.remove(id)
        }) else {
            return;
        };
        (self.tell)(Event::Ended {
            id: id.to_owned(),
            ended: Ended {
                from: transfer.from,
                received: transfer.received,
                failed: transfer.failed.into_values().collect(),
                cancelled: transfer.ended == Some(true),
            },
        });
    }
}

/// The text of a message: LocalSend sends one as a single text file whose
/// preview holds all of it.
fn message(files: &HashMap<String, FileDto>) -> Option<String> {
    let mut files = files.values();
    match (files.next(), files.next()) {
        (Some(file), None) if file.file_type.split('/').next() == Some("text") => {
            file.preview.clone()
        }
        _ => None,
    }
}

/// A sender's name for a file, safe to show on one line.
fn tidy_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// A sender's name as folders and a file, each part safe for APFS and none
/// leading up and out.
fn parts(name: &str) -> Vec<String> {
    let parts: Vec<String> = name
        .split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .map(|part| sanitize(part, Rules::Hfs))
        .collect();
    if parts.is_empty() {
        vec![sanitize("", Rules::Hfs)]
    } else {
        parts
    }
}

/// Where each file of an accepted offer goes in `folder`. Each item at the
/// top takes a free name, Finder's `name 2` when its own is taken, and a
/// folder's files follow it there.
fn plan(folder: &Path, files: &HashMap<String, FileDto>, id: &str) -> HashMap<String, Planned> {
    let mut ordered: Vec<(&String, &FileDto)> = files.iter().collect();
    ordered.sort_by(|a, b| a.1.file_name.cmp(&b.1.file_name).then(a.0.cmp(b.0)));
    let short: String = id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let mut taken = HashSet::new();
    let mut tops: HashMap<(String, bool), PathBuf> = HashMap::new();
    ordered
        .into_iter()
        .enumerate()
        .map(|(ix, (file_id, file))| {
            let parts = parts(&file.file_name);
            let in_folder = parts.len() > 1;
            let top = tops
                .entry((parts[0].clone(), in_folder))
                .or_insert_with(|| free(folder.join(&parts[0]), &mut taken))
                .clone();
            let to = parts[1..]
                .iter()
                .fold(top.clone(), |path, part| path.join(part));
            let planned = Planned {
                name: file.file_name.clone(),
                temp: folder.join(format!(".monodrive-localsend-{short}-{ix}")),
                to,
                top,
                size: file.size,
            };
            (file_id.clone(), planned)
        })
        .collect()
}

/// `path`, or its first free `name 2`, counting names this offer took.
fn free(path: PathBuf, taken: &mut HashSet<PathBuf>) -> PathBuf {
    let open =
        |path: &Path, taken: &HashSet<PathBuf>| disk::id(path).is_none() && !taken.contains(path);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let chosen = if open(&path, taken) {
        path
    } else {
        (2..)
            .map(|n| path.with_file_name(disk::numbered_name(&name, n)))
            .find(|path| open(path, taken))
            .unwrap_or(path)
    };
    taken.insert(chosen.clone());
    chosen
}

/// Moves a whole file from its hidden name into place, never over anything,
/// and marks it as from the network as browsers do. Says what the folder
/// shows for it.
fn place(planned: &Planned, temp: &Path) -> Result<PathBuf, String> {
    let fail = |e: std::io::Error| format!("Couldn't save {}: {e}", planned.to.display());
    if let Some(parent) = planned.to.parent() {
        std::fs::create_dir_all(parent).map_err(fail)?;
    }
    quarantine(temp);
    let mut to = planned.to.clone();
    if let Err(e) = disk::rename_new(temp, &to) {
        if e.kind() != std::io::ErrorKind::AlreadyExists {
            return Err(fail(e));
        }
        to = disk::free_name(&to);
        disk::rename_new(temp, &to).map_err(fail)?;
    }
    Ok(if planned.top == planned.to {
        to
    } else {
        planned.top.clone()
    })
}

/// What Gatekeeper reads to ask before an app or script from the network
/// first runs.
fn quarantine(path: &Path) {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let value = format!("0081;{now:08x};Monodrive;");
    // SAFETY: both strings are NUL-terminated and outlive the call, and the
    // value is `value.len()` bytes long.
    unsafe {
        libc::setxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            libc::XATTR_NOFOLLOW,
        );
    }
}

/// Receiving, until dropped: the server, the announcements, and the inbox.
pub struct Receiver {
    identity: Arc<Identity>,
    inbox: Arc<Inbox>,
    server: Arc<ServerHandle>,
    discovery: Arc<discovery::DiscoveryHandle>,
    stops: Vec<oneshot::Sender<()>>,
    port: u16,
}

impl Receiver {
    /// Starts receiving into `save_to`, asking senders for `pin` if it's
    /// set. Takes LocalSend's port, or one macOS picks while another app
    /// (the LocalSend app) has it.
    pub fn start(
        identity: Arc<Identity>,
        save_to: PathBuf,
        pin: Option<String>,
        tell: impl Fn(Event) + Send + Sync + 'static,
    ) -> Result<Receiver, String> {
        let inbox = Inbox::new(identity.fingerprint.clone(), save_to, tell);
        runtime().block_on(async {
            let (events, mut hearing) = mpsc::channel(64);
            let mut stops = Vec::new();
            let mut server = None;
            for port in [PORT, 0] {
                let (stop, stopped) = oneshot::channel();
                let started = start_with_port(
                    port,
                    Some(identity.tls()),
                    identity.info(),
                    None,
                    Some(ServerConfigV2 {
                        pin: pin.clone(),
                        verify_checksums: true,
                        event_tx: events.clone(),
                    }),
                    WebConfig::default(),
                    stopped,
                )
                .await;
                match started {
                    Ok(handle) => {
                        stops.push(stop);
                        server = Some(handle);
                        break;
                    }
                    Err(e) if port == 0 => {
                        return Err(format!("Couldn't start receiving: {e:#}"));
                    }
                    Err(_) => {}
                }
            }
            let server = Arc::new(server.ok_or("Couldn't start receiving.")?);
            let port = server.port();

            let (found, mut finding) = mpsc::channel(64);
            let (stop, stopped) = oneshot::channel();
            stops.push(stop);
            let discovery = Arc::new(
                discovery::start(
                    DiscoveryConfig {
                        group: localsend::multicast::DEFAULT_MULTICAST_GROUP,
                        group_v6: Some(localsend::multicast::DEFAULT_MULTICAST_GROUP_V6),
                        port: PORT,
                        interface_filter: Default::default(),
                        device: identity.multicast(port),
                        identity: identity.device_identity(),
                        timeout: discovery::DEFAULT_DISCOVERY_TIMEOUT,
                        event_tx: Some(found),
                    },
                    stopped,
                )
                .await,
            );

            let (inbox_, discovery_) = (inbox.clone(), discovery.clone());
            tokio::spawn(async move {
                while let Some(event) = hearing.recv().await {
                    if let ServerEventV2::Register { ip, info } = &event {
                        discovery_
                            .observe_inbound(DiscoveredDevice {
                                alias: info.alias.clone(),
                                version: info.version.clone(),
                                device_model: info.device_model.clone(),
                                device_type: info.device_type.clone(),
                                fingerprint: info.fingerprint.clone(),
                                channel: discovery::DeviceChannel::Http(discovery::HttpChannel {
                                    host: ip.to_string(),
                                    port: info.port,
                                    protocol: info.protocol,
                                }),
                                download: info.download,
                            })
                            .await;
                    }
                    if let ServerEventV2::ListenerFailed { .. } = &event {
                        let _ = RECEIVING_ON.compare_exchange(
                            port,
                            0,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        );
                    }
                    inbox_.handle(event);
                }
            });
            let inbox_ = inbox.clone();
            tokio::spawn(async move {
                while let Some(event) = finding.recv().await {
                    if let DiscoveryEvent::Discovered { device }
                    | DiscoveryEvent::Updated { device } = event
                        && let Some(device) = seen(&inbox_, device)
                    {
                        (inbox_.tell)(Event::Seen(device));
                    }
                }
            });
            let discovery_ = discovery.clone();
            tokio::spawn(async move { discovery_.announce().await });
            RECEIVING_ON.store(port, Ordering::Relaxed);
            Ok(Receiver {
                identity,
                inbox,
                server,
                discovery,
                stops,
                port,
            })
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Tells devices nearby this Mac is here, which they answer.
    pub fn announce(&self) {
        let discovery = self.discovery.clone();
        runtime().spawn(async move { discovery.announce().await });
    }

    /// Accepts or declines offer `id`. Accepting reads and makes folders,
    /// so it runs on LocalSend's thread.
    pub fn answer(&self, id: &str, accept: bool) {
        let (inbox, id) = (self.inbox.clone(), id.to_owned());
        runtime().spawn_blocking(move || inbox.answer(&id, accept));
    }

    /// Where the next accepted offer goes.
    pub fn save_to(&self, folder: PathBuf) {
        self.inbox.save_to(folder);
    }

    /// Stops transfer `id`: what's arrived stays, the rest goes to the Trash.
    pub fn cancel(&self, id: &str) {
        let Some(peer) = self.inbox.stop(id) else {
            return;
        };
        let (server, identity, id) = (self.server.clone(), self.identity.clone(), id.to_owned());
        runtime().spawn(async move {
            server.cancel_v2_session(&id).await;
            let pinned = peer.https.then_some(peer.fingerprint.as_str());
            if let Ok(client) = identity.client(pinned, Some(Duration::from_secs(5))) {
                let protocol = if peer.https {
                    ProtocolType::Https
                } else {
                    ProtocolType::Http
                };
                let _ = client.cancel(protocol, &peer.host, peer.port, &id).await;
            }
        });
    }
}

impl Receiver {
    /// Stops receiving and waits until the ports are free, so receiving can
    /// start again on the same one.
    pub fn stop(mut self) {
        self.stop_now();
        let (server, discovery) = (self.server.clone(), self.discovery.clone());
        runtime().block_on(async move {
            server.wait_stopped().await;
            discovery.wait_stopped().await;
        });
    }

    fn stop_now(&mut self) {
        let _ = RECEIVING_ON.compare_exchange(self.port, 0, Ordering::Relaxed, Ordering::Relaxed);
        for stop in self.stops.drain(..) {
            let _ = stop.send(());
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop_now();
    }
}

fn seen(inbox: &Inbox, device: DiscoveredDevice) -> Option<Device> {
    let http = device.http()?.clone();
    (device.fingerprint != inbox.fingerprint).then(|| Device {
        alias: tidy(&device.alias),
        fingerprint: device.fingerprint,
        kind: Kind::of(device.device_type.as_ref()),
        model: device.device_model.as_deref().map(tidy),
        host: http.host,
        port: http.port,
        https: http.protocol == ProtocolType::Https,
    })
}
