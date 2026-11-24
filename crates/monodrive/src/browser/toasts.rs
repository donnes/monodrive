//! The toast at the foot of the window: what just happened, with an offer to
//! follow it up, and LocalSend's transfers as they go.

use super::pane::Offer;
use super::{Browser, TOAST_FOR};
use crate::format::format_bytes;
use crate::localsend::{self, Direction, Said};
use crate::motion::{self};
use crate::preferences::Preferences;
use crate::theme::text_button;
use crate::theme::{self, ButtonLook, Elevation, accent, content, danger};
use gpui::{Context, FontWeight, Task, div, prelude::*, px};
use monodrive_core::ops::Request;

/// A message after a change.
pub(super) struct Toast {
    pub(super) text: String,
    pub(super) failed: bool,
    pub(super) offer: Option<Offer>,
    /// On its way out, for `motion::EXIT`.
    leaving: bool,
    _expire: Task<()>,
}

impl Browser {
    /// The toast at the bottom of the files, with its Undo.
    pub(super) fn toast_view(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let toast = self.toast.as_ref().filter(|_| self.fetching.is_none())?;
        if self.transferring(cx) {
            return None;
        }
        // Above the path bar, if it's showing.
        let bar = if cx.global::<Preferences>().show_path_bar {
            theme::PATH_BAR_HEIGHT
        } else {
            0.
        };
        let ink = if toast.failed { danger() } else { content(1.) };
        let card = theme::floating(Elevation::Popover)
            .id("toast")
            .occlude()
            .max_w(px(520.))
            .mx_4()
            .h(px(36.))
            .pl_3()
            .pr(px(6.))
            .flex()
            .items_center()
            .gap_2()
            .when(toast.failed, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(theme::RADIUS_CARD - 1.))
                        .bg(danger().opacity(0.12)),
                )
            })
            .child(
                div()
                    .relative()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(ink)
                    .child(toast.text.clone()),
            )
            .when_some(toast.offer.clone(), |d, offer| {
                let label = match &offer {
                    Offer::Undo(_) => "Undo",
                    Offer::Download(label, _) => label,
                    Offer::Show(_) => "Show",
                };
                d.child(
                    theme::text_button("toast-offer", label, ButtonLook::Ghost)
                        .debug_selector(|| "toast-offer".into())
                        .relative()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(accent())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toast = None;
                            this.pane().update(cx, |pane, cx| match &offer {
                                Offer::Undo(op) => {
                                    pane.submit(Request::UndoOf { op: *op }, window, cx)
                                }
                                Offer::Download(_, paths) => pane.download(paths.clone(), cx),
                                Offer::Show(path) => pane.show_item(path, window, cx),
                            });
                        })),
                )
            })
            .when(toast.offer.is_none(), |d| d.pr_3());
        // The entrance plays when a toast appears, not when a new one takes
        // the place of one on screen. Both run once and skip with Reduce Motion.
        let card = if toast.leaving {
            motion::sink_out(card, "toast-leave")
        } else {
            motion::rise_in(card, "toast-enter")
        };
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

    /// Says `text` at the bottom of the files for `TOAST_FOR`, in red if
    /// it's a failure, with a button for `offer` if given. It replaces the
    /// last one.
    pub(super) fn toast(
        &mut self,
        text: String,
        failed: bool,
        offer: Option<Offer>,
        cx: &mut Context<Self>,
    ) {
        let expire = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            let leaves = this.update(cx, |this, cx| {
                cx.notify();
                if cx.reduce_motion() {
                    this.toast = None;
                    return false;
                }
                if let Some(toast) = this.toast.as_mut() {
                    toast.leaving = true;
                }
                true
            });
            if leaves.unwrap_or(false) {
                cx.background_executor().timer(motion::EXIT).await;
                let _ = this.update(cx, |this, cx| {
                    this.toast = None;
                    cx.notify();
                });
            }
        });
        self.toast = Some(Toast {
            text,
            failed,
            offer,
            leaving: false,
            _expire: expire,
        });
        cx.notify();
    }
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
