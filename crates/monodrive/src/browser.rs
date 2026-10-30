//! The file browser window: a sidebar of places, a folder listing, and
//! search over the whole-disk index.

use crate::background::{Backdrop, Look};
use crate::commands::{self, OpenPalette, Scope};
use crate::empty_trash::{self, EmptyTrashView};
use crate::file_icons;
use crate::goto::{GotoEvent, GotoView};
use crate::jobs::{self, Jobs, JobsEvent};
use crate::menu::{ContextMenu, MenuEvent};
use crate::palette::{PaletteEvent, PaletteView};
use crate::pasteboard::Pasteboard;
use crate::preferences::{self, Preferences, SIDEBAR_WIDTHS, View, WorkspaceStyle};
use crate::selection::Selection;
use crate::settings::{self, SettingsView};
use crate::text_field::{self, TextField};
use crate::theme::{self, accent, content, danger, icon, icon_button, on_accent, section_label};
use crate::thumbnails::{self, Thumb};
use crate::{traffic_lights, workspace};
use chrono::{DateTime, Local};
use gpui::RenderImage;
use gpui::{
    Action, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, DispatchPhase, Entity,
    FocusHandle, Focusable, FontWeight, Global, KeyBinding, KeyDownEvent, Keystroke, Modifiers,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollStrategy,
    ScrollWheelEvent, Subscription, Task, UniformListScrollHandle, Window, WindowAppearance,
    WindowControlArea, actions, canvas, div, point, prelude::*, px, uniform_list,
};
use monodrive_core::entries::{self, Entry, Kind, Sort, SortBy};
use monodrive_core::index::{Index, Progress, Status};
use monodrive_core::ops::{self, Conflict, Done, Job, Origin, Outcome, Request};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
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
        ExtendPrevious,
        ExtendNext,
        ExtendLeft,
        ExtendRight,
        DeselectAll,
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
        Undo,
        Redo,
        ShowCopiesAndMoves,
        Rename,
        CommitRename,
        CancelRename,
        NewFolder,
        NewFolderWithSelection,
        NewFile,
        Duplicate,
        MoveToTrash,
        EmptyTrash,
        MoveHere,
        ShowPackageContents,
        RevealInFinder,
        OpenInTerminal,
        ShowContextMenu,
    ]
);

/// The window's connection to the operation owner, shared with background
/// work. `run` sets it; without it, as in benchmarks, Undo and Redo do
/// nothing.
pub struct Operations(pub Arc<Mutex<ops::Client>>);
impl Global for Operations {}

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
        // ⇧ with the arrows grows the selection, as in Finder; ← and → do
        // only in the grid, where they move sideways.
        KeyBinding::new("shift-up", ExtendPrevious, Some("FileList")),
        KeyBinding::new("shift-down", ExtendNext, Some("FileList")),
        KeyBinding::new("shift-left", ExtendLeft, Some("FileList")),
        KeyBinding::new("shift-right", ExtendRight, Some("FileList")),
        // Home, End and the page keys scroll without moving the selection,
        // as on macOS; the search field keeps them for its caret.
        KeyBinding::new("home", ScrollToTop, Some("FileList")),
        KeyBinding::new("end", ScrollToBottom, Some("FileList")),
        KeyBinding::new("pageup", PageUp, Some("FileList")),
        KeyBinding::new("pagedown", PageDown, Some("FileList")),
        KeyBinding::new("up", PlacePrevious, Some("Sidebar")),
        KeyBinding::new("down", PlaceNext, Some("Sidebar")),
        KeyBinding::new("enter", OpenPlace, Some("Sidebar")),
        // Return in the search field opens the best hit.
        KeyBinding::new("enter", OpenSelected, Some("Search")),
        KeyBinding::new("enter", CommitRename, Some("Rename")),
        KeyBinding::new("escape", CancelRename, Some("Rename")),
        // The list's arrows would move the selection out from under the
        // name; in it they go to its start and end, as in Finder.
        KeyBinding::new("up", text_field::Home, Some("Rename")),
        KeyBinding::new("down", text_field::End, Some("Rename")),
        // → steps from the sidebar into the files, as it does into a folder.
        KeyBinding::new("right", FocusList, Some("Sidebar")),
        // The context menu on the selection, as on other desktops. GPUI's
        // macOS keys have no name for a PC keyboard's Menu key.
        KeyBinding::new("shift-f10", ShowContextMenu, Some("FileList")),
    ]);
}

const SEARCH_LIMIT: usize = 200;
/// How long a window move or resize must settle before its frame is kept.
const FRAME_SETTLE: Duration = Duration::from_millis(300);
/// How long the disk must settle before a changed folder lists again.
const REFRESH_DELAY: Duration = Duration::from_millis(150);
/// How often the sidebar checks on an index that is still building.
const INDEX_POLL: Duration = Duration::from_millis(500);
/// How long a toast stays.
const TOAST_FOR: Duration = Duration::from_secs(4);

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
    /// The window's width less the sidebar and the grid, as last laid out:
    /// the margins around the grid, which stay put as the window resizes but
    /// differ by workspace style. `None` until the grid is first drawn, when
    /// `pane_margins` stands in.
    grid_inset: Rc<Cell<Option<(WorkspaceStyle, f32)>>>,
    /// How many cells fit across and how wide each is, settled once per
    /// frame so the row count, the cells and scrolling agree.
    across: usize,
    cell_width: f32,
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
    /// What the last change did or why it didn't happen, at the bottom of
    /// the files for a few seconds.
    toast: Option<Toast>,
    /// Copies and moves, the window's and agents': the title bar's ring and
    /// its popover.
    jobs: Entity<Jobs>,
    /// The popover's keys.
    jobs_focus: FocusHandle,
    /// A job of the window's needs an answer: the popover opens once the
    /// next frame has drawn.
    jobs_wanted: bool,
    /// The name being edited in place.
    renaming: Option<Renaming>,
    /// What a change made, to select once the listing that has it lands.
    select_after: Vec<PathBuf>,
    /// The trimmed search text; empty while showing the folder.
    query: String,
    selection: Selection,
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
    /// A rectangle being dragged over the listing to select.
    marquee: Option<Marquee>,
    /// A rectangle was just dragged. GPUI still reports a click when it
    /// ends on the entry it began on; this says it was a drag.
    swept: bool,
    /// Where the listing was laid out last frame, in the window.
    list_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The context menu, while it has the keys.
    menu: Option<OpenMenu>,
    /// A context menu asked for, opened once the next frame has drawn.
    menu_request: Option<MenuRequest>,
    /// Whether this lone selected entry is a package, as last worked out.
    package: Option<(PathBuf, bool)>,
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
                this.reveal_pending = this.selection.cursor().is_some();
            }
            cx.notify();
        })
        .detach();
        let jobs = cx.new(|_| Jobs::default());
        cx.subscribe(&jobs, |this, _, event: &JobsEvent, cx| match event {
            JobsEvent::Finished(job) => this.job_finished(job.clone(), cx),
            JobsEvent::NeedsYou => {
                this.jobs_wanted = true;
                cx.notify();
            }
        })
        .detach();
        // Repaints the ring as the jobs change.
        cx.observe(&jobs, |_, _, cx| cx.notify()).detach();
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
            grid_inset: Rc::default(),
            across: 1,
            cell_width: grid_cell(cx.global::<Preferences>().grid_icon).0,
            reveal_pending: false,
            thumbs: HashMap::new(),
            drawn: HashSet::new(),
            frame_save: Task::ready(()),
            held: Vec::new(),
            refresh: None,
            _changes: Task::ready(()),
            error: None,
            toast: None,
            jobs,
            jobs_focus: cx.focus_handle(),
            jobs_wanted: false,
            renaming: None,
            select_after: Vec::new(),
            query: String::new(),
            selection: Selection::default(),
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
            marquee: None,
            swept: false,
            list_bounds: Rc::default(),
            menu: None,
            menu_request: None,
            package: None,
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
                #[expect(
                    clippy::disallowed_methods,
                    reason = "a search before launch started the index starts it"
                )]
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
                let first = if this.query.is_empty() {
                    left.and_then(|path| this.entries.iter().position(|e| e.path == path))
                } else {
                    (!this.entries.is_empty()).then_some(0)
                };
                let made = std::mem::take(&mut this.select_after);
                if this.entries.iter().any(|e| made.contains(&e.path)) {
                    this.selection.clear();
                    for entry in this.entries.iter().filter(|e| made.contains(&e.path)) {
                        this.selection.toggle(entry);
                    }
                } else if let Some(ix) = first {
                    this.selection.select(&this.entries[ix]);
                }
                if let Some(renaming) = &this.renaming
                    && !this.entries.iter().any(|e| e.path == renaming.path)
                {
                    this.renaming = None;
                }
                // A refresh leaves the view where it was.
                match this.cursor_ix() {
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

    /// The row the keys move from.
    fn cursor_ix(&self) -> Option<usize> {
        self.selection.cursor_ix(&self.entries)
    }

    /// Lays the tree out as rows, keeping the selection on the same entry.
    fn rebuild(&mut self) {
        self.relayout(true);
    }

    /// `rebuild`; `follow` scrolls to the selection if rows moved it.
    fn relayout(&mut self, follow: bool) {
        let before = self.cursor_ix();
        let rows = if self.query.is_empty() && self.view == View::List {
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

    /// Opens or closes a folder in place. Opening lists it off the UI thread.
    fn toggle_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.listing {
            return;
        }
        if self.expanded.remove(&path) {
            self.expanding.remove(&path);
            self.watch.folders.remove(&path);
            // A selection inside the closed folder moves to it, as in Finder.
            if let Some(folder) = self.entries.iter().find(|e| e.path == path) {
                self.selection.fold(folder);
            }
            self.rebuild();
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
            return self.select(Move::By(1), false, cx);
        }
        if !self.query.is_empty() || self.listing {
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
    fn collapse(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key != "left" || keystroke.modifiers.modified() || self.renaming.is_some() {
            return;
        }
        cx.stop_propagation();
        if self.view == View::Grid {
            return self.select(Move::By(-1), false, cx);
        }
        if !self.query.is_empty() || self.listing {
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
            && let Some(ix) = self.cursor_ix()
        {
            self.reveal(ix, ScrollStrategy::Center);
        }
    }

    /// How many grid cells fit across, as last laid out.
    fn grid_columns(&self) -> usize {
        self.across
    }

    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        self.selection.select(entry);
        self.reveal(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// A click on row `ix`: ⌘ toggles it, ⇧ selects the range from the
    /// anchor, and a plain click selects it alone. A double click opens.
    fn click_entry(
        &mut self,
        ix: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The end of a dragged rectangle, whichever GPUI reports first.
        if std::mem::take(&mut self.swept) || self.marquee.as_ref().is_some_and(|m| m.dragging) {
            return;
        }
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        self.focus_list(window, cx);
        let modifiers = event.modifiers();
        if modifiers.platform {
            self.selection.toggle(entry);
        } else if modifiers.shift {
            self.selection.extend(&self.entries, ix);
        } else {
            self.selection.select(entry);
            if event.click_count() == 2 {
                self.open_selection(window, cx);
            }
        }
        cx.notify();
    }

    /// A right-click on row `ix`, or on no row; GPUI delivers ⌃-click as
    /// one. An entry outside
    /// the selection becomes the selection first, as in Finder, and no row
    /// lets go of it, so the menu is on the folder.
    fn context_click(
        &mut self,
        ix: Option<usize>,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        self.focus_list(window, cx);
        match ix.and_then(|ix| self.entries.get(ix)) {
            Some(entry) if !self.selection.contains(&entry.path) => self.selection.select(entry),
            Some(_) => {}
            None => self.selection.clear(),
        }
        let scopes = self.scopes();
        self.request_menu(scopes, at, self.list_focus.clone(), cx);
    }

    /// What a context menu on the listing is on: the selection, how many,
    /// and whether it's a lone package, or else the folder.
    fn scopes(&mut self) -> Vec<Scope> {
        let mut scopes = match self.selection.len() {
            0 => return vec![Scope::Folder],
            1 => vec![Scope::Selection],
            _ => vec![Scope::Selection, Scope::Several],
        };
        if self.package().is_some() {
            scopes.push(Scope::Package);
        }
        scopes
    }

    /// The lone selected entry, if it's a folder or link named as an app or
    /// another package: what Show Package Contents goes into. Each frame
    /// asks, so the answer is kept for the entry it was worked out for.
    fn package(&mut self) -> Option<PathBuf> {
        let only = self.selection.only()?;
        if self.package.as_ref().is_none_or(|(path, _)| path != only) {
            let is = workspace::is_package(only)
                && self
                    .entries
                    .iter()
                    .any(|e| e.path == only && matches!(e.kind, Kind::Dir | Kind::Link));
            self.package = Some((only.to_path_buf(), is));
        }
        self.package
            .as_ref()
            .filter(|(_, is)| *is)
            .map(|(path, _)| path.clone())
    }

    /// Asks for a context menu at `at` in the window, whose commands run as
    /// if from `origin`.
    fn request_menu(
        &mut self,
        scopes: Vec<Scope>,
        at: Point<Pixels>,
        origin: FocusHandle,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.menu_request = Some(MenuRequest { scopes, at, origin });
        cx.notify();
    }

    /// Opens a context menu with the commands that would run from its
    /// origin now. That's read off the frame drawn last, which is why it
    /// waits for the frame showing what the click selected.
    fn open_menu(&mut self, request: MenuRequest, window: &mut Window, cx: &mut Context<Self>) {
        let MenuRequest { scopes, at, origin } = request;
        let rows = commands::context_menu(&scopes, cx.global::<Preferences>(), |action| {
            window.is_action_available_in(action, &origin)
        });
        if rows.is_empty() {
            return;
        }
        let view = cx.new(|cx| ContextMenu::new(rows, at, cx));
        cx.subscribe_in(&view, window, |this, _, event: &MenuEvent, window, cx| {
            let Some(menu) = this.menu.take() else {
                return;
            };
            window.focus(&menu.origin, cx);
            if let MenuEvent::Run(action) = event {
                window.dispatch_action(action.boxed_clone(), cx);
            }
            cx.notify();
        })
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.menu = Some(OpenMenu { view, origin });
        cx.notify();
    }

    /// Where ⇧F10 opens the menu: under the cursor's entry if it's in view,
    /// else in the listing's corner.
    fn menu_point(&self, cx: &App) -> Point<Pixels> {
        let bounds = self.list_bounds.get();
        let corner = bounds.origin + point(px(12.), px(12.));
        let Some(ix) = self.cursor_ix() else {
            return corner;
        };
        let below = match self.view {
            View::List => {
                let height = cx.global::<Preferences>().row_size.metrics().0;
                point(px(60.), px(4. + height * (ix as f32 + 1.)))
            }
            View::Grid => {
                let (across, height) = (self.across.max(1), grid_cell(self.grid_icon).1);
                let column = (ix % across) as f32 + 0.5;
                let row = (ix / across) as f32 + 1.;
                point(px(8. + self.cell_width * column), px(4. + height * row))
            }
        };
        let at = bounds.origin + self.scroll.0.borrow().base_handle.offset() + below;
        if bounds.contains(&at) { at } else { corner }
    }

    /// A right-click on `element` opens the context menu on row `ix`.
    fn context_target<E: InteractiveElement>(element: E, ix: usize, cx: &mut Context<Self>) -> E {
        element.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.context_click(Some(ix), event.position, window, cx)
            }),
        )
    }

    /// A right-click on the sidebar's place `ix`.
    fn place_menu(
        &mut self,
        ix: usize,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
        window.prevent_default();
        self.sidebar_cursor = ix;
        window.focus(&self.sidebar_focus, cx);
        self.request_menu(vec![Scope::Sidebar], at, self.sidebar_focus.clone(), cx);
    }

    fn open_place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, _, path)) = places().into_iter().nth(self.sidebar_cursor) {
            self.go(path, window, cx);
        }
    }

    /// Open in Terminal: at each selected folder and the folder holding
    /// each other item, or here with nothing selected.
    fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
        let mut folders: Vec<PathBuf> = Vec::new();
        for entry in self.selection.in_order(&self.entries) {
            let folder = match entry.kind {
                Kind::Dir => Some(entry.path.as_path()),
                _ => entry.path.parent(),
            };
            if let Some(folder) = folder.filter(|f| !folders.iter().any(|seen| seen == f)) {
                folders.push(folder.to_path_buf());
            }
        }
        if folders.is_empty() {
            folders.push(self.folder().to_path_buf());
        }
        if let Err(e) = workspace::open_in_terminal(&folders) {
            self.toast(e, true, false, cx);
        }
    }

    /// Show Package Contents: goes inside the selected package, which ↩
    /// will one day launch instead. A link goes in only if it leads to a
    /// folder, which is on the disk, maybe a slow volume.
    fn show_package_contents(&mut self, cx: &mut Context<Self>) {
        let Some(package) = self.package() else {
            return;
        };
        let target = package.clone();
        let is_dir = cx
            .background_executor()
            .spawn(async move { target.is_dir() });
        cx.spawn(async move |this, cx| {
            if is_dir.await {
                let _ = this.update(cx, |this, cx| this.navigate(package, cx));
            }
        })
        .detach();
    }

    /// Whether the keys go to the list, the search over it, or a context
    /// menu opened on it, which leaves the selection looking as it was.
    fn list_active(&self, window: &Window, cx: &App) -> bool {
        self.list_focus.contains_focused(window, cx)
            || self.search.focus_handle(cx).is_focused(window)
            || self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.origin == self.list_focus)
    }

    /// A point in the window as a point in the listing, measured from the
    /// top left of all its rows, scrolled or not.
    fn listing_point(&self, point: Point<Pixels>) -> Point<Pixels> {
        let offset = self.scroll.0.borrow().base_handle.offset();
        point - self.list_bounds.get().origin - offset
    }

    /// The rows a rectangle in listing coordinates touches, in order.
    fn rows_in(&self, rect: Bounds<Pixels>, cx: &App) -> Vec<usize> {
        let len = self.entries.len();
        // The list's padding, then in the grid each row's too.
        let (top, bottom) = (rect.top().as_f32() - 4., rect.bottom().as_f32() - 4.);
        match self.view {
            View::List => {
                let height = cx.global::<Preferences>().row_size.metrics().0;
                span(top, bottom, height, len).collect()
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

    /// Follows the pointer while a rectangle is dragged.
    fn drag_marquee(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(marquee) = self.marquee.as_mut() else {
            return;
        };
        marquee.pointer = position;
        self.sweep_marquee(cx);
        self.auto_scroll(window, cx);
    }

    /// Selects what the rectangle covers, from where it began to the
    /// pointer, once the pointer has travelled a few points.
    fn sweep_marquee(&mut self, cx: &mut Context<Self>) {
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
    fn auto_scroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn scroll_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(marquee) = self.marquee.as_mut() {
            marquee.scrolling = false;
        }
        self.sweep_marquee(cx);
        self.auto_scroll(window, cx);
    }

    /// The button's up: a drag keeps what it covered, and a click on no
    /// entry, without ⌘ or ⇧, lets go of the selection, as in Finder.
    /// Clicks on entries select through `click_entry`.
    fn end_marquee(
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

    /// The rectangle, drawn over the listing where it is now.
    fn marquee_overlay(&self) -> Option<impl IntoElement + use<>> {
        let marquee = self.marquee.as_ref().filter(|m| m.dragging)?;
        let offset = self.scroll.0.borrow().base_handle.offset();
        let corner = marquee.start.min(&marquee.end) + offset;
        let size = marquee.end - marquee.start;
        Some(
            div()
                .absolute()
                .left(corner.x)
                .top(corner.y)
                .w(size.x.abs())
                .h(size.y.abs())
                .rounded(px(2.))
                .bg(accent().opacity(0.15))
                .border_1()
                .border_color(accent().opacity(0.6)),
        )
    }

    /// Follows a rectangle's drag through the window, as `sidebar_drag`
    /// does, so it keeps up when the pointer leaves the listing.
    fn marquee_drag(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let this = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                let moved = this.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = moved
                            .update(cx, |this, cx| this.drag_marquee(event.position, window, cx));
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

    /// Opens what's selected: a lone folder goes in, and files open in
    /// their apps. Several folders at once would need tabs or windows. Past
    /// `OPEN_WITHOUT_ASKING` files it asks first, as Finder does, so ⌘A then
    /// ↩ can't launch a whole folder.
    fn open_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.selection.in_order(&self.entries);
        if let [only] = chosen.as_slice() {
            let ix = self.entries.iter().position(|e| e.path == only.path);
            if let Some(ix) = ix {
                self.open(ix, cx);
            }
            return;
        }
        let files: Vec<PathBuf> = chosen
            .into_iter()
            .filter(|entry| entry.kind != Kind::Dir)
            .map(|entry| entry.path.clone())
            .collect();
        if files.len() <= OPEN_WITHOUT_ASKING {
            for file in &files {
                cx.open_with_system(file);
            }
            return;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &format!("Open {} files?", files.len()),
            Some("Each opens in its app."),
            &["Open", "Cancel"],
            cx,
        );
        cx.spawn(async move |_, cx| {
            if answer.await == Ok(0) {
                cx.update(|cx| {
                    for file in &files {
                        cx.open_with_system(file);
                    }
                });
            }
        })
        .detach();
    }

    /// Starts the index with the window, as Spacedrive does, so it's ready
    /// before the first search, then follows it until it's ready. Searches
    /// fail until then, even one whose error is still on its way, so the
    /// current search runs again.
    pub fn start_index(&mut self, cx: &mut Context<Self>) {
        let index = self.index.clone();
        self._index_watch = cx.spawn(async move |this, cx| {
            #[expect(
                clippy::disallowed_methods,
                reason = "the app starts the index at launch"
            )]
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

    /// Moves the cursor a `step`, selecting what it lands on, or with ⇧
    /// (`extend`) the range from the anchor to it.
    fn select(&mut self, step: Move, extend: bool, cx: &mut Context<Self>) {
        let cursor = self.cursor_ix();
        let Some(next) = moved(cursor, self.entries.len(), step) else {
            return;
        };
        // A held arrow at either end repaints nothing, once the entry there
        // is all that's selected.
        let alone = self.selection.len() == 1 && self.selection.contains(&self.entries[next].path);
        if !extend && Some(next) == cursor && alone {
            return;
        }
        if extend {
            self.selection.extend(&self.entries, next);
        } else {
            self.selection.select(&self.entries[next]);
        }
        self.reveal(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// ↑ and ↓: a row of the list, or of the grid.
    fn select_vertically(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        let step = match (self.view, down) {
            (View::List, true) => Move::Next,
            (View::List, false) => Move::Previous,
            (View::Grid, _) => {
                let across = self.grid_columns() as isize;
                Move::By(if down { across } else { -across })
            }
        };
        self.select(step, extend, cx);
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
        // A name being edited takes its own typing.
        let Some(text) = typed(&event.keystroke).filter(|_| self.renaming.is_none()) else {
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

    /// Opens or closes a sheet; the jobs popover's list is kept fresh while
    /// it's open.
    fn set_sheet(&mut self, sheet: Option<Sheet>, cx: &mut Context<Self>) {
        let jobs = matches!(sheet, Some(Sheet::Jobs));
        self.sheet = sheet;
        self.jobs.update(cx, |list, cx| list.set_open(jobs, cx));
        cx.notify();
    }

    fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sheet(None, cx);
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
        self.set_sheet(Some(Sheet::Settings(view)), cx);
    }

    /// ⌥⌘J and the ring: the copies and moves popover, under the ring.
    fn toggle_jobs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.sheet, Some(Sheet::Jobs)) {
            return self.close_sheet(window, cx);
        }
        self.remember_focus(window, cx);
        window.focus(&self.jobs_focus, cx);
        self.set_sheet(Some(Sheet::Jobs), cx);
    }

    /// The popover's place, and what closes it: Esc, ⌥⌘J or a press
    /// outside, which reaches nothing behind it.
    fn jobs_popover(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let close =
            |this: &mut Self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>| {
                cx.stop_propagation();
                this.close_sheet(window, cx)
            };
        div()
            .id("jobs-layer")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(close))
            .on_mouse_down(MouseButton::Right, cx.listener(close))
            .child(
                div()
                    .id("jobs-popover")
                    .key_context("Jobs")
                    .track_focus(&self.jobs_focus)
                    .on_action(cx.listener(|this, _: &jobs::Dismiss, window, cx| {
                        this.close_sheet(window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &ShowCopiesAndMoves, window, cx| {
                        this.close_sheet(window, cx)
                    }))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .absolute()
                    .top(px(theme::TITLE_BAR_HEIGHT - 4.))
                    .right(px(8.))
                    .child(self.jobs.clone()),
            )
    }

    /// Checks the jobs each time `changes` says the owner's journal changed,
    /// which may be an agent's new job. The app starts this at launch.
    pub fn hear_jobs(
        &mut self,
        watch: ops::Watch,
        changes: async_channel::Receiver<()>,
        cx: &mut Context<Self>,
    ) {
        self.jobs
            .update(cx, |jobs, cx| jobs.hear(watch, changes, cx));
    }

    /// Shows `jobs` in the title bar as if the owner had just reported
    /// them. The snapshot example draws a running job this way, since a real
    /// one finishes before it can be drawn.
    pub fn show_jobs(&mut self, jobs: Vec<Job>, cx: &mut Context<Self>) {
        self.jobs.update(cx, |list, cx| list.take(Ok(jobs), cx));
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
        self.set_sheet(Some(Sheet::Goto(view)), cx);
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
        self.set_sheet(Some(Sheet::Palette(view)), cx);
    }

    /// Runs `work` with the operation owner off the UI thread, then `done`
    /// with what it returned. Without an owner, as in benchmarks, nothing
    /// runs and it returns false.
    fn with_owner<T: Send + 'static>(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
        work: impl FnOnce(&mut ops::Client) -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>) + 'static,
    ) -> bool {
        let Some(ops) = cx.try_global::<Operations>().map(|ops| ops.0.clone()) else {
            return false;
        };
        let work = cx
            .background_executor()
            .spawn(async move { work(&mut lock(&ops)) });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                done(this, result, window, cx);
                cx.notify();
            });
        })
        .detach();
        true
    }

    /// Asks the owner for a change, shared with agents' history.
    fn submit(&mut self, request: Request, window: &Window, cx: &mut Context<Self>) {
        // What these put somewhere is selected once it lists.
        let select = matches!(
            request,
            Request::NewFolder { .. }
                | Request::NewFile { .. }
                | Request::Duplicate { .. }
                | Request::Undo
                | Request::Redo
        );
        self.with_owner(
            window,
            cx,
            move |owner| {
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            move |this, done, _, cx| this.settle(done, select, cx),
        );
    }

    /// Shows how a change went: what it did, or why anything didn't happen,
    /// in a toast, and the folder as it is now, with what the change put
    /// here selected (`select`) or what it moved still selected where it
    /// went. The disk's own events would refresh it too, but only once the
    /// index runs.
    fn settle(
        &mut self,
        done: Result<(Request, Outcome), String>,
        select: bool,
        cx: &mut Context<Self>,
    ) {
        let (request, outcome) = match done {
            Ok(done) => done,
            Err(e) => return self.toast(e, true, false, cx),
        };
        if let Some(why) = outcome.failures().next() {
            let why = short_reason(why, [self.folder()]);
            self.toast(why, true, false, cx);
        } else if let Some(said) = said(&request, &outcome) {
            let undo = !matches!(request, Request::EmptyTrash { .. });
            self.toast(said, false, undo, cx);
        } else {
            // An undo leaves the last toast's Undo stale.
            self.toast = None;
        }
        let landed = outcome
            .items
            .iter()
            .filter_map(|item| Some((&item.path, item.result.as_ref().ok()?)));
        if select {
            self.select_after = landed.map(|(_, to)| to.clone()).collect();
        } else {
            for (from, to) in landed {
                self.selection.rename(from, to);
            }
        }
        self.list(!select, cx);
    }

    /// The selected paths, in the order shown.
    fn chosen(&self) -> Vec<PathBuf> {
        self.selection
            .in_order(&self.entries)
            .into_iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// New Folder and New File: an untitled one here, its name ready to
    /// edit.
    fn new_item(&mut self, folder: bool, window: &Window, cx: &mut Context<Self>) {
        let here = self.folder().to_path_buf();
        self.with_owner(
            window,
            cx,
            move |owner| {
                let name = if folder {
                    "untitled folder"
                } else {
                    "untitled"
                };
                let path = ops::free_name(&here.join(name));
                let request = if folder {
                    Request::NewFolder { path }
                } else {
                    Request::NewFile { path }
                };
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            move |this, outcome, window, cx| this.rename_made(outcome, folder, window, cx),
        );
    }

    /// Makes a folder beside the selection and moves the selection into it,
    /// one change to undo, then edits the folder's name.
    fn new_folder_with_selection(&mut self, window: &Window, cx: &mut Context<Self>) {
        let chosen = self.chosen();
        let Some(here) = chosen.first().and_then(|path| path.parent()) else {
            return;
        };
        let here = here.to_path_buf();
        if chosen
            .iter()
            .any(|path| path.parent() != Some(here.as_path()))
        {
            let why = "A new folder can only take items from one folder.".to_string();
            return self.toast(why, true, false, cx);
        }
        self.with_owner(
            window,
            cx,
            move |owner| {
                let path = ops::free_name(&here.join("New Folder With Items"));
                let request = Request::NewFolderWith {
                    path,
                    items: chosen,
                };
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            |this, outcome, window, cx| this.rename_made(outcome, true, window, cx),
        );
    }

    /// Selects what a New Folder or New File made and edits its name.
    fn rename_made(
        &mut self,
        done: Result<(Request, Outcome), String>,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let made = done
            .as_ref()
            .ok()
            .and_then(|(_, outcome)| outcome.items.first())
            .and_then(|item| item.result.clone().ok());
        self.settle(done, true, cx);
        if let Some(path) = made {
            self.start_rename(path, folder, window, cx);
        }
    }

    /// Copy: the selected files on the pasteboard, for any app to paste.
    fn copy_files(&self, cx: &App) {
        if let Some(board) = cx.try_global::<Pasteboard>() {
            board.write_files(&self.chosen());
        }
    }

    /// Paste, or with `moving` Move Here: the files on the pasteboard, from
    /// any app, into this folder as one job, followed until it ends.
    fn paste(&mut self, moving: bool, window: &Window, cx: &mut Context<Self>) {
        let Some(board) = cx.try_global::<Pasteboard>() else {
            return;
        };
        let paths = board.files();
        let into = self.folder().to_path_buf();
        if paths.is_empty() {
            return;
        }
        // Pasted where they already are, they're duplicated, as in Finder;
        // moved there, nothing changes.
        if paths
            .iter()
            .all(|path| path.parent() == Some(into.as_path()))
        {
            if !moving {
                self.submit(Request::Duplicate { paths }, window, cx);
            }
            return;
        }
        let conflict = Conflict::Stop;
        let request = if moving {
            Request::Move {
                paths,
                into,
                conflict,
            }
        } else {
            Request::Copy {
                paths,
                into,
                conflict,
            }
        };
        self.with_owner(
            window,
            cx,
            move |owner| owner.submit(Origin::Window, request),
            |this, outcome, _, cx| match outcome {
                Ok(outcome) => this.jobs.update(cx, |jobs, cx| jobs.follow(outcome.op, cx)),
                Err(e) => this.toast(e, true, false, cx),
            },
        );
    }

    /// A paste or Move Here finished: what it made is selected, if it's
    /// here, and a toast offers Undo.
    fn job_finished(&mut self, job: Job, cx: &mut Context<Self>) {
        self.toast(jobs::title(&job), false, true, cx);
        if self.query.is_empty() && job.into == self.folder() {
            self.select_after = job
                .tasks
                .into_iter()
                .filter_map(|task| match task.done {
                    Some(Done::To(path)) => Some(path),
                    _ => None,
                })
                .collect();
            self.list(false, cx);
        }
    }

    /// Empty Trash: erases what's in the Trash for good, once a person has
    /// confirmed how many items that is.
    fn empty_trash(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.with_owner(
            window,
            cx,
            |owner| owner.trash_items(),
            |this, items, window, cx| match items {
                Ok(items) if items.is_empty() => {
                    this.toast("The Trash is empty.".into(), false, false, cx)
                }
                Ok(items) => this.ask_empty_trash(items, window, cx),
                Err(e) => this.toast(e, true, false, cx),
            },
        );
    }

    fn ask_empty_trash(
        &mut self,
        items: Vec<ops::TrashItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remember_focus(window, cx);
        let view = cx.new(|cx| EmptyTrashView::new(items, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, answer: &empty_trash::Answer, window, cx| {
                let items = match answer {
                    empty_trash::Answer::Empty(items) => Some(items.clone()),
                    empty_trash::Answer::Cancel => None,
                };
                this.close_sheet(window, cx);
                if let Some(items) = items {
                    this.submit(Request::EmptyTrash { items }, window, cx);
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::EmptyTrash(view)), cx);
    }

    /// ↩: edits the selected item's name in place.
    fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.selection.in_order(&self.entries);
        let [only] = chosen.as_slice() else {
            return;
        };
        let (path, folder) = (only.path.clone(), only.kind == Kind::Dir);
        if let Some(ix) = self.entries.iter().position(|e| e.path == path) {
            self.reveal(ix, ScrollStrategy::Nearest);
        }
        self.start_rename(path, folder, window, cx);
    }

    /// Edits `path`'s name in place, selected up to its extension as in
    /// Finder, or whole for a folder. It needn't be listed yet.
    fn start_rename(
        &mut self,
        path: PathBuf,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = name
            .rfind('.')
            .filter(|dot| *dot > 0 && !folder)
            .unwrap_or(name.len());
        let prefs = cx.global::<Preferences>();
        let size = match self.view {
            View::List => prefs.row_size.metrics().2,
            View::Grid => prefs.grid_label.metrics().2,
        };
        let field = cx.new(|cx| {
            let mut field = TextField::new("Name", "", cx).with_font_size(size);
            field.set_text(&name, 0..stem, cx);
            field
        });
        let focus = field.focus_handle(cx);
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            this.commit_rename(window, cx)
        });
        window.focus(&focus, cx);
        self.renaming = Some(Renaming {
            path,
            field,
            error: None,
            sending: false,
            _blur: blur,
        });
        cx.notify();
    }

    /// ↩, or clicking away: renames through the owner. A name that can't be
    /// used keeps the field open with the reason; the same name just closes
    /// it.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.as_ref().filter(|r| !r.sending) else {
            return;
        };
        let path = renaming.path.clone();
        let name = renaming.field.read(cx).text().to_string();
        if path.file_name() == Some(name.as_ref()) {
            return self.end_rename(window, cx);
        }
        let request = Request::Rename {
            path: path.clone(),
            name,
        };
        let sent = self.with_owner(
            window,
            cx,
            move |owner| {
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            move |this, done, window, cx| {
                let failure = match &done {
                    Ok((_, outcome)) => outcome.failures().next().map(str::to_owned),
                    Err(e) => Some(e.clone()),
                };
                let failure = failure.map(|why| rename_reason(&why, &path));
                let still = this.renaming.as_mut().filter(|r| r.path == path);
                match (failure, still) {
                    (Some(why), Some(renaming)) => {
                        renaming.sending = false;
                        renaming.error = Some(why);
                        window.focus(&renaming.field.focus_handle(cx), cx);
                    }
                    (Some(why), None) => this.toast(why, true, false, cx),
                    (None, still) => {
                        if still.is_some() {
                            this.end_rename(window, cx);
                        }
                        this.settle(done, false, cx);
                    }
                }
            },
        );
        match self.renaming.as_mut() {
            Some(renaming) if sent => renaming.sending = true,
            _ => self.end_rename(window, cx),
        }
    }

    /// Closes the name being edited; the list takes the keys back unless
    /// a click already sent them elsewhere.
    fn end_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(renaming) = self.renaming.take()
            && renaming.field.focus_handle(cx).is_focused(window)
        {
            self.focus_list(window, cx);
        }
        cx.notify();
    }

    /// The name being edited in place, if it's `path`'s, outlined in red
    /// while the last name tried can't be used. The title bar says why.
    fn rename_field(&self, path: &Path, width: f32, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let renaming = self.renaming.as_ref().filter(|r| r.path == path)?;
        // Solid over the selection's accent, whatever the pane's glass.
        let mut fill = theme::pane_fill();
        fill.a = 1.;
        Some(
            div()
                .key_context("Rename")
                .on_action(
                    cx.listener(|this, _: &CommitRename, window, cx| {
                        this.commit_rename(window, cx)
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &CancelRename, window, cx| this.end_rename(window, cx)),
                )
                // A click in the name places the caret; it isn't a click on
                // the row or the list.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .w(px(width))
                .flex_shrink_0()
                .px(px(4.))
                .rounded(px(4.))
                .border_1()
                .border_color(if renaming.error.is_some() {
                    danger()
                } else {
                    accent()
                })
                .bg(fill)
                .text_color(content(1.))
                .child(renaming.field.clone()),
        )
    }

    /// Copies the selected paths, one per line in the order shown, or the
    /// folder's, ready to paste into an agent's prompt or a terminal.
    fn copy_path(&self, cx: &mut Context<Self>) {
        let chosen = self.selection.in_order(&self.entries);
        let text = if chosen.is_empty() {
            self.folder().display().to_string()
        } else {
            let paths: Vec<String> = chosen
                .iter()
                .map(|e| e.path.display().to_string())
                .collect();
            paths.join("\n")
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
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
        // A click focuses the sidebar too; only keys move its cursor, so
        // only they show it.
        let cursor = (self.sidebar_focus.is_focused(window) && window.last_input_was_keyboard())
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
                    .key_context("Search")
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
                        if this.selection.is_empty() && !this.entries.is_empty() {
                            this.select_row(0, cx);
                        }
                    }))
                    .on_action(
                        cx.listener(|this, _: &OpenPlace, window, cx| this.open_place(window, cx)),
                    )
                    // Open from the sidebar's menu, or ⌘O there, opens its place.
                    .on_action(
                        cx.listener(|this, _: &OpenSelected, window, cx| {
                            this.open_place(window, cx)
                        }),
                    )
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
            // The sidebar's keyboard cursor. The border is always there, so
            // showing it doesn't shift the row.
            .border_1()
            .border_color(if cursor == Some(ix) {
                accent().opacity(0.7)
            } else {
                gpui::transparent_black()
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.place_menu(ix, event.position, window, cx)
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.sidebar_cursor = ix;
                this.go(path.clone(), window, cx);
            }))
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .children(
                self.renaming
                    .as_ref()
                    .and_then(|renaming| renaming.error.clone())
                    .map(|why| {
                        // The path gives way first, as it does for the count.
                        div()
                            .flex_shrink_0()
                            .max_w(px(360.))
                            .truncate()
                            .text_size(px(12.))
                            .text_color(danger())
                            .child(why)
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(content(0.4))
                    .child(count),
            )
            .children(self.jobs.read(cx).ring().map(|ring| {
                ring.on_click(cx.listener(|this, _, window, cx| this.toggle_jobs(window, cx)))
            }))
    }

    /// The toast at the bottom of the files, with its Undo.
    fn toast_view(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let toast = self.toast.as_ref()?;
        let ink = if toast.failed { danger() } else { content(0.9) };
        Some(
            div()
                .absolute()
                .bottom(px(14.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("toast")
                        .occlude()
                        .relative()
                        .max_w(px(520.))
                        .mx_4()
                        .h(px(34.))
                        .pl_3()
                        .pr(px(6.))
                        .rounded(px(10.))
                        .shadow(vec![
                            gpui::BoxShadow::new(px(0.), px(6.), theme::black(0.25))
                                .blur_radius(px(18.))
                                .spread_radius(px(-4.)),
                        ])
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(theme::floating_surface(10.))
                        .when(toast.failed, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded(px(10.))
                                    .bg(danger().opacity(0.12)),
                            )
                        })
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .rounded(px(10.))
                                .border_1()
                                .border_color(content(0.12)),
                        )
                        .child(
                            div()
                                .relative()
                                .min_w_0()
                                .truncate()
                                .text_size(px(13.))
                                .text_color(ink)
                                .child(toast.text.clone()),
                        )
                        .when(toast.undo, |d| {
                            d.child(
                                div()
                                    .id("toast-undo")
                                    .debug_selector(|| "toast-undo".into())
                                    .role(gpui::Role::Button)
                                    .aria_label("Undo")
                                    .relative()
                                    .flex_shrink_0()
                                    .h(px(24.))
                                    .px_2()
                                    .rounded(px(6.))
                                    .flex()
                                    .items_center()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(accent())
                                    .cursor_pointer()
                                    .hover(|s| s.bg(content(0.1)))
                                    .child("Undo")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toast = None;
                                        this.submit(Request::Undo, window, cx)
                                    })),
                            )
                        })
                        .when(!toast.undo, |d| d.pr_3()),
                ),
        )
    }

    /// Says `text` at the bottom of the files for `TOAST_FOR`, in red if
    /// it's a failure, with Undo if `undo`. It replaces the last one.
    fn toast(&mut self, text: String, failed: bool, undo: bool, cx: &mut Context<Self>) {
        let expire = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_FOR).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        });
        self.toast = Some(Toast {
            text,
            failed,
            undo,
            _expire: expire,
        });
        cx.notify();
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
        let (cell, height) = (self.cell_width, grid_cell(prefs.grid_icon).1);
        let across = self.across;
        let active = self.list_active(window, cx);
        // Sharp on Retina screens.
        let pixels = (side * window.scale_factor()).ceil() as u32;
        range
            .map(|row| {
                let cells =
                    (row * across..((row + 1) * across).min(self.entries.len())).map(|ix| {
                        let entry = &self.entries[ix];
                        let selected = self.selection.contains(&entry.path);
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
                            // A square box: without one, `img` takes the
                            // picture's own shape, and a tall one spills
                            // onto its name.
                            Some(image) => gpui::img(image)
                                .size(px(side))
                                .aspect_square()
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
                            .child(match self.rename_field(&entry.path, cell - 8., cx) {
                                Some(field) => field.text_size(px(label)).into_any_element(),
                                None => div()
                                    .max_w(px(cell - 8.))
                                    .px(px(5.))
                                    .rounded(px(5.))
                                    .text_size(px(label))
                                    .text_center()
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
                                    .children(grid_name(
                                        &entry.name,
                                        px(cell - 18.),
                                        px(label),
                                        window,
                                    ))
                                    .into_any_element(),
                            })
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                this.click_entry(ix, event, window, cx)
                            }))
                            .map(|d| Self::context_target(d, ix, cx))
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
        let active = self.list_active(window, cx);
        range
            .filter_map(|ix| {
                let entry = self.entries.get(ix)?;
                let selected = self.selection.contains(&entry.path);
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
                                .child(match self.rename_field(&entry.path, 260., cx) {
                                    Some(field) => field.into_any_element(),
                                    None => div()
                                        .flex_shrink_0()
                                        .max_w(px(360.))
                                        .truncate()
                                        .child(entry.name.clone())
                                        .into_any_element(),
                                })
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
                            this.click_entry(ix, event, window, cx)
                        }))
                        .map(|d| Self::context_target(d, ix, cx))
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
                let icon = self.grid_icon;
                let prefs = cx.global::<Preferences>();
                let (style, sidebar) = (
                    prefs.workspace_style,
                    sidebar_width(prefs.sidebar_width, window),
                );
                let inset = self.grid_inset.clone();
                // The grid's width this frame, from the window's, so a resize
                // lays out the right columns now rather than a frame late. The
                // margins beside it are as last measured, or before that, as
                // the pane draws them.
                let margins = inset
                    .get()
                    .filter(|(measured, _)| *measured == style)
                    .map_or(pane_margins(style), |(_, margins)| margins);
                let wide = window.viewport_size().width.as_f32() - sidebar - margins;
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
                                // A width the estimate missed (the interface
                                // zoomed) lays out again. GPUI ignores a
                                // refresh while drawing, so it waits for the
                                // next frame.
                                if (wide - measured).abs() > 0.5 {
                                    window.on_next_frame(|window, _| window.refresh());
                                }
                                let viewport = window.viewport_size().width.as_f32();
                                inset.set(Some((style, viewport - sidebar - measured)));
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
        // The menu gives way to whatever took the keys from it.
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| !menu.view.focus_handle(cx).is_focused(window))
        {
            self.menu = None;
        }
        if let Some(request) = self.menu_request.take() {
            cx.defer_in(window, move |this, window, cx| {
                this.open_menu(request, window, cx)
            });
        }
        // A sheet the person opened meanwhile stays; the ring shows the job.
        if std::mem::take(&mut self.jobs_wanted) && !self.sheet_open() {
            cx.defer_in(window, |this, window, cx| {
                if !this.sheet_open() {
                    this.toggle_jobs(window, cx)
                }
            });
        }
        let package = self.package().is_some();
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
            .on_action(cx.listener(|this, _: &ExtendPrevious, _, cx| {
                this.select_vertically(false, true, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ExtendNext, _, cx| this.select_vertically(true, true, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendLeft, _, cx| {
                if this.view == View::Grid {
                    this.select(Move::By(-1), true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ExtendRight, _, cx| {
                if this.view == View::Grid {
                    this.select(Move::By(1), true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &text_field::SelectAll, _, cx| {
                this.selection.select_all(&this.entries);
                cx.notify();
            }))
            // Copy and Paste are AppKit's actions, as Select All is: here
            // they're files. Leaving one out turns its menu item off.
            .when(!self.selection.is_empty(), |d| {
                d.on_action(cx.listener(|this, _: &text_field::Copy, _, cx| this.copy_files(cx)))
            })
            .when(self.query.is_empty(), |d| {
                d.on_action(cx.listener(|this, _: &text_field::Paste, window, cx| {
                    this.paste(false, window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &MoveHere, window, cx| this.paste(true, window, cx)),
                )
            })
            .on_action(cx.listener(|this, _: &ShowContextMenu, _, cx| {
                if this.renaming.is_none() {
                    let at = this.menu_point(cx);
                    let scopes = this.scopes();
                    this.request_menu(scopes, at, this.list_focus.clone(), cx);
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
            .relative()
            // The rectangle stays inside the listing.
            .overflow_hidden()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(list)
            .child({
                let bounds = self.list_bounds.clone();
                canvas(move |laid_out, _, _| bounds.set(laid_out), |_, _, _, _| {})
                    .absolute()
                    .size_full()
            })
            .children(self.marquee_overlay())
            .when(self.marquee.is_some(), |d| d.child(self.marquee_drag(cx)));
        div()
            // A sheet or the context menu takes the keys while it's open:
            // the menu's commands act on what it was opened on.
            .when(!sheet && self.menu.is_none(), |d| d.key_context("Browser"))
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
                        this.select_vertically(false, false, cx)
                    }))
                    .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                        this.select_vertically(true, false, cx)
                    }))
                    .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                        this.select(Move::First, false, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &SelectLast, _, cx| {
                            this.select(Move::Last, false, cx)
                        }),
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
                    .on_action(cx.listener(|this, _: &OpenSelected, window, cx| {
                        this.open_selection(window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                        this.search.update(cx, |field, cx| {
                            window.focus(&field.focus_handle(cx), cx);
                            field.select_all(&text_field::SelectAll, window, cx);
                        });
                    }))
                    // Esc leaves search first, then lets go of the selection.
                    .on_action(cx.listener(|this, _: &ClearSearch, window, cx| {
                        this.toast = None;
                        if this.query.is_empty() {
                            this.selection.clear();
                            cx.notify();
                        } else {
                            this.show_folder(cx);
                        }
                        this.focus_list(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &DeselectAll, _, cx| {
                        this.selection.clear();
                        cx.notify();
                    }))
                    .on_action(
                        cx.listener(|this, _: &EmptyTrash, window, cx| {
                            this.empty_trash(window, cx)
                        }),
                    )
            })
            // While a name is edited, its field has the keys, and nothing
            // else changes files.
            .when(!sheet && self.renaming.is_none(), |d| {
                d.on_action(
                    cx.listener(|this, _: &Undo, window, cx| {
                        this.submit(Request::Undo, window, cx)
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &Redo, window, cx| {
                        this.submit(Request::Redo, window, cx)
                    }),
                )
                .when(self.query.is_empty(), |d| {
                    d.on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                        this.new_item(true, window, cx)
                    }))
                    .on_action(
                        cx.listener(|this, _: &NewFile, window, cx| {
                            this.new_item(false, window, cx)
                        }),
                    )
                })
                .when(self.selection.len() == 1, |d| {
                    d.on_action(
                        cx.listener(|this, _: &Rename, window, cx| {
                            this.rename_selected(window, cx)
                        }),
                    )
                })
                .when(!self.selection.is_empty(), |d| {
                    d.on_action(cx.listener(|this, _: &Duplicate, window, cx| {
                        let paths = this.chosen();
                        this.submit(Request::Duplicate { paths }, window, cx)
                    }))
                    .on_action(cx.listener(|this, _: &MoveToTrash, window, cx| {
                        let paths = this.chosen();
                        this.submit(Request::Trash { paths }, window, cx)
                    }))
                    .on_action(cx.listener(
                        |this, _: &NewFolderWithSelection, window, cx| {
                            this.new_folder_with_selection(window, cx)
                        },
                    ))
                })
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
                .on_action(cx.listener(|this, _: &OpenInTerminal, _, cx| this.open_in_terminal(cx)))
                .when(!self.selection.is_empty(), |d| {
                    d.on_action(cx.listener(|this, _: &RevealInFinder, _, _| {
                        workspace::reveal(&this.chosen())
                    }))
                })
                .when(package, |d| {
                    d.on_action(cx.listener(|this, _: &ShowPackageContents, _, cx| {
                        this.show_package_contents(cx)
                    }))
                })
                .on_action(cx.listener(
                    |this, _: &ShowCopiesAndMoves, window, cx| this.toggle_jobs(window, cx),
                ))
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
                            .child(list)
                            .children(self.toast_view(cx)),
                    ),
            )
            .children(self.sheet.as_ref().map(|sheet| match sheet {
                Sheet::Settings(view) => view.clone().into_any_element(),
                Sheet::Palette(view) => view.clone().into_any_element(),
                Sheet::Goto(view) => view.clone().into_any_element(),
                Sheet::Jobs => self.jobs_popover(cx).into_any_element(),
                Sheet::EmptyTrash(view) => view.clone().into_any_element(),
            }))
            .children(self.menu.as_ref().map(|menu| menu.view.clone()))
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

/// The context menu, and what had the keys before it, where its commands
/// run.
struct OpenMenu {
    view: Entity<ContextMenu>,
    origin: FocusHandle,
}

struct MenuRequest {
    scopes: Vec<Scope>,
    at: Point<Pixels>,
    origin: FocusHandle,
}

/// A message after a change.
struct Toast {
    text: String,
    failed: bool,
    /// Offers Undo, which reverses the newest change, whoever made it.
    undo: bool,
    _expire: Task<()>,
}

/// What a toast says once `request` has changed what `outcome` lists.
fn said(request: &Request, outcome: &Outcome) -> Option<String> {
    let asked: Vec<&Path> = outcome
        .items
        .iter()
        .map(|item| item.path.as_path())
        .collect();
    let what = match asked.as_slice() {
        [one] => jobs::quoted(one),
        many => format!("{} items", many.len()),
    };
    Some(match request {
        Request::Trash { .. } => format!("Moved {what} to the Trash"),
        Request::Duplicate { .. } => format!("Duplicated {what}"),
        Request::Rename { name, .. } => format!("Renamed to \"{name}\""),
        Request::NewFolder { path } | Request::NewFile { path } => {
            format!("Made {}", jobs::quoted(path))
        }
        Request::NewFolderWith { path, items } => {
            let what = match items.as_slice() {
                [one] => jobs::quoted(one),
                many => format!("{} items", many.len()),
            };
            format!("Moved {what} into {}", jobs::quoted(path))
        }
        Request::EmptyTrash { .. } => format!("Erased {what} for good"),
        _ => return None,
    })
}

/// `why` without the folders it names, which the person can see, and with
/// the home folder as ~.
pub(crate) fn short_reason<'a>(why: &str, folders: impl IntoIterator<Item = &'a Path>) -> String {
    let mut why = why.to_string();
    for folder in folders {
        why = why.replace(&format!("{}/", folder.display()), "");
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => why.replace(&home, "~"),
        _ => why,
    }
}

/// Why `path` couldn't take a name, short enough for the title bar:
/// "\"draft.txt\" is already taken."
fn rename_reason(why: &str, path: &Path) -> String {
    let why = short_reason(why, path.parent());
    match why.strip_suffix(" already exists.") {
        Some(name) => format!("\"{name}\" is already taken."),
        None => why,
    }
}

/// A name being edited in place.
struct Renaming {
    path: PathBuf,
    field: Entity<TextField>,
    /// Why the last name didn't take, shown in the title bar.
    error: Option<String>,
    /// Sent to the owner and not yet answered, so a blur meanwhile doesn't
    /// send it again.
    sending: bool,
    /// Clicking away commits.
    _blur: Subscription,
}

pub(crate) fn lock(ops: &Mutex<ops::Client>) -> MutexGuard<'_, ops::Client> {
    ops.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The sheet over the window, one at a time.
enum Sheet {
    Settings(Entity<SettingsView>),
    Palette(Entity<PaletteView>),
    Goto(Entity<GotoView>),
    /// The copies and moves popover; `jobs` draws it.
    Jobs,
    EmptyTrash(Entity<EmptyTrashView>),
}

/// A grid cell's name in at most two lines, as Finder sets it: the first
/// breaks where it would wrap, at a word when it can, and the rest is cut
/// in the middle, so the start and the extension both show.
fn grid_name(
    name: &str,
    width: gpui::Pixels,
    size: gpui::Pixels,
    window: &Window,
) -> Vec<gpui::Div> {
    let line = |text: &str| {
        div()
            .whitespace_nowrap()
            .overflow_hidden()
            .child(text.to_owned())
    };
    let mut wrapper = window
        .text_system()
        .line_wrapper(gpui::font(theme::UI_FONT), size);
    let wrap = wrapper
        .wrap_line(
            &[gpui::LineFragment::text(name)],
            width,
            gpui::IndentAdjustment::NoIndent,
        )
        .next();
    match wrap {
        Some(boundary) if boundary.ix < name.len() => vec![
            line(name[..boundary.ix].trim_end()),
            line(&name[boundary.ix..]).text_ellipsis_middle(),
        ],
        _ => vec![line(name)],
    }
}

/// The pane's margin and borders across, as `render` draws them: Rounded's
/// card has `mr_2` and a border each side, Boxy's pane a left border. Only
/// the grid's first frame relies on these; after that it measures.
fn pane_margins(style: WorkspaceStyle) -> f32 {
    match style {
        WorkspaceStyle::Rounded => 8. + 2.,
        WorkspaceStyle::Boxy => 1.,
    }
}

/// The grid's padding across: the list's and each row's, both sides.
const GRID_PADDING: f32 = 16.;

/// How many cells fit across a grid `width` wide, and how wide each is. The
/// room left over is shared out, as in Finder, rather than left at the right.
fn grid_layout(width: f32, icon: u32) -> (usize, f32) {
    let least = grid_cell(icon).0;
    let room = (width - GRID_PADDING).max(0.);
    let across = (room / least).floor().max(1.) as usize;
    (across, (room / across as f32).max(least))
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

/// More files than this at once ask before opening.
const OPEN_WITHOUT_ASKING: usize = 20;

/// A rectangle dragged over the listing to select what it touches. Its
/// corners are in listing coordinates, so scrolling mid-drag keeps the
/// start where it was.
struct Marquee {
    start: Point<Pixels>,
    end: Point<Pixels>,
    /// The pointer in the window, to sweep again when the listing scrolls.
    pointer: Point<Pixels>,
    /// ⌘ or ⇧ was held: the drag adds to what's selected.
    additive: bool,
    /// Past a few points of travel; a click until then.
    dragging: bool,
    /// An auto-scroll step waits on the next frame.
    scrolling: bool,
}

/// The cells `step` apart from 0, of `count`, that `from..=to` overlaps.
fn span(from: f32, to: f32, step: f32, count: usize) -> std::ops::Range<usize> {
    if to < 0. || count == 0 || step <= 0. {
        return 0..0;
    }
    let first = ((from.max(0.) / step) as usize).min(count);
    let last = ((to / step) as usize).min(count - 1);
    first..(last + 1).max(first)
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
    format_bytes(entry.size)
}

/// Finder's decimal units.
pub fn format_bytes(size: u64) -> String {
    let bytes = size as f64;
    match size {
        0..1_000 => format!("{size} bytes"),
        1_000..1_000_000 => format!("{:.0} KB", bytes / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes / 1e6),
        _ => format!("{:.2} GB", bytes / 1e9),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, VisualTestContext};

    /// A browser on `folder` with the app's keys and default preferences,
    /// active and focused on its list. Type navigation keys only: a printable key
    /// starts a search, and a search starts the whole-disk index.
    fn browse<'a>(
        folder: &Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<Browser>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Preferences::default());
            commands::apply(cx);
        });
        let folder = folder.to_path_buf();
        let (browser, cx) = cx.add_window_view(|_, cx| Browser::new(folder, cx));
        // Active, as `run` makes it: an inactive window reports no blur.
        cx.update(|window, cx| {
            window.activate_window();
            let list = browser.read(cx).list_focus.clone();
            window.focus(&list, cx);
        });
        cx.run_until_parked();
        (browser, cx)
    }

    /// The rows shown, folders inside open folders indented, and the
    /// selected row.
    fn shown(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> (Vec<String>, Vec<String>) {
        browser.read_with(cx, |browser, _| {
            let rows = browser
                .entries
                .iter()
                .zip(&browser.depths)
                .map(|(entry, depth)| format!("{}{}", "  ".repeat(*depth), entry.name))
                .collect();
            let selected = browser
                .selection
                .in_order(&browser.entries)
                .into_iter()
                .map(|entry| entry.name.clone())
                .collect();
            (rows, selected)
        })
    }

    fn press(cx: &mut VisualTestContext, keys: &str) {
        for key in keys.split(' ') {
            cx.simulate_keystrokes(key);
            cx.run_until_parked();
        }
    }

    /// `project/` holding `docs/` (with `inner.md`), `zeta/`, `a.txt` and
    /// the hidden `.env`. It sits in a temp folder, since tempfile's own
    /// folders are hidden and wouldn't list in their parent.
    fn folder() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(project.join("docs")).unwrap();
        std::fs::write(project.join("docs/inner.md"), "").unwrap();
        std::fs::create_dir(project.join("zeta")).unwrap();
        std::fs::write(project.join("a.txt"), "").unwrap();
        std::fs::write(project.join(".env"), "").unwrap();
        (dir, project)
    }

    #[gpui::test]
    fn open_goes_into_a_folder_and_back_returns_with_it_selected(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let (browser, cx) = browse(&project, cx);
        assert_eq!(
            shown(&browser, cx),
            (vec!["docs".into(), "zeta".into(), "a.txt".into()], vec![])
        );
        press(cx, "down cmd-down");
        assert_eq!(shown(&browser, cx).0, ["inner.md"]);
        press(cx, "cmd-[");
        assert_eq!(
            shown(&browser, cx),
            (
                vec!["docs".into(), "zeta".into(), "a.txt".into()],
                vec!["docs".into()]
            )
        );
    }

    #[gpui::test]
    fn arrows_walk_a_folder_open_in_place_and_out_of_it(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let (browser, cx) = browse(&project, cx);
        press(cx, "down right");
        assert_eq!(
            shown(&browser, cx),
            (
                vec![
                    "docs".into(),
                    "  inner.md".into(),
                    "zeta".into(),
                    "a.txt".into()
                ],
                vec!["docs".into()]
            )
        );
        press(cx, "right");
        assert_eq!(shown(&browser, cx).1, ["inner.md"]);
        press(cx, "left left");
        assert_eq!(
            shown(&browser, cx),
            (
                vec!["docs".into(), "zeta".into(), "a.txt".into()],
                vec!["docs".into()]
            )
        );
        // At the top, ← goes up to the enclosing folder with this one selected.
        press(cx, "left");
        assert_eq!(
            shown(&browser, cx),
            (vec!["project".into()], vec!["project".into()])
        );
    }

    /// `a.txt` to `d.txt`, each bigger than the last, so sorting by size,
    /// biggest first, reverses them.
    fn sized() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("sized");
        std::fs::create_dir(&folder).unwrap();
        for (name, size) in [("a.txt", 1), ("b.txt", 20), ("c.txt", 300), ("d.txt", 4000)] {
            std::fs::write(folder.join(name), vec![b'x'; size]).unwrap();
        }
        (dir, folder)
    }

    /// The middle of list row `ix` in the window, as the last frame laid out.
    fn row(browser: &Entity<Browser>, ix: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        browser.read_with(cx, |browser, cx| {
            let height = cx.global::<Preferences>().row_size.metrics().0;
            browser.list_bounds.get().origin
                + gpui::point(px(60.), px(4. + height * (ix as f32 + 0.5)))
        })
    }

    fn with(modifiers: &str) -> Modifiers {
        Modifiers {
            platform: modifiers.contains("cmd"),
            shift: modifiers.contains("shift"),
            ..Modifiers::default()
        }
    }

    #[gpui::test]
    fn clicks_build_a_selection_that_outlives_a_sort_and_a_refresh(cx: &mut TestAppContext) {
        let (_dir, folder) = sized();
        let (browser, cx) = browse(&folder, cx);
        let at = row(&browser, 0, cx);
        cx.simulate_click(at, with(""));
        let at = row(&browser, 2, cx);
        cx.simulate_click(at, with("shift"));
        assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt"]);
        let at = row(&browser, 1, cx);
        cx.simulate_click(at, with("cmd"));
        assert_eq!(shown(&browser, cx).1, ["a.txt", "c.txt"]);
        // Sorted by size, the same files stay selected in their new order.
        press(cx, "ctrl-alt-cmd-3");
        assert_eq!(
            shown(&browser, cx),
            (
                vec![
                    "d.txt".into(),
                    "c.txt".into(),
                    "b.txt".into(),
                    "a.txt".into()
                ],
                vec!["c.txt".into(), "a.txt".into()]
            )
        );
        // A refresh after the disk changes keeps it too.
        std::fs::write(folder.join("e.txt"), "").unwrap();
        browser.update(cx, |browser, cx| browser.list(true, cx));
        cx.run_until_parked();
        assert_eq!(shown(&browser, cx).0.len(), 5);
        assert_eq!(shown(&browser, cx).1, ["c.txt", "a.txt"]);
        assert_eq!(
            browser.read_with(cx, |browser, _| browser.selection.size()),
            301
        );
        press(cx, "escape");
        assert!(shown(&browser, cx).1.is_empty());
    }

    #[gpui::test]
    fn shift_arrows_extend_and_select_all_and_deselect_all(cx: &mut TestAppContext) {
        let (_dir, folder) = sized();
        let (browser, cx) = browse(&folder, cx);
        press(cx, "down shift-down shift-down");
        assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt"]);
        press(cx, "shift-up");
        assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt"]);
        // A plain arrow goes back to one.
        press(cx, "down");
        assert_eq!(shown(&browser, cx).1, ["c.txt"]);
        press(cx, "cmd-a");
        assert_eq!(shown(&browser, cx).1.len(), 4);
        press(cx, "cmd-alt-a");
        assert!(shown(&browser, cx).1.is_empty());
        // The cursor can sit at the end on an entry that isn't selected;
        // ↓ there still selects it.
        let (first, last) = (row(&browser, 0, cx), row(&browser, 3, cx));
        cx.simulate_click(first, with(""));
        cx.simulate_click(last, with("cmd"));
        cx.simulate_click(last, with("cmd"));
        assert_eq!(shown(&browser, cx).1, ["a.txt"]);
        press(cx, "down");
        assert_eq!(shown(&browser, cx).1, ["d.txt"]);
    }

    #[gpui::test]
    fn a_dragged_rectangle_selects_what_it_touches(cx: &mut TestAppContext) {
        let (_dir, folder) = sized();
        let (browser, cx) = browse(&folder, cx);
        let (from, to) = (row(&browser, 0, cx), row(&browser, 2, cx));
        cx.simulate_mouse_down(from, MouseButton::Left, with(""));
        cx.simulate_mouse_move(to, MouseButton::Left, with(""));
        cx.simulate_mouse_up(to, MouseButton::Left, with(""));
        assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt"]);
        // A click on empty space below the rows lets go of it.
        let below = row(&browser, 6, cx);
        cx.simulate_click(below, with(""));
        assert!(shown(&browser, cx).1.is_empty());
        // ⌘ adds a drag to what was selected.
        let last = row(&browser, 3, cx);
        cx.simulate_click(last, with(""));
        let (from, to) = (row(&browser, 0, cx), row(&browser, 1, cx));
        cx.simulate_mouse_down(from, MouseButton::Left, with("cmd"));
        cx.simulate_mouse_move(to, MouseButton::Left, with("cmd"));
        cx.simulate_mouse_up(to, MouseButton::Left, with("cmd"));
        assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "d.txt"]);
        // A drag that ends on the row it began on is a drag, not a click
        // that would toggle that row back out.
        let start = row(&browser, 2, cx);
        let end = start + gpui::point(px(80.), px(0.));
        cx.simulate_mouse_down(start, MouseButton::Left, with("cmd"));
        cx.simulate_mouse_move(end, MouseButton::Left, with("cmd"));
        cx.simulate_mouse_up(end, MouseButton::Left, with("cmd"));
        assert_eq!(shown(&browser, cx).1, ["a.txt", "b.txt", "c.txt", "d.txt"]);
        // In the grid it takes the cells it covers, not whole rows.
        press(cx, "cmd-2");
        let cell = |ix: usize, cx: &mut VisualTestContext| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            browser.read_with(cx, |browser, _| {
                let height = grid_cell(browser.grid_icon).1;
                assert!(browser.across >= 4, "the four files fit in one row");
                browser.list_bounds.get().origin
                    + gpui::point(
                        px(8. + browser.cell_width * (ix as f32 + 0.5)),
                        px(4. + height * 0.5),
                    )
            })
        };
        let (from, to) = (cell(1, cx), cell(2, cx));
        cx.simulate_mouse_down(from, MouseButton::Left, with(""));
        cx.simulate_mouse_move(to, MouseButton::Left, with(""));
        cx.simulate_mouse_up(to, MouseButton::Left, with(""));
        assert_eq!(shown(&browser, cx).1, ["b.txt", "c.txt"]);
    }

    #[gpui::test]
    fn closing_a_folder_keeps_what_was_selected_beside_it(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let (browser, cx) = browse(&project, cx);
        press(cx, "down right right");
        let a = row(&browser, 3, cx);
        cx.simulate_click(a, with("cmd"));
        assert_eq!(shown(&browser, cx).1, ["inner.md", "a.txt"]);
        browser.update(cx, |browser, cx| {
            browser.toggle_folder(project.join("docs"), cx)
        });
        assert_eq!(shown(&browser, cx).1, ["docs", "a.txt"]);
    }

    #[gpui::test]
    fn opening_many_files_asks_first(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let many = dir.path().join("many");
        std::fs::create_dir(&many).unwrap();
        for ix in 0..=OPEN_WITHOUT_ASKING {
            std::fs::write(many.join(format!("{ix:02}.txt")), "").unwrap();
        }
        let (_browser, cx) = browse(&many, cx);
        press(cx, "cmd-a cmd-o");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
    }

    #[gpui::test]
    fn a_drag_past_the_bottom_scrolls_and_keeps_selecting(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let tall = dir.path().join("tall");
        std::fs::create_dir(&tall).unwrap();
        for ix in 0..200 {
            std::fs::write(tall.join(format!("{ix:03}.txt")), "").unwrap();
        }
        let (browser, cx) = browse(&tall, cx);
        let start = row(&browser, 0, cx);
        let below = browser.read_with(cx, |browser, _| browser.list_bounds.get().bottom_left())
            + gpui::point(px(60.), px(80.));
        cx.simulate_mouse_down(start, MouseButton::Left, with(""));
        cx.simulate_mouse_move(below, MouseButton::Left, with(""));
        let first = shown(&browser, cx).1.len();
        // The test platform draws no frames on its own; deliver the ones
        // auto-scroll waits on.
        for _ in 0..20 {
            cx.update(|window, cx| window.simulate_next_frame(cx));
        }
        let scrolled = shown(&browser, cx).1.len();
        cx.simulate_mouse_up(below, MouseButton::Left, with(""));
        assert!(
            scrolled > first,
            "{first} rows, then {scrolled} after scrolling"
        );
        assert_eq!(shown(&browser, cx).1.len(), scrolled);
    }

    #[test]
    fn a_span_counts_the_cells_it_overlaps() {
        assert_eq!(span(0., 10., 28., 5), 0..1);
        assert_eq!(span(27., 29., 28., 5), 0..2);
        assert_eq!(span(-50., 500., 28., 5), 0..5);
        // Past the last cell, or above the first, it touches nothing.
        assert_eq!(span(200., 300., 28., 5), 5..5);
        assert_eq!(span(-20., -5., 28., 5), 0..0);
    }

    #[gpui::test]
    fn show_hidden_files_lists_dotfiles_and_hides_them_again(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let (browser, cx) = browse(&project, cx);
        press(cx, "cmd->");
        assert_eq!(shown(&browser, cx).0, ["docs", "zeta", ".env", "a.txt"]);
        press(cx, "cmd->");
        assert_eq!(shown(&browser, cx).0, ["docs", "zeta", "a.txt"]);
    }

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

    #[gpui::test]
    fn undo_reverses_an_agents_rename_after_that_agent_has_gone(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let state = tempfile::tempdir().unwrap();
        let paths = ops::Paths {
            state: state.path().join("state"),
            settings: state.path().join("settings.json"),
            trash: state.path().join("Trash"),
        };
        let serve = || {
            let owner = ops::Owner::bind(paths.clone()).unwrap();
            std::thread::spawn(move || owner.serve())
        };
        let (a, b) = (project.join("a.txt"), project.join("b.txt"));
        let serving = serve();
        let mut agent = ops::Client::new(paths.clone(), None);
        let rename = Request::Rename {
            path: a.clone(),
            name: "b.txt".into(),
        };
        agent
            .submit(Origin::Agent { client: None }, rename)
            .unwrap();
        // The agent leaves, and its owner with it; the window's undo
        // reaches the next owner.
        drop(agent);
        serving.join().unwrap().unwrap();
        let serving = serve();
        let client = ops::Client::new(paths.clone(), None);
        cx.update(|cx| cx.set_global(Operations(Arc::new(Mutex::new(client)))));
        let (browser, cx) = browse(&project, cx);
        press(cx, "cmd-z");
        assert!(a.exists() && !b.exists());
        press(cx, "cmd-shift-z");
        assert!(b.exists() && !a.exists());
        press(cx, "cmd-shift-z");
        assert_eq!(
            toast(&browser, cx),
            Some(("Nothing to redo.".into(), true, false))
        );
        cx.update(|_, cx| drop(cx.remove_global::<Operations>()));
        serving.join().unwrap().unwrap();
    }

    /// An operation owner on a temp state folder, with a Trash of its own,
    /// that the window's commands reach, and a private pasteboard so Copy
    /// and Paste leave the user's alone.
    struct Owned {
        state: tempfile::TempDir,
        paths: ops::Paths,
        serving: std::thread::JoinHandle<std::io::Result<()>>,
    }

    /// An owner serving `paths` on its own thread, and the window's client.
    fn serve(paths: &ops::Paths) -> (std::thread::JoinHandle<std::io::Result<()>>, Operations) {
        let owner = ops::Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let client = ops::Client::new(paths.clone(), None);
        (serving, Operations(Arc::new(Mutex::new(client))))
    }

    fn own(cx: &mut TestAppContext) -> Owned {
        let state = tempfile::tempdir().unwrap();
        let paths = ops::Paths {
            state: state.path().join("state"),
            settings: state.path().join("settings.json"),
            trash: state.path().join("Trash"),
        };
        let (serving, operations) = serve(&paths);
        cx.update(|cx| {
            cx.set_global(operations);
            let board = objc2_app_kit::NSPasteboard::pasteboardWithUniqueName();
            cx.set_global(Pasteboard(board));
        });
        Owned {
            state,
            paths,
            serving,
        }
    }

    /// Waits for the owner's jobs to end without polling: the window lets go,
    /// the owner exits once its jobs are done, and the next one reads how
    /// they ended from the journal.
    fn restart(owned: &mut Owned, cx: &mut VisualTestContext) {
        cx.update(|_, cx| drop(cx.remove_global::<Operations>()));
        let (serving, operations) = {
            let ended = std::mem::replace(&mut owned.serving, std::thread::spawn(|| Ok(())));
            ended.join().unwrap().unwrap();
            serve(&owned.paths)
        };
        owned.serving = serving;
        cx.update(|_, cx| cx.set_global(operations));
    }

    /// Lets the owner go once its jobs are done, and the pasteboard with it.
    fn release(owned: Owned, cx: &mut VisualTestContext) {
        cx.update(|_, cx| {
            drop(cx.remove_global::<Operations>());
            let board = cx.remove_global::<Pasteboard>();
            // SAFETY: a unique pasteboard this test made and no longer uses.
            let _: () = unsafe { objc2::msg_send![&*board.0, releaseGlobally] };
        });
        owned.serving.join().unwrap().unwrap();
    }

    /// The toast's text, whether it's a failure, and whether it offers Undo.
    fn toast(
        browser: &Entity<Browser>,
        cx: &mut VisualTestContext,
    ) -> Option<(String, bool, bool)> {
        browser.read_with(cx, |browser, _| {
            let toast = browser.toast.as_ref()?;
            Some((toast.text.clone(), toast.failed, toast.undo))
        })
    }

    /// Draws a frame, then clicks the middle of what `selector` marks.
    fn click(cx: &mut VisualTestContext, selector: &'static str) {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let at = cx.debug_bounds(selector).unwrap().center();
        cx.simulate_click(at, with(""));
        cx.run_until_parked();
    }

    /// The name being edited, and why the last one didn't take.
    fn editing(
        browser: &Entity<Browser>,
        cx: &mut VisualTestContext,
    ) -> Option<(String, Option<String>)> {
        browser.read_with(cx, |browser, cx| {
            let renaming = browser.renaming.as_ref()?;
            let text = renaming.field.read(cx).text().to_string();
            Some((text, renaming.error.clone()))
        })
    }

    fn retype(cx: &mut VisualTestContext, name: &str) {
        press(cx, "cmd-a");
        if name.is_empty() {
            press(cx, "backspace");
        } else {
            cx.simulate_input(name);
        }
        cx.run_until_parked();
    }

    #[gpui::test]
    fn return_renames_in_place_and_the_selection_follows(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let owned = own(cx);
        let (browser, cx) = browse(&project, cx);
        press(cx, "down down down enter");
        // The name is selected up to its extension, so typing keeps it.
        cx.simulate_input("b");
        cx.run_until_parked();
        assert_eq!(editing(&browser, cx), Some(("b.txt".into(), None)));
        // A name that can't be used says why and stays open.
        for (name, why) in [
            ("zeta", "\"zeta\" is already taken."),
            ("x/y", "can't be a name"),
            ("", "can't be a name"),
        ] {
            retype(cx, name);
            press(cx, "enter");
            let (text, error) = editing(&browser, cx).unwrap();
            assert_eq!(text, name);
            assert!(
                error
                    .as_deref()
                    .is_some_and(|e| e.contains(why) && !e.contains(&*project.to_string_lossy())),
                "{name}: {error:?}"
            );
        }
        assert!(project.join("a.txt").exists());
        retype(cx, "b.txt");
        press(cx, "enter");
        assert_eq!(editing(&browser, cx), None);
        assert!(project.join("b.txt").exists() && !project.join("a.txt").exists());
        assert_eq!(shown(&browser, cx).1, ["b.txt"]);
        // Escape leaves the name as it was.
        press(cx, "enter");
        retype(cx, "gone.txt");
        press(cx, "escape");
        assert_eq!(editing(&browser, cx), None);
        assert_eq!(
            shown(&browser, cx),
            (
                vec!["docs".into(), "zeta".into(), "b.txt".into()],
                vec!["b.txt".into()]
            )
        );
        // Clicking away keeps the new name.
        press(cx, "enter");
        retype(cx, "c.txt");
        let docs = row(&browser, 0, cx);
        cx.simulate_click(docs, with(""));
        assert_eq!(editing(&browser, cx), None);
        assert!(project.join("c.txt").exists());
        assert_eq!(shown(&browser, cx).1, ["docs"]);
        release(owned, cx);
    }

    #[gpui::test]
    fn new_folder_counts_on_from_untitled_and_starts_its_name(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let owned = own(cx);
        let (browser, cx) = browse(&project, cx);
        press(cx, "cmd-shift-n");
        assert!(project.join("untitled folder").is_dir());
        assert_eq!(
            editing(&browser, cx),
            Some(("untitled folder".into(), None))
        );
        assert_eq!(shown(&browser, cx).1, ["untitled folder"]);
        press(cx, "escape");
        press(cx, "cmd-shift-n");
        assert_eq!(
            editing(&browser, cx),
            Some(("untitled folder 2".into(), None))
        );
        cx.simulate_input("notes");
        press(cx, "enter");
        assert!(project.join("notes").is_dir() && !project.join("untitled folder 2").exists());
        assert_eq!(shown(&browser, cx).1, ["notes"]);
        cx.dispatch_action(NewFile);
        cx.run_until_parked();
        assert!(project.join("untitled").is_file());
        assert_eq!(editing(&browser, cx), Some(("untitled".into(), None)));
        press(cx, "escape");
        // With the selection inside, and one undo to take it out again.
        let (folder, file) = (project.join("New Folder With Items"), project.join("a.txt"));
        let a = row(&browser, 4, cx);
        cx.simulate_click(a, with(""));
        assert_eq!(shown(&browser, cx).1, ["a.txt"]);
        press(cx, "ctrl-cmd-n");
        assert!(folder.join("a.txt").exists() && !file.exists());
        assert_eq!(
            editing(&browser, cx),
            Some(("New Folder With Items".into(), None))
        );
        press(cx, "escape");
        // The folder waits in the user's own Trash until redo takes it back.
        press(cx, "cmd-z");
        assert!(file.exists() && !folder.exists());
        press(cx, "cmd-shift-z");
        assert!(folder.join("a.txt").exists() && !file.exists());
        release(owned, cx);
    }

    #[gpui::test]
    fn duplicate_selects_the_copy_and_trash_undoes(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let owned = own(cx);
        let (browser, cx) = browse(&project, cx);
        press(cx, "down down down cmd-d");
        assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
        assert!(project.join("a.txt").exists());
        // This goes to the user's own Trash, and comes back out before the
        // test ends.
        press(cx, "cmd-backspace");
        assert!(!project.join("a copy.txt").exists());
        assert_eq!(
            shown(&browser, cx),
            (vec!["docs".into(), "zeta".into(), "a.txt".into()], vec![])
        );
        assert_eq!(
            toast(&browser, cx),
            Some(("Moved \"a copy.txt\" to the Trash".into(), false, true))
        );
        // The toast's Undo is the shared one.
        click(cx, "toast-undo");
        assert!(project.join("a copy.txt").exists());
        assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
        assert_eq!(toast(&browser, cx), None);
        release(owned, cx);
    }

    #[gpui::test]
    fn copy_and_paste_carry_files_through_the_pasteboard(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let owned = own(cx);
        let (browser, cx) = browse(&project, cx);
        press(cx, "down down down cmd-c");
        let on_board = cx.update(|_, cx| cx.global::<Pasteboard>().files());
        assert_eq!(on_board, [project.join("a.txt")]);
        // Pasted where it is, it's duplicated.
        press(cx, "cmd-v");
        assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
        assert_eq!(
            toast(&browser, cx),
            Some(("Duplicated \"a.txt\"".into(), false, true))
        );
        // Elsewhere it's a job, here a move. When it ends is the owner
        // thread's business; the next test waits for it.
        press(cx, "up up cmd-down cmd-alt-v");
        assert!(toast(&browser, cx).is_some_and(|(_, failed, _)| !failed));
        release(owned, cx);
        assert!(project.join("docs/a.txt").exists() && !project.join("a.txt").exists());
    }

    #[gpui::test]
    fn a_paste_that_meets_a_taken_name_asks_and_keep_both_finishes_the_same_job(
        cx: &mut TestAppContext,
    ) {
        let (_dir, project) = folder();
        let docs = project.join("docs");
        std::fs::write(docs.join("a.txt"), "theirs").unwrap();
        let mut owned = own(cx);
        let (browser, cx) = browse(&project, cx);
        press(cx, "down down down cmd-c up up cmd-down cmd-v");
        restart(&mut owned, cx);
        cx.executor().advance_clock(jobs::CHECK);
        cx.run_until_parked();
        let (popover, ring) = (
            |cx: &mut VisualTestContext| {
                browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::Jobs)))
            },
            |cx: &mut VisualTestContext| {
                browser.read_with(cx, |b, cx| b.jobs.read(cx).ring().is_some())
            },
        );
        // The person just pasted, so the question opens by itself.
        assert!(popover(cx) && ring(cx));
        click(cx, "job-Keep Both");
        restart(&mut owned, cx);
        cx.executor().advance_clock(jobs::CHECK);
        cx.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(docs.join("a.txt")).unwrap(),
            "theirs"
        );
        assert!(docs.join("a 2.txt").exists());
        assert_eq!(shown(&browser, cx).1, ["a 2.txt"]);
        assert_eq!(
            toast(&browser, cx),
            Some(("Copied \"a.txt\" to docs".into(), false, true))
        );
        // One job, answered in place.
        let jobs = ops::Client::new(owned.paths.clone(), None).jobs().unwrap();
        let ended: Vec<_> = jobs.iter().map(|job| (job.id, job.state)).collect();
        assert_eq!(ended, [(1, ops::State::Completed)]);
        // It leaves the ring a few seconds after it finished.
        press(cx, "escape");
        assert!(!popover(cx) && ring(cx));
        cx.executor().advance_clock(jobs::LINGER);
        assert!(!ring(cx));
        release(owned, cx);
    }

    /// The context menu's rows and the one highlighted, while it's open.
    fn menu(
        browser: &Entity<Browser>,
        cx: &mut VisualTestContext,
    ) -> Option<(Vec<&'static str>, Option<&'static str>)> {
        browser.read_with(cx, |browser, cx| {
            let menu = browser.menu.as_ref()?.view.read(cx);
            let titles: Vec<_> = menu.rows.iter().map(|row| row.title).collect();
            let active = menu.active.map(|ix| titles[ix]);
            Some((titles, active))
        })
    }

    /// As a pointer does it: it arrives, then presses.
    fn right_click(cx: &mut VisualTestContext, at: Point<Pixels>) {
        cx.simulate_mouse_move(at, None, with(""));
        cx.simulate_mouse_down(at, MouseButton::Right, with(""));
        cx.simulate_mouse_move(at, MouseButton::Right, with(""));
        cx.simulate_mouse_up(at, MouseButton::Right, with(""));
    }

    const ON_A_FILE: [&str; 8] = [
        "Open",
        "Reveal in Finder",
        "Open in Terminal",
        "Rename",
        "Duplicate",
        "Move to Trash",
        "Copy",
        "Copy Path",
    ];

    #[gpui::test]
    fn a_right_click_selects_first_and_the_menu_offers_what_applies(cx: &mut TestAppContext) {
        let (_dir, folder) = sized();
        let (browser, cx) = browse(&folder, cx);
        let rows: Vec<_> = [0, 1, 2, 3, 6].map(|ix| row(&browser, ix, cx)).into();
        cx.simulate_click(rows[0], with(""));
        // Outside the selection, the entry clicked becomes the selection.
        right_click(cx, rows[2]);
        assert_eq!(shown(&browser, cx).1, ["c.txt"]);
        assert_eq!(menu(&browser, cx), Some((ON_A_FILE.to_vec(), None)));
        // While it's open, the window's keys can't move what it acts on.
        press(cx, "alt-up cmd-[ cmd-alt-a");
        assert_eq!(shown(&browser, cx).1, ["c.txt"]);
        assert!(menu(&browser, cx).is_some());
        // Esc closes it, and leaves the selection as it was.
        press(cx, "escape");
        assert_eq!(menu(&browser, cx), None);
        assert_eq!(shown(&browser, cx).1, ["c.txt"]);
        // Inside the selection it keeps it, and the menu has what runs on
        // several: New Folder with Selection, and no Rename.
        cx.simulate_click(rows[1], with("cmd"));
        right_click(cx, rows[1]);
        assert_eq!(shown(&browser, cx).1, ["b.txt", "c.txt"]);
        let (titles, _) = menu(&browser, cx).unwrap();
        assert_eq!(
            titles[2..5],
            ["Open in Terminal", "New Folder with Selection", "Duplicate"]
        );
        // A press outside closes it and reaches nothing behind it. The menu
        // opened rightward from the pointer, so this is left of it.
        let outside = rows[3] - gpui::point(px(50.), px(0.));
        cx.simulate_click(outside, with(""));
        assert_eq!(menu(&browser, cx), None);
        assert_eq!(shown(&browser, cx).1, ["b.txt", "c.txt"]);
        // Empty space lets go of the selection for the folder's menu.
        right_click(cx, rows[4]);
        assert!(shown(&browser, cx).1.is_empty());
        let folder_menu = vec![
            "New Folder",
            "New File",
            "Paste",
            "as List",
            "as Grid",
            "Show Hidden Files",
            "Sort by Name",
            "Sort by Date Modified",
            "Sort by Size",
            "Sort by Kind",
        ];
        assert_eq!(menu(&browser, cx), Some((folder_menu, None)));
        press(cx, "escape");
    }

    #[gpui::test]
    fn in_the_grid_a_right_click_opens_an_items_menu(cx: &mut TestAppContext) {
        let (_dir, folder) = sized();
        let (browser, cx) = browse(&folder, cx);
        press(cx, "cmd-2");
        let cell = |ix: usize, cx: &mut VisualTestContext| {
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            browser.read_with(cx, |browser, _| {
                let height = grid_cell(browser.grid_icon).1;
                browser.list_bounds.get().origin
                    + gpui::point(
                        px(8. + browser.cell_width * (ix as f32 + 0.5)),
                        px(4. + height * 0.5),
                    )
            })
        };
        let at = cell(2, cx);
        right_click(cx, at);
        assert_eq!(shown(&browser, cx).1, ["c.txt"]);
        assert_eq!(menu(&browser, cx), Some((ON_A_FILE.to_vec(), None)));
        press(cx, "escape");
    }

    #[gpui::test]
    fn shift_f10_opens_the_menu_on_the_selection_and_return_runs_a_row(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let owned = own(cx);
        let (browser, cx) = browse(&project, cx);
        press(cx, "down down down shift-f10");
        assert_eq!(menu(&browser, cx), Some((ON_A_FILE.to_vec(), None)));
        // ↑ and ↓ come round from the ends.
        press(cx, "up");
        assert_eq!(menu(&browser, cx).unwrap().1, Some("Copy Path"));
        press(cx, "down down down down down");
        assert_eq!(menu(&browser, cx).unwrap().1, Some("Duplicate"));
        press(cx, "enter");
        assert_eq!(menu(&browser, cx), None);
        assert!(project.join("a copy.txt").exists());
        assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
        // Only a package has Show Package Contents, in the menu bar too, and
        // it goes inside.
        std::fs::create_dir(project.join("Kit.app")).unwrap();
        browser.update(cx, |browser, cx| browser.list(true, cx));
        cx.run_until_parked();
        let offered = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| window.is_action_available(&ShowPackageContents, cx))
        };
        for (keys, name) in [("alt-up", "docs"), ("alt-down", "a.txt")] {
            press(cx, keys);
            assert_eq!(shown(&browser, cx).1, [name]);
            assert!(!offered(cx), "{name}");
        }
        press(cx, "alt-up down");
        assert_eq!(shown(&browser, cx).1, ["Kit.app"]);
        assert!(offered(cx));
        press(cx, "shift-f10 down down");
        assert_eq!(menu(&browser, cx).unwrap().1, Some("Show Package Contents"));
        press(cx, "enter");
        let inside = browser.read_with(cx, |browser, _| browser.folder().to_path_buf());
        assert_eq!(inside, project.join("Kit.app"));
        release(owned, cx);
    }

    #[gpui::test]
    fn empty_trash_says_how_many_and_erases_only_once_confirmed(cx: &mut TestAppContext) {
        let (_dir, project) = folder();
        let owned = own(cx);
        let trash = owned.state.path().join("Trash");
        std::fs::create_dir(&trash).unwrap();
        for name in ["old.txt", "older.txt"] {
            std::fs::write(trash.join(name), "").unwrap();
        }
        let (browser, cx) = browse(&project, cx);
        let asking = |cx: &mut VisualTestContext| {
            browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::EmptyTrash(_))))
        };
        // Esc and Cancel keep everything.
        press(cx, "cmd-shift-backspace");
        assert!(asking(cx));
        press(cx, "escape");
        assert!(!asking(cx));
        press(cx, "cmd-shift-backspace");
        click(cx, "empty-trash-cancel");
        assert!(!asking(cx));
        assert_eq!(ops::trash_items(&trash).unwrap().len(), 2);
        // ↩ empties, as Finder's default button does.
        press(cx, "cmd-shift-backspace enter");
        assert!(!asking(cx));
        assert!(ops::trash_items(&trash).unwrap().is_empty());
        press(cx, "cmd-shift-backspace");
        assert!(!asking(cx));
        assert_eq!(
            toast(&browser, cx),
            Some(("The Trash is empty.".into(), false, false))
        );
        release(owned, cx);
    }
}
