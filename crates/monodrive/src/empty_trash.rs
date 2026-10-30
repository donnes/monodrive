//! Empty Trash's question, in the palette's card: how many items will be
//! erased for good. Esc, Cancel or a click outside keeps them; ↩ empties, as
//! Finder's default button does.

use crate::palette;
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Window, actions,
    div, prelude::*, px,
};
use monodrive_core::ops::TrashItem;

actions!(empty_trash, [Cancel, Confirm]);

pub fn bind(cx: &mut App) {
    let context = Some("EmptyTrash");
    cx.bind_keys([
        KeyBinding::new("escape", Cancel, context),
        KeyBinding::new("enter", Confirm, context),
    ]);
}

pub enum Answer {
    Cancel,
    /// Erase these, the items counted.
    Empty(Vec<TrashItem>),
}

pub struct EmptyTrashView {
    items: Vec<TrashItem>,
    focus: FocusHandle,
}

impl EventEmitter<Answer> for EmptyTrashView {}

impl Focusable for EmptyTrashView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EmptyTrashView {
    pub fn new(items: Vec<TrashItem>, cx: &mut Context<Self>) -> Self {
        Self {
            items,
            focus: cx.focus_handle(),
        }
    }

    fn empty(&mut self, cx: &mut Context<Self>) {
        cx.emit(Answer::Empty(std::mem::take(&mut self.items)));
    }
}

impl Render for EmptyTrashView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = match self.items.len() {
            1 => "1 item".to_string(),
            n => format!("{n} items"),
        };
        let body = div()
            .px_2()
            .py_1()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(content(0.65))
                    .child(format!("{count} will be erased for good.")),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("empty-trash-cancel", "Cancel", ButtonLook::Secondary)
                            .debug_selector(|| "empty-trash-cancel".into())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(Answer::Cancel))),
                    )
                    .child(
                        text_button(
                            "empty-trash-confirm",
                            "Empty Trash",
                            ButtonLook::Destructive,
                        )
                        .debug_selector(|| "empty-trash-confirm".into())
                        .on_click(cx.listener(|this, _, _, cx| this.empty(cx))),
                    ),
            );
        palette::overlay("empty-trash-overlay")
            .key_context("EmptyTrash")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(Answer::Cancel)))
            .on_action(cx.listener(|this, _: &Confirm, _, cx| this.empty(cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(Answer::Cancel)),
            )
            .child(
                palette::card("empty-trash", "Empty the Trash?", "Empty the Trash?", body)
                    .w(px(400.)),
            )
    }
}
