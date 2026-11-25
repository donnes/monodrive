//! How a pane draws, its path bar, the list and the grid, and how it answers
//! the keys and the pointer: moving the selection, opening folders in place,
//! and the rectangle dragged over the list.

use super::drag::{Spot, Target, offers};
use super::rows::GRID_BADGE;
use super::{
    Back, ClearRecents, Compress, CopyPath, DeselectAll, Download, Duplicate, EditPreview,
    EnclosingFolder, ExpandFolder, ExtendLeft, ExtendNext, ExtendPrevious, ExtendRight, Extract,
    FindDuplicates, FocusSearch, Forward, GoAirDrop, GoApplications, GoComputer, GoDesktop,
    GoDocuments, GoDownloads, GoHome, GoLocalSend, GoOverview, GoRecents, GoToFolder, GoTrash,
    GoWorktrees, MoveHere, MoveToTrash, NewFile, NewFolder, NewFolderWithSelection, OpenInTerminal,
    OpenSelected, OrganizeFolder, PageDown, PageUp, PutBack, QuickLook, Redo, RemoveDownload,
    Rename, RevealInFinder, ReverseSort, ScrollToBottom, ScrollToTop, SearchContents,
    SearchThisFolder, SelectFirst, SelectLast, SelectNext, SelectPrevious, SendWithLocalSend,
    Share, ShowContextMenu, ShowInEnclosingFolder, ShowPackageContents, SortByKind, SortByModified,
    SortByName, SortBySize, SuggestNames, Undo, ViewAsColumns, ViewAsGrid, ViewAsList, *,
};
use crate::background::Look;
use crate::commands::Eject;
use crate::format::format_bytes;
use crate::goto::{GotoEvent, GotoView};
use crate::preferences::{Preferences, View, WorkspaceStyle};
use crate::session::Shows;
use crate::text_field::{self};
use crate::theme::{self, ButtonLook, accent, content, icon, on_accent, text_button};
use crate::thumbnails::{self, Thumb};
use crate::{file_icons, home};
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, ClickEvent, ClipboardItem, Context, DispatchPhase, Div, Entity,
    FocusHandle, Focusable, FontWeight, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, ScrollStrategy, ScrollWheelEvent, Subscription, Window, canvas,
    div, px, uniform_list,
};
use monodrive_core::cloud::CloudFolder;
use monodrive_core::entries::{Sort, SortBy};
use monodrive_core::ops::Request;
use std::path::{Path, PathBuf};

/// The cells `step` apart from 0, of `count`, that `from..=to` overlaps.
pub(in crate::browser) fn span(
    from: f32,
    to: f32,
    step: f32,
    count: usize,
) -> std::ops::Range<usize> {
    if to < 0. || count == 0 || step <= 0. {
        return 0..0;
    }
    let first = ((from.max(0.) / step) as usize).min(count);
    let last = ((to / step) as usize).min(count - 1);
    first..(last + 1).max(first)
}

impl Pane {
    /// → opens the selected folder in place, or steps into it if it's open.
    pub(in crate::browser) fn expand(&mut self, cx: &mut Context<Self>) {
        if self.showing_duplicates() {
            return self.open_duplicate_group(cx);
        }
        match self.layout() {
            View::Grid => return self.select(Move::By(1), false, cx),
            View::Columns => return self.enter_column(cx),
            View::List => {}
        }
        if !self.showing_folder() || self.listing {
            return;
        }
        let Some(ix) = self.cursor_ix() else { return };
        match tree_step(&self.depths, ix, Arrow::Right, self.is_open_folder(ix)) {
            TreeStep::Toggle => self.toggle_folder(self.entries[ix].path.clone(), cx),
            TreeStep::Select(to) => self.select_row(to, cx),
            TreeStep::Up | TreeStep::Stay => {}
        }
    }
    /// ← closes the selected folder, steps out to its parent row, or at the
    /// top goes up to the enclosing folder. Going up takes a fresh press, so
    /// holding ← to fold a tree stops at its top instead of climbing on.
    pub(in crate::browser) fn collapse(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        if keystroke.key != "left" || keystroke.modifiers.modified() || self.renaming.is_some() {
            return;
        }
        cx.stop_propagation();
        match self.layout() {
            View::Grid => return self.select(Move::By(-1), false, cx),
            View::Columns => return self.leave_column(event.is_held, cx),
            View::List => {}
        }
        if !self.showing_folder() || self.listing {
            return;
        }
        let cursor = self.cursor_ix();
        let step = match cursor {
            Some(ix) => tree_step(&self.depths, ix, Arrow::Left, self.is_open_folder(ix)),
            None => TreeStep::Up,
        };
        match step {
            TreeStep::Toggle => {
                if let Some(ix) = cursor {
                    self.toggle_folder(self.entries[ix].path.clone(), cx)
                }
            }
            TreeStep::Select(to) => self.select_row(to, cx),
            TreeStep::Up if event.is_held => {}
            TreeStep::Up => {
                if let Some(parent) = self.folder().parent().map(Path::to_path_buf) {
                    self.navigate(parent, cx);
                }
            }
            TreeStep::Stay => {}
        }
    }
    pub(in crate::browser) fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        self.selection.select(entry);
        self.reveal(ix, ScrollStrategy::Nearest);
        cx.notify();
    }
    /// ↑ and ↓: a row of the list, or of the grid.
    pub(in crate::browser) fn select_vertically(
        &mut self,
        down: bool,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if self.showing_overview() {
            return self.step_overview(down, cx);
        }
        if self.showing_worktrees() {
            return self.step_worktree(down, cx);
        }
        if self.showing_duplicates() {
            return self.step_duplicates(down, cx);
        }
        let step = match (self.layout(), down) {
            (View::List | View::Columns, true) => Move::Next,
            (View::List | View::Columns, false) => Move::Previous,
            (View::Grid, _) => {
                let across = self.grid_columns() as isize;
                Move::By(if down { across } else { -across })
            }
        };
        self.select(step, extend, cx);
    }
    /// Scrolls a page, leaving the selection where it is.
    pub(in crate::browser) fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        let scroll = &self.scroll.0.borrow().base_handle;
        // A page less one row, so the row at the edge stays in view.
        let page = scroll.bounds().size.height - px(self.row_pitch(cx));
        let mut offset = scroll.offset();
        // Offsets run negative as the list scrolls down.
        offset.y = if down {
            (offset.y - page).max(-scroll.max_offset().y)
        } else {
            (offset.y + page).min(px(0.))
        };
        scroll.set_offset(offset);
        cx.notify();
    }
    pub(in crate::browser) fn type_to_search(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Delete takes a picked-up mark off the picture, not the file.
        let modifiers = event.keystroke.modifiers;
        if matches!(event.keystroke.key.as_str(), "backspace" | "delete")
            && !modifiers.modified()
            && !self.changing_names()
            && let Some(view) = &self.preview
            && view.update(cx, |view, cx| view.delete_mark(window, cx))
        {
            return cx.stop_propagation();
        }
        // A name being edited takes its own typing.
        let Some(text) = typed(&event.keystroke).filter(|_| !self.changing_names()) else {
            return;
        };
        let text = text.to_owned();
        if let Some(view) = &self.preview {
            let editing = view.read(cx).editing();
            if text == "e" {
                self.edit_preview(window, cx);
                return cx.stop_propagation();
            }
            // A stray letter mustn't start a search and lose an edit.
            if editing {
                return cx.stop_propagation();
            }
        }
        self.search.update(cx, |field, cx| {
            window.focus(&field.focus_handle(cx), cx);
            field.insert(&text, cx);
        });
        cx.stop_propagation();
    }
    /// Esc: leaves search, or else lets go of the selection.
    pub(in crate::browser) fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.batch.is_some() {
            return self.end_batch(window, cx);
        }
        if self.organizing.is_some() {
            return self.end_organize(window, cx);
        }
        if self.showing_duplicates() && self.stop_duplicates(cx) {
            return;
        }
        if self.showing_localsend() && self.localsend.history {
            self.localsend.history = false;
            return cx.notify();
        }
        if self.showing_localsend() && self.localsend.sending.take().is_some() {
            return cx.notify();
        }
        if self.query.is_empty() {
            if self.search_mode_is_plain() {
                self.selection.clear();
            } else {
                self.reset_search_mode(cx);
            }
            cx.notify();
        } else {
            self.show_folder(cx);
        }
        self.focus_list(window, cx);
    }
    pub(in crate::browser) fn take_pending_reveal(&mut self) {
        if std::mem::take(&mut self.reveal_pending)
            && let Some(ix) = self.cursor_ix()
        {
            self.reveal(ix, ScrollStrategy::Center);
        }
    }
    /// ⌥⌘1 and ⌥⌘2: this pane's view, and where the next tab starts.
    pub(in crate::browser) fn choose_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.set_view(view);
        preferences::update(cx, |p| p.view = view);
        cx.emit(PaneEvent::Moved);
        cx.notify();
    }
    /// A Sort by command or a column title: this pane's order, and where the
    /// next tab starts.
    pub(in crate::browser) fn choose_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
        if sort != self.sort && !self.batch_sending() {
            self.sort = sort;
            preferences::update(cx, |p| p.sort = sort);
            cx.emit(PaneEvent::Moved);
            if self.showing_remote() {
                self.resort_remote(cx);
            } else if self.query.is_empty() {
                self.reload(cx);
            }
        }
    }
    /// How many grid cells fit across, as last laid out.
    pub(in crate::browser) fn grid_columns(&self) -> usize {
        self.across
    }
    /// A right-click on row `ix`, or on no row; GPUI delivers ⌃-click as
    /// one. An entry outside
    /// the selection becomes the selection first, as in Finder, and no row
    /// lets go of it, so the menu is on the folder.
    pub(in crate::browser) fn context_click(
        &mut self,
        ix: Option<usize>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        if self.batch.is_some() {
            return;
        }
        self.focus_list(window, cx);
        match ix.and_then(|ix| self.entries.get(ix)) {
            Some(entry) if !self.selection.contains(&entry.path) => self.selection.select(entry),
            Some(_) => {}
            None => self.selection.clear(),
        }
        let scopes = self.scopes();
        cx.emit(PaneEvent::Menu { scopes, at });
    }
    /// Preferences changed: lists again if the hidden files did. With `adopt`,
    /// Settings chose them for this pane, so it takes their view and sort.
    pub(in crate::browser) fn preferences_changed(&mut self, adopt: bool, cx: &mut Context<Self>) {
        if self.showing_localsend() {
            match cx.global::<Preferences>().localsend {
                true => self.enter_localsend(cx),
                false => self.leave_localsend(cx),
            }
        }
        let prefs = cx.global::<Preferences>();
        let (view, sort, show_hidden) = (prefs.view, prefs.sort, prefs.show_hidden);
        self.grid_icon = prefs.grid_icon;
        let sorted = adopt && sort != self.sort;
        if adopt {
            self.sort = sort;
            self.set_view(view);
            cx.emit(PaneEvent::Moved);
        }
        if (sorted || show_hidden != self.show_hidden) && self.query.is_empty() {
            self.reload(cx);
        }
        cx.notify();
    }
    pub(in crate::browser) fn set_view(&mut self, view: View) {
        if view != self.view {
            self.view = view;
            // The chain starts at the folder it's switched in.
            self.columns.root = None;
            self.rebuild();
            // The other layout put it somewhere else; the grid shows it
            // once it knows how many fit across.
            self.reveal_pending = self.selection.cursor().is_some();
        }
    }
    /// A Sort by command picks a column in its natural direction; choosing the
    /// sort in use again leaves it, as a checked menu item should. Reverse
    /// Sort Order flips it.
    pub(in crate::browser) fn sort_by(&mut self, by: SortBy, cx: &mut Context<Self>) {
        if self.sort.by != by {
            self.choose_sort(sorted_by(self.sort, by), cx);
        }
    }
    pub(in crate::browser) fn collapse_all(&mut self) {
        self.opening = Task::ready(());
        self.peek_hidden = false;
        // A refresh due for the folder being left has nothing to refresh.
        self.refresh = None;
        self.held.clear();
        self.expanded.clear();
        self.expanding.clear();
        if self.layout() == View::Columns {
            // The folder being left may become a column; the next listing
            // replaces what it keeps.
            self.children
                .insert(self.folder().to_path_buf(), self.root.clone());
        } else {
            self.children.clear();
        }
    }
    /// `rebuild`; `follow` scrolls to the selection if rows moved it.
    pub(in crate::browser) fn relayout(&mut self, follow: bool) {
        let before = self.cursor_ix();
        let rows = if self.showing_folder() && self.view == View::List {
            flatten(&self.root, &self.children, &self.expanded)
        } else {
            self.root.iter().cloned().map(|entry| (entry, 0)).collect()
        };
        (self.entries, self.depths) = rows.into_iter().unzip();
        self.selection.retain(&self.entries);
        // Rows opening above the selection push it down; keep it in view.
        if let Some(ix) = self.cursor_ix().filter(|ix| follow && Some(*ix) != before) {
            self.reveal(ix, ScrollStrategy::Nearest);
        }
    }

    /// A point in the window as a point in the listing, measured from the
    /// top left of all its rows, scrolled or not.
    pub(in crate::browser) fn listing_point(&self, point: Point<Pixels>) -> Point<Pixels> {
        let offset = self.scroll.0.borrow().base_handle.offset();
        point - self.list_bounds.get().origin - offset
    }
    /// The rows a rectangle in listing coordinates touches, in order.
    pub(in crate::browser) fn rows_in(&self, rect: Bounds<Pixels>, cx: &App) -> Vec<usize> {
        let len = self.entries.len();
        // The list's padding, then in the grid each row's too.
        let pad = match self.layout() {
            View::List | View::Columns => theme::CARD_PADDING,
            View::Grid => 4.,
        };
        let (top, bottom) = (rect.top().as_f32() - pad, rect.bottom().as_f32() - pad);
        match self.layout() {
            View::List | View::Columns => {
                let height = self.row_pitch(cx);
                let rows = span(top, bottom, height, self.matches.row_count(len));
                self.matches.entries_in(rows)
            }
            View::Grid => {
                let across = self.across;
                let height = grid_cell(self.grid_icon).1;
                let (left, right) = (rect.left().as_f32() - 8., rect.right().as_f32() - 8.);
                let columns = span(left, right, self.cell_width, across);
                span(top, bottom, height, len.div_ceil(across))
                    .flat_map(|row| columns.clone().map(move |column| row * across + column))
                    .filter(|ix| *ix < len)
                    .collect()
            }
        }
    }
    /// Follows the pointer while a rectangle is dragged. A move with the
    /// button up means its release went unseen, so the press ends there.
    pub(in crate::browser) fn drag_marquee(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.pressed_button != Some(MouseButton::Left) {
            self.end_marquee(event.position, event.modifiers, cx);
            return;
        }
        let Some(marquee) = self.marquee.as_mut() else {
            return;
        };
        marquee.pointer = event.position;
        self.sweep_marquee(cx);
        self.auto_scroll(window, cx);
    }
    /// Selects what the rectangle covers, from where it began to the
    /// pointer, once the pointer has travelled a few points.
    pub(in crate::browser) fn sweep_marquee(&mut self, cx: &mut Context<Self>) {
        let Some(pointer) = self.marquee.as_ref().map(|m| m.pointer) else {
            return;
        };
        let at = self.listing_point(pointer);
        let Some(marquee) = self.marquee.as_mut() else {
            return;
        };
        marquee.end = at;
        let travel = at - marquee.start;
        if !marquee.dragging && travel.x.abs().max(travel.y.abs()) > px(3.) {
            marquee.dragging = true;
            self.selection.begin_sweep(marquee.additive);
        }
        if !marquee.dragging {
            return;
        }
        let rect = Bounds::from_corners(marquee.start.min(&at), marquee.start.max(&at));
        let hit = self.rows_in(rect, cx);
        self.selection.sweep(&self.entries, &hit);
        cx.notify();
    }
    /// Past the listing's top or bottom, a drag scrolls it a step a frame,
    /// faster the further out the pointer is, until the pointer comes back,
    /// the listing can't go further, or the button comes up.
    pub(in crate::browser) fn auto_scroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(marquee) = self.marquee.as_mut().filter(|m| m.dragging && !m.scrolling) else {
            return;
        };
        let bounds = self.list_bounds.get();
        let y = marquee.pointer.y;
        let over = if y < bounds.top() {
            y - bounds.top()
        } else if y > bounds.bottom() {
            y - bounds.bottom()
        } else {
            return;
        };
        let step = px((over.as_f32() / 4.).clamp(-40., 40.));
        {
            let scroll = &self.scroll.0.borrow().base_handle;
            let before = scroll.offset();
            // Offsets run negative as the list scrolls down.
            let mut offset = before;
            offset.y = (offset.y - step).clamp(-scroll.max_offset().y, px(0.));
            if offset == before {
                return;
            }
            scroll.set_offset(offset);
        }
        marquee.scrolling = true;
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            let _ = this.update(cx, |this, cx| this.scroll_tick(window, cx));
        });
        cx.notify();
    }
    /// The frame an auto-scroll step waited on: the rows under the
    /// rectangle have moved, so sweep again, then take the next step.
    pub(in crate::browser) fn scroll_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(marquee) = self.marquee.as_mut() {
            marquee.scrolling = false;
        }
        self.sweep_marquee(cx);
        self.auto_scroll(window, cx);
    }
    /// The button's up: a drag keeps what it covered, and a click on no
    /// entry, without ⌘ or ⇧, lets go of the selection, as in Finder.
    /// Clicks on entries select through `click_entry`.
    pub(in crate::browser) fn end_marquee(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let Some(marquee) = self.marquee.take() else {
            return;
        };
        self.swept = marquee.dragging;
        if marquee.dragging {
            self.selection.end_sweep();
        } else {
            let at = self.listing_point(position);
            let on_entry = !self.rows_in(Bounds::from_corners(at, at), cx).is_empty();
            if !on_entry && !(modifiers.platform || modifiers.shift) {
                self.selection.clear();
            }
        }
        cx.notify();
    }
    /// Follows a rectangle's drag through the window, as `sidebar_drag`
    /// does, so it keeps up when the pointer leaves the listing.
    pub(in crate::browser) fn marquee_drag(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let this = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let moved = this.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = moved.update(cx, |this, cx| this.drag_marquee(event, window, cx));
                    }
                });
                // A trackpad scroll mid-drag moves the rows under the
                // rectangle; sweep again once the list has scrolled.
                let scrolled = this.clone();
                window.on_mouse_event(move |_: &ScrollWheelEvent, phase, window, _| {
                    if phase == DispatchPhase::Bubble {
                        let scrolled = scrolled.clone();
                        window.on_next_frame(move |_, cx| {
                            let _ = scrolled.update(cx, |this, cx| this.sweep_marquee(cx));
                        });
                    }
                });
                let released = this.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                        let _ = released.update(cx, |this, cx| {
                            this.end_marquee(event.position, event.modifiers, cx)
                        });
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    /// The folder's path as clickable steps, from the disk to here.
    pub(in crate::browser) fn crumbs(
        &self,
        room: f32,
        most: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let current = self.folder().to_path_buf();
        let steps: Vec<PathBuf> = current.ancestors().map(Path::to_path_buf).collect();
        let last = steps.len().saturating_sub(1);
        let widths: Vec<f32> = steps
            .iter()
            .rev()
            .enumerate()
            .map(|(ix, path)| {
                let weight = if ix == last {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::NORMAL
                };
                let text = text_width(&level_name(path), theme::TEXT_SMALL, weight, window);
                text + CRUMB_PADDING + if ix > 0 { CRUMB_CHEVRON } else { 0. }
            })
            .collect();
        let ellipsis =
            text_width(COLLAPSED, theme::TEXT_SMALL, FontWeight::NORMAL, window) + CRUMB_PADDING;
        let hidden = hidden_levels(&widths, ellipsis, room);
        // The folder itself has its pane's say on whether files land in it.
        let takes_here = self.takes_files(cx);
        let dragging = self.drag.active();
        let id = cx.entity_id();
        div()
            .flex_shrink_0()
            .max_w(px(most))
            .flex()
            .items_center()
            .gap(px(CRUMB_GAP))
            .text_size(px(theme::TEXT_SMALL))
            .overflow_hidden()
            .when(hidden > 0, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(px(theme::CRUMB_INSET))
                        .text_color(content(0.65))
                        .child(COLLAPSED),
                )
            })
            .children(
                steps
                    .iter()
                    .rev()
                    .cloned()
                    .enumerate()
                    .skip(hidden)
                    .map(|(ix, path)| {
                        let name = level_name(&path);
                        let here = ix == last;
                        let spot = Spot::Crumb(id, ix);
                        let outlined = self.drag.outlines(&spot);
                        let in_archive = self
                            .stop()
                            .archive
                            .as_ref()
                            .is_some_and(|archive| path.starts_with(archive));
                        let drops = dragging && !in_archive && (!here || takes_here);
                        let target = drops.then(|| Target::Folder {
                            spot,
                            path: path.clone(),
                            name: name.clone(),
                        });
                        // Clicking an ancestor selects the folder on the way back down.
                        let child = current
                            .ancestors()
                            .find(|step| step.parent() == Some(path.as_path()))
                            .map(Path::to_path_buf);
                        div()
                            .flex()
                            .items_center()
                            .gap(px(CRUMB_GAP))
                            .min_w_0()
                            .when(ix > 0, |d| {
                                d.child(icon("chevron-right", 12.).text_color(content(0.3)))
                            })
                            .child(
                                div()
                                    .id(("path-step", ix))
                                    .debug_selector(move || format!("crumb-{ix}"))
                                    .role(gpui::Role::Link)
                                    .aria_label(name.clone())
                                    .px(px(theme::CRUMB_INSET))
                                    .rounded_full()
                                    .line_height(px(18.))
                                    .truncate()
                                    .map(|d| {
                                        if here {
                                            d.font_weight(FontWeight::SEMIBOLD)
                                        } else {
                                            d.text_color(content(0.65))
                                                .cursor_pointer()
                                                .hover(|s| {
                                                    s.bg(content(0.08)).text_color(content(1.))
                                                })
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.go_to(
                                                            path.clone(),
                                                            child.iter().cloned().collect(),
                                                            window,
                                                            cx,
                                                        )
                                                    },
                                                ))
                                        }
                                    })
                                    .when_some(target, |d, target| {
                                        let drag = self.drag.clone();
                                        offers(d, move |_| drag.offer(target.clone()))
                                    })
                                    .child(name)
                                    .when(outlined, |d| {
                                        d.relative().child(theme::drop_outline().rounded_full())
                                    }),
                            )
                    }),
            )
    }
    /// The thin bar under the files: the path on the left, what's selected
    /// and the free space on the right. It belongs to the pane, so each pane
    /// gets its own. The path has what the status leaves, and only when the
    /// folder's own name won't fit in that does the status give way.
    pub(in crate::browser) fn path_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if let Some(address) = &self.address {
            return Some(
                theme::pane_bar(theme::PATH_BAR_HEIGHT)
                    .debug_selector(|| "path-bar".into())
                    .border_t_1()
                    .border_color(content(0.06))
                    .child(address.view.clone()),
            );
        }
        if !cx.global::<Preferences>().show_path_bar
            || self.unavailable.is_some()
            || self.showing_page()
        {
            return None;
        }
        if self.showing_remote() {
            let status = self.remote_status();
            let most = (self.chrome.width - PATH_BAR_CHROME).max(0.);
            let taken =
                text_width(&status, theme::TEXT_SMALL, FontWeight::NORMAL, window) + 2. * 12.;
            return Some(
                theme::pane_bar(theme::PATH_BAR_HEIGHT)
                    .debug_selector(|| "path-bar".into())
                    .pl(px(theme::PANE_INSET - theme::CRUMB_INSET))
                    .gap_3()
                    .border_t_1()
                    .border_color(content(0.06))
                    .text_size(px(theme::TEXT_SMALL))
                    .child(self.remote_crumbs(most - taken, window, cx))
                    .child(div().flex_1())
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(content(0.65))
                            .child(status),
                    ),
            );
        }
        // As Finder's status bar: what's selected and the size of its files.
        let count = match (self.selection.len(), self.entries.len()) {
            (0, 1) => "1 item".to_string(),
            (0, n) => format!("{n} items"),
            (chosen, _) => match self.selection.size() {
                0 => format!("{chosen} of {} selected", self.entries.len()),
                size => format!(
                    "{chosen} of {} selected, {}",
                    self.entries.len(),
                    format_bytes(size)
                ),
            },
        };
        let count = if self.matches.cut {
            format!("{count}, more not shown")
        } else {
            count
        };
        let status = match self.chrome.space.clone() {
            Some(space) => format!("{count} · {space}"),
            None => count,
        };
        let width = self.chrome.width - PATH_BAR_CHROME;
        let most = width.max(0.);
        let taken = text_width(&status, theme::TEXT_SMALL, FontWeight::NORMAL, window) + 2. * 12.;
        let crumbs = self.showing_folder() || self.showing_archive();
        Some(
            theme::pane_bar(theme::PATH_BAR_HEIGHT)
                .debug_selector(|| "path-bar".into())
                // The first crumb's padding is part of the inset, so its text
                // lines up with the bars above.
                .when(crumbs, |d| d.pl(px(theme::PANE_INSET - theme::CRUMB_INSET)))
                .gap_3()
                .border_t_1()
                .border_color(content(0.06))
                .text_size(px(theme::TEXT_SMALL))
                .when(crumbs, |d| {
                    d.child(self.crumbs(most - taken, most, window, cx))
                })
                .child(div().flex_1())
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(content(0.65))
                        .child(status),
                ),
        )
    }
    /// The grid's rows of cells: an icon or picture over the name.
    pub(in crate::browser) fn grid_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let prefs = cx.global::<Preferences>();
        let side = prefs.grid_icon as f32;
        // An archive's entries have no file on the disk to picture or drag.
        let archive = self.showing_archive();
        let label = prefs.grid_label.metrics().2;
        let (cell, height) = (self.cell_width, grid_cell(prefs.grid_icon).1);
        let across = self.across;
        let active = self.list_active(window, cx);
        let drops = self.drag.active() && !self.in_trash(cx) && !archive;
        let id = cx.entity_id();
        // Sharp on Retina screens.
        let pixels = (side * window.scale_factor()).ceil() as u32;
        let shown = range.clone();
        let rows = range
            .map(|row| {
                let cells =
                    (row * across..((row + 1) * across).min(self.entries.len())).map(|ix| {
                        let entry = &self.entries[ix];
                        let selected = self.selection.contains(&entry.path);
                        let missing = self.missing.contains(&entry.path);
                        let picture = if archive {
                            None
                        } else {
                            thumbnail(
                                (&mut self.thumbs, &mut self.drawn),
                                entry,
                                pixels,
                                window,
                                cx,
                            )
                        };
                        let art = match picture {
                            // A square box: without one, `img` takes the
                            // picture's own shape, and a tall one spills
                            // onto its name.
                            Some(image) => gpui::img(image)
                                .size(px(side))
                                .aspect_square()
                                .object_fit(gpui::ObjectFit::Contain)
                                .rounded(px(theme::RADIUS_THUMB))
                                .into_any_element(),
                            None if archive => {
                                file_icons::kind_icon(entry.kind, &entry.path, side, window, cx)
                            }
                            None => file_icons::icon(entry, side, window, cx),
                        };
                        let rename = self.rename_field(&entry.path, cell - 8., cx);
                        let grabs = !missing && rename.is_none() && !archive;
                        let badge = self.cloud_badge(
                            entry,
                            ix,
                            GRID_BADGE,
                            match (selected, active) {
                                (true, true) => on_accent(),
                                (true, false) => content(1.),
                                (false, _) => content(0.55),
                            },
                            cx,
                        );
                        div()
                            .id(ix)
                            .role(gpui::Role::ListItem)
                            .aria_label(entry.name.clone())
                            .aria_selected(selected)
                            .w(px(cell))
                            .h(px(height))
                            .when(missing, |d| d.opacity(0.5))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(6.))
                            .pt(px(8.))
                            .child(
                                div()
                                    .id(("grab-art", ix))
                                    .p_1()
                                    .rounded(px(theme::RADIUS_ROW))
                                    .when(selected, |d| d.bg(content(0.14)))
                                    .when(grabs, |d| Self::grab(d, ix, cx))
                                    .child(art),
                            )
                            .child(match rename {
                                Some(field) => field.text_size(px(label)).into_any_element(),
                                None => div()
                                    .id(("grab-name", ix))
                                    .when(grabs, |d| Self::grab(d, ix, cx))
                                    .max_w(px(cell - 8.))
                                    .px(px(6.))
                                    .rounded(px(theme::RADIUS_ROW))
                                    .text_size(px(label))
                                    .text_center()
                                    .map(|d| {
                                        if selected {
                                            d.bg(if active { accent() } else { content(0.14) })
                                                .text_color(if active {
                                                    on_accent()
                                                } else {
                                                    content(1.)
                                                })
                                        } else {
                                            d
                                        }
                                    })
                                    .children(grid_name(
                                        &entry.name,
                                        px(cell - 18.),
                                        px(label),
                                        badge,
                                        window,
                                    ))
                                    .into_any_element(),
                            })
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.click_entry(ix, event, window, cx)
                            }))
                            .map(|d| Self::context_target(d, ix, cx))
                            .when(drops, |d| Self::takes_drops(d, ix, cx))
                            .when(self.drag.outlines(&Spot::Row(id, ix)), |d| {
                                d.relative().child(
                                    self.drag.pulsed(
                                        theme::drop_outline().rounded(px(theme::RADIUS_ROW)),
                                    ),
                                )
                            })
                    });
                div().flex().px_1().children(cells).into_any_element()
            })
            .collect();
        // After the cells on screen, so their pictures decode first.
        if !archive {
            self.prefetch_pictures(shown, pixels, window, cx);
        }
        rows
    }
    /// Starts the pictures of the grid's rows a screen above and below
    /// `shown`, so scrolling finds them decoded rather than as icons.
    pub(in crate::browser) fn prefetch_pictures(
        &mut self,
        shown: std::ops::Range<usize>,
        pixels: u32,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (across, len) = (self.across.max(1), self.entries.len());
        let near = shown.start.saturating_sub(shown.len())..shown.end + shown.len();
        let cells = |rows: std::ops::Range<usize>| {
            (rows.start * across).min(len)..(rows.end * across).min(len)
        };
        let (above, below) = (cells(near.start..shown.start), cells(shown.end..near.end));
        for ix in above.chain(below) {
            let entry = &self.entries[ix];
            thumbnail(
                (&mut self.thumbs, &mut self.drawn),
                entry,
                pixels,
                window,
                cx,
            );
        }
    }
    /// The commands that act on this pane wherever the keys are in the window,
    /// so the window puts them on its root for the pane they're meant for.
    pub(in crate::browser) fn commands(
        &mut self,
        root: Div,
        sheet: bool,
        cloud: &[CloudFolder],
        cx: &mut Context<Self>,
    ) -> Div {
        if self.showing_remote() {
            return self.remote_commands(root, sheet, cx);
        }
        // What's in an archive isn't on the disk for macOS or other apps to
        // reach.
        let on_disk = !self.showing_archive();
        let package = self.package().is_some();
        let download = on_disk && self.can_download(cx);
        let remove_download = on_disk && !self.removable(cloud).is_empty();
        // Nothing new goes in search results, Recents or the Trash.
        let can_make_here = self.takes_files(cx);
        let put_back = !self.put_back().0.is_empty() && self.in_trash(cx);
        let selected = !self.selection.is_empty();
        let localsend = cx.global::<Preferences>().localsend;
        let scattered = self.showing_recents() || !self.query.is_empty();
        let renaming = self.changing_names();
        let can_extract = self.can_extract(cx);
        let can_compress = self.can_compress(cx);
        // While a sheet is open, its commands (from the menu bar too) wait.
        root.when(!sheet, |d| {
            d.on_action(cx.listener(|this, _: &Back, window, cx| this.go_back(window, cx)))
                .on_action(cx.listener(|this, _: &Forward, _, cx| this.step_history(1, cx)))
                .on_action(cx.listener(|this, _: &EnclosingFolder, _, cx| {
                    if let Some(parent) = this.folder().parent().map(Path::to_path_buf) {
                        this.navigate(parent, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                    this.select_vertically(false, false, cx)
                }))
                .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                    this.select_vertically(true, false, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &SelectFirst, _, cx| this.select(Move::First, false, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &SelectLast, _, cx| this.select(Move::Last, false, cx)),
                )
                .on_action(cx.listener(|this, _: &GoHome, window, cx| this.go(home(), window, cx)))
                .on_action(
                    cx.listener(|this, _: &GoRecents, window, cx| this.open_recents(window, cx)),
                )
                .on_action(cx.listener(|this, _: &GoOverview, window, cx| {
                    this.open_page(Shows::Overview, window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoWorktrees, window, cx| {
                    this.open_page(Shows::Worktrees, window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoAirDrop, window, cx| {
                    this.open_page(Shows::AirDrop, window, cx)
                }))
                .when(localsend, |d| {
                    d.on_action(cx.listener(|this, _: &GoLocalSend, window, cx| {
                        this.open_page(Shows::LocalSend, window, cx)
                    }))
                })
                .when(self.showing_folder(), |d| {
                    d.on_action(cx.listener(|this, _: &FindDuplicates, window, cx| {
                        this.find_duplicates(window, cx)
                    }))
                })
                .on_action(cx.listener(|this, _: &GoTrash, window, cx| this.open_trash(window, cx)))
                .on_action(cx.listener(|this, _: &GoDesktop, window, cx| {
                    this.go(home().join("Desktop"), window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoDocuments, window, cx| {
                    this.go(home().join("Documents"), window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoDownloads, window, cx| {
                    this.go(home().join("Downloads"), window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoApplications, window, cx| {
                    this.go("/Applications".into(), window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &GoComputer, window, cx| this.go("/".into(), window, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &OpenSelected, window, cx| {
                        this.open_selection(window, cx)
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)),
                )
                .on_action(cx.listener(|this, _: &SearchContents, window, cx| {
                    this.toggle_contents(window, cx)
                }))
                .on_action(cx.listener(|this, _: &DeselectAll, _, cx| {
                    this.selection.clear();
                    cx.notify();
                }))
                .when(on_disk, |d| {
                    d.on_action(cx.listener(|this, _: &SearchThisFolder, window, cx| {
                        this.toggle_this_folder(window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(cx)))
                    .on_action(
                        cx.listener(|this, _: &OpenInTerminal, _, cx| this.open_in_terminal(cx)),
                    )
                })
                .when(selected && on_disk, |d| {
                    d.on_action(cx.listener(|this, _: &RevealInFinder, _, _| {
                        crate::finder::reveal(&this.chosen())
                    }))
                    .on_action(cx.listener(|this, _: &Share, window, cx| {
                        let at = this.menu_point(cx);
                        this.share(window, at, cx)
                    }))
                    .when(localsend, |d| {
                        d.on_action(cx.listener(|this, _: &SendWithLocalSend, window, cx| {
                            let chosen = this.chosen();
                            this.send_with_localsend(chosen, window, cx)
                        }))
                    })
                })
                .when(selected && on_disk && scattered, |d| {
                    d.on_action(cx.listener(|this, _: &ShowInEnclosingFolder, window, cx| {
                        this.show_in_enclosing_folder(window, cx)
                    }))
                })
                .when(package, |d| {
                    d.on_action(cx.listener(|this, _: &ShowPackageContents, _, cx| {
                        this.show_package_contents(cx)
                    }))
                })
                .when(download, |d| {
                    d.on_action(cx.listener(|this, _: &Download, _, cx| this.download_selected(cx)))
                })
                .when(remove_download, |d| {
                    let cloud = cloud.to_vec();
                    d.on_action(cx.listener(move |this, _: &RemoveDownload, _, cx| {
                        let paths = this.removable(&cloud);
                        this.remove_downloads_of(paths, cx)
                    }))
                })
        })
        .on_action(cx.listener(|this, _: &ViewAsList, _, cx| this.choose_view(View::List, cx)))
        .on_action(cx.listener(|this, _: &ViewAsGrid, _, cx| this.choose_view(View::Grid, cx)))
        .on_action(
            cx.listener(|this, _: &ViewAsColumns, _, cx| this.choose_view(View::Columns, cx)),
        )
        .on_action(cx.listener(|this, _: &SortByName, _, cx| this.sort_by(SortBy::Name, cx)))
        .on_action(
            cx.listener(|this, _: &SortByModified, _, cx| this.sort_by(SortBy::Modified, cx)),
        )
        .on_action(cx.listener(|this, _: &SortBySize, _, cx| this.sort_by(SortBy::Size, cx)))
        .on_action(cx.listener(|this, _: &SortByKind, _, cx| this.sort_by(SortBy::Kind, cx)))
        .on_action(cx.listener(|this, _: &ReverseSort, _, cx| {
            let sort = Sort {
                descending: !this.sort.descending,
                ..this.sort
            };
            this.choose_sort(sort, cx);
        }))
        // While a name is edited, its field has the keys, and nothing else
        // changes files.
        .when(!sheet && !renaming, |d| {
            d.on_action(
                cx.listener(|this, _: &Undo, window, cx| this.submit(Request::Undo, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Redo, window, cx| this.submit(Request::Redo, window, cx)),
            )
            .when(can_extract, |d| {
                d.on_action(cx.listener(|this, _: &Extract, window, cx| this.extract(window, cx)))
            })
            .when(can_compress, |d| {
                d.on_action(cx.listener(|this, _: &Compress, window, cx| this.compress(window, cx)))
            })
            .when(can_make_here, |d| {
                d.on_action(
                    cx.listener(|this, _: &NewFolder, window, cx| this.new_item(true, window, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &NewFile, window, cx| this.new_item(false, window, cx)),
                )
            })
            .when(put_back, |d| {
                d.on_action(
                    cx.listener(|this, _: &PutBack, window, cx| this.put_back_selected(window, cx)),
                )
            })
        })
    }
    /// A restored location that can't open: why, and the ways out.
    pub(in crate::browser) fn unavailable_view(
        &self,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let name = level_name(self.folder());
        let (title, about) = match &self.unavailable {
            Some(Unavailable::Gone(why)) => (format!("{name} is unavailable"), why.clone()),
            _ => (
                format!("Open {name}?"),
                "macOS may ask for permission, so it waits until you open it.".to_string(),
            ),
        };
        let consent = self.unavailable == Some(Unavailable::NeedsConsent);
        div()
            .debug_selector(|| "unavailable".into())
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
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(content(0.65))
                    .child(about),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .gap_2()
                    .child(
                        text_button(
                            "unavailable-back",
                            "Back",
                            if self.at > 0 {
                                ButtonLook::Secondary
                            } else {
                                ButtonLook::Disabled
                            },
                        )
                        .when(self.at > 0, |d| {
                            d.on_click(cx.listener(|this, _, _, cx| this.step_history(-1, cx)))
                        }),
                    )
                    .child(
                        text_button(
                            "unavailable-choose",
                            "Go to Folder…",
                            if consent {
                                ButtonLook::Secondary
                            } else {
                                ButtonLook::Primary
                            },
                        )
                        .debug_selector(|| "unavailable-choose".into())
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(GoToFolder), cx)),
                    )
                    .when(consent, |d| {
                        d.child(
                            text_button("unavailable-open", "Open", ButtonLook::Primary)
                                .debug_selector(|| "unavailable-open".into())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.consented = true;
                                    this.reload(cx);
                                })),
                        )
                    }),
            )
    }
    /// The list or grid, with its keys, its menus and its marquee.
    pub(in crate::browser) fn file_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // The list's rows don't depend on its width, so it shows at once.
        if matches!(self.layout(), View::List | View::Columns) {
            self.take_pending_reveal();
        }
        // Column view draws its own bounds and rectangle in the keys' column.
        let in_columns = self.layout() == View::Columns
            && self.error.is_none()
            && self.unavailable.is_none()
            && self.organizing.is_none()
            && !self.showing_overview()
            && self.preview.is_none();
        let list = match &self.error {
            _ if self.unavailable.is_some() => self.unavailable_view(cx).into_any_element(),
            Some(error) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px_6()
                .text_size(px(theme::TEXT_BODY))
                .text_color(content(0.65))
                .child(error.clone())
                .into_any_element(),
            None if self.layout() == View::Grid => {
                let icon = self.grid_icon;
                let inset = self.grid_inset.clone();
                // The grid's width this frame, from the window's, so a resize
                // lays out the right columns now rather than a frame late. What
                // the estimate last missed by, the interface zoomed, is as
                // last measured.
                let room = self.chrome.width;
                let wide = room - inset.get();
                (self.across, self.cell_width) = grid_layout(wide, icon);
                self.take_pending_reveal();
                let rows = self.entries.len().div_ceil(self.across);
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                let measured = bounds.size.width.as_f32();
                                // A width the estimate missed lays out again.
                                // GPUI ignores a refresh while drawing, so it
                                // waits for the next frame.
                                if (wide - measured).abs() > 0.5 {
                                    window.on_next_frame(|window, _| window.refresh());
                                }
                                inset.set(room - measured);
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(
                        uniform_list("grid", rows, cx.processor(Self::grid_rows))
                            .track_scroll(&self.scroll)
                            .flex_1()
                            .p_1(),
                    )
                    .into_any_element()
            }
            None if in_columns => self.columns_view(window, cx),
            None => uniform_list(
                "entries",
                self.matches.row_count(self.entries.len()),
                cx.processor(Self::rows),
            )
            .track_scroll(&self.scroll)
            .flex_1()
            // Rows' corners run parallel to the pane's.
            .p(px(theme::CARD_PADDING))
            .into_any_element(),
        };
        // Thumbnails the last frame didn't draw (scrolled away, another folder,
        // another size, the list view) are given back, loaded or not.
        let drawn = std::mem::take(&mut self.drawn);
        let gone: Vec<Thumb> = self
            .thumbs
            .keys()
            .filter(|t| !drawn.contains(*t))
            .cloned()
            .collect();
        for thumb in gone {
            let image = self.thumbs.remove(&thumb).flatten();
            thumbnails::release(&thumb, image, Some(&mut *window), cx);
        }
        let can_make_here = self.takes_files(cx);
        let pastes_to_localsend = self.showing_localsend() && cx.global::<Preferences>().localsend;
        // Nothing in an archive changes, and its rows aren't files to copy.
        let writable = !self.showing_archive();
        div()
            .id("file-list")
            .key_context("FileList")
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(Self::type_to_search))
            .on_action(cx.listener(|this, _: &ScrollToTop, _, cx| {
                this.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| {
                if let Some(last) = this.matches.row_count(this.entries.len()).checked_sub(1) {
                    this.scroll
                        .scroll_to_item_strict(last, ScrollStrategy::Bottom);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ExpandFolder, _, cx| this.expand(cx)))
            .on_key_down(cx.listener(Self::collapse))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(true, cx)))
            .on_action(cx.listener(|this, _: &ExtendPrevious, _, cx| {
                this.select_vertically(false, true, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ExtendNext, _, cx| this.select_vertically(true, true, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendLeft, _, cx| {
                if this.layout() == View::Grid {
                    this.select(Move::By(-1), true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ExtendRight, _, cx| {
                if this.layout() == View::Grid {
                    this.select(Move::By(1), true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &text_field::SelectAll, _, cx| {
                if let Some(preview) = this.preview.clone()
                    && preview.update(cx, |preview, cx| preview.select_all_text(cx))
                {
                    return;
                }
                this.selection.select_all(&this.entries);
                cx.notify();
            }))
            // Copy and Paste are AppKit's actions, as Select All is: here
            // they're files, or the text selected in the preview. Leaving one
            // out turns its menu item off.
            .when(
                self.preview.is_some() || (!self.selection.is_empty() && writable),
                |d| {
                    d.on_action(cx.listener(move |this, _: &text_field::Copy, _, cx| {
                        let text = this
                            .preview
                            .as_ref()
                            .and_then(|p| p.read(cx).selected_text());
                        match text {
                            Some(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                            None if writable && !this.selection.is_empty() => this.copy_files(cx),
                            None => {}
                        }
                    }))
                },
            )
            .when(can_make_here, |d| {
                d.on_action(cx.listener(|this, _: &text_field::Paste, window, cx| {
                    this.paste(false, window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &MoveHere, window, cx| this.paste(true, window, cx)),
                )
            })
            .when(pastes_to_localsend, |d| {
                d.on_action(cx.listener(|this, _: &text_field::Paste, window, cx| {
                    this.paste_into_localsend(window, cx)
                }))
            })
            // Only here, so the menu bar's items are off and can't change the
            // selection's files while the keys are somewhere else, such as the
            // sidebar.
            .when(
                self.showing_worktrees() && self.worktrees.selected().is_some(),
                |d| {
                    d.on_action(cx.listener(|this, _: &MoveToTrash, window, cx| {
                        this.ask_remove_worktree(window, cx)
                    }))
                },
            )
            // Move to Trash lists the chosen copies first; Space chooses one.
            .when(self.showing_duplicates(), |d| {
                d.on_action(cx.listener(|this, _: &MoveToTrash, window, cx| {
                    this.review_duplicates(window, cx)
                }))
                .on_action(cx.listener(|this, _: &QuickLook, _, cx| this.choose_duplicate(cx)))
            })
            .when(
                writable && !self.changing_names() && !self.selection.is_empty(),
                |d| {
                    d.on_action(cx.listener(|this, _: &MoveToTrash, window, cx| {
                        let paths = this.chosen();
                        this.submit(Request::Trash { paths }, window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &Duplicate, window, cx| {
                        let paths = this.chosen();
                        this.submit(Request::Duplicate { paths }, window, cx)
                    }))
                    .on_action(cx.listener(
                        |this, _: &NewFolderWithSelection, window, cx| {
                            this.new_folder_with_selection(window, cx)
                        },
                    ))
                },
            )
            .when(
                writable && self.renaming.is_none() && !self.selection.is_empty(),
                |d| {
                    d.on_action(
                        cx.listener(|this, _: &Rename, window, cx| {
                            this.rename_selected(window, cx)
                        }),
                    )
                    .on_action(cx.listener(
                        |this, _: &SuggestNames, window, cx| this.suggest_names(window, cx),
                    ))
                },
            )
            .when(writable, |d| {
                d.on_action(cx.listener(|this, _: &OrganizeFolder, window, cx| {
                    this.start_organize(window, cx)
                }))
            })
            .when(self.preview.is_some(), |d| {
                d.on_action(
                    cx.listener(|this, _: &EditPreview, window, cx| this.edit_preview(window, cx)),
                )
            })
            .when(self.showing_recents(), |d| {
                d.on_action(cx.listener(|this, _: &ClearRecents, _, cx| this.clear_recents(cx)))
            })
            .when_some(self.chrome.ejectable.clone(), |d, mount| {
                d.on_action(
                    cx.listener(move |_, _: &Eject, _, cx| {
                        cx.emit(PaneEvent::Eject(mount.clone()))
                    }),
                )
            })
            .on_action(cx.listener(|this, _: &ShowContextMenu, _, cx| {
                if !this.changing_names() && !this.showing_page() {
                    let at = this.menu_point(cx);
                    let scopes = this.scopes();
                    cx.emit(PaneEvent::Menu { scopes, at });
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.context_click(None, event.position, window, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.focus_list(window, cx);
                    this.swept = false;
                    if std::mem::take(&mut this.grabbed) {
                        return;
                    }
                    // In column view a rectangle selects in the keys' column.
                    let elsewhere = this.layout() == View::Columns
                        && !this.list_bounds.get().contains(&event.position);
                    if elsewhere {
                        return;
                    }
                    let at = this.listing_point(event.position);
                    this.marquee = Some(Marquee {
                        start: at,
                        end: at,
                        pointer: event.position,
                        additive: event.modifiers.platform || event.modifiers.shift,
                        dragging: false,
                        scrolling: false,
                    });
                    cx.notify();
                }),
            )
            // `marquee_drag` hears releases only from the frame after the
            // press, and a quick tap releases sooner.
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    this.end_marquee(event.position, event.modifiers, cx)
                }),
            )
            .relative()
            // The rectangle stays inside the listing.
            .overflow_hidden()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                self.organize_page(cx)
                    .or_else(|| self.overview_page(cx))
                    .or_else(|| self.worktrees_page(cx))
                    .or_else(|| self.airdrop_page(cx))
                    .or_else(|| self.localsend_page(cx))
                    .or_else(|| self.duplicates_page(cx))
                    .or_else(|| {
                        // Until it has something to show, so it never
                        // appears in pieces.
                        self.preview
                            .clone()
                            .filter(|view| view.read(cx).ready())
                            .map(gpui::IntoElement::into_any_element)
                    })
                    .unwrap_or(list),
            )
            .when(!in_columns, |d| {
                let bounds = self.list_bounds.clone();
                d.child(
                    canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .children(self.marquee_overlay())
            })
            .when(self.marquee.is_some(), |d| d.child(self.marquee_drag(cx)))
    }
    /// A column title that sorts by its column; the sorted one shows which way.
    pub(in crate::browser) fn sort_header(
        by: SortBy,
        title: &'static str,
        sort: Option<Sort>,
        cx: &mut Context<Pane>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = sort.filter(|sort| sort.by == by);
        div()
            .id(title)
            .role(gpui::Role::ColumnHeader)
            .aria_label(title)
            .flex_shrink_0()
            .h_full()
            .flex()
            .items_center()
            .gap_1()
            .when(active.is_some(), |d| d.text_color(content(1.)))
            // During a search the order is relevance, so the titles don't sort.
            .when(sort.is_some(), |d| {
                d.cursor_pointer()
                    .hover(|s| s.text_color(content(1.)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_sort(sorted_by(this.sort, by), cx)
                    }))
            })
            .child(div().truncate().child(title))
            .children(active.map(|sort| {
                icon("chevron-down-hi", 12.)
                    .text_color(content(0.65))
                    .when(!sort.descending, |i| {
                        i.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.5)))
                    })
            }))
    }
}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.list_focus.clone()
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.follow_batch(window, cx);
        self.follow_organize(window, cx);
        self.follow_columns(cx);
        self.preview_typing = false;
        if let Some(preview) = &self.preview {
            let covered = self.chrome.covered;
            preview.update(cx, |preview, cx| preview.set_covered(covered, cx));
            self.preview_typing = preview.read(cx).typing();
        }
        let prefs = cx.global::<Preferences>();
        let boxy = prefs.workspace_style == WorkspaceStyle::Boxy;
        // Search results stay in relevance order, so no column shows a sort.
        let remote = self.showing_remote();
        let sort_shown =
            (self.showing_folder() || self.showing_archive() || remote).then_some(self.sort);
        // The Overview and Worktrees list no entries but are full of text.
        let empty = self.root.is_empty() && self.error.is_none() && !self.showing_page();
        let look = prefs.background.clone().map(|path| Look {
            path,
            effect: prefs.background_effect,
            opacity: prefs.background_opacity(empty),
            empty,
            preview: false,
            // The card's corners inside its border.
            radius: if boxy { 0. } else { theme::RADIUS_CARD - 1. },
        });
        self.backdrop
            .update(cx, |backdrop, cx| backdrop.set(look, cx));
        let list = if remote {
            self.remote_list(window, cx)
        } else {
            self.file_list(window, cx).into_any_element()
        };
        let (date, size, kind) = columns(self.chrome.width);
        // A server folder is always a list, with titles once it's listed.
        let titled = if remote {
            self.remote.listing == super::remote::Listing::Listed
        } else {
            self.layout() == View::List
        };
        let id = cx.entity_id();
        let dropping_here = self.drag.active() && self.takes_files(cx);
        let outlined = self.drag.outlines(&Spot::Pane(id));
        div()
            .relative()
            .size_full()
            // A press anywhere in the other pane moves the commands here.
            .when(!self.chrome.focused, |d| {
                d.capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                    this.focus_list(window, cx);
                }))
            })
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(self.backdrop.clone())
            .children(self.trash_bar(cx))
            .children(self.recents_bar(cx))
            .children(self.archive_bar(cx))
            .children(self.batch_bar(window, cx))
            // The grid sorts from the View menu and Settings.
            .when(
                titled
                    && self.unavailable.is_none()
                    && self.organizing.is_none()
                    && self.preview.is_none()
                    && !self.showing_page(),
                |d| {
                    d.child(
                        theme::pane_bar(theme::ROW_HEIGHT)
                            .gap_3()
                            .border_b_1()
                            .border_color(content(0.06))
                            .text_size(px(theme::TEXT_SMALL))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(0.65))
                            .child(Self::sort_header(SortBy::Name, "Name", sort_shown, cx).flex_1())
                            .when(date, |d| {
                                d.child(
                                    Self::sort_header(
                                        SortBy::Modified,
                                        "Date Modified",
                                        sort_shown,
                                        cx,
                                    )
                                    .w(px(180.)),
                                )
                            })
                            .when(size, |d| {
                                d.child(
                                    Self::sort_header(SortBy::Size, "Size", sort_shown, cx)
                                        .w(px(80.))
                                        .justify_end(),
                                )
                            })
                            .when(kind, |d| {
                                d.child(
                                    Self::sort_header(SortBy::Kind, "Kind", sort_shown, cx)
                                        .w(px(110.)),
                                )
                            }),
                    )
                },
            )
            .child(list)
            .children(self.path_bar(window, cx))
            .when(dropping_here, |d| {
                let drag = self.drag.clone();
                let target = Target::Folder {
                    spot: Spot::Pane(id),
                    path: self.folder().to_path_buf(),
                    name: level_name(self.folder()),
                };
                offers(d, move |_| drag.offer(target.clone()))
            })
            .when(outlined, |d| {
                d.child(theme::drop_outline().when(!boxy, |d| {
                    // The card's corners inside its border.
                    d.rounded(px(theme::RADIUS_CARD - 1.))
                }))
            })
            // No press reaches the rows, titles or crumbs under a running Apply.
            .when(self.batch_sending(), |d| {
                d.child(div().absolute().size_full().occlude())
            })
    }
}

impl Pane {
    /// What it shows and has selected, to tell when either changed.
    pub(in crate::browser) fn shown(&self) -> Shown {
        Shown {
            folder: self.folder().to_path_buf(),
            real_folder: self.shows_a_real_folder(),
            archive: self.showing_archive(),
            only: self.selection.only().map(Path::to_path_buf),
            count: self.selection.len(),
            bytes: self.selection.size(),
            cursor: self.selection.cursor().map(Path::to_path_buf),
        }
    }

    pub(in crate::browser) fn still_shows(&self, shown: &Shown) -> bool {
        shown.folder == self.folder()
            && shown.real_folder == self.shows_a_real_folder()
            && shown.archive == self.showing_archive()
            && shown.only.as_deref() == self.selection.only()
            && shown.count == self.selection.len()
            && shown.bytes == self.selection.size()
            && shown.cursor.as_deref() == self.selection.cursor()
    }

    /// What a rename or a job selected isn't listed yet; its listing is on its way.
    pub(in crate::browser) fn selection_unlisted(&self) -> bool {
        !self.selection.is_empty() && self.chosen_entries().is_empty()
    }

    pub(in crate::browser) fn lists(&self, path: &Path) -> bool {
        self.entries.iter().any(|entry| entry.path == path)
    }

    /// Files on the disk or a server are selected, to open, move or look at.
    /// An archive's entries aren't on the disk.
    pub(in crate::browser) fn selects_files(&self) -> bool {
        (!self.selection.is_empty() && !self.showing_archive()) || self.remote_has_selection()
    }

    /// Arrives somewhere the arrows can go on from.
    pub(in crate::browser) fn select_first_if_none(&mut self, cx: &mut Context<Self>) {
        if self.selection.is_empty() && !self.entries.is_empty() {
            self.select_row(0, cx);
        }
    }

    /// What the preview shows of the selection, when any of it previews.
    pub(in crate::browser) fn preview_of_selection(&self) -> Option<Preview> {
        Preview::of(&self.selection, &self.entries)
    }

    /// The preview takes over the list while it's open.
    pub(in crate::browser) fn show_preview(
        &mut self,
        view: Option<Entity<crate::preview::PreviewView>>,
        cx: &mut Context<Self>,
    ) {
        self.preview = view;
        cx.notify();
    }

    /// Something here takes typed keys: a name being edited, a review, the
    /// preview's text, or a server's rename.
    pub(in crate::browser) fn takes_typing(&self, cx: &App) -> bool {
        self.changing_names()
            || self
                .preview
                .as_ref()
                .is_some_and(|view| view.read(cx).typing())
            || self.remote_renaming()
    }

    /// The keys are in it: in its list, or in a batch's bar outside it.
    pub(in crate::browser) fn has_keys(&self, window: &Window, cx: &App) -> bool {
        self.list_focus.contains_focused(window, cx)
            || self.batch.as_ref().is_some_and(|b| b.has_keys(window, cx))
    }

    /// What the window tells it this frame, and whether Tab may stop in it.
    pub(in crate::browser) fn frame(
        &mut self,
        chrome: Chrome,
        tab_stops: bool,
        cx: &mut Context<Self>,
    ) {
        self.chrome = chrome;
        let stops = tab_stops && !self.batch_sending();
        self.search.focus_handle(cx).tab_stop(stops);
        self.list_focus.clone().tab_stop(stops);
    }

    /// The folder on screen, while it's there to read.
    pub(in crate::browser) fn reachable_folder(&self) -> Option<&Path> {
        (self.showing_folder() && self.unavailable.is_none()).then(|| self.folder())
    }

    /// Where Go to Folder starts. Not a location that's unavailable: its path
    /// may be another's now.
    pub(in crate::browser) fn goto_from(&self) -> PathBuf {
        match self.unavailable {
            Some(_) => crate::home(),
            None => self.disk_folder().to_path_buf(),
        }
    }

    pub(in crate::browser) fn can_go_back(&self) -> bool {
        self.at > 0 || self.organizing.is_some()
    }

    pub(in crate::browser) fn can_go_forward(&self) -> bool {
        self.at + 1 < self.history.len()
    }

    /// Lists the folder again after a job, selecting what it made when it
    /// landed here.
    pub(in crate::browser) fn relist_after_job(
        &mut self,
        into: &Path,
        made: &[PathBuf],
        cx: &mut Context<Self>,
    ) {
        let landed = into == self.folder();
        if landed {
            self.select_after = made.to_vec();
        }
        self.list(!landed, cx);
    }

    /// Goes to the folder holding `path` and selects it there.
    pub(in crate::browser) fn show_item(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(folder) = path.parent() {
            self.go(folder.to_path_buf(), window, cx);
            self.select_after = vec![path.to_path_buf()];
        }
    }

    pub(in crate::browser) fn commit_inspector_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.renaming.as_ref().is_some_and(|r| r.in_inspector) {
            self.commit_rename(window, cx);
        }
    }
}

/// What a pane shows and has selected.
#[derive(Clone)]
pub(in crate::browser) struct Shown {
    pub(in crate::browser) folder: PathBuf,
    pub(in crate::browser) real_folder: bool,
    pub(in crate::browser) archive: bool,
    pub(in crate::browser) only: Option<PathBuf>,
    pub(in crate::browser) count: usize,
    pub(in crate::browser) bytes: u64,
    pub(in crate::browser) cursor: Option<PathBuf>,
}

/// The path bar, edited as a path.
pub(in crate::browser) struct Address {
    view: Entity<GotoView>,
    /// Where the list takes the keys back when the pane goes elsewhere.
    window: AnyWindowHandle,
    /// Going, Esc and clicking away close it.
    _closes: [Subscription; 2],
}

impl Pane {
    /// Turns the path bar into the folder's path, selected, to type over.
    /// Only a folder on this Mac has one; elsewhere it says no.
    pub(in crate::browser) fn edit_path(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.showing_folder() || self.unavailable.is_some() {
            return false;
        }
        if let Some(address) = &self.address {
            window.focus(&address.view.focus_handle(cx), cx);
            return true;
        }
        let folder = self.folder().to_path_buf();
        let view = cx.new(|cx| GotoView::in_bar(folder, cx));
        let focus = view.focus_handle(cx);
        let went = cx.subscribe_in(&view, window, |this, _, event: &GotoEvent, window, cx| {
            this.address = None;
            this.focus_list(window, cx);
            if let GotoEvent::Go(destination) = event {
                let select = destination.select.iter().cloned().collect();
                this.go_to(destination.folder.clone(), select, window, cx);
            }
            cx.notify();
        });
        let blur = cx.on_blur(&focus, window, |this, _, cx| {
            this.address = None;
            cx.notify();
        });
        window.focus(&focus, cx);
        self.address = Some(Address {
            view,
            window: window.window_handle(),
            _closes: [went, blur],
        });
        cx.notify();
        true
    }

    /// A path typed for the folder being left would lead back to it.
    pub(super) fn close_address(&mut self, cx: &mut Context<Self>) {
        if let Some(address) = self.address.take() {
            let list = self.list_focus.clone();
            cx.defer(move |cx| {
                let _ = address
                    .window
                    .update(cx, |_, window, cx| window.focus(&list, cx));
            });
        }
    }

    pub(in crate::browser) fn types_path_in(&self, focus: &FocusHandle, cx: &App) -> bool {
        self.address
            .as_ref()
            .is_some_and(|address| address.view.focus_handle(cx) == *focus)
    }
}
