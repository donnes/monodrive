//! Connect to Server (⇧⌘K), in the palette's card: an address, the saved
//! servers (a click fills the address, ✕ forgets one), Cancel and Connect.
//! An smb:// share mounts, and macOS asks for any credentials and keeps them
//! in the Keychain, so none pass through here; the share then shows in the
//! sidebar. An sftp:// or ssh:// server connects with the user's ssh keys and
//! opens in the pane, and is saved only once it has connected.

use crate::browser::Places;
use crate::palette;
use crate::text_field::{self, TextField};
use crate::theme::{
    self, ButtonLook, content, danger, on_accent, row, row_icon_button, text_button,
};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Role,
    Task, Window, actions, div, prelude::*, px,
};
use monodrive_core::remote::Source;
use monodrive_core::servers::{self, Cancel, Server};
use std::path::PathBuf;
use std::sync::Arc;

actions!(connect, [Up, Down, Go, Dismiss]);

pub fn bind(cx: &mut App) {
    let context = Some("Connect");
    cx.bind_keys([
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("enter", Go, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

pub enum ConnectEvent {
    Dismissed,
    Connected {
        address: String,
        mount: PathBuf,
    },
    /// An SFTP server connected, and is saved as this source.
    ConnectedServer(String),
}

pub struct ConnectView {
    pub(crate) field: Entity<TextField>,
    places: Places,
    /// Saved addresses: smb:// shares, then SFTP servers as sftp:// ones.
    saved: Vec<String>,
    /// The saved SFTP servers, by address, for forgetting them.
    remotes: Vec<(String, String)>,
    /// The saved server the arrows are on.
    selected: Option<usize>,
    pub(crate) error: Option<String>,
    /// Stops the mount in flight when the card goes away.
    pub(crate) connecting: Option<Arc<Cancel>>,
    work: Task<()>,
}

impl EventEmitter<ConnectEvent> for ConnectView {}

impl Focusable for ConnectView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

impl ConnectView {
    pub fn new(
        places: Places,
        mut saved: Vec<String>,
        remotes: &[Source],
        cx: &mut Context<Self>,
    ) -> Self {
        let field = cx
            .new(|cx| TextField::new("Server address", "smb://nas.local/media or sftp://host", cx));
        let remotes: Vec<(String, String)> = remotes
            .iter()
            .map(|source| (sftp_address(source), source.id.clone()))
            .collect();
        saved.extend(remotes.iter().map(|(address, _)| address.clone()));
        cx.subscribe(&field, |this, _, _: &text_field::Changed, cx| {
            this.error = None;
            cx.notify();
        })
        .detach();
        Self {
            field,
            places,
            saved,
            remotes,
            selected: None,
            error: None,
            connecting: None,
            work: Task::ready(()),
        }
    }

    fn fill(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(address) = self.saved.get(ix) else {
            return;
        };
        let end = address.len();
        self.field
            .update(cx, |field, cx| field.set_text(address, end..end, cx));
        self.selected = Some(ix);
        self.error = None;
        cx.notify();
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(last) = self.saved.len().checked_sub(1) else {
            return;
        };
        let ix = match (self.selected, down) {
            (None, true) => 0,
            (None, false) => last,
            (Some(ix), true) => (ix + 1).min(last),
            (Some(ix), false) => ix.saturating_sub(1),
        };
        self.fill(ix, cx);
    }

    /// Forgets a saved server. A share it mounted stays mounted.
    fn forget(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.saved.len() {
            return;
        }
        let address = self.saved.remove(ix);
        self.selected = None;
        cx.notify();
        let store = self.places.store.clone();
        let remote = self
            .remotes
            .iter()
            .find(|(saved, _)| *saved == address)
            .map(|(_, id)| id.clone());
        if let Some(id) = &remote {
            crate::browser::remote::disconnect(id, cx);
        }
        let forgotten = cx.background_executor().spawn(async move {
            match remote {
                Some(id) => store.forget_remote(&id).map(drop),
                None => store.forget_server(&address),
            }
        });
        cx.spawn(async move |this, cx| {
            if let Err(why) = forgotten.await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(why);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Mounts what's typed off the UI thread. The address saves only once
    /// it's connected; a failure stays in the card.
    pub(crate) fn connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting.is_some() {
            return;
        }
        let typed = self.field.read(cx).text().to_owned();
        let server = match servers::server(&typed) {
            Ok(server) => server,
            Err(why) => {
                self.error = Some(why);
                cx.notify();
                return;
            }
        };
        let cancel = Arc::new(Cancel::default());
        self.connecting = Some(cancel.clone());
        self.error = None;
        cx.notify();
        let places = self.places.clone();
        let done = cx.background_executor().spawn(async move {
            match server {
                Server::Smb(_) => servers::connect(&places.store, &typed, &cancel, |address| {
                    places.drives.mount(address, &cancel)
                })
                .map(|(address, mount)| ConnectEvent::Connected { address, mount }),
                Server::Sftp { host, start } => places
                    .remote
                    .add(&host, start.as_deref(), || cancel.is_cancelled())
                    .map(|source| ConnectEvent::ConnectedServer(source.id))
                    .map_err(String::from),
            }
        });
        self.work = cx.spawn(async move |this, cx| {
            let done = done.await;
            let _ = this.update(cx, |this, cx| {
                this.connecting = None;
                match done {
                    Ok(event) => cx.emit(event),
                    Err(why) => this.error = Some(why),
                }
                cx.notify();
            });
        });
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(ConnectEvent::Dismissed);
    }
}

/// However the card goes (Esc, another sheet, the window closing), a mount
/// it started goes with it.
impl Drop for ConnectView {
    fn drop(&mut self) {
        if let Some(cancel) = self.connecting.take() {
            cancel.cancel();
        }
    }
}

impl Render for ConnectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.saved.iter().enumerate().map(|(ix, address)| {
            let selected = self.selected == Some(ix);
            theme::row_slot(("saved-server", ix))
                .role(Role::ListBoxOption)
                .aria_label(address.clone())
                .aria_selected(selected)
                .on_click(cx.listener(move |this, _, _, cx| this.fill(ix, cx)))
                .child(
                    row(selected)
                        .debug_selector(move || format!("saved-server-{ix}"))
                        .pr_1()
                        .child(div().flex_1().min_w_0().truncate().child(address.clone()))
                        .child(
                            theme::tip(
                                row_icon_button(
                                    ("forget-server", ix),
                                    "Forget this server",
                                    "cancel-01",
                                    if selected { on_accent() } else { content(0.55) },
                                ),
                                "Forget this server",
                            )
                            .debug_selector(move || format!("forget-server-{ix}"))
                            .occlude()
                            .on_click(cx.listener(move |this, _, _, cx| this.forget(ix, cx))),
                        ),
                )
        });
        let connecting = self.connecting.is_some();
        let body = div()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(if self.saved.is_empty() {
                        "A server you connect to is saved here. macOS asks for an SMB password and keeps it in the Keychain; SFTP uses your ssh keys."
                    } else {
                        "Saved servers"
                    }),
            )
            .children(rows)
            .children(self.error.clone().map(|why| {
                div()
                    .debug_selector(|| "connect-error".into())
                    .px_2()
                    .py_1()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(danger())
                    .child(why)
            }))
            .child(
                div()
                    .pt_2()
                    // On the card's 14 pt inset, as its text is.
                    .px_2()
                    .pb_2()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("connect-cancel", "Cancel", ButtonLook::Secondary)
                            .debug_selector(|| "connect-cancel".into())
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
                    )
                    .child(
                        text_button(
                            "connect-go",
                            if connecting { "Connecting…" } else { "Connect" },
                            if connecting {
                                ButtonLook::Disabled
                            } else {
                                ButtonLook::Primary
                            },
                        )
                        .debug_selector(|| "connect-go".into())
                        .when(!connecting, |d| {
                            d.on_click(cx.listener(|this, _, _, cx| this.connect(cx)))
                        }),
                    ),
            );
        palette::overlay("connect-overlay")
            .key_context("Connect")
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &Go, _, cx| this.connect(cx)))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss(cx)),
            )
            .child(palette::card(
                "connect",
                "Connect to Server",
                self.field.clone(),
                body,
            ))
    }
}

/// How a saved SFTP server is written as an address.
fn sftp_address(source: &Source) -> String {
    format!(
        "sftp://{}{}",
        source.host,
        source.start.as_deref().unwrap_or_default()
    )
}
