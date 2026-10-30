//! Connect to Server (⌘K), in the palette's card: an address, the saved
//! servers (a click fills the address, ✕ forgets one), Cancel and Connect.
//! macOS asks for any credentials and keeps them in the Keychain, so none
//! pass through here. Connecting only mounts; the share then shows in the
//! sidebar, and opening it is a separate step.

use crate::browser::Places;
use crate::palette::{self, Look};
use crate::text_field::{self, TextField};
use crate::theme::{accent, content, danger, icon, on_accent};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Role,
    Task, Window, actions, div, prelude::*, px,
};
use monodrive_core::servers::{self, Cancel};
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
    Connected { address: String, mount: PathBuf },
}

const ROW: f32 = 28.;

pub struct ConnectView {
    pub(crate) field: Entity<TextField>,
    places: Places,
    saved: Vec<String>,
    /// The saved server the arrows are on.
    selected: Option<usize>,
    pub(crate) error: Option<String>,
    /// Stops the mount in flight when the card goes away.
    connecting: Option<Arc<Cancel>>,
    work: Task<()>,
}

impl EventEmitter<ConnectEvent> for ConnectView {}

impl Focusable for ConnectView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

impl ConnectView {
    pub fn new(places: Places, saved: Vec<String>, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| TextField::new("Server address", "smb://nas.local/media", cx));
        cx.subscribe(&field, |this, _, _: &text_field::Changed, cx| {
            this.error = None;
            cx.notify();
        })
        .detach();
        Self {
            field,
            places,
            saved,
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
        let forgotten = cx
            .background_executor()
            .spawn(async move { store.forget_server(&address) });
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
    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting.is_some() {
            return;
        }
        let typed = self.field.read(cx).text().to_owned();
        if let Err(why) = servers::address(&typed) {
            self.error = Some(why);
            cx.notify();
            return;
        }
        let cancel = Arc::new(Cancel::default());
        self.connecting = Some(cancel.clone());
        self.error = None;
        cx.notify();
        let places = self.places.clone();
        let done = cx.background_executor().spawn(async move {
            servers::connect(&places.store, &typed, |address| {
                places.drives.mount(address, &cancel)
            })
        });
        self.work = cx.spawn(async move |this, cx| {
            let done = done.await;
            let _ = this.update(cx, |this, cx| {
                this.connecting = None;
                match done {
                    Ok((address, mount)) => cx.emit(ConnectEvent::Connected { address, mount }),
                    Err(why) => this.error = Some(why),
                }
                cx.notify();
            });
        });
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.connecting.take() {
            cancel.cancel();
        }
        cx.emit(ConnectEvent::Dismissed);
    }
}

impl Render for ConnectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.saved.iter().enumerate().map(|(ix, address)| {
            let selected = self.selected == Some(ix);
            div()
                .id(("saved-server", ix))
                .debug_selector(move || format!("saved-server-{ix}"))
                .role(Role::ListBoxOption)
                .aria_label(address.clone())
                .aria_selected(selected)
                .h(px(ROW))
                .pl_3()
                .pr_1()
                .rounded(px(7.))
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(13.))
                .cursor_pointer()
                .map(|d| {
                    if selected {
                        d.bg(accent()).text_color(on_accent())
                    } else {
                        d.text_color(content(0.85)).hover(|s| s.bg(content(0.08)))
                    }
                })
                .child(div().flex_1().min_w_0().truncate().child(address.clone()))
                .child(
                    div()
                        .id(("forget-server", ix))
                        .debug_selector(move || format!("forget-server-{ix}"))
                        .role(Role::Button)
                        .aria_label("Forget this server")
                        .occlude()
                        .size(px(20.))
                        .flex_shrink_0()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .justify_center()
                        .hover(|s| s.bg(content(0.12)))
                        .child(icon("cancel-01", 12.).text_color(if selected {
                            on_accent()
                        } else {
                            content(0.55)
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.forget(ix, cx))),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.fill(ix, cx)))
        });
        let connecting = self.connecting.is_some();
        let body = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .text_size(px(12.))
                    .text_color(content(0.5))
                    .child(if self.saved.is_empty() {
                        "A server you connect to is saved here. macOS asks for any password and keeps it in the Keychain."
                    } else {
                        "Saved servers"
                    }),
            )
            .children(rows)
            .children(self.error.clone().map(|why| {
                div()
                    .debug_selector(|| "connect-error".into())
                    .px_3()
                    .py_1()
                    .text_size(px(12.))
                    .text_color(danger())
                    .child(why)
            }))
            .child(
                div()
                    .pt_1()
                    .px_1()
                    .pb_1()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        palette::button("connect-cancel", "Cancel", Look::Plain)
                            .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
                    )
                    .child(
                        palette::button(
                            "connect-go",
                            if connecting { "Connecting…" } else { "Connect" },
                            if connecting {
                                Look::Disabled
                            } else {
                                Look::Default
                            },
                        )
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
