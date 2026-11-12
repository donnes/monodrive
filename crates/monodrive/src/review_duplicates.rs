//! Find Duplicates' question, in the palette's card: every copy that goes
//! to the Trash by its whole path, beside the copies of it that stay, and
//! what that frees. Esc, Cancel or a click outside goes back to choosing;
//! ↩ moves them.

use crate::palette;
use crate::theme::{self, ButtonLook, content, icon, text_button};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Window, actions,
    div, prelude::*, px,
};
use monodrive_core::ops::Request;
use monodrive_core::storage::Cleanup;
use std::path::Path;

actions!(review_duplicates, [Cancel, Confirm]);

/// How far the list of paths goes before it scrolls.
const LIST_HEIGHT: f32 = 280.;

pub fn bind(cx: &mut App) {
    let context = Some("ReviewDuplicates");
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("enter", Confirm, context),
    ]);
}

/// What the person is asked about: a plan the owner checks again as it
/// moves each copy.
#[derive(Clone, Debug)]
pub struct Review {
    pub cleanup: Cleanup,
}

pub enum Answer {
    Cancel,
    Trash(Request),
}

pub struct ReviewDuplicatesView {
    review: Review,
    focus: FocusHandle,
}

impl EventEmitter<Answer> for ReviewDuplicatesView {}

impl Focusable for ReviewDuplicatesView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ReviewDuplicatesView {
    pub fn new(review: Review, cx: &mut Context<Self>) -> Self {
        Self {
            review,
            focus: cx.focus_handle(),
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        cx.emit(Answer::Trash(self.review.cleanup.request()));
    }
}

/// A path, whole and wrapped, with what happens to it.
fn path_row(path: &Path, trashed: bool) -> impl IntoElement {
    let ink = if trashed { content(1.) } else { content(0.65) };
    div()
        .min_h(px(theme::ROW_HEIGHT))
        .px_2()
        .py_1()
        .flex()
        .items_center()
        .gap_2()
        .child(
            icon(if trashed { "trash" } else { "check-hi" }, 12.).text_color(if trashed {
                content(1.)
            } else {
                content(0.55)
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .font_family(theme::MONO_FONT)
                .text_size(px(theme::TEXT_SMALL))
                .text_color(ink)
                .child(crate::browser::short_reason(
                    &path.display().to_string(),
                    [],
                )),
        )
        .when(!trashed, |d| {
            d.child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::TEXT_CAPTION))
                    .text_color(content(0.55))
                    .child("Stays"),
            )
        })
}

impl Render for ReviewDuplicatesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let cleanup = &self.review.cleanup;
        let moving: usize = cleanup.groups.iter().map(|copies| copies.trash.len()).sum();
        let title = if moving == 1 {
            "Move 1 copy to the Trash?".to_string()
        } else {
            format!("Move {moving} copies to the Trash?")
        };
        let groups = cleanup.groups.iter().enumerate().map(|(ix, copies)| {
            div()
                .when(ix > 0, |d| {
                    d.mt_1().pt_1().border_t_1().border_color(content(0.06))
                })
                .children(copies.trash.iter().map(|seen| path_row(&seen.path, true)))
                .children(copies.keep.iter().map(|seen| path_row(&seen.path, false)))
        });
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .id("review-duplicates-paths")
                    .max_h(px(LIST_HEIGHT))
                    .overflow_y_scroll()
                    .px_1()
                    .py_1()
                    .rounded(px(theme::RADIUS_CARD))
                    .bg(content(0.03))
                    .border_1()
                    .border_color(content(0.06))
                    .children(groups),
            )
            .child(
                div()
                    .px_2()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(content(0.65))
                    .child(format!(
                        "Frees at least {} once the Trash is emptied. A copy that changed since it was read stays. ⌘Z puts them back.",
                        crate::browser::format_bytes(cleanup.frees)
                    )),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("review-duplicates-cancel", "Cancel", ButtonLook::Secondary)
                            .debug_selector(|| "review-duplicates-cancel".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Answer::Cancel))),
                    )
                    .child(
                        text_button(
                            "review-duplicates-confirm",
                            "Move to Trash",
                            ButtonLook::Primary,
                        )
                        .debug_selector(|| "review-duplicates-confirm".into())
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                    ),
            );
        palette::overlay("review-duplicates-overlay")
            .key_context("ReviewDuplicates")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(Answer::Cancel)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.confirm(cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(Answer::Cancel)),
            )
            .child(palette::card(
                "review-duplicates",
                "Review Duplicates",
                title,
                body,
            ))
    }
}
