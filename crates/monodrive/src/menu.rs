//! The context menu: Monocode's popover menu (`popover.rs`) cut down to rows
//! of commands, their shortcuts and the separators between sections. It
//! opens at the pointer, inside the window, and has the keys while open: ↑
//! and ↓ move, ↩ runs, a letter jumps to the next row starting with it, and
//! Esc or a press outside closes it. A row can open a submenu, by hovering, →
//! or ↩; ← comes back from it, and the two stay inside the window.

use crate::commands::MenuRow;
use crate::file_icons;
use crate::native::{Offered, native, offered};
use crate::theme::{self, Elevation, content, icon};
use gpui::{
    Action, App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Point, Role, SharedString, Size, Task, Window, actions,
    div, point, prelude::*, px,
};
use monodrive_core::entries::{Entry, Kind};
use std::path::PathBuf;
use std::time::Duration;

actions!(
    context_menu,
    [Up, Down, Run, Dismiss, EnterSubmenu, LeaveSubmenu]
);

pub fn bind(cx: &mut App) {
    let context = Some("ContextMenu");
    cx.bind_keys([
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("enter", Run, context),
        KeyBinding::new("escape", Dismiss, context),
        KeyBinding::new("right", EnterSubmenu, context),
        KeyBinding::new("left", LeaveSubmenu, context),
    ]);
}

pub enum MenuEvent {
    Run(Box<dyn Action>),
    Dismissed,
    /// Open the selection in this app.
    OpenWith(PathBuf),
    /// Ask for an app, then open the selection in it.
    OtherApp,
    /// Ask whether this app should always open files of this type.
    AlwaysOpenWith(PathBuf),
}

/// What a row of the Open With submenu does. A row without one is dimmed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Choice {
    Open(PathBuf),
    Other,
    /// Offers the apps again, to choose one to always open with.
    Always,
    AlwaysWith(PathBuf),
}

pub(crate) struct SubRow {
    pub(crate) title: SharedString,
    /// Whose icon it shows.
    app: Option<Entry>,
    pub(crate) choice: Option<Choice>,
    separated: bool,
}

/// The open submenu.
pub(crate) struct Submenu {
    /// The row that opened it.
    parent: usize,
    pub(crate) active: Option<usize>,
    /// The keys move in it, not in the rows behind it.
    pub(crate) has_keys: bool,
    /// The apps are offered to always open with.
    always: bool,
}

const WIDTH: f32 = 240.;
const PADDING: f32 = 4.;
/// A separator's line and the space around it.
const SEPARATOR: f32 = 9.;
/// Kept clear of the window's edges.
const MARGIN: f32 = 8.;
/// How far a submenu overlaps the menu it came from.
const OVERLAP: f32 = 2.;
/// How long a submenu waits after the pointer leaves its row, so the way
/// across a row below on the way into it doesn't close it.
const CLOSE_DELAY: Duration = Duration::from_millis(200);

pub struct ContextMenu {
    pub(crate) rows: Vec<MenuRow>,
    /// Where it opened, in the window.
    at: Point<Pixels>,
    /// The row the keys or the pointer are on. None at first, as in macOS.
    pub(crate) active: Option<usize>,
    focus: FocusHandle,
    /// What Open With opens.
    files: Vec<PathBuf>,
    pub(crate) submenu: Option<Submenu>,
    /// The apps, once they've loaded.
    offered: Option<Offered>,
    loading: Option<Task<()>>,
    /// Closing the submenu, once the pointer has been away a moment.
    closing: Option<Task<()>>,
}

impl EventEmitter<MenuEvent> for ContextMenu {}

impl Focusable for ContextMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ContextMenu {
    pub fn new(
        rows: Vec<MenuRow>,
        at: Point<Pixels>,
        files: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            rows,
            at,
            active: None,
            focus: cx.focus_handle(),
            files,
            submenu: None,
            offered: None,
            loading: None,
            closing: None,
        }
    }

    /// Opens the first row's submenu that there is, with the keys in it.
    pub(crate) fn enter_first_submenu(&mut self, cx: &mut Context<Self>) {
        if let Some(ix) = self.rows.iter().position(|row| row.submenu) {
            self.active = Some(ix);
            self.open_submenu(ix, true, cx);
        }
    }

    /// Opens row `parent`'s submenu, loading the apps off the UI thread the
    /// first time.
    fn open_submenu(&mut self, parent: usize, keys: bool, cx: &mut Context<Self>) {
        self.closing = None;
        if self.loading.is_none() {
            let (native, files) = (native(cx), self.files.clone());
            let load = cx
                .background_executor()
                .spawn(async move { offered(&*native, &files) });
            self.loading = Some(cx.spawn(async move |this, cx| {
                let apps = load.await;
                let _ = this.update(cx, |this, cx| {
                    this.offered = Some(apps);
                    this.land_on_first();
                    cx.notify();
                });
            }));
        }
        match &mut self.submenu {
            Some(submenu) if submenu.parent == parent => submenu.has_keys = keys,
            _ => {
                self.submenu = Some(Submenu {
                    parent,
                    active: None,
                    has_keys: keys,
                    always: false,
                })
            }
        }
        self.land_on_first();
        cx.notify();
    }

    /// With the keys in the submenu, a row is always under them.
    fn land_on_first(&mut self) {
        let first = self.sub_rows().iter().position(|row| row.choice.is_some());
        if let Some(submenu) = self.submenu.as_mut().filter(|s| s.has_keys)
            && submenu.active.is_none()
        {
            submenu.active = first;
        }
    }

    /// The submenu's rows as things stand: a loading row until the apps land.
    pub(crate) fn sub_rows(&self) -> Vec<SubRow> {
        let row = |title: SharedString, app, choice, separated| SubRow {
            title,
            app,
            choice,
            separated,
        };
        let Some(offered) = &self.offered else {
            return vec![row("Loading…".into(), None, None, false)];
        };
        let always = self.submenu.as_ref().is_some_and(|s| s.always);
        let mut rows = Vec::new();
        if always {
            let kind = self.files.first().and_then(|file| file.extension());
            let title = match kind {
                Some(extension) => format!("Always open .{} with", extension.to_string_lossy()),
                None => "Always open with".to_string(),
            };
            rows.push(row(title.into(), None, None, false));
        }
        for (ix, app) in offered.apps.iter().enumerate() {
            let default = offered.has_default && ix == 0;
            let title = if default {
                format!("{} (default)", app.name)
            } else {
                app.name.clone()
            };
            let path = app.path.clone();
            let choice = if always {
                Choice::AlwaysWith(path)
            } else {
                Choice::Open(path)
            };
            let entry = Entry {
                path: app.path.clone(),
                name: app.name.clone(),
                kind: Kind::Dir,
                size: 0,
                modified: 0,
                availability: Default::default(),
            };
            rows.push(row(title.into(), Some(entry), Some(choice), false));
        }
        if always {
            return rows;
        }
        if rows.is_empty() {
            rows.push(row("No apps found".into(), None, None, false));
        }
        rows.push(row("Other…".into(), None, Some(Choice::Other), true));
        // Its default is by type, so one file says which type.
        if self.files.len() == 1 && !offered.apps.is_empty() {
            let title = "Always Open With…".into();
            rows.push(row(title, None, Some(Choice::Always), false));
        }
        rows
    }

    /// One row up or down in the submenu, skipping what's dimmed.
    fn step_submenu(&mut self, down: bool, cx: &mut Context<Self>) {
        let enabled: Vec<usize> = self
            .sub_rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.choice.is_some())
            .map(|(ix, _)| ix)
            .collect();
        let Some(submenu) = self.submenu.as_mut() else {
            return;
        };
        let at = submenu
            .active
            .and_then(|active| enabled.iter().position(|ix| *ix == active));
        let len = enabled.len();
        if len > 0 {
            let next = match (at, down) {
                (None, true) => 0,
                (None, false) => len - 1,
                (Some(at), true) => (at + 1) % len,
                (Some(at), false) => (at + len - 1) % len,
            };
            submenu.active = Some(enabled[next]);
            cx.notify();
        }
    }

    fn run_submenu(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(choice) = self.sub_rows().into_iter().nth(ix).and_then(|r| r.choice) else {
            return;
        };
        match choice {
            Choice::Open(app) => cx.emit(MenuEvent::OpenWith(app)),
            Choice::Other => cx.emit(MenuEvent::OtherApp),
            Choice::AlwaysWith(app) => cx.emit(MenuEvent::AlwaysOpenWith(app)),
            Choice::Always => {
                if let Some(submenu) = &mut self.submenu {
                    submenu.always = true;
                    submenu.has_keys = true;
                    submenu.active = None;
                }
                self.land_on_first();
                cx.notify();
            }
        }
    }

    /// ←: from the apps to always open with back to the rest, or out of the
    /// submenu.
    fn leave_submenu(&mut self, cx: &mut Context<Self>) {
        let Some(submenu) = &mut self.submenu else {
            return;
        };
        if submenu.always {
            submenu.always = false;
            submenu.active = None;
            self.land_on_first();
        } else {
            self.submenu = None;
        }
        cx.notify();
    }

    /// The pointer is on row `ix` of the menu: a submenu of its own opens at
    /// once, and another closes after a moment.
    fn hover_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.active != Some(ix) {
            self.active = Some(ix);
            cx.notify();
        }
        if self.rows.get(ix).is_some_and(|row| row.submenu) {
            return self.open_submenu(ix, false, cx);
        }
        if self.submenu.is_some() && self.closing.is_none() {
            let wait = cx.background_executor().timer(CLOSE_DELAY);
            self.closing = Some(cx.spawn(async move |this, cx| {
                wait.await;
                let _ = this.update(cx, |this, cx| {
                    this.submenu = None;
                    this.closing = None;
                    cx.notify();
                });
            }));
        }
    }

    /// The pointer is on row `ix` of the submenu.
    fn hover_submenu(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.closing = None;
        if let Some(submenu) = &mut self.submenu
            && (submenu.active != Some(ix) || !submenu.has_keys)
        {
            submenu.active = Some(ix);
            submenu.has_keys = true;
            cx.notify();
        }
    }

    fn keys_in_submenu(&self) -> bool {
        self.submenu.as_ref().is_some_and(|s| s.has_keys)
    }

    /// One row up or down, round from the ends as Monocode's menus go.
    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        if self.keys_in_submenu() {
            return self.step_submenu(down, cx);
        }
        self.submenu = None;
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
            if row.submenu {
                return self.open_submenu(ix, true, cx);
            }
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
        if self.keys_in_submenu() {
            let rows = self.sub_rows();
            let titles: Vec<&str> = rows
                .iter()
                .map(|row| match row.choice {
                    Some(_) => row.title.as_ref(),
                    None => "",
                })
                .collect();
            if let Some(submenu) = &mut self.submenu
                && let Some(ix) = next_starting(&titles, submenu.active, letter)
            {
                submenu.active = Some(ix);
                cx.notify();
            }
        } else {
            let titles: Vec<&str> = self.rows.iter().map(|row| row.title).collect();
            if let Some(ix) = next_starting(&titles, self.active, letter) {
                self.active = Some(ix);
                cx.notify();
            }
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
        Size::new(
            px(WIDTH),
            px(rows_height(self.rows.iter().map(|row| row.separated))),
        )
    }

    /// The submenu's card, beside the row that opened it, with its own rows.
    fn submenu_card(
        &self,
        corner: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let submenu = self.submenu.as_ref()?;
        let rows = self.sub_rows();
        let above = self.rows[..submenu.parent].iter().map(|row| row.separated);
        // Its first row level with the row that opened it.
        let top = rows_height(above) - 2. * (PADDING + 1.)
            + if self.rows[submenu.parent].separated {
                SEPARATOR
            } else {
                0.
            };
        let height = rows_height(rows.iter().map(|row| row.separated));
        let viewport = window.viewport_size();
        let at = place_submenu(
            point(corner.x, corner.y + px(top)),
            WIDTH,
            Size::new(px(WIDTH), px(height)),
            viewport,
        );
        let active = submenu.active;
        let items: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(ix, row)| {
                let on = active == Some(ix);
                let enabled = row.choice.is_some();
                div()
                    .flex_shrink_0()
                    .when(row.separated, |d| {
                        d.child(separator().id(("sub-separator", ix)).role(Role::Splitter))
                    })
                    .child(
                        theme::row_slot(ix)
                            .debug_selector(move || format!("submenu-row-{ix}"))
                            .role(Role::MenuItem)
                            .aria_label(row.title.clone())
                            .aria_selected(on)
                            .when(!enabled, |d| d.cursor_default())
                            .when(enabled, |d| {
                                d.on_mouse_move(
                                    cx.listener(move |this, _, _, cx| this.hover_submenu(ix, cx)),
                                )
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.run_submenu(ix, cx)),
                                )
                            })
                            .child(
                                theme::row(on)
                                    .when(!enabled, |d| d.text_color(content(0.65)))
                                    .children(
                                        row.app
                                            .as_ref()
                                            .map(|app| file_icons::icon(app, 16., window, cx)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(row.title.clone()),
                                    ),
                            ),
                    )
            })
            .collect();
        Some(
            card(WIDTH, viewport.height - px(2. * MARGIN), items)
                .id("context-submenu")
                .debug_selector(|| "context-submenu".into())
                .role(Role::Menu)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .absolute()
                .left(at.x)
                .top(at.y),
        )
    }
}

/// How tall a card of rows is, given whether each starts a new section.
fn rows_height(separated: impl Iterator<Item = bool>) -> f32 {
    let (mut rows, mut separators) = (0, 0);
    for separated in separated {
        rows += 1;
        separators += usize::from(separated);
    }
    2. * (PADDING + 1.)
        + (theme::ROW_HEIGHT + theme::ROW_GAP) * rows as f32
        + SEPARATOR * separators as f32
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

/// Where a submenu of `size` goes, beside a menu `width` wide whose row starts
/// at `row`: to the right, or to the left where the window runs out, and moved
/// up to fit its height.
fn place_submenu(
    row: Point<Pixels>,
    width: f32,
    size: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    let wide = size.width.as_f32();
    let right = row.x.as_f32() + width - OVERLAP;
    let left = row.x.as_f32() - wide + OVERLAP;
    let x = if right + wide + MARGIN <= viewport.width.as_f32() || left < MARGIN {
        right.min(viewport.width.as_f32() - wide - MARGIN)
    } else {
        left
    };
    let lowest = viewport.height.as_f32() - size.height.as_f32() - MARGIN;
    point(
        px(x.max(MARGIN)),
        px(row.y.as_f32().min(lowest).max(MARGIN)),
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
                        .debug_selector(move || format!("menu-row-{ix}"))
                        .role(Role::MenuItem)
                        .aria_label(row.title)
                        .aria_selected(active)
                        .when_some(row.keys.clone(), |d, keys| d.aria_keyshortcuts(keys))
                        .child(
                            if row.destructive {
                                theme::destructive_row(active)
                            } else {
                                theme::row(active)
                            }
                            .when(checks, |d| d.child(check(row.checked, active)))
                            .child(div().flex_1().min_w_0().truncate().child(row.title))
                            .when(row.submenu, |d| {
                                d.child(
                                    icon("chevron-right", 12.)
                                        .flex_shrink_0()
                                        .text_color(theme::row_muted(active)),
                                )
                            })
                            .children(row.keys.clone().map(|keys| {
                                shortcut(
                                    keys,
                                    if row.destructive {
                                        theme::destructive_ink(active)
                                    } else {
                                        theme::row_muted(active)
                                    },
                                )
                            })),
                        )
                        .on_mouse_move(cx.listener(move |this, _, _, cx| this.hover_row(ix, cx)))
                        .on_click(cx.listener(move |this, _, _, cx| this.run(ix, cx))),
                )
        });
        let rows: Vec<_> = rows.collect();
        let submenu = self.submenu_card(corner, window, cx);
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
                        if this.keys_in_submenu() {
                            if let Some(ix) = this.submenu.as_ref().and_then(|s| s.active) {
                                this.run_submenu(ix, cx);
                            }
                        } else if let Some(ix) = this.active {
                            this.run(ix, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &EnterSubmenu, _, cx| {
                        let ix = this.active.filter(|ix| {
                            !this.keys_in_submenu()
                                && this.rows.get(*ix).is_some_and(|row| row.submenu)
                        });
                        if let Some(ix) = ix {
                            this.open_submenu(ix, true, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &LeaveSubmenu, _, cx| this.leave_submenu(cx)))
                    .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(MenuEvent::Dismissed)))
                    .on_key_down(cx.listener(Self::jump))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .absolute()
                    .left(corner.x)
                    .top(corner.y),
            )
            .children(submenu)
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

pub(crate) fn shortcut(keys: String, ink: gpui::Hsla) -> gpui::Div {
    div()
        .flex_shrink_0()
        .text_size(px(theme::TEXT_CAPTION))
        .text_color(ink)
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
    fn a_submenu_opens_beside_its_row_and_stays_inside_the_window() {
        let window = size(px(800.), px(500.));
        let submenu = size(px(240.), px(200.));
        let at = |x: f32, y: f32| place_submenu(point(px(x), px(y)), 240., submenu, window);
        assert_eq!(at(100., 50.), point(px(338.), px(50.)));
        // Near the right edge it goes to the left, and near the bottom it
        // moves up.
        assert_eq!(at(500., 50.), point(px(262.), px(50.)));
        assert_eq!(at(100., 400.), point(px(338.), px(292.)));
        // Room on neither side keeps it in the window.
        let narrow = size(px(400.), px(500.));
        let there = place_submenu(point(px(100.), px(50.)), 240., submenu, narrow);
        assert_eq!(there, point(px(152.), px(50.)));
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
