//! Column view: Finder's Miller columns. The column with the keys is the
//! pane's folder, so every command, the path bar and Back act on it as they do
//! in the list. The columns before it are the folders it was reached through,
//! from where the chain started; the last shows the selected folder's files or
//! a preview of the selected file. Each column's listing is one of the pane's
//! `children`, the same cache the list's folders opened in place use, so a
//! refresh relists and watches them alike.

use super::inspector::{READ_DELAY, fact, header, kinds_summary};
use super::pane::{LIST_BADGE, Pane, format_date, format_size, level_name, real, thumbnail};
use super::{format_bytes, sidebar};
use crate::file_icons;
use crate::preferences::{Preferences, View};
use crate::theme::{self, ButtonLook, content, icon, text_button};
use crate::workspace;
use chrono::Local;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, MouseButton, ScrollHandle, ScrollStrategy,
    UniformListScrollHandle, Window, canvas, div, prelude::*, px, uniform_list,
};
use monodrive_core::entries::{self, Entry, Kind};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Every column but the last.
pub(super) const COLUMN_WIDTH: f32 = 220.;
/// The last column takes what's left, and at least this.
const LAST_WIDTH: f32 = 280.;
/// Narrower than this, the last column isn't drawn.
const LAST_LEAST: f32 = 120.;
/// The picture in the preview column.
const PREVIEW_SIDE: f32 = 128.;

#[derive(Default)]
pub(super) struct Columns {
    /// The chain's first folder: where it was opened from. Going above it
    /// starts the chain there.
    pub(super) root: Option<PathBuf>,
    /// → went into a folder, so its first item is selected once it lists.
    pub(super) enter_first: bool,
    /// Sideways, so the last columns stay in view as the chain grows.
    scroll: ScrollHandle,
    /// The folder, chain and width the columns were last scrolled for.
    laid: Option<(PathBuf, usize, f32)>,
    /// Each side column's scrolling, and whether it has shown its highlight.
    scrolls: HashMap<PathBuf, (UniformListScrollHandle, bool)>,
    /// Selected folders that could show a privacy prompt, so they aren't
    /// listed until a person asks.
    gated: HashSet<PathBuf>,
    /// Folders a person asked to open anyway.
    opened: HashSet<PathBuf>,
}

/// What a side column's listing turned out to be.
enum Listed {
    Files(Vec<Entry>, PathBuf),
    Gated,
}

impl Pane {
    /// The folders before the one with the keys, from the chain's root down.
    pub(super) fn trail(&self) -> Vec<PathBuf> {
        let Some(root) = &self.columns.root else {
            return Vec::new();
        };
        let mut dirs: Vec<PathBuf> = self
            .folder()
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(root))
            .map(Path::to_path_buf)
            .collect();
        dirs.reverse();
        dirs
    }

    /// The chain starts again at the folder on screen unless it's inside it.
    pub(super) fn root_columns(&mut self) {
        let folder = self.folder();
        if !self
            .columns
            .root
            .as_ref()
            .is_some_and(|root| folder.starts_with(root))
        {
            self.columns.root = Some(folder.to_path_buf());
        }
    }

    /// The lone selected entry, if it's a folder whose files the last column
    /// shows. Packages and links show a preview instead, as in Finder.
    pub(super) fn next_folder(&self) -> Option<PathBuf> {
        let only = self.selection.only()?;
        let entry = self.entries.iter().find(|e| e.path == only)?;
        (entry.kind == Kind::Dir && !workspace::is_package(&entry.path)).then(|| entry.path.clone())
    }

    /// The folders a listing in column view lists besides the one on screen:
    /// the trail, and the selected folder once it's been listed.
    pub(super) fn column_folders(&self) -> Vec<PathBuf> {
        let mut dirs = self.trail();
        dirs.extend(
            self.next_folder()
                .filter(|dir| dir.parent() == Some(self.folder()))
                .filter(|dir| self.children.contains_key(dir)),
        );
        dirs
    }

    /// → in column view: into the selected folder, with its first item
    /// selected.
    pub(super) fn enter_column(&mut self, cx: &mut Context<Self>) {
        if self.listing {
            return;
        }
        if let Some(dir) = self.next_folder() {
            self.columns.enter_first = true;
            self.navigate(dir, cx);
        }
    }

    /// ← in column view: back a column, selecting the folder just left.
    /// Holding it stops at the chain's first column.
    pub(super) fn leave_column(&mut self, held: bool, cx: &mut Context<Self>) {
        if self.listing || (held && self.columns.root.as_deref() == Some(self.folder())) {
            return;
        }
        if let Some(parent) = self.folder().parent().map(Path::to_path_buf) {
            self.navigate(parent, cx);
        }
    }

    /// Lists the selected folder for the last column once the selection has
    /// rested, so holding ↓ lists nothing it passes. One that could show a
    /// privacy prompt waits for a person to ask.
    pub(super) fn follow_columns(&mut self, cx: &mut Context<Self>) {
        if self.layout() != View::Columns || self.listing {
            return;
        }
        let next = self.next_folder();
        // What's no longer in the chain is dropped, except folders the list
        // view has open, which it shows again when it comes back.
        let keep: HashSet<PathBuf> = self
            .trail()
            .into_iter()
            .chain(next.clone())
            .chain(self.expanded.iter().cloned())
            .collect();
        self.children.retain(|dir, _| keep.contains(dir));
        self.expanding.retain(|dir, _| keep.contains(dir));
        self.columns.scrolls.retain(|dir, _| keep.contains(dir));
        let Some(dir) = next else { return };
        if self.children.contains_key(&dir)
            || self.expanding.contains_key(&dir)
            || self.columns.gated.contains(&dir)
        {
            return;
        }
        let rest = cx.background_executor().timer(READ_DELAY);
        let drives: Arc<dyn sidebar::Drives> = match cx.try_global::<super::Places>() {
            Some(places) => places.drives.clone(),
            None => Arc::new(sidebar::System),
        };
        let asked = self.columns.opened.contains(&dir);
        let show_hidden = self.show_hidden || self.peek_hidden;
        let tidy = self.tidy_folders(cx);
        let sort = self.sort;
        let (path, key) = (dir.clone(), dir.clone());
        let task = cx.spawn(async move |this, cx| {
            rest.await;
            let listed = cx
                .background_executor()
                .spawn(async move {
                    if !asked && drives.needs_consent(&path) {
                        return Listed::Gated;
                    }
                    let hidden = sidebar::lists_hidden(&path, show_hidden, &tidy);
                    // A folder that can't be read shows as empty, as in Finder.
                    let list = entries::list_dir(&path, hidden, sort).unwrap_or_default();
                    Listed::Files(list, real(&path))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.expanding.remove(&dir).is_none() {
                    return;
                }
                match listed {
                    Listed::Gated => {
                        this.columns.gated.insert(dir);
                    }
                    Listed::Files(list, watched) => {
                        this.ask_downloads(&list, cx);
                        this.watch.folders.insert(dir.clone(), watched);
                        this.children.insert(dir, list);
                    }
                }
                this.recheck_held(cx);
                cx.notify();
            });
        });
        self.expanding.insert(key, task);
    }

    /// The columns, the one with the keys drawn by the list's own rows.
    pub(super) fn columns_view(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.columns.root.is_none() {
            self.root_columns();
        }
        let trail = self.trail();
        let width = self.chrome.width;
        let chain = (trail.len() + 1) as f32 * COLUMN_WIDTH;
        let rest = width - COLUMN_WIDTH;
        let last = if rest < LAST_LEAST {
            None
        } else {
            Some((width - chain).max(LAST_WIDTH.min(rest)))
        };
        // Scrolled to the end as the chain changes, so the keys' column and
        // the last are in view; a person can scroll back from there.
        let laid = (self.folder().to_path_buf(), trail.len(), width);
        if self.columns.laid.as_ref() != Some(&laid) {
            self.columns.laid = Some(laid);
            let count = trail.len() + 1 + usize::from(last.is_some());
            self.columns.scroll.scroll_to_item(count - 1);
        }
        let mut columns: Vec<AnyElement> = Vec::new();
        for (ix, dir) in trail.iter().enumerate() {
            let on = trail
                .get(ix + 1)
                .map_or_else(|| self.folder().to_path_buf(), Clone::clone);
            columns.push(self.side_column(ix, dir, Some(on), COLUMN_WIDTH, cx));
        }
        let bounds = self.list_bounds.clone();
        columns.push(
            div()
                .debug_selector(|| "column-keys".into())
                .relative()
                .w(px(COLUMN_WIDTH))
                .h_full()
                .flex_shrink_0()
                .flex()
                .flex_col()
                .border_r_1()
                .border_color(content(0.06))
                .child(
                    uniform_list("entries", self.entries.len(), cx.processor(Self::rows))
                        .track_scroll(&self.scroll)
                        .flex_1()
                        .p_1(),
                )
                .child(
                    canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .children(self.marquee_overlay())
                .into_any_element(),
        );
        if let Some(last) = last {
            columns.push(self.last_column(trail.len(), last, window, cx));
        }
        div()
            .id("columns")
            .flex_1()
            .min_h_0()
            .flex()
            .overflow_x_scroll()
            .track_scroll(&self.columns.scroll)
            .children(columns)
            .into_any_element()
    }

    /// What follows the keys' column: the selected folder's files, a preview
    /// of the selected file, or what several selected come to.
    fn last_column(
        &mut self,
        ix: usize,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let frame = div()
            .debug_selector(|| "column-last".into())
            .w(px(width))
            .h_full()
            .flex_shrink_0();
        if let Some(dir) = self.next_folder() {
            if self.columns.gated.contains(&dir) {
                return frame.child(self.gated_column(dir, cx)).into_any_element();
            }
            return self.side_column(ix + 1, &dir, None, width, cx);
        }
        let chosen: Vec<Entry> = self
            .selection
            .in_order(&self.entries)
            .into_iter()
            .cloned()
            .collect();
        let today = Local::now().date_naive();
        let contents = match chosen.as_slice() {
            [] => return frame.into_any_element(),
            [entry] => {
                let pixels = (PREVIEW_SIDE * window.scale_factor()).ceil() as u32;
                let art = match thumbnail(
                    (&mut self.thumbs, &mut self.drawn),
                    entry,
                    pixels,
                    window,
                    cx,
                ) {
                    Some(image) => gpui::img(image)
                        .size(px(PREVIEW_SIDE))
                        .object_fit(gpui::ObjectFit::Contain)
                        .rounded(px(theme::RADIUS_THUMB))
                        .into_any_element(),
                    None => file_icons::icon(entry, PREVIEW_SIDE, window, cx),
                };
                let name = div()
                    .text_center()
                    .child(entry.name.clone())
                    .into_any_element();
                let mut rows = vec![header(art, name, entry.kind_label(), None)];
                if entry.kind == Kind::File {
                    rows.push(fact("Size", format_size(entry)));
                }
                if entry.modified > 0 {
                    rows.push(fact("Modified", format_date(entry.modified, today)));
                }
                rows
            }
            several => {
                let art = file_icons::icon(&several[0], PREVIEW_SIDE, window, cx);
                let refs: Vec<&Entry> = several.iter().collect();
                let bytes: u64 = several.iter().map(Entry::listed_size).sum();
                let name = div()
                    .child(format!("{} items", several.len()))
                    .into_any_element();
                vec![
                    header(art, name, kinds_summary(&refs), None),
                    fact("Size", format_bytes(bytes)),
                ]
            }
        };
        frame
            .child(
                div()
                    .id("column-preview")
                    .debug_selector(|| "column-preview".into())
                    .size_full()
                    .overflow_y_scroll()
                    .px(px(theme::PANE_INSET))
                    .pb(px(theme::PANE_INSET))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .text_size(px(theme::TEXT_BODY))
                    .children(contents),
            )
            .into_any_element()
    }

    /// A selected folder that waits for a person, as a restored one does.
    fn gated_column(&self, dir: PathBuf, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px(px(theme::PANE_INSET))
            .text_center()
            .child(
                div()
                    .text_size(px(theme::TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child(format!("Open {}?", level_name(&dir))),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(content(0.65))
                    .child("macOS may ask for permission, so it waits until you open it."),
            )
            .child(
                div().mt_2().child(
                    text_button("column-open", "Open", ButtonLook::Primary)
                        .debug_selector(|| "column-open".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.columns.gated.remove(&dir);
                            this.columns.opened.insert(dir.clone());
                            cx.notify();
                        })),
                ),
            )
    }

    /// A folder's column other than the keys': `on` is the folder the chain
    /// goes through, highlighted as a selection whose keys are elsewhere.
    fn side_column(
        &mut self,
        ix: usize,
        dir: &Path,
        on: Option<PathBuf>,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = self.children.get(dir).map_or(0, Vec::len);
        let (scroll, revealed) = self
            .columns
            .scrolls
            .entry(dir.to_path_buf())
            .or_insert_with(|| (UniformListScrollHandle::new(), false));
        let scroll = scroll.clone();
        if !*revealed && let Some(listed) = self.children.get(dir) {
            *revealed = true;
            if let Some(at) = on
                .as_ref()
                .and_then(|on| listed.iter().position(|e| &e.path == on))
            {
                scroll.scroll_to_item(at, ScrollStrategy::Center);
            }
        }
        let empty = self.children.get(dir).is_some_and(Vec::is_empty);
        let folder = dir.to_path_buf();
        let column = div()
            .debug_selector(move || format!("column-{ix}"))
            .w(px(width))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .when(on.is_some(), |d| d.border_r_1().border_color(content(0.06)))
            // Only the keys' column takes a right-click, for its menu.
            .on_mouse_down(MouseButton::Right, |_, window, cx| {
                cx.stop_propagation();
                window.prevent_default();
            });
        if empty {
            return column
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_BODY))
                .text_color(content(0.65))
                .child("Empty folder")
                .into_any_element();
        }
        column
            .child(
                uniform_list(
                    ("column", ix),
                    count,
                    cx.processor(move |this, range, window, cx| {
                        this.side_rows(&folder, on.as_deref(), range, window, cx)
                    }),
                )
                .track_scroll(&scroll)
                .flex_1()
                .p_1(),
            )
            .into_any_element()
    }

    fn side_rows(
        &mut self,
        dir: &Path,
        on: Option<&Path>,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let (row_height, icon_size, text_size) = cx.global::<Preferences>().row_size.metrics();
        let Some(listed) = self.children.get(dir) else {
            return Vec::new();
        };
        let rows: Vec<Entry> = listed.get(range.clone()).unwrap_or_default().to_vec();
        rows.into_iter()
            .zip(range)
            .map(|(entry, ix)| {
                let here = on == Some(entry.path.as_path());
                let folder = entry.kind == Kind::Dir && !workspace::is_package(&entry.path);
                let (dir, path) = (dir.to_path_buf(), entry.path.clone());
                let ink = content(if here { 1. } else { 0.55 });
                let badge = self.cloud_badge(&entry, ix, LIST_BADGE, ink, cx);
                div()
                    .id(ix)
                    .role(gpui::Role::ListItem)
                    .aria_label(entry.name.clone())
                    .aria_selected(here)
                    .w_full()
                    .h(px(row_height))
                    .px_3()
                    .rounded(px(theme::RADIUS_ROW))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(text_size))
                    .map(|d| {
                        if here {
                            d.bg(content(0.14))
                        } else if ix % 2 == 1 {
                            d.bg(content(0.03))
                        } else {
                            d
                        }
                    })
                    .child(file_icons::icon(&entry, icon_size, window, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().min_w_0().truncate().child(entry.name.clone()))
                            .children(badge),
                    )
                    .when(folder, |d| d.child(chevron(ink)))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.click_column(dir.clone(), path.clone(), folder, event, window, cx)
                    }))
                    .into_any_element()
            })
            .collect()
    }

    /// A click in a column other than the keys': the keys move to that
    /// column, with what was clicked selected. A double click opens it.
    fn click_column(
        &mut self,
        folder: PathBuf,
        path: PathBuf,
        is_folder: bool,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_list(window, cx);
        // The first click moved the keys here before the listing landed.
        if event.click_count() == 2 {
            return if is_folder {
                self.navigate(path, cx)
            } else {
                self.open_file(&path, cx)
            };
        }
        self.select_on_listing(vec![path]);
        self.navigate(folder, cx);
    }
}

/// A folder row's chevron, which says the next column shows its files.
pub(super) fn chevron(ink: Hsla) -> gpui::Svg {
    icon("chevron-right", 12.).flex_shrink_0().text_color(ink)
}
