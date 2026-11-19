//! The LocalSend page: who this Mac is to others, the devices nearby to drop
//! files on or click to send to, and whether this Mac receives. Laid out as
//! Finder's AirDrop page is.

use super::drag::{Spot, Target, offers};
use super::pane::Pane;
use super::{Browser, Offer, Sheet, format_bytes};
use crate::localsend::{self, Direction, LocalSend, Receiving, Said, count};
use crate::localsend_ask::{Done, OfferView, PinView};
use crate::preferences::{self, Preferences};
use crate::session::Shows;
use crate::theme::{self, ButtonLook, content, danger, text_button};
use gpui::{
    AnyElement, AppContext, Context, Entity, Focusable, FontWeight, Hsla, PathPromptOptions, Role,
    Subscription, Toggled, Window, div, prelude::*, px,
};
use monodrive_core::localsend::{Device, Kind};
use std::path::PathBuf;

/// Looking for devices while the page is on screen.
pub(super) struct LocalSendPage {
    /// Repaints as devices come and go, while looking.
    looking: Option<Subscription>,
    /// What Send with LocalSend chose, for the device clicked next.
    pub(super) sending: Vec<PathBuf>,
    /// Waits for LocalSend to be picked in the share menu on screen.
    pub(super) sharing: gpui::Task<()>,
}

impl Default for LocalSendPage {
    fn default() -> Self {
        Self {
            looking: None,
            sending: Vec::new(),
            sharing: gpui::Task::ready(()),
        }
    }
}

impl LocalSendPage {
    pub(super) fn looking(&self) -> bool {
        self.looking.is_some()
    }
}

/// For a pane going away, which can't be updated any more.
pub(super) fn stop_looking(cx: &mut gpui::App) {
    localsend::get(cx).update(cx, |localsend, _| localsend.stop_looking());
}

impl Pane {
    pub(super) fn showing_localsend(&self) -> bool {
        self.query.is_empty() && self.stop().shows == Shows::LocalSend
    }

    /// Starts looking for devices, once.
    pub(super) fn enter_localsend(&mut self, cx: &mut Context<Self>) {
        if self.localsend.looking.is_some() {
            return;
        }
        let localsend = localsend::get(cx);
        self.localsend.looking = Some(cx.observe(&localsend, |_, _, cx| cx.notify()));
        localsend.update(cx, |localsend, cx| localsend.look(cx));
    }

    /// Stops looking, and lets go of what Send with LocalSend chose.
    pub(super) fn leave_localsend(&mut self, cx: &mut Context<Self>) {
        self.localsend.sending.clear();
        if self.localsend.looking.take().is_some() {
            localsend::get(cx).update(cx, |localsend, _| localsend.stop_looking());
        }
    }

    /// Opens the page holding `paths`, for the device clicked next.
    pub(super) fn send_with_localsend(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_page(Shows::LocalSend, window, cx);
        self.localsend.sending = paths;
        cx.notify();
    }

    /// Sends what was chosen, or asks for files first.
    fn send_to(&mut self, device: Device, window: &mut Window, cx: &mut Context<Self>) {
        let held = std::mem::take(&mut self.localsend.sending);
        if !held.is_empty() {
            return send(device, held, cx);
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Send".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |_, cx| send(device, paths, cx));
        })
        .detach();
    }

    pub(super) fn localsend_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.showing_localsend() {
            return None;
        }
        if !cx.global::<Preferences>().localsend {
            return Some(turned_off().into_any_element());
        }
        let entity = localsend::get(cx);
        let shown = entity.read(cx);
        let body = if shown.devices.is_empty() {
            empty(shown.scanning).into_any_element()
        } else {
            div()
                .id("localsend-devices")
                .flex_1()
                .overflow_y_scroll()
                .p(px(theme::PANE_INSET))
                .flex()
                .flex_wrap()
                .content_start()
                .gap_2()
                .children(
                    shown
                        .devices
                        .clone()
                        .into_iter()
                        .enumerate()
                        .map(|(ix, device)| self.device(ix, device, cx)),
                )
                .into_any_element()
        };
        Some(
            div()
                .id("localsend-page")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.localsend_header(&entity, cx))
                .child(body)
                .child(footer(&entity, cx))
                .into_any_element(),
        )
    }

    fn localsend_header(
        &self,
        entity: &Entity<LocalSend>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let shown = entity.read(cx);
        let me = shown.alias().unwrap_or("This Mac").to_owned();
        let scanning = shown.scanning;
        let held = &self.localsend.sending;
        let (line, name) = match held.as_slice() {
            [] => ("Others see this Mac as".to_string(), me.clone()),
            [one] => (
                "Click a device to send it".to_string(),
                one.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            items => (
                "Click a device to send it".to_string(),
                count(items.len(), "item"),
            ),
        };
        let holding = !held.is_empty();
        div()
            .flex()
            .items_center()
            .gap_3()
            .px(px(theme::PANE_INSET))
            .py_4()
            .border_b_1()
            .border_color(content(0.06))
            .child(avatar(&me))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SMALL))
                            .text_color(content(0.65))
                            .child(line),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(theme::TEXT_TITLE))
                            .font_weight(FontWeight::MEDIUM)
                            .child(name),
                    ),
            )
            .when(holding, |d| {
                d.child(
                    text_button("localsend-let-go", "Cancel", ButtonLook::Secondary).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.localsend.sending.clear();
                            cx.notify();
                        }),
                    ),
                )
            })
            .child(
                text_button(
                    "localsend-scan",
                    "Scan Again",
                    if scanning {
                        ButtonLook::Disabled
                    } else {
                        ButtonLook::Secondary
                    },
                )
                .when(!scanning, |d| {
                    d.on_click(cx.listener(|_, _, _, cx| {
                        localsend::get(cx).update(cx, |localsend, cx| localsend.scan(cx))
                    }))
                }),
            )
    }

    fn device(
        &self,
        ix: usize,
        device: Device,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let spot = Spot::Device(ix);
        let outlined = self.drag.outlines(&spot);
        let name = device.alias.clone();
        let drag = self.drag.clone();
        let target = Target::LocalSend {
            spot,
            device: Box::new(device.clone()),
        };
        let tint = tint_of(&device.fingerprint);
        let detail = device
            .model
            .clone()
            .unwrap_or_else(|| kind_name(device.kind).into());
        let picture = kind_icon(device.kind);
        div()
            .id(("localsend-device", ix))
            .role(Role::Button)
            .aria_label(format!("Send to {name}"))
            .w(px(120.))
            .py_3()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .rounded(px(theme::RADIUS_CARD))
            .cursor_pointer()
            .hover(|s| s.bg(content(0.08)))
            .when(outlined, |d| d.bg(theme::accent().opacity(0.14)))
            .when(self.drag.active(), |d| {
                offers(d, move |_| drag.offer(target.clone()))
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.send_to(device.clone(), window, cx)),
            )
            .child(
                div()
                    .size(px(64.))
                    .rounded_full()
                    .bg(tint.opacity(0.14))
                    .flex()
                    .items_center()
                    .justify_center()
                    // design: a device's picture, sized as AirDrop's people are.
                    .child(theme::icon(picture, 28.).text_color(tint)),
            )
            .child(
                div()
                    .w_full()
                    .px_2()
                    .text_center()
                    .truncate()
                    .text_size(px(theme::TEXT_BODY))
                    .child(name),
            )
            .child(
                div()
                    .w_full()
                    .px_2()
                    .text_center()
                    .truncate()
                    .text_size(px(theme::TEXT_CAPTION))
                    .text_color(content(0.65))
                    .child(detail),
            )
    }
}

fn send(device: Device, paths: Vec<PathBuf>, cx: &mut gpui::App) {
    localsend::get(cx).update(cx, |localsend, cx| localsend.send(device, paths, None, cx));
}

/// Reached through Back after Settings turned LocalSend off.
fn turned_off() -> impl IntoElement {
    div()
        .id("localsend-off")
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .px_6()
        .child(
            div()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(FontWeight::MEDIUM)
                .child("LocalSend is off"),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child("Turn it on to send files to devices nearby and receive theirs."),
        )
        .child(
            div().pt_2().child(
                text_button("localsend-turn-on", "Use LocalSend", ButtonLook::Primary)
                    .on_click(|_, _, cx| preferences::update(cx, |p| p.localsend = true)),
            ),
        )
}

/// No device yet: looking, or none answered.
fn empty(scanning: bool) -> impl IntoElement {
    let (title, line) = if scanning {
        (
            "Looking for devices nearby…",
            "Phones and computers with LocalSend open on this network show here.",
        )
    } else {
        (
            "No devices found",
            "Open LocalSend on the other device, on this network, then Scan Again.",
        )
    };
    div()
        .id("localsend-empty")
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .px_6()
        .child(
            div()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child(line),
        )
}

/// Whether this Mac receives, and how: the switch Settings has too.
fn footer(entity: &Entity<LocalSend>, cx: &mut Context<Pane>) -> impl IntoElement + use<> {
    let prefs = cx.global::<Preferences>();
    let on = prefs.localsend_receive;
    let folder = prefs.localsend_save_to();
    let folder = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string());
    let pin = prefs.localsend_pin.clone();
    let (said, failed) = match &entity.read(cx).receiving {
        Receiving::Off => (
            "Devices nearby can offer you files. You accept each one.".to_string(),
            false,
        ),
        Receiving::Starting => ("Starting…".to_string(), false),
        Receiving::On { .. } => {
            let pin = pin
                .map(|pin| format!(" Senders type PIN {pin}."))
                .unwrap_or_default();
            (
                format!("You accept each offer; files go to {folder}.{pin}"),
                false,
            )
        }
        Receiving::Failed(why) => (why.clone(), true),
    };
    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(theme::PANE_INSET))
        .py_3()
        .border_t_1()
        .border_color(content(0.06))
        .text_size(px(theme::TEXT_SMALL))
        .child(
            theme::switch(on)
                .id("localsend-receive")
                .role(Role::Switch)
                .aria_label("Receive files")
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .cursor_pointer()
                .on_click(move |_, _, cx| preferences::update(cx, |p| p.localsend_receive = !on)),
        )
        .child(div().flex_shrink_0().child("Receive files"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if failed { danger() } else { content(0.65) })
                .child(said),
        )
}

/// A circle with this Mac's initials.
fn avatar(name: &str) -> impl IntoElement + use<> {
    let initials: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    div()
        .size(px(48.))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::accent())
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme::on_accent())
        .text_size(px(theme::TEXT_TITLE))
        .font_weight(FontWeight::MEDIUM)
        .child(initials)
}

pub(super) fn kind_icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Phone => "smartphone",
        Kind::Computer => "laptop",
        Kind::Browser => "globe",
        Kind::Headless => "terminal",
        Kind::Server => "server",
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Phone => "Phone",
        Kind::Computer => "Computer",
        Kind::Browser => "Browser",
        Kind::Headless => "Command line",
        Kind::Server => "Server",
    }
}

/// A steady tint per device, so each keeps its color as others come and go.
fn tint_of(fingerprint: &str) -> Hsla {
    let tints = [
        theme::tint_blue(),
        theme::tint_purple(),
        theme::tint_pink(),
        theme::tint_orange(),
        theme::tint_indigo(),
    ];
    let hash = fingerprint.bytes().fold(5381usize, |hash, byte| {
        hash.wrapping_mul(33) ^ usize::from(byte)
    });
    tints[hash % tints.len()]
}

impl Browser {
    /// Repaints as transfers move, and says in a toast how each ended.
    pub(super) fn follow_localsend(cx: &mut Context<Self>) {
        let localsend = localsend::get(cx);
        cx.observe(&localsend, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&localsend, |this, _, said: &Said, cx| {
            let offer = said.show.clone().map(Offer::Show);
            this.toast(said.text.clone(), said.failed, offer, cx);
        })
        .detach();
    }

    /// Asks what LocalSend needs a person for: an offer, in every window
    /// until one answers it, and a PIN, in the window in front.
    pub(super) fn ask_localsend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Waits for the sheet on screen, so a question never takes the keys
        // from another.
        if self.sheet.is_some() {
            return;
        }
        let entity = localsend::get(cx);
        if let Some(offer) = entity.read(cx).offers.first().cloned() {
            let folder = cx.global::<Preferences>().localsend_save_to();
            self.remember_focus(window, cx);
            let view = cx.new(|cx| OfferView::new(offer, folder, cx));
            cx.subscribe_in(&view, window, |this, _, _: &Done, window, cx| {
                this.close_sheet(window, cx)
            })
            .detach();
            window.focus(&view.focus_handle(cx), cx);
            return self.set_sheet(Some(Sheet::LocalSendOffer(view)), cx);
        }
        if !window.is_window_active() {
            return;
        }
        let Some(wanted) = entity.update(cx, |localsend, _| localsend.pin_wanted.take()) else {
            return;
        };
        self.remember_focus(window, cx);
        let view = cx.new(|cx| PinView::new(wanted.device, wanted.paths, wanted.wrong, cx));
        cx.subscribe_in(&view, window, |this, _, _: &Done, window, cx| {
            this.close_sheet(window, cx)
        })
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::LocalSendPin(view)), cx);
    }

    /// A send or receive is under way, whose card takes the toast's place.
    pub(super) fn transferring(&self, cx: &mut Context<Self>) -> bool {
        !localsend::get(cx).read(cx).transfers.is_empty()
    }

    /// The send or receive under way, where a toast shows: who, how far,
    /// Cancel.
    pub(super) fn localsend_toast(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let entity = localsend::get(cx);
        let shown = entity.read(cx);
        let transfer = shown.transfers.first()?;
        let more = shown.transfers.len() - 1;
        let key = transfer.key;
        let peer = &transfer.peer;
        let said = match (transfer.direction, transfer.waiting) {
            (Direction::Out, true) => format!("Waiting for {peer} to accept…"),
            (Direction::Out, false) => format!("Sending to {peer}"),
            (Direction::In, _) => format!("Receiving from {peer}"),
        };
        let said = match more {
            0 => said,
            n => format!("{said}, and {n} more"),
        };
        let fraction = if transfer.total == 0 {
            0.
        } else {
            (transfer.done as f32 / transfer.total as f32).min(1.)
        };
        let progress = (!transfer.waiting).then(|| {
            format!(
                "{} of {}",
                format_bytes(transfer.done),
                format_bytes(transfer.total)
            )
        });
        let bar = if cx.global::<Preferences>().show_path_bar {
            theme::PATH_BAR_HEIGHT
        } else {
            0.
        };
        let card = theme::floating(theme::Elevation::Popover)
            .id("localsend-transfer")
            .debug_selector(|| "localsend-transfer".into())
            .occlude()
            .max_w(px(520.))
            .mx_4()
            .h(px(36.))
            .pl_3()
            .pr(px(6.))
            .flex()
            .items_center()
            .gap_3()
            .text_size(px(theme::TEXT_BODY))
            .child(div().relative().min_w_0().truncate().child(said))
            .when_some(progress, |d, progress| {
                d.child(
                    theme::progress_track()
                        .relative()
                        .w(px(90.))
                        .flex_shrink_0()
                        .child(theme::progress_fill(fraction)),
                )
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(progress),
                )
            })
            .child(
                text_button("localsend-cancel", "Cancel", ButtonLook::Ghost)
                    .debug_selector(|| "localsend-cancel".into())
                    .relative()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        localsend::get(cx).update(cx, |localsend, cx| localsend.cancel(key, cx))
                    })),
            );
        Some(
            div()
                .absolute()
                .bottom(px(16. + bar))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(card),
        )
    }
}
