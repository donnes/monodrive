//! The file browser window: a sidebar of places, a folder listing, and
//! search over the whole-disk index.

use crate::background::{Backdrop, Look};
use crate::commands::{self, OpenPalette};
use crate::file_icons;
use crate::goto::{GotoEvent, GotoView};
use crate::palette::{PaletteEvent, PaletteView};
use crate::preferences::{self, Preferences, SIDEBAR_WIDTHS, View, WorkspaceStyle};
use crate::settings::{self, SettingsView};
use crate::text_field::{self, TextField};
use crate::theme::{self, accent, content, icon, icon_button, on_accent, section_label};
use crate::thumbnails::{self, Thumb};
use crate::traffic_lights;
use chrono::{DateTime, Local};
use gpui::RenderImage;
use gpui::{
    Action, App, ClickEvent, ClipboardItem, Context, CursorStyle, DispatchPhase, Entity,
    FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, Keystroke, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ScrollStrategy, Task, UniformListScrollHandle,
    Window, WindowAppearance, WindowControlArea, actions, canvas, div, prelude::*, px,
    uniform_list,
};
use monodrive_core::entries::{self, Entry, Kind, Sort, SortBy};
use monodrive_core::index::{Index, Progress, Status};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

actions!(
    browser,
    [
        Back,
        Forward,
        EnclosingFolder,
        SelectPrevious,
        SelectNext,
        SelectFirst,
        SelectLast,
        ScrollToTop,
        ScrollToBottom,
        ExpandFolder,
        PageUp,
        PageDown,
        OpenSelected,
        FocusSearch,
        ClearSearch,
        FocusNext,
        FocusPrevious,
        PlacePrevious,
        PlaceNext,
        FocusList,
        OpenPlace,
        GoHome,
        GoDesktop,
        GoDocuments,
        GoDownloads,
        GoApplications,
        GoComputer,
        OpenSettings,
        ToggleHiddenFiles,
        GoToFolder,
        CopyPath,
        ViewAsList,
        ViewAsGrid,
        SortByName,
        SortByModified,
        SortBySize,
        SortByKind,
        ReverseSort,
        ZoomIn,
        ZoomOut,
        ResetZoom,
    ]
);

pub fn bind(cx: &mut App) {
    let context = Some("Browser");
    // Commands and their shortcuts live in `commands`; these are the keys
    // that move around the window, which aren't rebindable.
    cx.bind_keys([
        // The list's selection keys also work from the search field.
        KeyBinding::new("up", SelectPrevious, context),
        KeyBinding::new("down", SelectNext, context),
        KeyBinding::new("alt-up", SelectFirst, context),
        KeyBinding::new("alt-down", SelectLast, context),
        KeyBinding::new("escape", ClearSearch, context),
        KeyBinding::new("tab", FocusNext, context),
        KeyBinding::new("shift-tab", FocusPrevious, context),
        // As in Monocode's explorer: → opens a folder in place and steps in.
        // ← is a key handler on the list instead of a binding, since it
        // needs to know whether the key is held (see `Browser::collapse`).
        KeyBinding::new("right", ExpandFolder, Some("FileList")),
        // Home, End and the page keys scroll without moving the selection,
        // as on macOS; the search field keeps them for its caret.
        KeyBinding::new("home", ScrollToTop, Some("FileList")),
        KeyBinding::new("end", ScrollToBottom, Some("FileList")),
        KeyBinding::new("pageup", PageUp, Some("FileList")),
        KeyBinding::new("pagedown", PageDown, Some("FileList")),
        KeyBinding::new("up", PlacePrevious, Some("Sidebar")),
        KeyBinding::new("down", PlaceNext, Some("Sidebar")),
        KeyBinding::new("enter", OpenPlace, Some("Sidebar")),
        // → steps from the sidebar into the files, as it does into a folder.
        KeyBinding::new("right", FocusList, Some("Sidebar")),
    ]);
}

const SEARCH_LIMIT: usize = 200;
/// How long a window move or resize must settle before its frame is kept.
const FRAME_SETTLE: Duration = Duration::from_millis(300);
/// How long the disk must settle before a changed folder lists again.
const REFRESH_DELAY: Duration = Duration::from_millis(150);
/// How often the sidebar checks on an index that is still building.
const INDEX_POLL: Duration = Duration::from_millis(500);

pub struct Browser {
    /// Keeps the browser's keys working wherever focus is.
    focus: FocusHandle,
    /// The file list: where focus starts and returns.
    list_focus: FocusHandle,
    sidebar_focus: FocusHandle,
    settings_focus: FocusHandle,
    /// The place the sidebar's keys are on, apart from the folder shown.
    sidebar_cursor: usize,
    /// What had focus when Settings opened, to return to.
    /// What had focus when a sheet (Settings or the palette) opened.
    focus_before_sheet: Option<FocusHandle>,
    /// The folder just left, to select once its parent lists.
    reselect: Option<PathBuf>,
    search: Entity<TextField>,
    /// Started with the window; searches wait on it.
    index: Arc<OnceLock<Result<Index, String>>>,
    history: Vec<PathBuf>,
    /// Position in `history` of the folder on screen.
    at: usize,
    /// The rows on screen: the folder's entries with the contents of each
    /// expanded folder under it, or the search results while `query` is set.
    entries: Vec<Entry>,
    /// Each row's depth in the tree; 0 for the folder's own entries.
    depths: Vec<usize>,
    /// The folder's own entries, or the search results.
    root: Vec<Entry>,
    /// Folders expanded in place, as in Monocode's explorer.
    expanded: HashSet<PathBuf>,
    /// The last listing of each expanded folder.
    children: HashMap<PathBuf, Vec<Entry>>,
    /// Folder listings in flight for expanded folders, dropped if superseded.
    expanding: HashMap<PathBuf, Task<()>>,
    /// A listing or search is on its way, so the rows on screen are about to
    /// change: the tree's keys wait rather than act on rows that are leaving.
    listing: bool,
    /// What on disk the listing follows.
    watch: Watch,
    /// List or grid, and the grid's icon size, as the preferences last said.
    view: View,
    grid_icon: u32,
    /// The grid's width as last laid out, for how many fit across.
    grid_width: Rc<Cell<f32>>,
    /// How many cells fit across, settled once per frame so the row count,
    /// the cells and scrolling agree.
    across: usize,
    /// Scroll to the selection once the layout settles (after a view switch).
    reveal_pending: bool,
    /// Every thumbnail asked for and not yet given back, loaded or not.
    thumbs: HashMap<Thumb, Option<Arc<RenderImage>>>,
    /// The thumbnails the last frame drew; the rest are given back.
    drawn: HashSet<Thumb>,
    /// Saves the window's frame once a move or resize settles.
    frame_save: Task<()>,
    /// Changes that arrived while a listing was on its way, checked against
    /// what that listing watches once it lands.
    held: Vec<(PathBuf, bool)>,
    /// A refresh waiting out the first change of a burst; later changes fold
    /// into it, so a folder that never stops changing still refreshes.
    refresh: Option<Task<()>>,
    /// Hands disk changes from fsearch's thread to this view.
    _changes: Task<()>,
    error: Option<String>,
    /// The trimmed search text; empty while showing the folder.
    query: String,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    /// The listing or search in flight. Replacing it drops the stale one.
    loading: Task<()>,
    /// Whether the listing on screen includes hidden files, and its order.
    show_hidden: bool,
    /// This folder shows its hidden files anyway, because Go to Folder was
    /// asked for one of them. Cleared on leaving the folder.
    peek_hidden: bool,
    sort: Sort,
    /// One sheet at a time: opening one replaces the other.
    sheet: Option<Sheet>,
    backdrop: Entity<Backdrop>,
    /// The backdrop last asked of the window, so it changes only on a switch.
    glass: Option<bool>,
    /// `None` until the index has started.
    index_status: Option<Status>,
    /// Starts the index, then polls it until it's ready.
    _index_watch: Task<()>,
    index_hovered: bool,
    /// While dragging the sidebar's edge: the sidebar's width less the
    /// pointer's x at the press, so the edge stays under the pointer.
    resizing_sidebar: Option<f32>,
}

impl Browser {
    pub fn new(folder: PathBuf, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| TextField::new("Search", "Search", cx));
        cx.subscribe(&search, |this, search, _: &text_field::Changed, cx| {
            let query = search.read(cx).text().trim().to_string();
            if query != this.query {
                this.query = query;
                this.reload(cx);
            }
        })
        .detach();
        cx.observe_global::<Preferences>(|this, cx| {
            let prefs = cx.global::<Preferences>();
            let listed = (prefs.show_hidden, prefs.sort) != (this.show_hidden, this.sort);
            let (view, grid_icon) = (prefs.view, prefs.grid_icon);
            if listed && this.query.is_empty() {
                this.reload(cx);
            }
            this.grid_icon = grid_icon;
            if view != this.view {
                this.view = view;
                this.rebuild();
                // The other layout put it somewhere else; the grid shows it
                // once it knows how many fit across.
                this.reveal_pending = this.selected.is_some();
                // Measured afresh: the window may have changed meanwhile.
                this.grid_width.set(0.);
            }
            cx.notify();
        })
        .detach();
        let mut browser = Self {
            focus: cx.focus_handle(),
            list_focus: cx.focus_handle().tab_stop(true),
            sidebar_focus: cx.focus_handle().tab_stop(true),
            settings_focus: cx.focus_handle().tab_stop(true),
            sidebar_cursor: 0,
            focus_before_sheet: None,
            reselect: None,
            search,
            index: Arc::default(),
            history: vec![folder],
            at: 0,
            entries: Vec::new(),
            depths: Vec::new(),
            root: Vec::new(),
            expanded: HashSet::new(),
            children: HashMap::new(),
            expanding: HashMap::new(),
            listing: false,
            watch: Watch::default(),
            view: cx.global::<Preferences>().view,
            grid_icon: cx.global::<Preferences>().grid_icon,
            grid_width: Rc::default(),
            across: 1,
            reveal_pending: false,
            thumbs: HashMap::new(),
            drawn: HashSet::new(),
            frame_save: Task::ready(()),
            held: Vec::new(),
            refresh: None,
            _changes: Task::ready(()),
            error: None,
            query: String::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            loading: Task::ready(()),
            show_hidden: cx.global::<Preferences>().show_hidden,
            sort: cx.global::<Preferences>().sort,
            peek_hidden: false,
            sheet: None,
            backdrop: cx.new(|_| Backdrop::default()),
            glass: None,
            index_status: None,
            _index_watch: Task::ready(()),
            index_hovered: false,
            resizing_sidebar: None,
        };
        browser.reload(cx);
        browser
    }

    /// A listing or search is on its way.
    pub fn is_listing(&self) -> bool {
        self.listing
    }

    fn folder(&self) -> &Path {
        &self.history[self.at]
    }

    fn navigate(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.reselect = Some(self.folder().to_path_buf());
        self.collapse_all();
        self.history.truncate(self.at + 1);
        self.history.push(folder);
        self.at = self.history.len() - 1;
        self.show_folder(cx);
    }

    fn step_history(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(at) = self
            .at
            .checked_add_signed(step)
            .filter(|at| *at < self.history.len())
        else {
            return;
        };
        self.reselect = Some(self.folder().to_path_buf());
        self.collapse_all();
        self.at = at;
        self.show_folder(cx);
    }

    /// Leaves search, if any, and lists the current folder.
    fn show_folder(&mut self, cx: &mut Context<Self>) {
        self.query.clear();
        self.search.update(cx, |field, cx| field.clear(cx));
        self.reload(cx);
    }

    fn collapse_all(&mut self) {
        self.peek_hidden = false;
        // A refresh due for the folder being left has nothing to refresh.
        self.refresh = None;
        self.held.clear();
        self.expanded.clear();
        self.children.clear();
        self.expanding.clear();
    }

    /// Lists the folder and every folder expanded in it, or runs the search
    /// when there is a query, off the UI thread.
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.list(false, cx);
    }

    /// `reload`, but `quiet` keeps the view where it is: a refresh after the
    /// disk changed mustn't scroll.
    fn list(&mut self, quiet: bool, cx: &mut Context<Self>) {
        let folder = self.folder().to_path_buf();
        let query = self.query.clone();
        let index = self.index.clone();
        let (show_hidden, sort) = {
            let prefs = cx.global::<Preferences>();
            (prefs.show_hidden, prefs.sort)
        };
        self.show_hidden = show_hidden;
        self.sort = sort;
        let show_hidden = show_hidden || self.peek_hidden;
        // This listing covers the expanded folders too.
        self.expanding.clear();
        self.listing = true;
        let open: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        let work = cx.background_executor().spawn(async move {
            if !query.is_empty() {
                let hits = match index.get_or_init(Index::start) {
                    Ok(index) => index.find(&query, SEARCH_LIMIT),
                    Err(e) => Err(e.clone()),
                };
                return hits.map(|hits| (hits, HashMap::new(), Watch::default()));
            }
            let root = entries::list_dir(&folder, show_hidden, sort)
                .map_err(|e| format!("{}: {e}", folder.display()))?;
            let real_folder = real(&folder);
            let watch = Watch {
                parent: real_folder.parent().map(Path::to_path_buf),
                folders: std::iter::once((folder.clone(), real_folder))
                    .chain(open.iter().map(|dir| (dir.clone(), real(dir))))
                    .collect(),
            };
            // A folder that can't be read shows as empty, as in Finder.
            let children = open
                .into_iter()
                .map(|dir| {
                    let list = entries::list_dir(&dir, show_hidden, sort).unwrap_or_default();
                    (dir, list)
                })
                .collect();
            Ok((root, children, watch))
        });
        self.loading = cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.listing = false;
                let kept = this.selected_path();
                match result {
                    Ok((root, children, watch)) => {
                        this.root = root;
                        this.children = children;
                        this.watch = watch;
                        this.error = None;
                    }
                    Err(e) => {
                        this.root.clear();
                        this.children.clear();
                        this.error = Some(e);
                    }
                }
                this.relayout(!quiet);
                this.recheck_held(cx);
                // Search selects its best hit so Enter opens it. A folder
                // selects the one just left, else keeps its selection.
                let left = this.reselect.take();
                this.selected = if this.query.is_empty() {
                    left.or(kept)
                        .and_then(|path| this.entries.iter().position(|e| e.path == path))
                } else {
                    (!this.entries.is_empty()).then_some(0)
                };
                // A refresh leaves the view where it was.
                match this.selected {
                    _ if quiet => {}
                    Some(ix) => this.reveal(ix, ScrollStrategy::Center),
                    None => this.scroll.scroll_to_item(0, ScrollStrategy::Top),
                }
                cx.notify();
            });
        });
    }

    /// Refreshes the listing shortly after the disk changes under a folder on
    /// screen.
    fn disk_changed(&mut self, batch: &[(PathBuf, bool)], cx: &mut Context<Self>) {
        if !self.query.is_empty() {
            return;
        }
        // What a pending listing will watch isn't known until it lands, and
        // it reads the disk after these changes anyway: hold them for then,
        // and don't start a listing that would replace it.
        if self.listing || !self.expanding.is_empty() {
            self.held.extend_from_slice(batch);
        }
        if self.listing {
            return;
        }
        if batch
            .iter()
            .any(|(changed, below)| self.watch.touches(changed, *below))
        {
            self.schedule_refresh(cx);
        }
    }

    /// Checks the changes held during a listing against what it watches.
    fn recheck_held(&mut self, cx: &mut Context<Self>) {
        if self.listing || !self.expanding.is_empty() {
            return;
        }
        let held = std::mem::take(&mut self.held);
        if held
            .iter()
            .any(|(changed, below)| self.watch.touches(changed, *below))
        {
            self.schedule_refresh(cx);
        }
    }

    /// One refresh `REFRESH_DELAY` after the first change of a burst; changes
    /// meanwhile fold into it.
    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_some() {
            return;
        }
        let wait = cx.background_executor().timer(REFRESH_DELAY);
        self.refresh = Some(cx.spawn(async move |this, cx| {
            wait.await;
            let _ = this.update(cx, |this, cx| {
                this.refresh = None;
                // A listing that started since reads the disk after the
                // change; it rechecks what it holds when it lands.
                if this.query.is_empty() && !this.listing {
                    this.list(true, cx);
                }
            });
        }));
    }

    fn selected_path(&self) -> Option<PathBuf> {
        Some(self.entries.get(self.selected?)?.path.clone())
    }

    /// Lays the tree out as rows, keeping the selection on the same entry.
    fn rebuild(&mut self) {
        self.relayout(true);
    }

    /// `rebuild`; `follow` scrolls to the selection if rows moved it.
    fn relayout(&mut self, follow: bool) {
        let kept = self.selected_path();
        let rows = if self.query.is_empty() && self.view == View::List {
            flatten(&self.root, &self.children, &self.expanded)
        } else {
            self.root.iter().cloned().map(|entry| (entry, 0)).collect()
        };
        let before = self.selected;
        (self.entries, self.depths) = rows.into_iter().unzip();
        self.selected = kept.and_then(|path| self.entries.iter().position(|e| e.path == path));
        // Rows opening above the selection push it down; keep it in view.
        if let Some(ix) = self.selected.filter(|ix| follow && Some(*ix) != before) {
            self.reveal(ix, ScrollStrategy::Nearest);
        }
    }

    /// Opens or closes a folder in place. Opening lists it off the UI thread.
    fn toggle_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.listing {
            return;
        }
        if self.expanded.remove(&path) {
            self.expanding.remove(&path);
            self.watch.folders.remove(&path);
            let inside = self.selected_path().is_some_and(|p| p.starts_with(&path));
            self.rebuild();
            // A selection inside the closed folder moves to it, as in Finder.
            if inside {
                self.selected = self.entries.iter().position(|e| e.path == path);
            }
            cx.notify();
            return;
        }
        self.expanded.insert(path.clone());
        let key = path.clone();
        let (show_hidden, sort) = (self.show_hidden || self.peek_hidden, self.sort);
        let dir = path.clone();
        let list = cx.background_executor().spawn(async move {
            let list = entries::list_dir(&dir, show_hidden, sort).unwrap_or_default();
            (list, real(&dir))
        });
        let task = cx.spawn(async move |this, cx| {
            let (list, watched) = list.await;
            let _ = this.update(cx, |this, cx| {
                if this.expanding.remove(&path).is_some() && this.expanded.contains(&path) {
                    // Watched from now on, not just from the next listing.
                    this.watch.folders.insert(path.clone(), watched);
                    this.children.insert(path, list);
                    this.rebuild();
                    this.recheck_held(cx);
                    cx.notify();
                }
            });
        });
        self.expanding.insert(key, task);
        self.rebuild();
        cx.notify();
    }

    /// → opens the selected folder in place, or steps into it if it's open.
    fn expand(&mut self, cx: &mut Context<Self>) {
        if self.view == View::Grid {
            return self.select(Move::By(1), cx);
        }
        if !self.query.is_empty() || self.listing {
            return;
        }
        let Some(ix) = self.selected else { return };
        match tree_step(&self.depths, ix, Arrow::Right, self.is_open_folder(ix)) {
            TreeStep::Toggle => self.toggle_folder(self.entries[ix].path.clone(), cx),
            TreeStep::Select(to) => self.select_row(to, cx),
            TreeStep::Up | TreeStep::Stay => {}
        }
    }

    /// ← closes the selected folder, steps out to its parent row, or at the
    /// top goes up to the enclosing folder. Going up takes a fresh press, so
    /// holding ← to fold a tree stops at its top instead of climbing on.
    fn collapse(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key != "left" || keystroke.modifiers.modified() {
            return;
        }
        cx.stop_propagation();
        if self.view == View::Grid {
            return self.select(Move::By(-1), cx);
        }
        if !self.query.is_empty() || self.listing {
            return;
        }
        let step = match self.selected {
            Some(ix) => tree_step(&self.depths, ix, Arrow::Left, self.is_open_folder(ix)),
            None => TreeStep::Up,
        };
        match step {
            TreeStep::Toggle => {
                if let Some(ix) = self.selected {
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

    /// Whether row `ix` is a folder, and whether it's open. Links aren't
    /// opened in place: telling where they lead would read the disk here.
    fn is_open_folder(&self, ix: usize) -> Option<bool> {
        let entry = self.entries.get(ix)?;
        (entry.kind == Kind::Dir).then(|| self.expanded.contains(&entry.path))
    }

    /// The indent for a row's depth, then its disclosure triangle if it's a
    /// folder: pressing it opens or closes the folder in place.
    fn disclosure(
        &self,
        ix: usize,
        depth: usize,
        open: Option<bool>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id(("disclosure", ix))
            .ml(px(depth as f32 * 16.))
            .size(px(14.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .when_some(open, |d, open| {
                d.cursor_pointer()
                    .child(
                        icon("chevron-right", 12.)
                            .text_color(content(0.5))
                            .when(open, |i| {
                                i.with_transformation(gpui::Transformation::rotate(
                                    gpui::percentage(0.25),
                                ))
                            }),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.focus_list(window, cx);
                            if let Some(entry) = this.entries.get(ix) {
                                this.toggle_folder(entry.path.clone(), cx);
                            }
                        }),
                    )
            })
    }

    /// Scrolls entry `ix` into view: in the grid, its row of the grid.
    fn reveal(&self, ix: usize, strategy: ScrollStrategy) {
        let item = match self.view {
            View::Grid => ix / self.grid_columns(),
            View::List => ix,
        };
        self.scroll.scroll_to_item(item, strategy);
    }

    fn take_pending_reveal(&mut self) {
        if std::mem::take(&mut self.reveal_pending)
            && let Some(ix) = self.selected
        {
            self.reveal(ix, ScrollStrategy::Center);
        }
    }

    /// How many grid cells fit across, as last laid out.
    fn grid_columns(&self) -> usize {
        self.across
    }

    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        self.reveal(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Starts the index with the window, as Spacedrive does, so it's ready
    /// before the first search, then follows it until it's ready. Searches
    /// fail until then, even one whose error is still on its way, so the
    /// current search runs again.
    pub fn start_index(&mut self, cx: &mut Context<Self>) {
        let index = self.index.clone();
        self._index_watch = cx.spawn(async move |this, cx| {
            let started = cx.background_executor().spawn({
                let index = index.clone();
                async move { index.get_or_init(Index::start).is_ok() }
            });
            // A search shows why it couldn't start.
            if !started.await {
                return;
            }
            let changes = index
                .get()
                .and_then(|index| index.as_ref().ok())
                .map(|index| {
                    let (send, receive) = async_channel::unbounded();
                    index.on_change(move |batch| {
                        let _ = send.try_send(batch);
                    });
                    receive
                });
            if let Some(receive) = changes {
                let _ = this.update(cx, |this, cx| {
                    this._changes = cx.spawn(async move |this, cx| {
                        while let Ok(batch) = receive.recv().await {
                            if this
                                .update(cx, |this, cx| this.disk_changed(&batch, cx))
                                .is_err()
                            {
                                return;
                            }
                        }
                    });
                });
            }
            loop {
                let index = index.clone();
                let status = cx
                    .background_executor()
                    .spawn(
                        async move { index.get().and_then(|i| i.as_ref().ok()).map(Index::status) },
                    )
                    .await;
                let Some(status) = status else { return };
                let ready = matches!(status.progress, Progress::Ready { .. });
                let alive = this.update(cx, |this, cx| {
                    if this.index_status != Some(status) {
                        this.index_status = Some(status);
                        if ready && !this.query.is_empty() {
                            this.reload(cx);
                        }
                        cx.notify();
                    }
                });
                if ready || alive.is_err() {
                    return;
                }
                cx.background_executor().timer(INDEX_POLL).await;
            }
        });
    }

    /// A bar of how much of the disk is indexed, shown only while the index
    /// builds; hovering swaps it for words.
    fn index_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let status = self
            .index_status
            .filter(|s| matches!(s.progress, Progress::Listing { .. } | Progress::Following))?;
        let mut label = status.progress.to_string();
        let no_access = status.full_disk_access == Some(false);
        if no_access {
            label.push_str(". No Full Disk Access, so some folders are skipped.");
        }
        let bar = div()
            .id("index-status")
            .role(gpui::Role::ProgressIndicator)
            .aria_label(label)
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .justify_end()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.index_hovered = *hovered;
                cx.notify();
            }))
            .map(|d| {
                if self.index_hovered {
                    d.child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .items_end()
                            .text_size(px(11.))
                            .line_height(px(14.))
                            .text_color(content(0.6))
                            .child(
                                div()
                                    .max_w_full()
                                    .truncate()
                                    .child(status.progress.to_string()),
                            )
                            .when(no_access, |d| {
                                d.child(
                                    div()
                                        .max_w_full()
                                        .truncate()
                                        .text_color(content(0.4))
                                        .child("No Full Disk Access"),
                                )
                            }),
                    )
                } else {
                    d.child(
                        div()
                            .w(px(72.))
                            .h(px(4.))
                            .rounded_full()
                            .bg(content(0.12))
                            .children(status.fraction().map(|fraction| {
                                div()
                                    .h_full()
                                    .w(gpui::relative(fraction))
                                    .rounded_full()
                                    .bg(accent())
                            })),
                    )
                }
            });
        Some(bar)
    }

    fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        let path = entry.path.clone();
        match entry.kind {
            Kind::Dir => self.navigate(path, cx),
            // Where a link leads is on the disk, maybe a slow volume.
            Kind::Link => {
                let target = path.clone();
                let is_dir = cx
                    .background_executor()
                    .spawn(async move { target.is_dir() });
                cx.spawn(async move |this, cx| {
                    let is_dir = is_dir.await;
                    let _ = this.update(cx, |this, cx| {
                        if is_dir {
                            this.navigate(path, cx);
                        } else {
                            cx.open_with_system(&path);
                        }
                    });
                })
                .detach();
            }
            _ => cx.open_with_system(&path),
        }
    }

    fn select(&mut self, step: Move, cx: &mut Context<Self>) {
        let next = moved(self.selected, self.entries.len(), step);
        // A held arrow at either end repaints nothing.
        if next == self.selected {
            return;
        }
        self.selected = next;
        if let Some(ix) = next {
            self.reveal(ix, ScrollStrategy::Nearest);
        }
        cx.notify();
    }

    /// ↑ and ↓: a row of the list, or of the grid.
    fn select_vertically(&mut self, down: bool, cx: &mut Context<Self>) {
        let step = match (self.view, down) {
            (View::List, true) => Move::Next,
            (View::List, false) => Move::Previous,
            (View::Grid, _) => {
                let across = self.grid_columns() as isize;
                Move::By(if down { across } else { -across })
            }
        };
        self.select(step, cx);
    }

    /// Scrolls a page, leaving the selection where it is.
    fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        let scroll = &self.scroll.0.borrow().base_handle;
        // A page less one row, so the row at the edge stays in view.
        let row_height = cx.global::<Preferences>().row_size.metrics().0;
        let page = scroll.bounds().size.height - px(row_height);
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

    /// Moves the sidebar's cursor, repainting only when it moved.
    fn move_sidebar_cursor(&mut self, to: usize, cx: &mut Context<Self>) {
        if to != self.sidebar_cursor {
            self.sidebar_cursor = to;
            cx.notify();
        }
    }

    /// Tabbing into the sidebar starts on the place that's open, if any.
    fn sync_sidebar_cursor(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.sidebar_focus.is_focused(window) {
            return;
        }
        let folder = self.folder();
        if let Some(ix) = places().iter().position(|(_, _, path)| path == folder) {
            self.move_sidebar_cursor(ix, cx);
        }
    }

    fn focus_list(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.list_focus, cx);
    }

    /// Opens a place and moves focus to its files.
    fn go(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(path, cx);
        self.focus_list(window, cx);
    }

    /// Typing in the list or sidebar searches: the first character moves
    /// focus to the search field and types there. Finder selects by name
    /// instead; search is Monodrive's way in.
    fn type_to_search(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = typed(&event.keystroke) else {
            return;
        };
        let text = text.to_owned();
        self.search.update(cx, |field, cx| {
            window.focus(&field.focus_handle(cx), cx);
            field.insert(&text, cx);
        });
        cx.stop_propagation();
    }

    /// Resizes the window when Default size changes, and keeps its frame for
    /// Last Used once a move or resize settles, rather than on every step of
    /// a drag.
    pub fn track_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Choosing a Default size shows it on this window at once.
        let mut size = cx.global::<Preferences>().window_size;
        cx.observe_global_in::<Preferences>(window, move |_, window, cx| {
            let chosen = cx.global::<Preferences>().window_size;
            if chosen != size {
                size = chosen;
                crate::frame::apply(chosen, window, cx);
            }
        })
        .detach();
        cx.observe_window_bounds(window, |this, window, cx| {
            if window.is_fullscreen() {
                return;
            }
            let bounds = window.bounds();
            let frame = preferences::Frame {
                x: bounds.origin.x.as_f32(),
                y: bounds.origin.y.as_f32(),
                width: bounds.size.width.as_f32(),
                height: bounds.size.height.as_f32(),
            };
            let settle = cx.background_executor().timer(FRAME_SETTLE);
            this.frame_save = cx.spawn(async move |_, cx| {
                settle.await;
                cx.update(|cx| preferences::update(cx, |p| p.window_frame = Some(frame)));
            });
        })
        .detach();
    }

    fn sheet_open(&self) -> bool {
        self.sheet.is_some()
    }

    /// Remembers where focus was before the first sheet opens.
    fn remember_focus(&mut self, window: &Window, cx: &App) {
        if !self.sheet_open() {
            // Clicking the gear focuses the window's root first; that isn't
            // somewhere to come back to.
            self.focus_before_sheet = window.focused(cx).filter(|f| *f != self.focus);
        }
    }

    fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sheet = None;
        let back = self
            .focus_before_sheet
            .take()
            .unwrap_or_else(|| self.list_focus.clone());
        window.focus(&back, cx);
        cx.notify();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Settings(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self.remember_focus(window, cx);
        let view = cx.new(SettingsView::new);
        cx.subscribe_in(
            &view,
            window,
            |this, _, _: &settings::Dismissed, window, cx| this.close_sheet(window, cx),
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.sheet = Some(Sheet::Settings(view));
        cx.notify();
    }

    fn open_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Goto(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self.remember_focus(window, cx);
        let folder = self.folder().to_path_buf();
        let view = cx.new(|cx| GotoView::new(folder, cx));
        cx.subscribe_in(&view, window, |this, _, event: &GotoEvent, window, cx| {
            let destination = match event {
                GotoEvent::Go(destination) => {
                    Some((destination.folder.clone(), destination.select.clone()))
                }
                GotoEvent::Dismissed => None,
            };
            this.close_sheet(window, cx);
            if let Some((folder, select)) = destination {
                this.go_to(folder, select, window, cx);
            }
        })
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.sheet = Some(Sheet::Goto(view));
        cx.notify();
    }

    /// Shows `folder` with `select` selected, the way Go to Folder and a
    /// pasted path land.
    fn go_to(
        &mut self,
        folder: PathBuf,
        select: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A hidden file asked for by name shows, though hidden files are off.
        let hidden = select
            .as_ref()
            .and_then(|path| path.file_name())
            .is_some_and(|name| name.to_string_lossy().starts_with('.'));
        // Asked after any navigation, which clears the last folder's peek.
        let needs_peek = |this: &Self| hidden && !this.show_hidden && !this.peek_hidden;
        if folder == self.folder() && self.query.is_empty() {
            let found = select
                .as_ref()
                .filter(|_| !self.listing)
                .and_then(|path| self.entries.iter().position(|e| &e.path == path));
            match found {
                Some(ix) => self.select_row(ix, cx),
                None if select.is_some() => {
                    // Selected once the listing that has it lands.
                    self.reselect = select;
                    if needs_peek(self) {
                        self.peek_hidden = true;
                        self.reload(cx);
                    }
                }
                None => {}
            }
        } else {
            self.navigate(folder, cx);
            if select.is_some() {
                self.reselect = select;
            }
            if needs_peek(self) {
                self.peek_hidden = true;
                self.reload(cx);
            }
        }
        self.focus_list(window, cx);
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Palette(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self.remember_focus(window, cx);
        let view = {
            let view = cx.new(PaletteView::new);
            cx.subscribe_in(
                &view,
                window,
                |this, _, event: &PaletteEvent, window, cx| {
                    let run = match event {
                        PaletteEvent::Run(action) => Some(action.boxed_clone()),
                        PaletteEvent::Dismissed => None,
                    };
                    this.close_sheet(window, cx);
                    if let Some(action) = run {
                        // The browser's commands come back once a frame without
                        // the sheet is drawn; dispatch reads the drawn frame.
                        window.on_next_frame(move |window, _| {
                            window.on_next_frame(move |window, cx| {
                                window.dispatch_action(action, cx)
                            });
                        });
                    }
                },
            )
            .detach();
            view
        };
        window.focus(&view.focus_handle(cx), cx);
        self.sheet = Some(Sheet::Palette(view));
        cx.notify();
    }

    /// Copies the selected item's full path, or the folder's, ready to paste
    /// into an agent's prompt or a terminal.
    fn copy_path(&self, cx: &mut Context<Self>) {
        let path = self
            .selected
            .and_then(|ix| self.entries.get(ix))
            .map_or_else(|| self.folder().to_path_buf(), |entry| entry.path.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
    }

    /// The folder's path as clickable steps, from the disk to here.
    fn path_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let current = self.folder().to_path_buf();
        let steps: Vec<PathBuf> = current.ancestors().map(Path::to_path_buf).collect();
        let last = steps.len().saturating_sub(1);
        div()
            .ml_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(2.))
            .text_size(px(13.))
            .overflow_hidden()
            .children(steps.into_iter().rev().enumerate().map(|(ix, path)| {
                let name = path.file_name().map_or_else(
                    || "Macintosh HD".to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let here = ix == last;
                // Clicking an ancestor selects the folder on the way back down.
                let child = current
                    .ancestors()
                    .find(|step| step.parent() == Some(path.as_path()))
                    .map(Path::to_path_buf);
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .min_w_0()
                    // The folder you're in keeps its name; ancestors give way.
                    .when(here, |d| d.flex_shrink_0())
                    .when(ix > 0, |d| {
                        d.child(icon("chevron-right", 10.).text_color(content(0.3)))
                    })
                    .child(
                        div()
                            .id(("path-step", ix))
                            .role(gpui::Role::Link)
                            .aria_label(name.clone())
                            .px(px(4.))
                            .rounded(px(5.))
                            .truncate()
                            .map(|d| {
                                if here {
                                    d.font_weight(FontWeight::SEMIBOLD)
                                } else {
                                    d.text_color(content(0.55))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.go_to(path.clone(), child.clone(), window, cx)
                                        }))
                                }
                            })
                            .child(name),
                    )
            }))
    }

    fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut shown = places().into_iter().enumerate();
        let cursor = self
            .sidebar_focus
            .is_focused(window)
            .then_some(self.sidebar_cursor);
        let focused = self.search.focus_handle(cx).is_focused(window);
        let width = sidebar_width(cx.global::<Preferences>().sidebar_width, window);
        div()
            .relative()
            .w(px(width))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .px_2()
            .child(drag_area().h(px(theme::TITLE_BAR_HEIGHT)).w_full())
            .child(
                div()
                    .h(px(32.))
                    .mb(px(2.))
                    .px(px(6.))
                    .rounded_md()
                    .border_1()
                    .border_color(content(if focused { 0.2 } else { 0.08 }))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon("search", 16.).text_color(content(0.35)))
                    .child(div().flex_1().min_w_0().child(self.search.clone()))
                    .when_some(
                        (!focused)
                            .then(|| commands::shortcut_label(FocusSearch.name(), cx))
                            .flatten(),
                        |d, keys| {
                            d.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(content(0.4))
                                    .child(keys),
                            )
                        },
                    ),
            )
            .child(
                div()
                    .id("places")
                    .key_context("Sidebar")
                    .track_focus(&self.sidebar_focus)
                    .on_key_down(cx.listener(Self::type_to_search))
                    .on_action(cx.listener(|this, _: &PlacePrevious, _, cx| {
                        this.move_sidebar_cursor(this.sidebar_cursor.saturating_sub(1), cx)
                    }))
                    .on_action(cx.listener(|this, _: &PlaceNext, _, cx| {
                        this.move_sidebar_cursor((this.sidebar_cursor + 1).min(PLACES - 1), cx)
                    }))
                    .on_action(cx.listener(|this, _: &FocusList, window, cx| {
                        this.focus_list(window, cx);
                        // Arrive somewhere the arrows can go on from.
                        if this.selected.is_none() && !this.entries.is_empty() {
                            this.select_row(0, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &OpenPlace, window, cx| {
                        if let Some((_, _, path)) = places().into_iter().nth(this.sidebar_cursor) {
                            this.go(path, window, cx);
                        }
                    }))
                    .flex()
                    .flex_col()
                    .child(section_label("Favorites"))
                    .children(
                        shown
                            .by_ref()
                            .take(PLACES - 1)
                            .map(|(ix, place)| self.place(ix, place, cursor, cx)),
                    )
                    .child(section_label("Locations"))
                    .children(shown.map(|(ix, place)| self.place(ix, place, cursor, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .h(px(40.))
                    .flex_shrink_0()
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        icon_button("settings", "Settings", "settings", true)
                            .track_focus(&self.settings_focus)
                            .focus_visible(|s| s.bg(content(0.1)).border_1().border_color(accent()))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_settings(window, cx)),
                            ),
                    )
                    .children(self.index_bar(cx)),
            )
            .child(
                // The edge that drags to resize, as NSSplitView's divider does.
                div()
                    .id("sidebar-edge")
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(6.))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.resizing_sidebar = Some(width - event.position.x.as_f32());
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            )
    }

    /// Follows a drag of the sidebar's edge. Element mouse handlers only fire
    /// while hovered, so this listens to the whole window, which keeps the
    /// resize cursor and tracking even when the pointer outruns the edge or
    /// leaves the window.
    fn sidebar_drag(&self, offset: f32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.set_window_cursor_style(CursorStyle::ResizeLeftRight);
                let moved = this.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = moved.update(cx, |this, cx| {
                        if event.pressed_button != Some(MouseButton::Left) {
                            this.resizing_sidebar = None;
                            cx.notify();
                            return;
                        }
                        // The sidebar starts at the window's left edge.
                        let width = sidebar_width(event.position.x.as_f32() + offset, window);
                        preferences::update(cx, |p| p.sidebar_width = width);
                    });
                });
                let released = this.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = released.update(cx, |this, cx| {
                            this.resizing_sidebar = None;
                            cx.notify();
                        });
                    }
                });
            },
        )
        .absolute()
        .size_0()
    }

    fn place(
        &self,
        ix: usize,
        (label, glyph, path): Place,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.query.is_empty() && self.folder() == path;
        div()
            .id(label)
            // The sidebar's keyboard cursor, while it has focus.
            .when(cursor == Some(ix), |d| {
                d.border_1().border_color(accent().opacity(0.7))
            })
            .role(gpui::Role::Button)
            .aria_label(label)
            .aria_selected(selected)
            .cursor_pointer()
            .h(px(32.))
            .px_2()
            .rounded_md()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(content(if selected { 1. } else { 0.5 }))
            .when(selected, |d| d.bg(content(0.1)))
            .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
            .child(icon(glyph, 16.).text_color(content(if selected { 0.7 } else { 0.35 })))
            .child(div().flex_1().truncate().child(label))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.sidebar_cursor = ix;
                this.go(path.clone(), window, cx);
            }))
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let count = match self.root.len() {
            1 => "1 item".to_string(),
            n => format!("{n} items"),
        };
        div()
            .h(px(theme::TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .pr_3()
            .child(
                icon_button("back", "Back", "chevron-left", self.at > 0)
                    .on_click(cx.listener(|this, _, _, cx| this.step_history(-1, cx))),
            )
            .child(
                icon_button(
                    "forward",
                    "Forward",
                    "chevron-right",
                    self.at + 1 < self.history.len(),
                )
                .on_click(cx.listener(|this, _, _, cx| this.step_history(1, cx))),
            )
            .map(|d| {
                if self.query.is_empty() {
                    d.child(self.path_bar(cx))
                } else {
                    d.child(
                        div()
                            .ml_1()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Search results"),
                    )
                }
            })
            .child(drag_area().flex_1().h_full())
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(content(0.4))
                    .child(count),
            )
    }

    /// The grid's rows of cells: an icon or picture over the name.
    fn grid_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let prefs = cx.global::<Preferences>();
        let (side, thumbs_on) = (prefs.grid_icon as f32, prefs.thumbnails);
        let label = prefs.grid_label.metrics().2;
        let (cell, height) = grid_cell(prefs.grid_icon);
        let across = self.across;
        let active =
            self.list_focus.is_focused(window) || self.search.focus_handle(cx).is_focused(window);
        // Sharp on Retina screens.
        let pixels = (side * window.scale_factor()).ceil() as u32;
        range
            .map(|row| {
                let cells =
                    (row * across..((row + 1) * across).min(self.entries.len())).map(|ix| {
                        let entry = &self.entries[ix];
                        let selected = self.selected == Some(ix);
                        let picture = (thumbs_on
                            && entry.kind == Kind::File
                            && thumbnails::is_picture(&entry.path))
                        .then(|| Thumb {
                            path: entry.path.clone(),
                            side: pixels,
                            modified: entry.modified,
                        })
                        .and_then(|thumb| {
                            let image = thumbnails::get(&thumb, window, cx);
                            self.drawn.insert(thumb.clone());
                            self.thumbs.insert(thumb, image.clone());
                            image
                        });
                        let art = match picture {
                            Some(image) => gpui::img(image)
                                .size(px(side))
                                .object_fit(gpui::ObjectFit::Contain)
                                .rounded(px(4.))
                                .into_any_element(),
                            None => file_icons::icon(entry, side, window, cx),
                        };
                        div()
                            .id(ix)
                            .w(px(cell))
                            .h(px(height))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(6.))
                            .pt(px(8.))
                            .child(
                                div()
                                    .p(px(4.))
                                    .rounded(px(8.))
                                    .when(selected, |d| d.bg(content(0.1)))
                                    .child(art),
                            )
                            .child(
                                div()
                                    .max_w(px(cell - 8.))
                                    .px(px(5.))
                                    .rounded(px(5.))
                                    .text_size(px(label))
                                    .text_center()
                                    .line_clamp(2)
                                    .map(|d| {
                                        if selected {
                                            d.bg(if active { accent() } else { content(0.15) })
                                                .text_color(if active {
                                                    on_accent()
                                                } else {
                                                    content(1.)
                                                })
                                        } else {
                                            d
                                        }
                                    })
                                    .child(entry.name.clone()),
                            )
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.focus_list(window, cx);
                                this.selected = Some(ix);
                                if event.click_count() == 2 {
                                    this.open(ix, cx);
                                }
                                cx.notify();
                            }))
                    });
                div().flex().px_1().children(cells).into_any_element()
            })
            .collect()
    }

    fn rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let searching = !self.query.is_empty();
        let tree = !searching;
        let today = Local::now().date_naive();
        let (row_height, icon_size, text_size) = cx.global::<Preferences>().row_size.metrics();
        // Finder greys the selection when the list isn't where keys go.
        let active =
            self.list_focus.is_focused(window) || self.search.focus_handle(cx).is_focused(window);
        range
            .filter_map(|ix| {
                let entry = self.entries.get(ix)?;
                let selected = self.selected == Some(ix);
                let depth = self.depths.get(ix).copied().unwrap_or(0);
                let open = self.is_open_folder(ix);
                let parent = searching
                    .then(|| entry.path.parent().map(|p| p.display().to_string()))
                    .flatten();
                // Finder's list: alternate rows striped, the selection in the
                // accent with its ink.
                let selected_ink = if active { on_accent() } else { content(1.) };
                let muted = if selected {
                    selected_ink.opacity(0.8)
                } else {
                    content(0.5)
                };
                Some(
                    div()
                        .id(ix)
                        .w_full()
                        .h(px(row_height))
                        .px_3()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_size(px(text_size))
                        .map(|d| {
                            if selected {
                                d.bg(if active { accent() } else { content(0.15) })
                                    .text_color(selected_ink)
                            } else if ix % 2 == 1 {
                                d.bg(content(0.03))
                            } else {
                                d
                            }
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(tree, |d| d.child(self.disclosure(ix, depth, open, cx)))
                                .child(file_icons::icon(entry, icon_size, window, cx))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .max_w(px(360.))
                                        .truncate()
                                        .child(entry.name.clone()),
                                )
                                .children(parent.map(|parent| {
                                    div().min_w_0().truncate().text_color(muted).child(parent)
                                })),
                        )
                        .child(
                            column(180., false)
                                .text_color(muted)
                                .child(format_date(entry.modified, today)),
                        )
                        .child(
                            column(80., true)
                                .text_color(muted)
                                .child(format_size(entry)),
                        )
                        .child(
                            column(110., false)
                                .text_color(muted)
                                .child(entry.kind_label()),
                        )
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.focus_list(window, cx);
                            this.selected = Some(ix);
                            if event.click_count() == 2 {
                                this.open(ix, cx);
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Focusable for Browser {
    /// The file list, so the arrows work as soon as the window opens; typing
    /// there still searches.
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.list_focus.clone()
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = cx.global::<Preferences>().clone();
        let system_light = matches!(
            window.appearance(),
            WindowAppearance::Light | WindowAppearance::VibrantLight
        );
        let glass = theme::configure(&prefs, system_light);
        if self.glass != Some(glass) {
            self.glass = Some(glass);
            window.set_background_appearance(theme::backdrop(glass));
        }
        let zoom = prefs.ui_scale.clamp(50, 200) as f32 / 100.;
        if (window.content_zoom() - zoom).abs() > f32::EPSILON {
            // Zoom after this frame, so its layout never sees the viewport change.
            window.defer(cx, move |window, cx| {
                window.set_content_zoom(zoom, cx);
                traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT * zoom);
            });
        }
        let boxy = prefs.workspace_style == WorkspaceStyle::Boxy;
        // Search results stay in relevance order, so no column shows a sort.
        let sort_shown = self.query.is_empty().then_some(prefs.sort);
        let empty = self.root.is_empty() && self.error.is_none();
        let look = prefs.background.clone().map(|path| Look {
            path,
            effect: prefs.background_effect,
            opacity: prefs.background_opacity(empty),
            empty,
            preview: false,
            // The card's corners inside its border.
            radius: if boxy { 0. } else { 7. },
        });
        self.backdrop
            .update(cx, |backdrop, cx| backdrop.set(look, cx));
        // The list's rows don't depend on its width, so it shows at once.
        if self.view == View::List {
            self.take_pending_reveal();
        }
        let list = match &self.error {
            Some(error) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px_6()
                .text_size(px(13.))
                .text_color(content(0.5))
                .child(error.clone())
                .into_any_element(),
            None if self.view == View::Grid => {
                let width = self.grid_width.clone();
                let icon = self.grid_icon;
                self.across = columns(width.get(), icon);
                // Measured at least once, so the row is right.
                if width.get() > 0. {
                    self.take_pending_reveal();
                }
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
                                let wide = bounds.size.width.as_f32();
                                // A first measurement or a new count of cells
                                // across lays out again, so rows, cells and a
                                // pending reveal use the real width. GPUI
                                // ignores a refresh while drawing, so it waits
                                // for the next frame.
                                let fresh = width.get() == 0.;
                                if fresh || columns(wide, icon) != columns(width.get(), icon) {
                                    window.on_next_frame(|window, _| window.refresh());
                                }
                                width.set(wide);
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
            None => uniform_list("entries", self.entries.len(), cx.processor(Self::rows))
                .track_scroll(&self.scroll)
                .flex_1()
                .p_1()
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
            thumbnails::release(&thumb, image, window, cx);
        }
        // Only the sheet's controls take Tab while it's open.
        let sheet = self.sheet_open();
        let regions = !sheet;
        self.search.focus_handle(cx).tab_stop(regions);
        self.list_focus.clone().tab_stop(regions);
        self.sidebar_focus.clone().tab_stop(regions);
        self.settings_focus.clone().tab_stop(regions);
        let list = div()
            .id("file-list")
            .key_context("FileList")
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(Self::type_to_search))
            .on_action(cx.listener(|this, _: &ScrollToTop, _, cx| {
                this.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| {
                if let Some(last) = this.entries.len().checked_sub(1) {
                    this.scroll
                        .scroll_to_item_strict(last, ScrollStrategy::Bottom);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ExpandFolder, _, cx| this.expand(cx)))
            .on_key_down(cx.listener(Self::collapse))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(true, cx)))
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(list);
        div()
            // The sheet takes the keys while it is open.
            .when(!sheet, |d| d.key_context("Browser"))
            .track_focus(&self.focus)
            // While a sheet is open, its commands (from the menu bar too) wait.
            .when(!sheet, |d| {
                d.on_action(cx.listener(|this, _: &Back, _, cx| this.step_history(-1, cx)))
                    .on_action(cx.listener(|this, _: &Forward, _, cx| this.step_history(1, cx)))
                    .on_action(cx.listener(|this, _: &EnclosingFolder, _, cx| {
                        if let Some(parent) = this.folder().parent().map(Path::to_path_buf) {
                            this.navigate(parent, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                        this.select_vertically(false, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &SelectNext, _, cx| this.select_vertically(true, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &SelectFirst, _, cx| this.select(Move::First, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &SelectLast, _, cx| this.select(Move::Last, cx)),
                    )
                    .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                        window.focus_next(cx);
                        this.sync_sidebar_cursor(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| {
                        window.focus_prev(cx);
                        this.sync_sidebar_cursor(window, cx);
                    }))
                    .on_action(
                        cx.listener(|this, _: &GoHome, window, cx| this.go(home(), window, cx)),
                    )
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
                    .on_action(cx.listener(|this, _: &GoComputer, window, cx| {
                        this.go("/".into(), window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &OpenSelected, _, cx| {
                        if let Some(ix) = this.selected {
                            this.open(ix, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                        this.search.update(cx, |field, cx| {
                            window.focus(&field.focus_handle(cx), cx);
                            field.select_all(&text_field::SelectAll, window, cx);
                        });
                    }))
                    .on_action(cx.listener(|this, _: &ClearSearch, window, cx| {
                        if !this.query.is_empty() {
                            this.show_folder(cx);
                        }
                        this.focus_list(window, cx);
                    }))
            })
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)),
            )
            .when(!sheet, |d| {
                d.on_action(
                    cx.listener(|this, _: &GoToFolder, window, cx| this.open_goto(window, cx)),
                )
                .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(cx)))
            })
            .on_action(|_: &ToggleHiddenFiles, _, cx| {
                preferences::update(cx, |p| p.show_hidden = !p.show_hidden)
            })
            .on_action(|_: &ViewAsList, _, cx| preferences::update(cx, |p| p.view = View::List))
            .on_action(|_: &ViewAsGrid, _, cx| preferences::update(cx, |p| p.view = View::Grid))
            .on_action(|_: &SortByName, _, cx| sort_by(SortBy::Name, cx))
            .on_action(|_: &SortByModified, _, cx| sort_by(SortBy::Modified, cx))
            .on_action(|_: &SortBySize, _, cx| sort_by(SortBy::Size, cx))
            .on_action(|_: &SortByKind, _, cx| sort_by(SortBy::Kind, cx))
            .on_action(|_: &ReverseSort, _, cx| {
                preferences::update(cx, |p| p.sort.descending = !p.sort.descending)
            })
            .on_action(|_: &ZoomIn, _, cx| preferences::update(cx, |p| p.step_ui_scale(1)))
            .on_action(|_: &ZoomOut, _, cx| preferences::update(cx, |p| p.step_ui_scale(-1)))
            .on_action(|_: &ResetZoom, _, cx| preferences::update(cx, |p| p.step_ui_scale(0)))
            .relative()
            .size_full()
            .flex()
            .font_family(theme::UI_FONT)
            .text_color(content(1.))
            .bg(theme::shell_fill())
            .child(self.sidebar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    // Boxy runs the pane the window's full height.
                    .when(boxy, |d| {
                        d.bg(theme::pane_fill())
                            .border_l_1()
                            .border_color(content(0.06))
                    })
                    .child(self.title_bar(cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .border_color(content(0.06))
                            .map(|d| {
                                if boxy {
                                    d.border_t_1()
                                } else {
                                    d.mr_2()
                                        .mb_2()
                                        .rounded_lg()
                                        .border_1()
                                        .bg(theme::pane_fill())
                                }
                            })
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(self.backdrop.clone())
                            // The grid sorts from the View menu and Settings.
                            .when(self.view == View::List, |d| {
                                d.child(
                                    div()
                                        .h(px(theme::ROW_HEIGHT))
                                        .flex_shrink_0()
                                        .px_4()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .border_b_1()
                                        .border_color(content(0.06))
                                        .text_size(px(12.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(content(0.5))
                                        .child(
                                            sort_header(SortBy::Name, "Name", sort_shown).flex_1(),
                                        )
                                        .child(
                                            sort_header(
                                                SortBy::Modified,
                                                "Date Modified",
                                                sort_shown,
                                            )
                                            .w(px(180.)),
                                        )
                                        .child(
                                            sort_header(SortBy::Size, "Size", sort_shown)
                                                .w(px(80.))
                                                .justify_end(),
                                        )
                                        .child(
                                            sort_header(SortBy::Kind, "Kind", sort_shown)
                                                .w(px(110.)),
                                        ),
                                )
                            })
                            .child(list),
                    ),
            )
            .children(self.sheet.as_ref().map(|sheet| match sheet {
                Sheet::Settings(view) => view.clone().into_any_element(),
                Sheet::Palette(view) => view.clone().into_any_element(),
                Sheet::Goto(view) => view.clone().into_any_element(),
            }))
            // Last, so its window listeners run before any element's.
            .children(
                self.resizing_sidebar
                    .map(|offset| self.sidebar_drag(offset, cx)),
            )
    }
}

/// A Sort by command picks a column in its natural direction; choosing the
/// sort in use again leaves it, as a checked menu item should. Reverse Sort
/// Order flips it.
fn sort_by(by: SortBy, cx: &mut App) {
    preferences::update(cx, |p| {
        if p.sort.by != by {
            p.sort = sorted_by(p.sort, by);
        }
    });
}

/// The sheet over the window, one at a time.
enum Sheet {
    Settings(Entity<SettingsView>),
    Palette(Entity<PaletteView>),
    Goto(Entity<GotoView>),
}

/// How many grid cells of icons `icon` points fit across `width`.
fn columns(width: f32, icon: u32) -> usize {
    let (cell, _) = grid_cell(icon);
    ((width - 8.) / cell).floor().max(1.) as usize
}

/// A grid cell's width and height for icons `icon` points across: room for
/// the icon and two lines of name.
fn grid_cell(icon: u32) -> (f32, f32) {
    let icon = icon as f32;
    (icon + 48., icon + 60.)
}

/// The file list keeps at least this much of the window.
const MIN_LIST_WIDTH: f32 = 320.;

/// `wanted` within `SIDEBAR_WIDTHS`, and narrow enough to leave the list room,
/// so a large interface scale can't push the sidebar's edge out of the window.
fn sidebar_width(wanted: f32, window: &Window) -> f32 {
    let room = window.viewport_size().width.as_f32() - MIN_LIST_WIDTH;
    wanted
        .min(room)
        .clamp(*SIDEBAR_WIDTHS.start(), *SIDEBAR_WIDTHS.end())
}

/// A column title that sorts by its column; the sorted one shows which way.
fn sort_header(by: SortBy, title: &'static str, sort: Option<Sort>) -> gpui::Stateful<gpui::Div> {
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
        .when(active.is_some(), |d| d.text_color(content(0.9)))
        // During a search the order is relevance, so the titles don't sort.
        .when(sort.is_some(), |d| {
            d.cursor_pointer()
                .hover(|s| s.text_color(content(0.9)))
                .on_click(move |_, _, cx| {
                    preferences::update(cx, |p| p.sort = sorted_by(p.sort, by))
                })
        })
        .child(div().truncate().child(title))
        .children(active.map(|sort| {
            icon("chevron-down-hi", 10.)
                .text_color(content(0.6))
                .when(!sort.descending, |i| {
                    i.with_transformation(gpui::Transformation::rotate(gpui::percentage(0.5)))
                })
        }))
}

/// Choosing the column already sorted by reverses it; another column starts
/// in its natural direction: newest and largest first, names and kinds A to Z.
pub fn sorted_by(current: Sort, by: SortBy) -> Sort {
    Sort {
        by,
        descending: if current.by == by {
            !current.descending
        } else {
            matches!(by, SortBy::Modified | SortBy::Size)
        },
        ..current
    }
}

fn column(width: f32, right: bool) -> gpui::Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .truncate()
        .text_color(content(0.5))
        .when(right, |d| d.text_right())
}

/// Blank title bar space that drags the window, and zooms it on double-click,
/// as AppKit's titlebar would. GPUI owns the titlebar, so buttons inside it
/// never start a drag.
fn drag_area() -> gpui::Div {
    div()
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |event, window, cx| {
            cx.stop_propagation();
            if event.click_count == 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
        })
}

/// `dir` as FSEvents names it: links resolved (/var is /private/var).
/// Reads the disk.
fn real(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// What on disk the listing follows, in FSEvents' real paths.
#[derive(Default)]
struct Watch {
    /// The folder and each open folder in it, by the path shown.
    folders: HashMap<PathBuf, PathBuf>,
    /// The folder's parent: renaming or deleting the folder changes it.
    parent: Option<PathBuf>,
}

impl Watch {
    fn touches(&self, changed: &Path, below: bool) -> bool {
        self.parent.as_deref() == Some(changed) || touches(changed, below, self.folders.values())
    }
}

/// Whether FSEvents reporting `changed` (and everything below it, if
/// `below`) touches one of the `shown` folders.
fn touches<'a>(changed: &Path, below: bool, shown: impl IntoIterator<Item = &'a PathBuf>) -> bool {
    shown
        .into_iter()
        .any(|dir| dir == changed || (below && dir.starts_with(changed)))
}

/// The rows a folder shows: each entry, with the contents of each expanded
/// folder right under it, one level deeper.
fn flatten(
    root: &[Entry],
    children: &HashMap<PathBuf, Vec<Entry>>,
    expanded: &HashSet<PathBuf>,
) -> Vec<(Entry, usize)> {
    fn walk(
        entries: &[Entry],
        depth: usize,
        children: &HashMap<PathBuf, Vec<Entry>>,
        expanded: &HashSet<PathBuf>,
        rows: &mut Vec<(Entry, usize)>,
    ) {
        for entry in entries {
            rows.push((entry.clone(), depth));
            if expanded.contains(&entry.path)
                && let Some(inside) = children.get(&entry.path)
            {
                walk(inside, depth + 1, children, expanded, rows);
            }
        }
    }
    let mut rows = Vec::with_capacity(root.len());
    walk(root, 0, children, expanded, &mut rows);
    rows
}

#[derive(Clone, Copy, Debug)]
enum Arrow {
    Left,
    Right,
}

#[derive(Debug, PartialEq, Eq)]
enum TreeStep {
    /// Open or close the folder in place.
    Toggle,
    Select(usize),
    /// Go up to the enclosing folder.
    Up,
    Stay,
}

/// What ← or → does on row `ix` of a tree whose rows have `depths`. `open`
/// is `None` for a file, else whether the folder is open.
fn tree_step(depths: &[usize], ix: usize, arrow: Arrow, open: Option<bool>) -> TreeStep {
    let depth = depths.get(ix).copied().unwrap_or(0);
    match (arrow, open) {
        (Arrow::Right, Some(false)) | (Arrow::Left, Some(true)) => TreeStep::Toggle,
        (Arrow::Right, Some(true)) if depths.get(ix + 1) == Some(&(depth + 1)) => {
            TreeStep::Select(ix + 1)
        }
        (Arrow::Right, _) => TreeStep::Stay,
        (Arrow::Left, _) if depth == 0 => TreeStep::Up,
        (Arrow::Left, _) => depths[..ix]
            .iter()
            .rposition(|d| *d + 1 == depth)
            .map_or(TreeStep::Stay, TreeStep::Select),
    }
}

/// The sidebar's places, Favorites then Locations, in their order on screen.
type Place = (&'static str, &'static str, PathBuf);
const PLACES: usize = 6;

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

fn places() -> [Place; PLACES] {
    let home = home();
    [
        ("Home", "folder-01", home.clone()),
        ("Desktop", "folder-01", home.join("Desktop")),
        ("Documents", "folder-01", home.join("Documents")),
        ("Downloads", "arrow-down-circle", home.join("Downloads")),
        ("Applications", "app-window", "/Applications".into()),
        ("Macintosh HD", "folder-01", "/".into()),
    ]
}

#[derive(Clone, Copy, Debug)]
enum Move {
    Previous,
    Next,
    First,
    Last,
    /// So many entries on: a row of the grid, or one across.
    By(isize),
}

/// Where a key moves the selection in a list of `len`. With nothing
/// selected, ↓ starts at the top and ↑ at the bottom, as in Finder.
fn moved(selected: Option<usize>, len: usize, step: Move) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match (step, selected) {
        (Move::First, _) | (Move::Next, None) => 0,
        (Move::Last, _) | (Move::Previous, None) => last,
        (Move::Previous, Some(ix)) => ix.min(last).saturating_sub(1),
        (Move::Next, Some(ix)) => (ix + 1).min(last),
        (Move::By(step), None) => {
            if step > 0 {
                0
            } else {
                last
            }
        }
        (Move::By(step), Some(ix)) => ix.min(last).saturating_add_signed(step).min(last),
    })
}

/// The text a key types, when it's one that should start a search: no ⌘,
/// ⌃ or fn, and not space (kept for Quick Look) or a control key. Option
/// and Shift characters count. A dead key or IME composition has no text
/// yet, so its first character is lost.
fn typed(keystroke: &Keystroke) -> Option<&str> {
    let modifiers = keystroke.modifiers;
    if modifiers.platform || modifiers.control || modifiers.function {
        return None;
    }
    keystroke.key_char.as_deref().filter(|text| {
        !text.is_empty() && !text.chars().any(|c| c.is_control() || c.is_whitespace())
    })
}

fn format_date(secs: u64, today: chrono::NaiveDate) -> String {
    let Some(time) = i64::try_from(secs)
        .ok()
        .and_then(|s| DateTime::from_timestamp(s, 0))
    else {
        return "--".into();
    };
    let time = time.with_timezone(&Local);
    if time.date_naive() == today {
        time.format("Today at %H:%M").to_string()
    } else {
        time.format("%-d %b %Y at %H:%M").to_string()
    }
}

/// Finder's decimal units: 1 KB is 1,000 bytes.
fn format_size(entry: &Entry) -> String {
    if entry.kind != Kind::File {
        return "--".into();
    }
    let bytes = entry.size as f64;
    match entry.size {
        0..1_000 => format!("{} bytes", entry.size),
        1_000..1_000_000 => format!("{:.0} KB", bytes / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes / 1e6),
        _ => format!("{:.2} GB", bytes / 1e9),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_step_and_stop_at_the_ends() {
        assert_eq!(moved(None, 5, Move::Next), Some(0));
        assert_eq!(moved(None, 5, Move::Previous), Some(4));
        assert_eq!(moved(Some(2), 5, Move::Next), Some(3));
        assert_eq!(moved(Some(4), 5, Move::Next), Some(4));
        assert_eq!(moved(Some(0), 5, Move::Previous), Some(0));
        assert_eq!(moved(Some(3), 5, Move::First), Some(0));
        assert_eq!(moved(Some(1), 5, Move::Last), Some(4));
        // A selection past a shorter listing comes back inside it.
        assert_eq!(moved(Some(9), 5, Move::Previous), Some(3));
        assert_eq!(moved(Some(9), 5, Move::Next), Some(4));
        assert_eq!(moved(None, 0, Move::Next), None);
        // The grid steps a row (here 3 across) or one sideways, and stops
        // at the ends rather than wrapping.
        assert_eq!(moved(Some(1), 8, Move::By(3)), Some(4));
        assert_eq!(moved(Some(6), 8, Move::By(3)), Some(7));
        assert_eq!(moved(Some(1), 8, Move::By(-3)), Some(0));
        assert_eq!(moved(None, 8, Move::By(-3)), Some(7));
    }

    fn entry(path: &str, kind: Kind) -> Entry {
        Entry {
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            kind,
            size: 0,
            modified: 0,
        }
    }

    #[test]
    fn expanded_folders_list_their_contents_under_them() {
        let root = [entry("/a", Kind::Dir), entry("/b", Kind::File)];
        let children = HashMap::from([
            (
                "/a".into(),
                vec![entry("/a/c", Kind::Dir), entry("/a/d", Kind::File)],
            ),
            ("/a/c".into(), vec![entry("/a/c/e", Kind::File)]),
        ]);
        let rows = |expanded: &[&str]| {
            let expanded = expanded.iter().map(PathBuf::from).collect();
            flatten(&root, &children, &expanded)
                .into_iter()
                .map(|(e, depth)| (e.path.display().to_string(), depth))
                .collect::<Vec<_>>()
        };
        let at = |path: &str, depth| (path.to_string(), depth);
        assert_eq!(rows(&[]), [at("/a", 0), at("/b", 0)]);
        assert_eq!(
            rows(&["/a", "/a/c"]),
            [
                at("/a", 0),
                at("/a/c", 1),
                at("/a/c/e", 2),
                at("/a/d", 1),
                at("/b", 0)
            ]
        );
        // A folder inside a closed one stays closed with it.
        assert_eq!(rows(&["/a/c"]), [at("/a", 0), at("/b", 0)]);
    }

    #[test]
    fn arrows_walk_the_tree_like_monocodes_explorer() {
        // /a (open) > /a/c (open) > /a/c/e, then /a/d, then /b
        let depths = [0, 1, 2, 1, 0];
        let step = |ix, arrow, open| tree_step(&depths, ix, arrow, open);
        assert_eq!(step(4, Arrow::Right, Some(false)), TreeStep::Toggle);
        assert_eq!(step(0, Arrow::Right, Some(true)), TreeStep::Select(1));
        assert_eq!(step(2, Arrow::Right, None), TreeStep::Stay);
        assert_eq!(step(1, Arrow::Left, Some(true)), TreeStep::Toggle);
        assert_eq!(step(2, Arrow::Left, None), TreeStep::Select(1));
        assert_eq!(step(3, Arrow::Left, None), TreeStep::Select(0));
        // At the top, ← leaves for the enclosing folder.
        assert_eq!(step(4, Arrow::Left, None), TreeStep::Up);
        assert_eq!(step(0, Arrow::Left, Some(false)), TreeStep::Up);
    }

    #[test]
    fn sorting_a_column_again_reverses_it_and_a_new_one_starts_its_own_way() {
        let name = Sort::default();
        let newest = sorted_by(name, SortBy::Modified);
        assert_eq!((newest.by, newest.descending), (SortBy::Modified, true));
        let oldest = sorted_by(newest, SortBy::Modified);
        assert_eq!((oldest.by, oldest.descending), (SortBy::Modified, false));
        let kinds = sorted_by(oldest, SortBy::Kind);
        assert_eq!((kinds.by, kinds.descending), (SortBy::Kind, false));
        // Keeping folders on top is a setting of its own, left as it was.
        let mixed = Sort {
            folders_first: false,
            ..name
        };
        assert!(!sorted_by(mixed, SortBy::Size).folders_first);
    }

    #[test]
    fn a_change_refreshes_only_the_folders_on_screen() {
        let shown = [PathBuf::from("/a/b"), PathBuf::from("/a/b/c")];
        assert!(touches(Path::new("/a/b"), false, &shown));
        assert!(touches(Path::new("/a/b/c"), false, &shown));
        assert!(!touches(Path::new("/a/b/c/d"), false, &shown));
        assert!(!touches(Path::new("/a"), false, &shown));
        // A lost track of everything below /a covers what's shown there.
        assert!(touches(Path::new("/a"), true, &shown));
        assert!(!touches(Path::new("/x"), true, &shown));
    }

    #[test]
    fn only_plain_typing_starts_a_search() {
        let key = |source: &str, text: Option<&str>| {
            let mut keystroke = Keystroke::parse(source).unwrap();
            keystroke.key_char = text.map(str::to_owned);
            keystroke
        };
        assert_eq!(typed(&key("a", Some("a"))), Some("a"));
        assert_eq!(typed(&key("shift-a", Some("A"))), Some("A"));
        assert_eq!(typed(&key("alt-e", Some("é"))), Some("é"));
        assert_eq!(typed(&key("cmd-a", Some("a"))), None);
        assert_eq!(typed(&key("ctrl-a", Some("a"))), None);
        assert_eq!(typed(&key("space", Some(" "))), None);
        assert_eq!(typed(&key("enter", Some("\r"))), None);
        assert_eq!(typed(&key("up", None)), None);
    }
}
