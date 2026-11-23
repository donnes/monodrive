//! LocalSend's questions, in the palette's card: whether to take what a
//! device offers (↩ accepts, Esc declines), and the PIN a device asks for
//! before it takes what this Mac sends.

use crate::browser::format_bytes;
use crate::localsend::{self, LocalSend, count};
use crate::palette;
use crate::text_field::TextField;
use crate::theme::{self, ButtonLook, content, danger, text_button};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Window,
    actions, div, prelude::*, px,
};
use monodrive_core::localsend::{Device, Offer};
use std::path::PathBuf;

actions!(localsend_ask, [Decline, Accept]);

pub fn bind(cx: &mut App) {
    for context in ["LocalSendOffer", "LocalSendPin"] {
        cx.bind_keys([
            KeyBinding::new("escape", Decline, Some(context)),
            KeyBinding::new("enter", Accept, Some(context)),
        ]);
    }
}

/// The card is done with: answered here, in another window, or withdrawn.
pub struct Done;

/// How many of an offer's names the card lists.
const LISTED: usize = 5;

pub struct OfferView {
    offer: Offer,
    /// The save folder's name.
    folder: String,
    localsend: Entity<LocalSend>,
    focus: FocusHandle,
}

impl EventEmitter<Done> for OfferView {}

impl Focusable for OfferView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OfferView {
    pub fn new(offer: Offer, folder: PathBuf, cx: &mut Context<Self>) -> Self {
        let localsend = localsend::get(cx);
        cx.observe(&localsend, |this: &mut Self, localsend, cx| {
            let waiting = localsend
                .read(cx)
                .offers
                .iter()
                .any(|offer| offer.id == this.offer.id);
            if !waiting {
                cx.emit(Done);
            }
        })
        .detach();
        Self {
            offer,
            folder: folder
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.display().to_string()),
            localsend,
            focus: cx.focus_handle(),
        }
    }

    fn answer(&mut self, accept: bool, cx: &mut Context<Self>) {
        let id = self.offer.id.clone();
        self.localsend
            .update(cx, |localsend, cx| localsend.answer(&id, accept, cx));
        cx.emit(Done);
    }
}

impl Render for OfferView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let offer = &self.offer;
        let (title, listing, about, take) = match &offer.text {
            Some(text) => (
                format!("{} wants to send you text", offer.from),
                message(text).into_any_element(),
                None,
                "Copy to Clipboard",
            ),
            None => (
                format!(
                    "{} wants to send you {}",
                    offer.from,
                    count(offer.files.len(), "file")
                ),
                files(offer).into_any_element(),
                Some(format!(
                    "{} in all. They'll go to {}.",
                    format_bytes(offer.bytes),
                    self.folder
                )),
                "Accept",
            ),
        };
        let body = div()
            .p_2()
            .flex()
            .flex_col()
            .gap_3()
            .child(listing)
            .children(about.map(|about| {
                div()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(about)
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("localsend-decline", "Decline", ButtonLook::Secondary)
                            .debug_selector(|| "localsend-decline".into())
                            .on_click(cx.listener(|this, _, _, cx| this.answer(false, cx))),
                    )
                    .child(
                        text_button("localsend-accept", take, ButtonLook::Primary)
                            .debug_selector(|| "localsend-accept".into())
                            .on_click(cx.listener(|this, _, _, cx| this.answer(true, cx))),
                    ),
            );
        palette::overlay("localsend-offer-overlay")
            .key_context("LocalSendOffer")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Decline, _, cx| this.answer(false, cx)))
            .on_action(cx.listener(|this, _: &Accept, _, cx| this.answer(true, cx)))
            .child(
                palette::card(
                    "localsend-offer",
                    "Receive with LocalSend",
                    div().min_w_0().truncate().child(title),
                    body,
                )
                .w(px(420.)),
            )
    }
}

/// The first names an offer lists, with their sizes.
fn files(offer: &Offer) -> impl IntoElement {
    let more = offer.files.len().saturating_sub(LISTED);
    div()
        .flex()
        .flex_col()
        .gap_1()
        .text_size(px(theme::TEXT_BODY))
        .children(offer.files.iter().take(LISTED).map(|(name, size)| {
            div()
                .flex()
                .gap_3()
                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(content(0.65))
                        .child(format_bytes(*size)),
                )
        }))
        .when(more > 0, |d| {
            d.child(
                div()
                    .text_color(content(0.65))
                    .child(format!("and {more} more")),
            )
        })
}

/// How much of a message the card draws: enough to recognize it.
const SHOWN: usize = 2_000;

/// A message as it reads, in a box that scrolls past a few lines.
fn message(text: &str) -> impl IntoElement {
    let shown = match text.char_indices().nth(SHOWN) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    };
    div()
        .id("localsend-message")
        .max_h(px(132.))
        .overflow_y_scroll()
        .px_3()
        .py_2()
        .rounded(px(theme::RADIUS_CARD))
        .bg(theme::well())
        .text_size(px(theme::TEXT_BODY))
        .child(shown)
}

/// The PIN a device asks for, then the send again with it.
pub struct PinView {
    field: Entity<TextField>,
    device: Device,
    paths: Vec<PathBuf>,
    wrong: bool,
}

impl EventEmitter<Done> for PinView {}

impl Focusable for PinView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

impl PinView {
    pub fn new(device: Device, paths: Vec<PathBuf>, wrong: bool, cx: &mut Context<Self>) -> Self {
        let placeholder = format!("PIN for {}", device.alias);
        let field = cx.new(|cx| TextField::new("PIN", placeholder, cx));
        Self {
            field,
            device,
            paths,
            wrong,
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        let pin = self.field.read(cx).text().trim().to_owned();
        if pin.is_empty() {
            return;
        }
        let (device, paths) = (self.device.clone(), std::mem::take(&mut self.paths));
        localsend::get(cx).update(cx, |localsend, cx| {
            localsend.send(device, paths, Some(pin), cx)
        });
        cx.emit(Done);
    }
}

impl Render for PinView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (said, ink) = if self.wrong {
            (
                format!("{} didn't take that PIN.", self.device.alias),
                danger(),
            )
        } else {
            (
                format!(
                    "{} asks for a PIN before it takes {}.",
                    self.device.alias,
                    count(self.paths.len(), "item")
                ),
                content(0.65),
            )
        };
        palette::overlay("localsend-pin-overlay")
            .key_context("LocalSendPin")
            .on_action(cx.listener(|_, _: &Decline, _, cx| cx.emit(Done)))
            .on_action(cx.listener(|this, _: &Accept, _, cx| this.send(cx)))
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _, _, cx| cx.emit(Done)))
            .child(
                palette::card(
                    "localsend-pin",
                    "Send with LocalSend",
                    self.field.clone(),
                    div()
                        .h(px(theme::ROW_HEIGHT))
                        .px_2()
                        .flex()
                        .items_center()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(ink)
                        .child(said),
                )
                .w(px(420.)),
            )
    }
}
