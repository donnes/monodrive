//! Search in the window beyond typing a name: content search's matching
//! lines under each file, and the search field's menu of what it looks at,
//! saved searches and recent ones. What's kept and how a search runs live in
//! `monodrive-core`; this draws them.
//!
//! A matching line is a list row of its own, the height of every other row,
//! so `uniform_list` still measures one row and the list stays virtualized.
//! `Matches` maps those rows back to entries, which remain one per file:
//! selection, the keys and every command work on files, and a line's row
//! stands for its file.

use super::pane::Pane;
use super::{Places, SearchContents, SearchThisFolder, ShowSearches};
use crate::commands;
use crate::menu::{self, Dismiss, Down, Run, Up};
use crate::palette::key_hint;
use crate::text_field::{self, TextField};
use crate::theme::{self, accent, content, danger, icon, on_accent, row_icon_button};
use gpui::{
    Action, App, Bounds, ClickEvent, Context, Div, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, HighlightStyle, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Role,
    SharedString, Stateful, StyledText, Task, Window, canvas, div, point, prelude::*, px,
};
use monodrive_core::index::{Found, Hit, LineMatch};
use monodrive_core::paths;
use monodrive_core::places::{RecentSearch, SavedSearch, Store};
use monodrive_core::search::{Looks, Scope as Where, Search};
use std::cell::Cell;
use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

/// How long a content search waits for the typing to stop, since it reads
/// files where a name search only asks the index.
pub(super) const CONTENT_SETTLE: Duration = Duration::from_millis(120);
/// How long results have to stay on screen to count as a search that was
/// run, rather than a step on the way to typing one.
pub(super) const REMEMBER_AFTER: Duration = Duration::from_secs(2);

const MENU_WIDTH: f32 = 360.;
const MENU_MARGIN: f32 = 8.;
/// A row with a muted line under its title.
const TALL_ROW: f32 = 40.;
/// The column of line numbers, under the file's icon and into its name.
const LINE_NUMBERS: f32 = 40.;

struct Line {
    number: SharedString,
    text: SharedString,
    matches: Vec<Range<usize>>,
}

/// The matching lines of a content search, and which rows show them. Empty
/// otherwise, when rows and entries are one to one.
#[derive(Default)]
pub(super) struct Matches {
    /// Each entry's lines.
    lines: Vec<Vec<Line>>,
    /// Each row's entry, and its line if it's one.
    rows: Vec<(usize, Option<usize>)>,
    /// Each entry's own row.
    first: Vec<usize>,
    /// The search left matches out.
    pub(super) cut: bool,
}

impl Matches {
    /// The entries and matching lines of `found`, off the UI thread: each
    /// line's preview is worked out here once.
    pub(super) fn of(found: Found) -> (Vec<monodrive_core::entries::Entry>, Self) {
        let cut = !found.complete;
        let (entries, lines): (Vec<_>, Vec<_>) = found
            .hits
            .into_iter()
            .map(|Hit { entry, lines }| (entry, lines))
            .unzip();
        (entries, Self::new(lines, cut))
    }

    fn new(lines: Vec<Vec<LineMatch>>, cut: bool) -> Self {
        let mut matches = Self {
            cut,
            ..Self::default()
        };
        if lines.iter().all(Vec::is_empty) {
            return matches;
        }
        for (entry, lines) in lines.into_iter().enumerate() {
            matches.first.push(matches.rows.len());
            matches.rows.push((entry, None));
            matches
                .rows
                .extend((0..lines.len()).map(|line| (entry, Some(line))));
            matches.lines.push(
                lines
                    .into_iter()
                    .map(|line| {
                        let (text, matches) = line.preview();
                        Line {
                            number: line.line.to_string().into(),
                            text: text.into(),
                            matches,
                        }
                    })
                    .collect(),
            );
        }
        matches
    }

    pub(super) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// How many rows the list has for `entries` entries.
    pub(super) fn row_count(&self, entries: usize) -> usize {
        if self.is_empty() {
            entries
        } else {
            self.rows.len()
        }
    }

    /// Row `row`'s entry, and its line if it's one.
    pub(super) fn at(&self, row: usize) -> (usize, Option<usize>) {
        self.rows.get(row).copied().unwrap_or((row, None))
    }

    /// Entry `entry`'s own row.
    pub(super) fn row_of(&self, entry: usize) -> usize {
        self.first.get(entry).copied().unwrap_or(entry)
    }

    /// The last row that shows entry `entry`.
    pub(super) fn last_row_of(&self, entry: usize) -> usize {
        self.row_of(entry) + self.lines.get(entry).map_or(0, Vec::len)
    }

    /// Whether `row` shows the end of its entry.
    fn ends_entry(&self, row: usize) -> bool {
        let (entry, _) = self.at(row);
        self.last_row_of(entry) == row
    }

    /// The entries `rows` show, each once.
    pub(super) fn entries_in(&self, rows: Range<usize>) -> Vec<usize> {
        let mut entries: Vec<usize> = rows.map(|row| self.at(row).0).collect();
        entries.dedup();
        entries
    }
}

impl Pane {
    /// What's on screen as a search, whether or not anything's typed.
    pub(super) fn current_search(&self) -> Search {
        Search {
            text: self.query.clone(),
            looks: self.looks,
            scope: self.search_scope.clone(),
        }
    }

    /// Looks and scope as a fresh search has them, which leaving one restores.
    pub(super) fn reset_search_mode(&mut self, cx: &mut Context<Self>) {
        self.set_search_mode(Looks::Names, Where::Everywhere, cx);
    }

    pub(super) fn search_mode_is_plain(&self) -> bool {
        self.looks == Looks::Names && self.search_scope == Where::Everywhere
    }

    /// The field's prompt says what a search would look at.
    pub(super) fn set_search_mode(&mut self, looks: Looks, scope: Where, cx: &mut Context<Self>) {
        self.looks = looks;
        self.search_scope = scope;
        let prompt = match (&self.looks, &self.search_scope) {
            (Looks::Names, Where::Everywhere) => "Search".to_string(),
            (Looks::Contents, Where::Everywhere) => "Search contents".to_string(),
            (Looks::Names, Where::Folder(folder)) => format!("Search {}", folder_name(folder)),
            (Looks::Contents, Where::Folder(folder)) => {
                format!("Search contents of {}", folder_name(folder))
            }
        };
        self.search
            .update(cx, |field, cx| field.set_placeholder(prompt, cx));
    }

    /// Search File Contents, or back to names: it runs again if there's
    /// something typed, and the keys go to the field.
    pub(super) fn toggle_contents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let looks = match self.looks {
            Looks::Names => Looks::Contents,
            Looks::Contents => Looks::Names,
        };
        self.set_search_mode(looks, self.search_scope.clone(), cx);
        self.mode_changed(window, cx);
    }

    /// Search This Folder, or back to everywhere.
    pub(super) fn toggle_this_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let scope = match self.search_scope {
            Where::Everywhere => Where::Folder(self.folder().to_path_buf()),
            Where::Folder(_) => Where::Everywhere,
        };
        self.set_search_mode(self.looks, scope, cx);
        self.mode_changed(window, cx);
    }

    fn mode_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.query.is_empty() {
            self.reload(cx);
        }
        self.focus_search(window, cx);
        cx.notify();
    }

    /// The keys go to the field, with what's typed selected.
    pub(super) fn focus_search(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |field, cx| {
            window.focus(&field.focus_handle(cx), cx);
            field.select_all(&text_field::SelectAll, window, cx);
        });
    }

    /// Runs `search`, kept or from the menu, as if it had been typed.
    pub(super) fn run_search(
        &mut self,
        search: Search,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_search_mode(search.looks, search.scope, cx);
        self.query = search.text.trim().to_string();
        let (query, end) = (self.query.clone(), self.query.len());
        self.search
            .update(cx, |field, cx| field.set_text(&query, end..end, cx));
        self.reload(cx);
        self.focus_search(window, cx);
    }

    /// Results that stay on screen for `REMEMBER_AFTER` are a recent search.
    pub(super) fn remember_search(&mut self, cx: &mut Context<Self>) {
        self.remember = Task::ready(());
        if self.query.is_empty() || self.entries.is_empty() || self.error.is_some() {
            return;
        }
        let Some(store) = cx.try_global::<Places>().map(|places| places.store.clone()) else {
            return;
        };
        let search = self.current_search();
        let Ok(query) = search.stored() else {
            return;
        };
        let wait = cx.background_executor().timer(REMEMBER_AFTER);
        let executor = cx.background_executor().clone();
        self.remember = cx.spawn(async move |_, _| {
            wait.await;
            let recent = RecentSearch {
                query,
                scope: search.scope,
            };
            // Nothing asked for this, so a list that won't save says nothing.
            let _ = executor.spawn(async move { store.searched(recent) }).await;
        });
    }

    /// A matching line's row. It selects, opens and drags its file.
    #[expect(
        clippy::too_many_arguments,
        reason = "what `rows` worked out for this frame"
    )]
    pub(super) fn line_row(
        &self,
        row: usize,
        ix: usize,
        line: usize,
        selected: bool,
        active: bool,
        height: f32,
        icon_size: f32,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let line = self.matches.lines.get(ix)?.get(line)?;
        let ink = if !selected {
            content(1.)
        } else if active {
            on_accent()
        } else {
            content(1.)
        };
        let muted = if selected { ink } else { content(0.65) };
        let highlights = line.matches.iter().map(|range| {
            (
                range.clone(),
                HighlightStyle {
                    color: Some(ink),
                    font_weight: Some(FontWeight::BOLD),
                    ..HighlightStyle::default()
                },
            )
        });
        let text = StyledText::new(line.text.clone()).with_highlights(highlights);
        let number_column = (LINE_NUMBERS - icon_size).max(0.) + icon_size;
        let row = div()
            .id(row)
            .w_full()
            .h(px(height))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .map(|d| self.row_fill(d, ix, selected, active))
            .when(self.matches.ends_entry(row), |d| {
                // design: the row radius, on the block's bottom corners.
                d.rounded_b(px(theme::RADIUS_ROW))
            })
            .child(
                div()
                    .w(px(number_column))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(theme::TEXT_CAPTION))
                    .text_color(muted)
                    .child(line.number.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::MONO_FONT)
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(muted)
                    .child(text),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_entry(ix, event, window, cx)
            }));
        let row = Self::context_target(row, ix, cx);
        Some(if selected {
            Self::grab(row, ix, cx)
        } else {
            row
        })
    }

    /// What a row of entry `ix` is filled with: the selection, or a stripe
    /// shared by a file and its lines.
    pub(super) fn row_fill(
        &self,
        row: Stateful<Div>,
        ix: usize,
        selected: bool,
        active: bool,
    ) -> Stateful<Div> {
        if selected {
            row.bg(if active { accent() } else { content(0.14) })
                .text_color(if active { on_accent() } else { content(1.) })
        } else if ix % 2 == 1 {
            row.bg(content(0.03))
        } else {
            row
        }
    }
}

fn folder_name(folder: &Path) -> String {
    folder.file_name().map_or_else(
        || folder.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What the menu asks of the window.
pub(super) enum SearchMenuEvent {
    Dismissed,
    ToggleContents,
    ToggleThisFolder,
    Run(Search),
}

/// A row the keys can land on, in order on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    Looks(Looks),
    ThisFolder,
    Saved(usize),
    Save,
    Recent(usize),
    ClearRecents,
}

/// A name being typed for a search, in place of its row.
struct Naming {
    /// The saved search being renamed, or a new one.
    renaming: Option<String>,
    field: Entity<TextField>,
}

/// The lists as last read, off the UI thread.
#[derive(Default)]
struct Kept {
    saved: Vec<SavedSearch>,
    recent: Vec<RecentSearch>,
}

fn read_kept(store: &Store) -> Result<Kept, String> {
    Ok(Kept {
        saved: store.searches()?,
        recent: store.recent_searches()?,
    })
}

/// The menu of the search field: a popover under it, with the keys while
/// it's open. Esc or a press outside closes it.
pub(super) struct SearchMenu {
    focus: FocusHandle,
    /// The field's place in the window, as last drawn.
    anchor: Rc<Cell<Bounds<Pixels>>>,
    /// The pane's search as the menu opened.
    current: Search,
    /// The folder This Folder would mean.
    folder: String,
    /// `None` without a store, as in benchmarks: only the modes show.
    store: Option<Store>,
    kept: Kept,
    active: Option<Item>,
    naming: Option<Naming>,
    /// Why the last change didn't happen.
    error: Option<String>,
    _reading: Task<()>,
}

impl EventEmitter<SearchMenuEvent> for SearchMenu {}

impl Focusable for SearchMenu {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SearchMenu {
    pub(super) fn new(
        anchor: Rc<Cell<Bounds<Pixels>>>,
        current: Search,
        folder: &Path,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = cx.try_global::<Places>().map(|places| places.store.clone());
        let mut menu = Self {
            focus: cx.focus_handle(),
            anchor,
            current,
            folder: folder_name(folder),
            store,
            kept: Kept::default(),
            active: None,
            naming: None,
            error: None,
            _reading: Task::ready(()),
        };
        menu.read(cx);
        menu
    }

    /// Reads the lists off the UI thread.
    fn read(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let read = cx
            .background_executor()
            .spawn(async move { read_kept(&store) });
        self._reading = cx.spawn(async move |this, cx| {
            let kept = read.await;
            let _ = this.update(cx, |this, cx| this.take(kept, cx));
        });
    }

    fn take(&mut self, kept: Result<Kept, String>, cx: &mut Context<Self>) {
        match kept {
            Ok(kept) => {
                self.kept = kept;
                if self
                    .active
                    .is_some_and(|active| !self.items().contains(&active))
                {
                    self.active = None;
                }
            }
            Err(why) => self.error = Some(why),
        }
        cx.notify();
    }

    /// Changes the lists off the UI thread, then shows them again. It runs
    /// to the end even if the menu closes first.
    fn change(
        &mut self,
        work: impl FnOnce(&Store) -> Result<(), String> + Send + 'static,
        done: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let work = cx.background_executor().spawn(async move {
            work(&store)?;
            read_kept(&store)
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(kept) => {
                    this.error = None;
                    this.take(Ok(kept), cx);
                    done(this, window, cx);
                }
                // The name being typed stays, to correct.
                Err(why) => {
                    this.error = Some(why);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The rows the keys land on, top to bottom.
    fn items(&self) -> Vec<Item> {
        let mut items = vec![
            Item::Looks(Looks::Names),
            Item::Looks(Looks::Contents),
            Item::ThisFolder,
        ];
        if self.store.is_some() {
            items.extend((0..self.kept.saved.len()).map(Item::Saved));
            if self.can_save() {
                items.push(Item::Save);
            }
            items.extend((0..self.kept.recent.len()).map(Item::Recent));
            if !self.kept.recent.is_empty() {
                items.push(Item::ClearRecents);
            }
        }
        items
    }

    fn can_save(&self) -> bool {
        !self.current.text.is_empty()
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let items = self.items();
        let len = items.len();
        let at = self
            .active
            .and_then(|active| items.iter().position(|item| *item == active));
        let next = match (at, down) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(at), true) => (at + 1) % len,
            (Some(at), false) => (at + len - 1) % len,
        };
        self.active = Some(items[next]);
        cx.notify();
    }

    fn run(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Looks(looks) if looks == self.current.looks => {
                cx.emit(SearchMenuEvent::Dismissed)
            }
            Item::Looks(_) => cx.emit(SearchMenuEvent::ToggleContents),
            Item::ThisFolder => cx.emit(SearchMenuEvent::ToggleThisFolder),
            Item::Saved(ix) => {
                if let Some(saved) = self.kept.saved.get(ix) {
                    let search = Search::from_stored(&saved.query, saved.scope.clone());
                    cx.emit(SearchMenuEvent::Run(search));
                }
            }
            Item::Recent(ix) => {
                if let Some(recent) = self.kept.recent.get(ix) {
                    let search = Search::from_stored(&recent.query, recent.scope.clone());
                    cx.emit(SearchMenuEvent::Run(search));
                }
            }
            Item::Save => self.name_new(window, cx),
            Item::ClearRecents => self.clear_recents(window, cx),
        }
    }

    /// Save Search…: a field takes the row, starting from what's typed.
    pub(super) fn name_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.current.text.clone();
        self.start_naming(None, &name, window, cx);
    }

    fn start_naming(
        &mut self,
        renaming: Option<String>,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| {
            let mut field = TextField::new("Name", "Name", cx);
            field.set_text(name, 0..name.len(), cx);
            field
        });
        window.focus(&field.focus_handle(cx), cx);
        self.naming = Some(Naming { renaming, field });
        self.error = None;
        cx.notify();
    }

    fn stop_naming(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.naming = None;
        self.error = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// ↩ in the name's field. A name that's taken by a saved search replaces
    /// it, as `save_search` does for agents; renaming onto one is refused.
    fn commit_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(naming) = &self.naming else {
            return;
        };
        let name = naming.field.read(cx).text().to_string();
        let done = |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
            this.stop_naming(window, cx)
        };
        match naming.renaming.clone() {
            Some(from) => self.change(
                move |store| store.rename_search(&from, &name),
                done,
                window,
                cx,
            ),
            None => match self.current.stored() {
                Ok(query) => {
                    let saved = SavedSearch {
                        name,
                        query,
                        scope: self.current.scope.clone(),
                    };
                    self.change(
                        move |store| store.save_search(saved, None).map(|_| ()),
                        done,
                        window,
                        cx,
                    );
                }
                Err(why) => {
                    self.error = Some(why);
                    cx.notify();
                }
            },
        }
    }

    fn remove_saved(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.kept.saved.get(ix).map(|saved| saved.name.clone()) {
            self.change(
                move |store| store.remove_search(&name),
                |_, _, _| {},
                window,
                cx,
            );
        }
    }

    fn rename_saved(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.kept.saved.get(ix).map(|saved| saved.name.clone()) {
            self.start_naming(Some(name.clone()), &name, window, cx);
        }
    }

    fn clear_recents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.change(Store::clear_recent_searches, |_, _, _| {}, window, cx);
    }

    /// ⌫ removes the saved search under the keys, and R renames it.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keys = &event.keystroke;
        let (Some(Item::Saved(ix)), None, false) =
            (self.active, &self.naming, keys.modifiers.modified())
        else {
            return;
        };
        match keys.key.as_str() {
            "backspace" => self.remove_saved(ix, window, cx),
            "r" => self.rename_saved(ix, window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn press_outside(&mut self, _: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        cx.emit(SearchMenuEvent::Dismissed);
    }

    /// A row of the menu, under the keys or the pointer when it's `active`.
    fn slot(
        &self,
        id: &'static str,
        ix: usize,
        item: Item,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        theme::row_slot((id, ix))
            .debug_selector(move || format!("{id}-{ix}"))
            .role(Role::MenuItem)
            .aria_selected(self.active == Some(item))
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.naming.is_none() && this.active != Some(item) {
                    this.active = Some(item);
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.naming.is_none() {
                    this.run(item, window, cx)
                }
            }))
    }

    fn check_row(
        &self,
        id: &'static str,
        title: String,
        checked: bool,
        keys: Option<String>,
        item: Item,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let on = self.active == Some(item);
        self.slot(id, 0, item, cx).child(
            theme::row(on)
                .child(div().w(px(12.)).flex_shrink_0().when(checked, |d| {
                    d.child(icon("check-hi", 12.).text_color(theme::row_muted(on)))
                }))
                .child(div().flex_1().min_w_0().truncate().child(title))
                .children(keys.map(|keys| menu::shortcut(keys, theme::row_muted(on)))),
        )
    }

    fn name_row(&self, naming: &Naming) -> Div {
        div()
            .flex_shrink_0()
            .pb(px(theme::ROW_GAP))
            .child(
                theme::field(accent())
                    .debug_selector(|| "search-name".into())
                    .h(px(theme::FIELD_HEIGHT))
                    .px_2()
                    .child(div().flex_1().min_w_0().child(naming.field.clone())),
            )
            .children(self.error_line())
    }

    fn error_line(&self) -> Option<Div> {
        self.error.clone().map(|why| {
            div()
                .debug_selector(|| "search-menu-error".into())
                .px_2()
                .py_1()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(danger())
                .child(why)
        })
    }

    fn saved_row(&self, ix: usize, saved: &SavedSearch, cx: &mut Context<Self>) -> Stateful<Div> {
        let on = self.active == Some(Item::Saved(ix));
        let ink = theme::row_muted(on);
        let about = about(&saved.query, &saved.scope);
        self.slot("saved-search", ix, Item::Saved(ix), cx).child(
            theme::row(on)
                .h(px(TALL_ROW))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().child(saved.name.clone()))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(theme::TEXT_CAPTION))
                                .text_color(ink)
                                .child(about),
                        ),
                )
                .child(
                    row_icon_button(("rename-search", ix), "Rename Search", "pencil", ink)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.rename_saved(ix, window, cx);
                        })),
                )
                .child(
                    row_icon_button(("remove-search", ix), "Remove Search", "cancel-01", ink)
                        .debug_selector(move || format!("remove-search-{ix}"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.remove_saved(ix, window, cx);
                        })),
                ),
        )
    }

    fn recent_row(
        &self,
        ix: usize,
        recent: &RecentSearch,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let on = self.active == Some(Item::Recent(ix));
        self.slot("recent-search", ix, Item::Recent(ix), cx).child(
            theme::row(on)
                .child(
                    icon("clock", 14.)
                        .flex_shrink_0()
                        .text_color(theme::row_muted(on)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(about(&recent.query, &recent.scope)),
                ),
        )
    }

    fn command_row(
        &self,
        id: &'static str,
        title: &'static str,
        item: Item,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let on = self.active == Some(item);
        self.slot(id, 0, item, cx)
            .child(theme::row(on).child(div().flex_1().truncate().child(title)))
    }

    fn footer(&self) -> Div {
        let saved = matches!(self.active, Some(Item::Saved(_))) && self.naming.is_none();
        div()
            .h(px(32.))
            .px_4()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(content(0.06))
            // A saved search's own keys take the arrows' room, which is all
            // the width has.
            .when(!saved, |d| d.child(key_hint(&["↑", "↓"], "Navigate")))
            .child(key_hint(&["Enter"], "Select"))
            .child(key_hint(&["Esc"], "Close"))
            .when(saved, |d| {
                d.child(key_hint(&["⌫"], "Remove"))
                    .child(key_hint(&["R"], "Rename"))
            })
    }
}

/// A kept search in a line: its query, and the folder it's limited to.
fn about(query: &str, scope: &Where) -> String {
    match scope {
        Where::Everywhere => query.to_string(),
        Where::Folder(folder) => format!("{query} in {}", paths::abbreviate(folder)),
    }
}

impl Render for SearchMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let anchor = self.anchor.get();
        let at = point(
            anchor
                .left()
                .min(viewport.width - px(MENU_WIDTH + MENU_MARGIN))
                .max(px(MENU_MARGIN)),
            anchor.bottom() + px(4.),
        );
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let contents = self.current.looks == Looks::Contents;
        let keys = |action: &dyn Action, cx: &App| commands::shortcut_label(action.name(), cx);
        rows.push(
            self.check_row(
                "search-names",
                "Search Names".into(),
                !contents,
                None,
                Item::Looks(Looks::Names),
                cx,
            )
            .into_any_element(),
        );
        let contents_keys = keys(&SearchContents, cx);
        rows.push(
            self.check_row(
                "search-contents",
                "Search File Contents".into(),
                contents,
                contents_keys,
                Item::Looks(Looks::Contents),
                cx,
            )
            .into_any_element(),
        );
        let this_folder = match &self.current.scope {
            Where::Everywhere => format!("Search Only {}", self.folder),
            Where::Folder(folder) => format!("Search Only {}", folder_name(folder)),
        };
        let folder_keys = keys(&SearchThisFolder, cx);
        rows.push(
            self.check_row(
                "search-this-folder",
                this_folder,
                self.current.scope != Where::Everywhere,
                folder_keys,
                Item::ThisFolder,
                cx,
            )
            .into_any_element(),
        );
        if self.store.is_some() {
            let renaming = self.naming.as_ref().and_then(|n| n.renaming.clone());
            if !self.kept.saved.is_empty() || self.can_save() || self.naming.is_some() {
                rows.push(theme::section_label("Saved").into_any_element());
            }
            for (ix, saved) in self.kept.saved.iter().enumerate() {
                match &self.naming {
                    Some(naming) if renaming.as_ref() == Some(&saved.name) => {
                        rows.push(self.name_row(naming).into_any_element())
                    }
                    _ => rows.push(self.saved_row(ix, saved, cx).into_any_element()),
                }
            }
            match &self.naming {
                Some(naming) if naming.renaming.is_none() => {
                    rows.push(self.name_row(naming).into_any_element())
                }
                _ if self.can_save() => rows.push(
                    self.command_row("save-search", "Save Search…", Item::Save, cx)
                        .into_any_element(),
                ),
                _ => {}
            }
            if !self.kept.recent.is_empty() {
                rows.push(theme::section_label("Recent").into_any_element());
                for (ix, recent) in self.kept.recent.iter().enumerate() {
                    rows.push(self.recent_row(ix, recent, cx).into_any_element());
                }
                rows.push(
                    self.command_row(
                        "clear-recent-searches",
                        "Clear Recent Searches",
                        Item::ClearRecents,
                        cx,
                    )
                    .into_any_element(),
                );
            }
        }
        if self.naming.is_none() {
            rows.extend(self.error_line().map(IntoElement::into_any_element));
        }
        div()
            .id("search-menu-layer")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press_outside))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::press_outside))
            .child(
                theme::floating(theme::Elevation::Popover)
                    .id("search-menu")
                    .debug_selector(|| "search-menu".into())
                    .key_context("ContextMenu")
                    .track_focus(&self.focus)
                    .role(Role::Menu)
                    .on_action(cx.listener(|this, _: &Up, _, cx| {
                        if this.naming.is_none() {
                            this.step(false, cx)
                        }
                    }))
                    .on_action(cx.listener(|this, _: &Down, _, cx| {
                        if this.naming.is_none() {
                            this.step(true, cx)
                        }
                    }))
                    .on_action(cx.listener(|this, _: &Run, window, cx| {
                        if this.naming.is_some() {
                            this.commit_name(window, cx);
                        } else if let Some(item) = this.active {
                            this.run(item, window, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                        if this.naming.is_some() {
                            this.stop_naming(window, cx);
                        } else {
                            cx.emit(SearchMenuEvent::Dismissed);
                        }
                    }))
                    .on_action(
                        cx.listener(|_, _: &ShowSearches, _, cx| {
                            cx.emit(SearchMenuEvent::Dismissed)
                        }),
                    )
                    .on_key_down(cx.listener(Self::key_down))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .absolute()
                    .left(at.x)
                    .top(at.y)
                    .w(px(MENU_WIDTH))
                    .max_h(viewport.height - at.y - px(MENU_MARGIN))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("search-menu-rows")
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .p_1()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .children(rows),
                    )
                    .child(div().relative().child(self.footer())),
            )
    }
}

/// Keeps `anchor` on where the search field is drawn, so the menu opens
/// under it. A change draws again, since the menu read it this frame.
pub(super) fn track_anchor(anchor: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    canvas(
        move |bounds, window, _| {
            if anchor.get() != bounds {
                anchor.set(bounds);
                window.on_next_frame(|window, _| window.refresh());
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize) -> LineMatch {
        LineMatch {
            line: n,
            text: format!("line {n}"),
            matches: std::iter::once(0..4).collect(),
        }
    }

    #[test]
    fn rows_list_each_file_then_its_lines_and_map_back_to_files() {
        // A file can come with no lines to show.
        let matches = Matches::new(vec![vec![line(3), line(9)], vec![], vec![line(1)]], false);
        let rows: Vec<_> = (0..matches.row_count(3)).map(|r| matches.at(r)).collect();
        assert_eq!(
            rows,
            [
                (0, None),
                (0, Some(0)),
                (0, Some(1)),
                (1, None),
                (2, None),
                (2, Some(0)),
            ]
        );
        assert_eq!([0, 1, 2].map(|e| matches.row_of(e)), [0, 3, 4]);
        assert_eq!([0, 1, 2].map(|e| matches.last_row_of(e)), [2, 3, 5]);
        assert_eq!(matches.entries_in(1..5), [0, 1, 2]);
    }

    #[test]
    fn without_lines_rows_are_entries() {
        let matches = Matches::new(vec![vec![], vec![]], false);
        assert!(matches.is_empty());
        assert_eq!(matches.row_count(2), 2);
        assert_eq!(matches.at(1), (1, None));
        assert_eq!(matches.row_of(1), 1);
        assert_eq!(matches.last_row_of(1), 1);
    }
}
