//! The context menu: Monocode's popover menu (`popover.rs`) cut down to rows
//! of commands, their shortcuts and the separators between sections. It
//! opens at the pointer, inside the window, and has the keys while open: ↑
//! and ↓ move, ↩ runs, a letter jumps to the next row starting with it, and
//! Esc or a press outside closes it.

use crate::commands::MenuRow;
use crate::theme::{self, Elevation, content, icon};
use gpui::{
    Action, App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Point, Role, Size, Window, actions, div, point,
    prelude::*, px,
};

actions!(context_menu, [Up, Down, Run, Dismiss]);

pub fn bind(cx: &mut App) {
    let context = Some("ContextMenu");
    cx.bind_keys([
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("enter", Run, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

pub enum MenuEvent {
    Run(Box<dyn Action>),
    Dismissed,
}

const WIDTH: f32 = 240.;
const PADDING: f32 = 4.;
/// A separator's line and the space around it.
const SEPARATOR: f32 = 9.;
/// Kept clear of the window's edges.
const MARGIN: f32 = 8.;

pub struct ContextMenu {
    pub(crate) rows: Vec<MenuRow>,
    /// Where it opened, in the window.
    at: Point<Pixels>,
    /// The row the keys or the pointer are on. None at first, as in macOS.
    pub(crate) active: Option<usize>,
    focus: FocusHandle,
}

impl EventEmitter<MenuEvent> for ContextMenu {}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ContextMenu {
    pub fn new(rows: Vec<MenuRow>, at: Point<Pixels>, cx: &mut Context<Self>) -> Self {
        Self {
            rows,
            at,
            active: None,
            focus: cx.focus_handle(),
        }
    }

    /// One row up or down, round from the ends as Monocode's menus go.
    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        self.active = Some(match (self.active, down) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(ix), true) => (ix + 1) % len,
            (Some(ix), false) => (ix + len - 1) % len,
        });
        cx.notify();
    }

    fn run(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(ix) {
            cx.emit(MenuEvent::Run(row.action.boxed_clone()));
        }
    }

    /// A letter moves to the next row that starts with it.
    fn jump(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if modifiers.platform || modifiers.control {
            return;
        }
        let Some(letter) = event
            .keystroke
            .key_char
            .as_deref()
            .and_then(|text| text.chars().next())
            .filter(|c| c.is_alphanumeric())
        else {
            return;
        };
        let titles: Vec<&str> = self.rows.iter().map(|row| row.title).collect();
        if let Some(ix) = next_starting(&titles, self.active, letter) {
            self.active = Some(ix);
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// A press outside closes it and reaches nothing behind it.
    fn press_outside(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        window.prevent_default();
        cx.emit(MenuEvent::Dismissed);
    }

    fn size(&self) -> Size<Pixels> {
        let separators = self.rows.iter().filter(|row| row.separated).count();
        let height = 2. * (PADDING + 1.)
            + (theme::ROW_HEIGHT + theme::ROW_GAP) * self.rows.len() as f32
            + SEPARATOR * separators as f32;
        Size::new(px(WIDTH), px(height))
    }
}

/// The row after `from` whose title starts with `letter`, ignoring case,
/// coming round from the top.
fn next_starting(titles: &[&str], from: Option<usize>, letter: char) -> Option<usize> {
    let start = from.map_or(0, |ix| ix + 1);
    let wanted = letter.to_lowercase().collect::<String>();
    (0..titles.len())
        .map(|step| (start + step) % titles.len())
        .find(|ix| titles[*ix].to_lowercase().starts_with(&wanted))
}

/// Where a menu of `size` opening at `at` goes: right and down from the
/// pointer, left or up where the window runs out, never past its edges.
fn place(at: Point<Pixels>, size: Size<Pixels>, viewport: Size<Pixels>) -> Point<Pixels> {
    let axis = |at: f32, length: f32, room: f32| {
        if at + length + MARGIN <= room {
            at
        } else if at - length >= MARGIN {
            at - length
        } else {
            (room - length - MARGIN).max(MARGIN)
        }
    };
    point(
        px(axis(
            at.x.as_f32(),
            size.width.as_f32(),
            viewport.width.as_f32(),
        )),
        px(axis(
            at.y.as_f32(),
            size.height.as_f32(),
            viewport.height.as_f32(),
        )),
    )
}

impl Render for ContextMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let corner = place(self.at, self.size(), viewport);
        let checks = self.rows.iter().any(|row| row.checked);
        let rows = self.rows.iter().enumerate().map(|(ix, row)| {
            let active = self.active == Some(ix);
            div()
                .flex_shrink_0()
                .when(row.separated, |d| {
                    d.child(separator().id(("separator", ix)).role(Role::Splitter))
                })
                .child(
                    theme::row_slot(ix)
                        .role(Role::MenuItem)
                        .aria_label(row.title)
                        .aria_selected(active)
                        .when_some(row.keys.clone(), |d, keys| d.aria_keyshortcuts(keys))
                        .child(
                            theme::row(active)
                                .when(checks, |d| d.child(check(row.checked, active)))
                                .child(div().flex_1().min_w_0().truncate().child(row.title))
                                .children(row.keys.clone().map(|keys| shortcut(keys, active))),
                        )
                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                            if this.active != Some(ix) {
                                this.active = Some(ix);
                                cx.notify();
                            }
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.run(ix, cx))),
                )
        });
        div()
            .id("context-menu-layer")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press_outside))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::press_outside))
            .child(
                card(WIDTH, viewport.height - px(2. * MARGIN), rows)
                    .id("context-menu")
                    .key_context("ContextMenu")
                    .track_focus(&self.focus)
                    .role(Role::Menu)
                    .on_action(cx.listener(|this, _: &Up, _, cx| this.step(false, cx)))
                    .on_action(cx.listener(|this, _: &Down, _, cx| this.step(true, cx)))
                    .on_action(cx.listener(|this, _: &Run, _, cx| {
                        if let Some(ix) = this.active {
                            this.run(ix, cx);
                        }
                    }))
                    .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(MenuEvent::Dismissed)))
                    .on_key_down(cx.listener(Self::jump))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .absolute()
                    .left(corner.x)
                    .top(corner.y),
            )
    }
}

// The look, Monocode's explorer menu, kept here and in the sizes above so
// another one is a change to these alone.

/// The card around the rows, `max_height` at most, scrolling past that.
/// The copies and moves popover is one too.
pub(crate) fn card(
    width: f32,
    max_height: Pixels,
    rows: impl IntoIterator<Item = impl IntoElement>,
) -> gpui::Div {
    theme::floating(Elevation::Popover)
        .w(px(width))
        .max_h(max_height)
        .child(
            div()
                .id("context-menu-rows")
                .relative()
                .p(px(PADDING))
                .max_h(max_height - px(2.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .children(rows),
        )
}

fn separator() -> gpui::Div {
    div().my_1().h(px(SEPARATOR - 8.)).bg(content(0.12))
}

/// The checkmark's column, in menus where a row can be checked.
fn check(checked: bool, active: bool) -> gpui::Div {
    div().w(px(12.)).flex_shrink_0().when(checked, |d| {
        d.child(icon("check-hi", 12.).text_color(theme::row_muted(active)))
    })
}

fn shortcut(keys: String, active: bool) -> gpui::Div {
    div()
        .flex_shrink_0()
        .text_size(px(theme::TEXT_CAPTION))
        .text_color(theme::row_muted(active))
        .child(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn it_opens_beside_the_pointer_and_stays_inside_the_window() {
        let window = size(px(800.), px(500.));
        let menu = size(px(240.), px(300.));
        let at = |x: f32, y: f32| place(point(px(x), px(y)), menu, window);
        assert_eq!(at(100., 50.), point(px(100.), px(50.)));
        // Near the right or the bottom it opens to the left or upward.
        assert_eq!(at(700., 50.), point(px(460.), px(50.)));
        assert_eq!(at(100., 400.), point(px(100.), px(100.)));
        // Too tall for either way, it keeps clear of the bottom edge.
        assert_eq!(at(100., 250.), point(px(100.), px(192.)));
    }

    #[test]
    fn a_letter_jumps_to_the_next_row_starting_with_it() {
        let titles = ["Open", "Rename", "Duplicate", "Copy", "Copy Path"];
        assert_eq!(next_starting(&titles, None, 'c'), Some(3));
        assert_eq!(next_starting(&titles, Some(3), 'C'), Some(4));
        // From the last match it comes round to the first.
        assert_eq!(next_starting(&titles, Some(4), 'c'), Some(3));
        assert_eq!(next_starting(&titles, None, 'z'), None);
    }
}
