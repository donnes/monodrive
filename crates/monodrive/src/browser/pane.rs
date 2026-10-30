//! One pane: a location with its history, listing, selection, view and sort,
//! drawn as a card of files with its own path bar. A tab holds one or two.
//! What's window-wide (toasts, sheets, the jobs ring, the sidebar) belongs to
//! `Browser`, which a pane reaches through `PaneEvent`.

use super::drag::{Drag, Spot, Target, offers};
use super::{
    Back, CancelRename, ClearRecents, CommitRename, CopyPath, DeselectAll, Duplicate, Eject,
    EnclosingFolder, ExpandFolder, ExtendLeft, ExtendNext, ExtendPrevious, ExtendRight,
    FocusSearch, Forward, GoApplications, GoComputer, GoDesktop, GoDocuments, GoDownloads, GoHome,
    GoRecents, GoToFolder, GoTrash, MoveHere, MoveToTrash, NewFile, NewFolder,
    NewFolderWithSelection, OpenInTerminal, OpenSelected, PageDown, PageUp, Places, PutBack, Redo,
    Rename, RevealInFinder, ReverseSort, ScrollToBottom, ScrollToTop, SelectFirst, SelectLast,
    SelectNext, SelectPrevious, ShowContextMenu, ShowPackageContents, SortByKind, SortByModified,
    SortByName, SortBySize, Undo, ViewAsGrid, ViewAsList, home, short_reason, sidebar, trash,
    with_owner,
};
use crate::background::{Backdrop, Look};
use crate::commands::Scope;
use crate::file_icons;
use crate::jobs;
use crate::onboarding::IndexGate;
use crate::pasteboard::Pasteboard;
use crate::preferences::{self, Preferences, View, WorkspaceStyle};
use crate::selection::Selection;
use crate::session::{PaneState, StopState};
use crate::text_field::{self, TextField};
use crate::theme::{self, ButtonLook, accent, content, danger, icon, on_accent, text_button};
use crate::thumbnails::{self, Thumb};
use crate::workspace;
use chrono::{DateTime, Local};
use gpui::{
    App, Bounds, ClickEvent, ClipboardItem, Context, DispatchPhase, Div, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, KeyDownEvent, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, RenderImage, ScrollStrategy,
    ScrollWheelEvent, Subscription, Task, UniformListScrollHandle, Window, canvas, div, point,
    prelude::*, px, uniform_list,
};
use monodrive_core::entries::{self, Entry, Kind, Sort, SortBy};
use monodrive_core::index::Index;
use monodrive_core::info;
use monodrive_core::ops::{self, Conflict, Origin, Outcome, Request};
use monodrive_core::places::{self, Availability, Favorite};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const SEARCH_LIMIT: usize = 200;
/// How long the disk must settle before a changed folder lists again.
pub(super) const REFRESH_DELAY: Duration = Duration::from_millis(150);
/// What stands for the path levels that don't fit.
const COLLAPSED: &str = "…";
/// A crumb's side padding.
const CRUMB_PADDING: f32 = 2. * theme::CRUMB_INSET;
/// The gap on each side of a crumb's chevron.
const CRUMB_GAP: f32 = 1.;
/// A crumb's chevron and the gaps either side of it.
const CRUMB_CHEVRON: f32 = 12. + 2. * CRUMB_GAP;
/// The path bar's side padding and a point of slack.
const PATH_BAR_CHROME: f32 = theme::PANE_INSET * 2. - theme::CRUMB_INSET + 2.;

/// What a pane asks of its window.
pub(super) enum PaneEvent {
    Toast {
        text: String,
        failed: bool,
        undo: Option<u64>,
    },
    ClearToast,
    /// Follow this job of the pane's until it ends.
    Follow(u64),
    /// A change may have moved bytes between volumes.
    VolumesChanged,
    /// The context menu on the listing, at this point in the window.
    Menu {
        scopes: Vec<Scope>,
        at: Point<Pixels>,
    },
    Eject(PathBuf),
    /// What should be there on the next launch changed.
    Moved,
}

/// Where a pane was before a drag opened folders in it.
pub(super) struct SpringReturn {
    history: Vec<Stop>,
    at: usize,
    expanded: HashSet<PathBuf>,
    selection: Selection,
    query: String,
}

/// Why a restored pane shows no files.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Unavailable {
    /// Its folder or drive is gone, in the words of `Favorite::resolve`.
    Gone(String),
    /// Opening it could show a privacy prompt, so a person has to ask.
    NeedsConsent,
}

/// Why a listing has no rows.
enum Failed {
    Error(String),
    Unavailable(Unavailable),
}

impl From<String> for Failed {
    fn from(why: String) -> Self {
        Self::Error(why)
    }
}

/// What the window tells a pane each frame, before the pane draws.
#[derive(Clone, Default)]
pub(super) struct Chrome {
    /// The pane's width inside its borders.
    pub(super) width: f32,
    /// Commands act on this pane.
    pub(super) focused: bool,
    /// A sheet or the context menu is over the window.
    pub(super) covered: bool,
    /// The context menu is open on this pane's list.
    pub(super) menu_here: bool,
    /// The free space on the folder's volume, as the path bar says it.
    pub(super) space: Option<String>,
    /// The volume to eject for the folder shown, if any.
    pub(super) ejectable: Option<PathBuf>,
}

/// Where a new pane starts.
pub(super) struct Opening {
    pub(super) history: Vec<Stop>,
    pub(super) at: usize,
    pub(super) view: View,
    pub(super) sort: Sort,
}

impl Opening {
    pub(super) fn restored(state: PaneState) -> Self {
        Self {
            history: state.history.into_iter().map(Stop::restored).collect(),
            at: state.at,
            view: state.view,
            sort: state.sort,
        }
    }

    pub(super) fn at(stop: Stop, view: View, sort: Sort) -> Self {
        Self {
            history: vec![stop],
            at: 0,
            view,
            sort,
        }
    }
}

pub struct Pane {
    /// Its tab is on screen. A hidden pane lists nothing, and lists again
    /// when it's shown.
    shown: bool,
    /// The file list: where focus starts and returns.
    pub(super) list_focus: FocusHandle,
    /// The folder just left, to select once its parent lists.
    reselect: Option<PathBuf>,
    pub(super) search: Entity<TextField>,
    /// Started with the window; searches wait on it.
    index: Arc<OnceLock<Result<Index, String>>>,
    pub(super) history: Vec<Stop>,
    /// Position in `history` of what's on screen.
    pub(super) at: usize,
    /// The rows on screen: the folder's entries with the contents of each
    /// expanded folder under it, or the search results while `query` is set.
    pub(super) entries: Vec<Entry>,
    /// Each row's depth in the tree; 0 for the folder's own entries.
    pub(super) depths: Vec<usize>,
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
    pub(super) listing: bool,
    /// What on disk the listing follows.
    watch: Watch,
    /// List or grid, and the grid's icon size, as the preferences last said.
    pub(super) view: View,
    pub(super) grid_icon: u32,
    /// How far the pane's width said by the window was from the grid's as
    /// last laid out.
    grid_inset: Rc<Cell<f32>>,
    /// How many cells fit across and how wide each is, settled once per
    /// frame so the row count, the cells and scrolling agree.
    pub(super) across: usize,
    pub(super) cell_width: f32,
    /// Scroll to the selection once the layout settles (after a view switch).
    reveal_pending: bool,
    /// Every thumbnail asked for and not yet given back, loaded or not.
    pub(super) thumbs: HashMap<Thumb, Option<Arc<RenderImage>>>,
    /// The thumbnails the last frame drew; the rest are given back.
    drawn: HashSet<Thumb>,
    /// Changes that arrived while a listing was on its way, checked against
    /// what that listing watches once it lands.
    held: Vec<(PathBuf, bool)>,
    /// A refresh waiting out the first change of a burst; later changes fold
    /// into it, so a folder that never stops changing still refreshes.
    refresh: Option<Task<()>>,
    pub(super) error: Option<String>,
    /// A restored location that can't be opened as it is, and why.
    pub(super) unavailable: Option<Unavailable>,
    /// A person asked to open the folder that needs consent.
    consented: bool,
    /// The name being edited in place.
    pub(super) renaming: Option<Renaming>,
    /// What's selected stays when the next listing lands, as it was put back.
    pub(super) keep_selection: bool,
    /// What a change made, to select once the listing that has it lands.
    select_after: Vec<PathBuf>,
    /// The trimmed search text; empty while showing the folder.
    pub(super) query: String,
    pub(super) selection: Selection,
    scroll: UniformListScrollHandle,
    /// The listing or search in flight. Replacing it drops the stale one.
    loading: Task<()>,
    /// Whether the listing on screen includes hidden files, and its order.
    show_hidden: bool,
    /// This folder shows its hidden files anyway, because Go to Folder was
    /// asked for one of them. Cleared on leaving the folder.
    peek_hidden: bool,
    pub(super) sort: Sort,
    /// A rectangle being dragged over the listing to select.
    marquee: Option<Marquee>,
    /// A rectangle was just dragged. GPUI still reports a click when it
    /// ends on the entry it began on; this says it was a drag.
    swept: bool,
    /// Where the listing was laid out last frame, in the window.
    pub(super) list_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Whether this lone selected entry is a package, as last worked out.
    package: Option<(PathBuf, bool)>,
    /// Whether this lone selected entry is a folder, as last worked out.
    lone_folder: Option<(PathBuf, bool)>,
    /// What the Trash's bar shows while the Trash is on screen.
    pub(super) trash_header: trash::TrashHeader,
    /// Rows in Recents whose files are gone.
    pub(super) missing: HashSet<PathBuf>,
    backdrop: Entity<Backdrop>,
    pub(super) chrome: Chrome,
    /// The window's drag of files.
    pub(super) drag: Drag,
    /// This press landed on an item, so what it starts is a drag of the item
    /// and not a rectangle.
    pub(super) grabbed: bool,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Pane {
    pub(super) fn new(
        opening: Opening,
        index: Arc<OnceLock<Result<Index, String>>>,
        drag: Drag,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| TextField::new("Search", "Search", cx));
        cx.subscribe(&search, |this, search, _: &text_field::Changed, cx| {
            let query = search.read(cx).text().trim().to_string();
            if query != this.query {
                this.query = query;
                this.reload(cx);
            }
        })
        .detach();
        // Cleanup otherwise runs while drawing, and a closed tab, pane or window
        // doesn't draw again. GPUI keeps a loaded asset until it's removed.
        cx.on_release(|this, cx| {
            for (thumb, image) in this.thumbs.drain() {
                thumbnails::release(&thumb, image, None, cx);
            }
        })
        .detach();
        // Whatever moves the selection repaints, so the Trash's bar follows it.
        cx.observe_self(|this, cx| this.follow_trash_selection(cx))
            .detach();
        let prefs = cx.global::<Preferences>();
        let (grid_icon, show_hidden) = (prefs.grid_icon, prefs.show_hidden);
        let Opening {
            history,
            at,
            view,
            sort,
        } = opening;
        Self {
            shown: false,
            list_focus: cx.focus_handle().tab_stop(true),
            reselect: None,
            search,
            index,
            history,
            at,
            entries: Vec::new(),
            depths: Vec::new(),
            root: Vec::new(),
            expanded: HashSet::new(),
            children: HashMap::new(),
            expanding: HashMap::new(),
            listing: false,
            watch: Watch::default(),
            view,
            grid_icon,
            grid_inset: Rc::default(),
            across: 1,
            cell_width: grid_cell(grid_icon).0,
            reveal_pending: false,
            thumbs: HashMap::new(),
            drawn: HashSet::new(),
            held: Vec::new(),
            refresh: None,
            error: None,
            unavailable: None,
            consented: false,
            renaming: None,
            keep_selection: false,
            select_after: Vec::new(),
            query: String::new(),
            selection: Selection::default(),
            scroll: UniformListScrollHandle::new(),
            loading: Task::ready(()),
            show_hidden,
            sort,
            peek_hidden: false,
            marquee: None,
            swept: false,
            list_bounds: Rc::default(),
            package: None,
            lone_folder: None,
            trash_header: trash::TrashHeader::default(),
            missing: HashSet::new(),
            backdrop: cx.new(|_| Backdrop::default()),
            chrome: Chrome::default(),
            drag,
            grabbed: false,
        }
    }

    /// Whether its tab is on screen.
    pub(super) fn set_shown(&mut self, shown: bool) {
        self.shown = shown;
        if !shown {
            self.refresh = None;
            self.held.clear();
            self.marquee = None;
        }
    }

    /// The keys may come here: what answers late mustn't take them from another
    /// tab or pane, or from under a sheet.
    fn has_the_window(&self) -> bool {
        self.shown && self.chrome.focused && !self.chrome.covered
    }

    /// Where the keys go on arriving at this pane: a name left being edited,
    /// or the list.
    pub(super) fn take_keys(&self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.renaming {
            Some(renaming) => window.focus(&renaming.field.focus_handle(cx), cx),
            None => self.focus_list(window, cx),
        }
    }

    /// What's on screen, as a place in the history.
    pub(super) fn stop(&self) -> &Stop {
        &self.history[self.at]
    }

    /// New files can land here: it's a real folder, listed.
    pub(super) fn takes_files(&self, cx: &App) -> bool {
        self.showing_folder() && !self.in_trash(cx) && self.unavailable.is_none()
    }

    /// It shows one folder that's been checked and can be read about.
    pub(super) fn shows_a_real_folder(&self) -> bool {
        self.showing_folder() && self.unavailable.is_none() && !self.unchecked()
    }

    /// What's on screen came from a saved session and hasn't been checked.
    pub(super) fn unchecked(&self) -> bool {
        self.stop().unverified
    }

    pub(super) fn waits_for_a_drive(&self) -> bool {
        matches!(self.unavailable, Some(Unavailable::Gone(_)))
    }

    /// The folders of the history's stops not yet known by volume, to resolve
    /// off the UI thread.
    pub(super) fn unplaced(&self) -> Vec<PathBuf> {
        self.history
            .iter()
            .filter(|stop| stop.place.is_none())
            .map(|stop| stop.folder.clone())
            .collect()
    }

    /// Keeps each stop with the volume its folder was first saved on.
    pub(super) fn place_stops(&mut self, placed: &HashMap<PathBuf, Favorite>) {
        for stop in &mut self.history {
            if stop.place.is_none() {
                stop.place = placed.get(&stop.folder).cloned();
            }
        }
    }

    /// What should come back on the next launch, once `place_stops` knows every
    /// stop: a path alone doesn't say which drive it was on.
    pub(super) fn state(&self) -> Option<PaneState> {
        let history = self
            .history
            .iter()
            .map(|stop| {
                Some(StopState {
                    place: stop.place.clone()?,
                    recents: stop.recents,
                })
            })
            .collect::<Option<_>>()?;
        Some(PaneState {
            history,
            at: self.at,
            view: self.view,
            sort: self.sort,
        })
    }

    /// Preferences changed: lists again if the hidden files did. With `adopt`,
    /// Settings chose them for this pane, so it takes their view and sort.
    pub(super) fn preferences_changed(&mut self, adopt: bool, cx: &mut Context<Self>) {
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

    fn set_view(&mut self, view: View) {
        if view != self.view {
            self.view = view;
            self.rebuild();
            // The other layout put it somewhere else; the grid shows it
            // once it knows how many fit across.
            self.reveal_pending = self.selection.cursor().is_some();
        }
    }

    /// ⌘1 and ⌘2: this pane's view, and where the next tab starts.
    fn choose_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.set_view(view);
        preferences::update(cx, |p| p.view = view);
        cx.emit(PaneEvent::Moved);
        cx.notify();
    }

    /// A Sort by command or a column title: this pane's order, and where the
    /// next tab starts.
    fn choose_sort(&mut self, sort: Sort, cx: &mut Context<Self>) {
        if sort != self.sort {
            self.sort = sort;
            preferences::update(cx, |p| p.sort = sort);
            cx.emit(PaneEvent::Moved);
            if self.query.is_empty() {
                self.reload(cx);
            }
        }
    }

    /// A Sort by command picks a column in its natural direction; choosing the
    /// sort in use again leaves it, as a checked menu item should. Reverse
    /// Sort Order flips it.
    fn sort_by(&mut self, by: SortBy, cx: &mut Context<Self>) {
        if self.sort.by != by {
            self.choose_sort(sorted_by(self.sort, by), cx);
        }
    }

    fn tidy_folders(&self, cx: &App) -> Vec<PathBuf> {
        cx.try_global::<Places>()
            .map_or_else(Vec::new, Places::tidy_folders)
    }

    /// What's selected once the next listing lands, if it shows any of it.
    pub(super) fn select_on_listing(&mut self, made: Vec<PathBuf>) {
        self.select_after = made;
    }

    /// What the top bar calls what's on screen.
    pub(super) fn title(&self, cx: &App) -> String {
        if !self.query.is_empty() {
            "Search results".to_string()
        } else if self.showing_recents() {
            "Recents".to_string()
        } else if self.in_trash(cx) {
            "Trash".to_string()
        } else {
            level_name(self.folder())
        }
    }

    /// Says `text` in the window's toast.
    pub(super) fn toast(
        &mut self,
        text: String,
        failed: bool,
        undo: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(PaneEvent::Toast { text, failed, undo });
    }

    pub(super) fn folder(&self) -> &Path {
        &self.history[self.at].folder
    }

    pub(super) fn navigate(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.reselect = Some(self.folder().to_path_buf());
        self.collapse_all();
        self.history.truncate(self.at + 1);
        self.history.push(Stop::folder(folder));
        self.at = self.history.len() - 1;
        self.show_folder(cx);
    }

    /// What to come back to if a drag that opens folders here is cancelled.
    pub(super) fn before_spring(&self) -> SpringReturn {
        SpringReturn {
            history: self.history.clone(),
            at: self.at,
            expanded: self.expanded.clone(),
            selection: self.selection.clone(),
            query: self.query.clone(),
        }
    }

    /// Back to `to`, as if the drag had opened nothing.
    pub(super) fn spring_back(&mut self, to: SpringReturn, cx: &mut Context<Self>) {
        self.collapse_all();
        self.reselect = None;
        self.select_after.clear();
        self.history = to.history;
        self.at = to.at;
        self.expanded = to.expanded;
        self.selection = to.selection;
        self.keep_selection = true;
        self.query = to.query;
        let (query, end) = (self.query.clone(), self.query.len());
        self.search
            .update(cx, |field, cx| field.set_text(&query, end..end, cx));
        cx.emit(PaneEvent::Moved);
        self.reload(cx);
    }

    pub(super) fn step_history(&mut self, step: isize, cx: &mut Context<Self>) {
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
    pub(super) fn show_folder(&mut self, cx: &mut Context<Self>) {
        cx.emit(PaneEvent::Moved);
        self.query.clear();
        self.search.update(cx, |field, cx| field.clear(cx));
        self.reload(cx);
    }

    pub(super) fn collapse_all(&mut self) {
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
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.list(false, cx);
    }

    /// `reload`, but `quiet` keeps the view where it is: a refresh after the
    /// disk changed mustn't scroll.
    pub(super) fn list(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        let mut folder = self.folder().to_path_buf();
        let query = self.query.clone();
        let index = self.index.clone();
        let gate = cx.try_global::<IndexGate>().cloned();
        let recents = self
            .showing_recents()
            .then(|| cx.try_global::<Places>().map(|places| places.store.clone()));
        // A restored place is checked once, before anything opens it.
        let check = self
            .stop()
            .place
            .clone()
            .filter(|_| self.stop().unverified && !self.showing_recents() && query.is_empty());
        let drives = cx
            .try_global::<Places>()
            .map(|places| places.drives.clone());
        let consented = std::mem::take(&mut self.consented);
        let show_hidden = cx.global::<Preferences>().show_hidden;
        self.show_hidden = show_hidden;
        let sort = self.sort;
        let tidy = self.tidy_folders(cx);
        let peek = self.peek_hidden;
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
                let started = match &gate {
                    Some(gate) => gate.for_search(&index),
                    None => index
                        .get_or_init(Index::start)
                        .as_ref()
                        .map_err(Clone::clone),
                };
                let hits = started.and_then(|index| index.find(&query, SEARCH_LIMIT));
                return hits
                    .map(|hits| (hits, HashMap::new(), Watch::default(), HashSet::new(), None))
                    .map_err(Failed::Error);
            }
            if let Some(store) = recents {
                let (rows, missing) = trash::recent_entries(trash::read_recents(store.as_ref())?);
                return Ok((rows, HashMap::new(), Watch::default(), missing, None));
            }
            let mut verified = None;
            if let Some(place) = check {
                let volumes = match (&place.volume, &drives) {
                    (Some(_), Some(drives)) => drives.list(),
                    _ => Vec::new(),
                };
                let there = place
                    .located(&volumes)
                    .map_err(|why| Failed::Unavailable(Unavailable::Gone(why)))?;
                // Before anything touches it: even a stat through a link could
                // reach a protected folder.
                let gated = drives.as_ref().is_some_and(|d| d.needs_consent(&there));
                if gated && !consented {
                    return Err(Failed::Unavailable(Unavailable::NeedsConsent));
                }
                match places::folder_at(there) {
                    Availability::Unavailable(why) => {
                        return Err(Failed::Unavailable(Unavailable::Gone(why)));
                    }
                    Availability::Available(there) => folder = there,
                }
                verified = Some(folder.clone());
            }
            let hidden = |dir: &Path| sidebar::lists_hidden(dir, show_hidden || peek, &tidy);
            let root = entries::list_dir(&folder, hidden(&folder), sort)
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
                    let list = entries::list_dir(&dir, hidden(&dir), sort).unwrap_or_default();
                    (dir, list)
                })
                .collect();
            Ok((root, children, watch, HashSet::new(), verified))
        });
        self.loading = cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.listing = false;
                match result {
                    Ok((root, children, watch, missing, verified)) => {
                        this.root = root;
                        this.children = children;
                        this.watch = watch;
                        this.missing = missing;
                        this.error = None;
                        this.unavailable = None;
                        if let Some(there) = verified {
                            let stop = &mut this.history[this.at];
                            stop.folder = there;
                            stop.unverified = false;
                        }
                    }
                    Err(failed) => {
                        this.root.clear();
                        this.children.clear();
                        this.missing.clear();
                        this.watch = Watch::default();
                        match failed {
                            Failed::Error(why) => {
                                this.error = Some(why);
                                this.unavailable = None;
                            }
                            Failed::Unavailable(why) => {
                                this.error = None;
                                this.unavailable = Some(why);
                            }
                        }
                    }
                }
                this.relayout(!quiet);
                this.recheck_held(cx);
                let left = this.reselect.take();
                this.pick_selection(left);
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
                this.refresh_trash(cx);
                cx.notify();
            });
        });
    }

    /// What a listing that's just landed selects. Search selects its best hit
    /// so Enter opens it. A folder selects the one just left, else keeps its
    /// selection. Put back after a cancelled drag, the selection stays.
    pub(super) fn pick_selection(&mut self, left: Option<PathBuf>) {
        let first = if std::mem::take(&mut self.keep_selection) {
            None
        } else if self.query.is_empty() {
            left.and_then(|path| self.entries.iter().position(|e| e.path == path))
        } else {
            (!self.entries.is_empty()).then_some(0)
        };
        let made = std::mem::take(&mut self.select_after);
        if self.entries.iter().any(|e| made.contains(&e.path)) {
            self.selection.clear();
            for entry in self.entries.iter().filter(|e| made.contains(&e.path)) {
                self.selection.toggle(entry);
            }
        } else if let Some(ix) = first {
            self.selection.select(&self.entries[ix]);
        }
    }

    /// Refreshes the listing shortly after the disk changes under a folder on
    /// screen.
    pub(super) fn disk_changed(&mut self, batch: &[(PathBuf, bool)], cx: &mut Context<Self>) {
        if !self.showing_folder() {
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
                if this.showing_folder() && !this.listing {
                    this.list(true, cx);
                }
            });
        }));
    }

    /// The row the keys move from.
    pub(super) fn cursor_ix(&self) -> Option<usize> {
        self.selection.cursor_ix(&self.entries)
    }

    /// Lays the tree out as rows, keeping the selection on the same entry.
    pub(super) fn rebuild(&mut self) {
        self.relayout(true);
    }

    /// `rebuild`; `follow` scrolls to the selection if rows moved it.
    fn relayout(&mut self, follow: bool) {
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

    /// Opens or closes a folder in place. Opening lists it off the UI thread.
    pub(super) fn toggle_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
        let (show_hidden, peek, sort) = (self.show_hidden, self.peek_hidden, self.sort);
        let tidy = self.tidy_folders(cx);
        let dir = path.clone();
        let list = cx.background_executor().spawn(async move {
            let hidden = sidebar::lists_hidden(&dir, show_hidden || peek, &tidy);
            let list = entries::list_dir(&dir, hidden, sort).unwrap_or_default();
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
    fn collapse(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.key != "left" || keystroke.modifiers.modified() || self.renaming.is_some() {
            return;
        }
        cx.stop_propagation();
        if self.view == View::Grid {
            return self.select(Move::By(-1), false, cx);
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

    /// Whether row `ix` is a folder, and whether it's open. Links aren't
    /// opened in place: telling where they lead would read the disk here.
    /// Packages aren't either, which their extension says without a read.
    pub(super) fn is_open_folder(&self, ix: usize) -> Option<bool> {
        let entry = self.entries.get(ix)?;
        (entry.kind == Kind::Dir && !workspace::is_package(&entry.path))
            .then(|| self.expanded.contains(&entry.path))
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
                            .text_color(content(0.65))
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

    pub(super) fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
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
        cx.emit(PaneEvent::Menu { scopes, at });
    }

    /// What a context menu on the listing is on: the selection, how many,
    /// and whether it's a lone package, or else the folder.
    pub(super) fn scopes(&mut self) -> Vec<Scope> {
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
    pub(super) fn package(&mut self) -> Option<PathBuf> {
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

    /// The lone selected entry, if it's a folder or a link that may lead to
    /// one: what Add to Sidebar pins. Each frame asks, so the answer is kept
    /// for the entry it was worked out for.
    pub(super) fn lone_folder(&mut self) -> Option<PathBuf> {
        let only = self.selection.only()?;
        if self
            .lone_folder
            .as_ref()
            .is_none_or(|(path, _)| path != only)
        {
            let is = self
                .entries
                .iter()
                .any(|e| e.path == only && matches!(e.kind, Kind::Dir | Kind::Link));
            self.lone_folder = Some((only.to_path_buf(), is));
        }
        self.lone_folder
            .as_ref()
            .filter(|(_, is)| *is)
            .map(|(path, _)| path.clone())
    }

    /// Where ⇧F10 opens the menu: under the cursor's entry if it's in view,
    /// else in the listing's corner.
    pub(super) fn menu_point(&self, cx: &App) -> Point<Pixels> {
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

    /// Open in Terminal: at each selected folder and the folder holding
    /// each other item, or here with nothing selected.
    pub(super) fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
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
        let terminal = cx.global::<Preferences>().terminal;
        if let Err(e) = workspace::open_in_terminal(&folders, &terminal) {
            self.toast(e, true, None, cx);
        }
    }

    /// Show Package Contents: goes inside the selected package, which Open
    /// launches instead. A link goes in only if it leads to a folder, which is
    /// on the disk, maybe a slow volume.
    pub(super) fn show_package_contents(&mut self, cx: &mut Context<Self>) {
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
            || self.chrome.menu_here
    }

    /// A point in the window as a point in the listing, measured from the
    /// top left of all its rows, scrolled or not.
    pub(super) fn listing_point(&self, point: Point<Pixels>) -> Point<Pixels> {
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

    /// Opens what's selected: a lone folder goes in, and files and packages
    /// open in their apps. Several folders at once would need tabs or windows. Past
    /// `OPEN_WITHOUT_ASKING` files it asks first, as Finder does, so ⌘A then
    /// ↩ can't launch a whole folder.
    pub(super) fn open_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
            .filter(|entry| entry.kind != Kind::Dir || workspace::is_package(&entry.path))
            .map(|entry| entry.path.clone())
            .collect();
        if files.len() <= OPEN_WITHOUT_ASKING {
            for file in &files {
                self.open_file(file, cx);
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
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    for file in &files {
                        this.open_file(file, cx);
                    }
                });
            }
        })
        .detach();
    }

    pub(super) fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        let path = entry.path.clone();
        let kind = entry.kind;
        if !matches!(kind, Kind::Dir | Kind::Link) {
            return self.open_file(&path, cx);
        }
        // Whether it's a package, or where a link leads, is on the disk, maybe
        // a slow volume. A package launches its handler, as in Finder.
        let target = path.clone();
        let goes_in = cx.background_executor().spawn(async move {
            match kind {
                Kind::Dir => !info::is_package(&target),
                _ => target.is_dir() && !info::is_package(&real(&target)),
            }
        });
        cx.spawn(async move |this, cx| {
            let goes_in = goes_in.await;
            let _ = this.update(cx, |this, cx| {
                if goes_in {
                    this.navigate(path, cx);
                } else {
                    this.open_file(&path, cx);
                }
            });
        })
        .detach();
    }

    /// Moves the cursor a `step`, selecting what it lands on, or with ⇧
    /// (`extend`) the range from the anchor to it.
    pub(super) fn select(&mut self, step: Move, extend: bool, cx: &mut Context<Self>) {
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
    pub(super) fn select_vertically(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
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

    pub(super) fn focus_list(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.list_focus, cx);
    }

    /// Opens a place and moves focus to its files.
    pub(super) fn go(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate(path, cx);
        self.focus_list(window, cx);
    }

    /// Typing in the list or sidebar searches: the first character moves
    /// focus to the search field and types there. Finder selects by name
    /// instead; search is Monodrive's way in.
    pub(super) fn type_to_search(
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

    /// Shows `folder` with `select` selected, the way Go to Folder and a
    /// pasted path land.
    pub(super) fn go_to(
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
        if folder == self.folder() && self.showing_folder() {
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

    /// Asks the owner for a change, shared with agents' history.
    pub(super) fn submit(&mut self, request: Request, window: &Window, cx: &mut Context<Self>) {
        // What these put somewhere is selected once it lists.
        let select = matches!(
            request,
            Request::NewFolder { .. }
                | Request::NewFile { .. }
                | Request::Duplicate { .. }
                | Request::Undo
                | Request::UndoOf { .. }
                | Request::Redo
        );
        with_owner(
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
    pub(super) fn settle(
        &mut self,
        done: Result<(Request, Outcome), String>,
        select: bool,
        cx: &mut Context<Self>,
    ) {
        let (request, outcome) = match done {
            Ok(done) => done,
            Err(e) => return self.toast(e, true, None, cx),
        };
        if let Some(why) = outcome.failures().next() {
            let why = short_reason(why, [self.folder()]);
            self.toast(why, true, None, cx);
        } else if let Some(said) = said(&request, &outcome) {
            // Undo from the toast undoes this change or nothing.
            let changed = !outcome.items.is_empty();
            let undo =
                (changed && !matches!(request, Request::EmptyTrash { .. })).then_some(outcome.op);
            self.toast(said, false, undo, cx);
            if matches!(request, Request::EmptyTrash { .. }) {
                cx.emit(PaneEvent::VolumesChanged);
            }
        } else {
            // An undo leaves the last toast's Undo stale.
            cx.emit(PaneEvent::ClearToast);
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
    pub(super) fn chosen(&self) -> Vec<PathBuf> {
        self.selection
            .in_order(&self.entries)
            .into_iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// New Folder and New File: an untitled one here, its name ready to
    /// edit.
    pub(super) fn new_item(&mut self, folder: bool, window: &Window, cx: &mut Context<Self>) {
        let here = self.folder().to_path_buf();
        with_owner(
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
            return self.toast(why, true, None, cx);
        }
        with_owner(
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
        if let Some(path) = made.filter(|_| self.has_the_window()) {
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
    pub(super) fn paste(&mut self, moving: bool, window: &Window, cx: &mut Context<Self>) {
        let Some(board) = cx.try_global::<Pasteboard>() else {
            return;
        };
        let paths = board.files();
        let into = self.folder().to_path_buf();
        self.send(paths, into, moving, window, cx);
    }

    /// Copies or moves `paths` into `into` as one job, followed until it ends.
    pub(super) fn send(
        &mut self,
        paths: Vec<PathBuf>,
        into: PathBuf,
        moving: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
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
        with_owner(
            window,
            cx,
            move |owner| owner.submit(Origin::Window, request),
            |this, outcome, _, cx| match outcome {
                Ok(outcome) => cx.emit(PaneEvent::Follow(outcome.op)),
                Err(e) => this.toast(e, true, None, cx),
            },
        );
    }

    /// ↩: edits the selected item's name in place.
    pub(super) fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// The lone selected item's name, edited where the inspector shows it
    /// and not in its row. Its field can draw in one place at a time.
    pub(super) fn rename_in_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rename_selected(window, cx);
        if let Some(renaming) = &mut self.renaming {
            renaming.in_inspector = true;
        }
    }

    /// Edits `path`'s name in place, selected up to its extension as in
    /// Finder, or whole for a folder. It needn't be listed yet.
    pub(super) fn start_rename(
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
            in_inspector: false,
            _blur: blur,
        });
        cx.notify();
    }

    /// ↩, or clicking away: renames through the owner. A name that can't be
    /// used keeps the field open with the reason; the same name just closes
    /// it.
    pub(super) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let sent = with_owner(
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
                let has_the_window = this.has_the_window();
                let still = this.renaming.as_mut().filter(|r| r.path == path);
                match (failure, still) {
                    (Some(why), Some(renaming)) => {
                        renaming.sending = false;
                        renaming.error = Some(why);
                        if has_the_window {
                            window.focus(&renaming.field.focus_handle(cx), cx);
                        }
                    }
                    (Some(why), None) => this.toast(why, true, None, cx),
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
        if renaming.in_inspector {
            return None;
        }
        self.name_field(renaming, width, cx)
    }

    /// The name being edited in the inspector, if it's `path`'s.
    pub(super) fn inspector_rename_field(
        &self,
        path: &Path,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let renaming = self
            .renaming
            .as_ref()
            .filter(|r| r.in_inspector && r.path == path)?;
        self.name_field(renaming, width, cx)
    }

    fn name_field(
        &self,
        renaming: &Renaming,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let covered = self.chrome.covered;
        // Solid over the selection's accent, whatever the pane's glass.
        let mut fill = theme::pane_fill();
        fill.a = 1.;
        Some(
            theme::field(if renaming.error.is_some() {
                danger()
            } else {
                accent()
            })
            .debug_selector(|| "rename-field".into())
            .key_context("Rename")
            .on_action(
                cx.listener(|this, _: &CommitRename, window, cx| this.commit_rename(window, cx)),
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
            .px(px(6.))
            .bg(fill)
            .text_color(content(1.))
            .child(renaming.field.clone())
            .children(renaming.error.clone().filter(|_| !covered).map(|why| {
                // Over the row below, so no row moves, and kept in the window.
                // It would draw over a sheet or a menu too, so it waits for them.
                div().absolute().left_0().top(gpui::relative(1.)).child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopLeft)
                            .offset(gpui::point(px(0.), px(2.)))
                            .snap_to_window_with_margin(px(8.))
                            .child(
                                div()
                                    .debug_selector(|| "rename-error".into())
                                    .max_w(px(360.))
                                    .px_2()
                                    .rounded_full()
                                    .bg(fill)
                                    .border_1()
                                    .border_color(content(0.12))
                                    .text_size(px(theme::TEXT_SMALL))
                                    .text_color(danger())
                                    .truncate()
                                    .child(why),
                            ),
                    )
                    .with_priority(1),
                )
            })),
        )
    }

    /// Copies the selected paths, one per line in the order shown, or the
    /// folder's, ready to paste into an agent's prompt or a terminal.
    pub(super) fn copy_path(&self, cx: &mut Context<Self>) {
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
    fn crumbs(
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
                        let drops = dragging && (!here || takes_here);
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
                                                            child.clone(),
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
    fn path_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !cx.global::<Preferences>().show_path_bar || self.unavailable.is_some() {
            return None;
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
        let status = match self.chrome.space.clone() {
            Some(space) => format!("{count} · {space}"),
            None => count,
        };
        let width = self.chrome.width - PATH_BAR_CHROME;
        let most = width.max(0.);
        let taken = text_width(&status, theme::TEXT_SMALL, FontWeight::NORMAL, window) + 2. * 12.;
        Some(
            theme::pane_bar(theme::PATH_BAR_HEIGHT)
                .debug_selector(|| "path-bar".into())
                // The first crumb's padding is part of the inset, so its text
                // lines up with the bars above.
                .when(self.showing_folder(), |d| {
                    d.pl(px(theme::PANE_INSET - theme::CRUMB_INSET))
                })
                .gap_3()
                .border_t_1()
                .border_color(content(0.06))
                .text_size(px(theme::TEXT_SMALL))
                .when(self.showing_folder(), |d| {
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
    pub(super) fn grid_rows(
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
        let drops = self.drag.active() && !self.in_trash(cx);
        let id = cx.entity_id();
        // Sharp on Retina screens.
        let pixels = (side * window.scale_factor()).ceil() as u32;
        range
            .map(|row| {
                let cells =
                    (row * across..((row + 1) * across).min(self.entries.len())).map(|ix| {
                        let entry = &self.entries[ix];
                        let selected = self.selection.contains(&entry.path);
                        let missing = self.missing.contains(&entry.path);
                        let picture = (thumbs_on && thumbnails::wanted(entry))
                            .then(|| Thumb {
                                path: entry.path.clone(),
                                side: pixels,
                                modified: entry.modified,
                            })
                            .and_then(|thumb| {
                                if !self.thumbs.contains_key(&thumb) {
                                    thumbnails::hold(&thumb, cx);
                                }
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
                                .rounded(px(theme::RADIUS_THUMB))
                                .into_any_element(),
                            None => file_icons::icon(entry, side, window, cx),
                        };
                        let rename = self.rename_field(&entry.path, cell - 8., cx);
                        let grabs = !missing && rename.is_none();
                        div()
                            .id(ix)
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
            .collect()
    }

    pub(super) fn rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let searching = !self.showing_folder();
        let tree = !searching;
        let today = Local::now().date_naive();
        let (date, size, kind) = columns(self.chrome.width);
        let (row_height, icon_size, text_size) = cx.global::<Preferences>().row_size.metrics();
        // Finder greys the selection when the list isn't where keys go.
        let active = self.list_active(window, cx);
        let drops = self.drag.active() && !self.in_trash(cx);
        let id = cx.entity_id();
        range
            .filter_map(|ix| {
                let entry = self.entries.get(ix)?;
                let selected = self.selection.contains(&entry.path);
                let missing = self.missing.contains(&entry.path);
                let depth = self.depths.get(ix).copied().unwrap_or(0);
                let open = self.is_open_folder(ix);
                let parent = searching
                    .then(|| entry.path.parent().map(|p| p.display().to_string()))
                    .flatten();
                // Finder's list: alternate rows striped, the selection in the
                // accent with its ink.
                let selected_ink = if active { on_accent() } else { content(1.) };
                let muted = if selected {
                    selected_ink
                } else {
                    content(0.65)
                };
                let rename = self.rename_field(&entry.path, 260., cx);
                let grabs = !missing && rename.is_none();
                Some(
                    div()
                        .id(ix)
                        .w_full()
                        .h(px(row_height))
                        .px_3()
                        .rounded(px(theme::RADIUS_ROW))
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_size(px(text_size))
                        .map(|d| {
                            if selected {
                                d.bg(if active { accent() } else { content(0.14) })
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
                                // As in Finder, the rest of a row that isn't
                                // selected starts a rectangle.
                                .child(
                                    div()
                                        .id(("grab", ix))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .when(grabs && !selected, |d| Self::grab(d, ix, cx))
                                        .child(file_icons::icon(entry, icon_size, window, cx))
                                        .child(match rename {
                                            // The name stays put; its outline grows around it.
                                            Some(field) => field.ml(px(-7.)).into_any_element(),
                                            None => div()
                                                .flex_shrink_0()
                                                .max_w(px(360.))
                                                .truncate()
                                                .child(entry.name.clone())
                                                .into_any_element(),
                                        }),
                                )
                                .children(parent.map(|parent| {
                                    div().min_w_0().truncate().text_color(muted).child(parent)
                                })),
                        )
                        .when(date, |d| {
                            d.child(column(180., false).text_color(muted).child(if missing {
                                "--".to_string()
                            } else {
                                format_date(entry.modified, today)
                            }))
                        })
                        .when(size, |d| {
                            d.child(
                                column(80., true)
                                    .text_color(muted)
                                    .child(format_size(entry)),
                            )
                        })
                        .when(kind, |d| {
                            d.child(column(110., false).text_color(muted).child(if missing {
                                "Missing".to_string()
                            } else {
                                entry.kind_label()
                            }))
                        })
                        .when(missing, |d| d.opacity(0.5))
                        .when(grabs && selected, |d| Self::grab(d, ix, cx))
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.click_entry(ix, event, window, cx)
                        }))
                        .map(|d| Self::context_target(d, ix, cx))
                        .when(drops, |d| Self::takes_drops(d, ix, cx))
                        .when(self.drag.outlines(&Spot::Row(id, ix)), |d| {
                            d.relative().child(
                                self.drag
                                    .pulsed(theme::drop_outline().rounded(px(theme::RADIUS_ROW))),
                            )
                        })
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// What a toast says once `request` has changed what `outcome` lists.
pub(super) fn said(request: &Request, outcome: &Outcome) -> Option<String> {
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
        Request::PutBack { .. } => format!("Put back {what}"),
        _ => return None,
    })
}

/// Why `path` couldn't take a name, short enough for the title bar:
/// "\"draft.txt\" is already taken."
pub(super) fn rename_reason(why: &str, path: &Path) -> String {
    let why = short_reason(why, path.parent());
    match why.strip_suffix(" already exists.") {
        Some(name) => format!("\"{name}\" is already taken."),
        None => why,
    }
}

/// A name being edited in place.
pub(super) struct Renaming {
    pub(super) path: PathBuf,
    pub(super) field: Entity<TextField>,
    /// Why the last name didn't take, shown in the title bar.
    pub(super) error: Option<String>,
    /// Sent to the owner and not yet answered, so a blur meanwhile doesn't
    /// send it again.
    sending: bool,
    /// Its field is in the inspector, so the row shows the name as usual.
    pub(super) in_inspector: bool,
    /// Clicking away commits.
    _blur: Subscription,
}

/// A place in the history: a folder, or Recents in front of one.
#[derive(Clone)]
pub(super) struct Stop {
    pub(super) folder: PathBuf,
    pub(super) recents: bool,
    /// How it's saved, from the first save on.
    place: Option<Favorite>,
    /// It came from a saved session and hasn't been checked since: its drive
    /// may be gone, or another may have taken its path.
    unverified: bool,
}

impl Stop {
    pub(super) fn folder(folder: PathBuf) -> Self {
        Self {
            folder,
            recents: false,
            place: None,
            unverified: false,
        }
    }

    pub(super) fn restored(state: StopState) -> Self {
        Self {
            folder: state.place.path.clone(),
            recents: state.recents,
            place: Some(state.place),
            unverified: true,
        }
    }
}

/// A grid cell's name in at most two lines, as Finder sets it: the first
/// breaks where it would wrap, at a word when it can, and the rest is cut
/// in the middle, so the start and the extension both show.
pub(super) fn grid_name(
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

/// The grid's padding across: the list's and each row's, both sides.
const GRID_PADDING: f32 = 16.;

/// How many cells fit across a grid `width` wide, and how wide each is. The
/// room left over is shared out, as in Finder, rather than left at the right.
pub(super) fn grid_layout(width: f32, icon: u32) -> (usize, f32) {
    let least = grid_cell(icon).0;
    let room = (width - GRID_PADDING).max(0.);
    let across = (room / least).floor().max(1.) as usize;
    (across, (room / across as f32).max(least))
}

/// A grid cell's width and height for icons `icon` points across: room for
/// the icon and two lines of name.
pub(super) fn grid_cell(icon: u32) -> (f32, f32) {
    let icon = icon as f32;
    (icon + 48., icon + 60.)
}

/// What a level of a path is called.
pub(super) fn level_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "Macintosh HD".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

pub(super) fn text_width(text: &str, size: f32, weight: FontWeight, window: &Window) -> f32 {
    let run = gpui::TextRun {
        len: text.len(),
        font: gpui::Font {
            weight,
            ..gpui::font(theme::UI_FONT)
        },
        color: content(1.),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let text = gpui::SharedString::from(text.to_owned());
    window
        .text_system()
        .shape_line(text, px(size), &[run], None)
        .width
        .as_f32()
}

/// How many of the path's leading levels collapse into one "…" crumb. `widths`
/// runs from the disk to the current folder, each with its chevron. Levels
/// are kept from the right at full width while they fit in `room`; the current
/// folder is always kept.
pub(super) fn hidden_levels(widths: &[f32], ellipsis: f32, room: f32) -> usize {
    if widths.iter().sum::<f32>() <= room {
        return 0;
    }
    let mut used = ellipsis;
    let mut shown = 0;
    for width in widths.iter().rev() {
        if shown > 0 && used + width > room {
            break;
        }
        used += width;
        shown += 1;
    }
    widths.len().saturating_sub(shown.max(1))
}

/// A column title that sorts by its column; the sorted one shows which way.
fn sort_header(
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
                .on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.choose_sort(sorted_by(this.sort, by), cx)
                    }),
                )
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

/// Which of Date Modified, Size and Kind a list `width` wide shows. Names keep
/// at least 160 pt: Kind goes first, then the date, then the size.
fn columns(width: f32) -> (bool, bool, bool) {
    // Each column with its gap, after the list's and the row's padding.
    let (name, date, size, kind) = (32. + 160., 192., 92., 122.);
    (
        width >= name + size + date,
        width >= name + size,
        width >= name + size + date + kind,
    )
}

pub(super) fn column(width: f32, right: bool) -> gpui::Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .truncate()
        .text_color(content(0.65))
        .when(right, |d| d.text_right())
}

/// `dir` as FSEvents names it: links resolved (/var is /private/var).
/// Reads the disk.
pub(super) fn real(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// More files than this at once ask before opening.
pub(super) const OPEN_WITHOUT_ASKING: usize = 20;

/// A rectangle dragged over the listing to select what it touches. Its
/// corners are in listing coordinates, so scrolling mid-drag keeps the
/// start where it was.
pub(super) struct Marquee {
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
pub(super) fn span(from: f32, to: f32, step: f32, count: usize) -> std::ops::Range<usize> {
    if to < 0. || count == 0 || step <= 0. {
        return 0..0;
    }
    let first = ((from.max(0.) / step) as usize).min(count);
    let last = ((to / step) as usize).min(count - 1);
    first..(last + 1).max(first)
}

/// What on disk the listing follows, in FSEvents' real paths.
#[derive(Default)]
pub(super) struct Watch {
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
pub(super) fn touches<'a>(
    changed: &Path,
    below: bool,
    shown: impl IntoIterator<Item = &'a PathBuf>,
) -> bool {
    shown
        .into_iter()
        .any(|dir| dir == changed || (below && dir.starts_with(changed)))
}

/// The rows a folder shows: each entry, with the contents of each expanded
/// folder right under it, one level deeper.
pub(super) fn flatten(
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
pub(super) enum Arrow {
    Left,
    Right,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum TreeStep {
    /// Open or close the folder in place.
    Toggle,
    Select(usize),
    /// Go up to the enclosing folder.
    Up,
    Stay,
}

/// What ← or → does on row `ix` of a tree whose rows have `depths`. `open`
/// is `None` for a file, else whether the folder is open.
pub(super) fn tree_step(depths: &[usize], ix: usize, arrow: Arrow, open: Option<bool>) -> TreeStep {
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

#[derive(Clone, Copy, Debug)]
pub(super) enum Move {
    Previous,
    Next,
    First,
    Last,
    /// So many entries on: a row of the grid, or one across.
    By(isize),
}

/// Where a key moves the selection in a list of `len`. With nothing
/// selected, ↓ starts at the top and ↑ at the bottom, as in Finder.
pub(super) fn moved(selected: Option<usize>, len: usize, step: Move) -> Option<usize> {
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
pub(super) fn typed(keystroke: &Keystroke) -> Option<&str> {
    let modifiers = keystroke.modifiers;
    if modifiers.platform || modifiers.control || modifiers.function {
        return None;
    }
    keystroke.key_char.as_deref().filter(|text| {
        !text.is_empty() && !text.chars().any(|c| c.is_control() || c.is_whitespace())
    })
}

pub(super) fn format_date(secs: u64, today: chrono::NaiveDate) -> String {
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
pub(super) fn format_size(entry: &Entry) -> String {
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

impl Pane {
    /// Esc: leaves search, or else lets go of the selection.
    pub(super) fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query.is_empty() {
            self.selection.clear();
            cx.notify();
        } else {
            self.show_folder(cx);
        }
        self.focus_list(window, cx);
    }

    /// The commands that act on this pane wherever the keys are in the window,
    /// so the window puts them on its root for the pane they're meant for.
    pub(super) fn commands(&mut self, root: Div, sheet: bool, cx: &mut Context<Self>) -> Div {
        let package = self.package().is_some();
        // Nothing new goes in search results, Recents or the Trash.
        let can_make_here = self.takes_files(cx);
        let put_back = !self.put_back().0.is_empty() && self.in_trash(cx);
        let selected = !self.selection.is_empty();
        let renaming = self.renaming.is_some();
        // While a sheet is open, its commands (from the menu bar too) wait.
        root.when(!sheet, |d| {
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
                .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                    this.search.update(cx, |field, cx| {
                        window.focus(&field.focus_handle(cx), cx);
                        field.select_all(&text_field::SelectAll, window, cx);
                    });
                }))
                .on_action(cx.listener(|this, _: &DeselectAll, _, cx| {
                    this.selection.clear();
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(cx)))
                .on_action(cx.listener(|this, _: &OpenInTerminal, _, cx| this.open_in_terminal(cx)))
                .when(selected, |d| {
                    d.on_action(cx.listener(|this, _: &RevealInFinder, _, _| {
                        workspace::reveal(&this.chosen())
                    }))
                })
                .when(package, |d| {
                    d.on_action(cx.listener(|this, _: &ShowPackageContents, _, cx| {
                        this.show_package_contents(cx)
                    }))
                })
        })
        .on_action(cx.listener(|this, _: &ViewAsList, _, cx| this.choose_view(View::List, cx)))
        .on_action(cx.listener(|this, _: &ViewAsGrid, _, cx| this.choose_view(View::Grid, cx)))
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
    fn unavailable_view(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
    fn file_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // The list's rows don't depend on its width, so it shows at once.
        if self.view == View::List {
            self.take_pending_reveal();
        }
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
            None if self.view == View::Grid => {
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
            thumbnails::release(&thumb, image, Some(&mut *window), cx);
        }
        let can_make_here = self.takes_files(cx);
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
            .when(can_make_here, |d| {
                d.on_action(cx.listener(|this, _: &text_field::Paste, window, cx| {
                    this.paste(false, window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &MoveHere, window, cx| this.paste(true, window, cx)),
                )
            })
            // Only here, so the menu bar's items are off and can't change the
            // selection's files while the keys are somewhere else, such as the
            // sidebar.
            .when(self.renaming.is_none() && !self.selection.is_empty(), |d| {
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
            })
            .when(self.renaming.is_none() && self.selection.len() == 1, |d| {
                d.on_action(
                    cx.listener(|this, _: &Rename, window, cx| this.rename_selected(window, cx)),
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
                if this.renaming.is_none() {
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
            .when(self.marquee.is_some(), |d| d.child(self.marquee_drag(cx)))
    }
}

impl Focusable for Pane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.list_focus.clone()
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = cx.global::<Preferences>();
        let boxy = prefs.workspace_style == WorkspaceStyle::Boxy;
        // Search results stay in relevance order, so no column shows a sort.
        let sort_shown = self.showing_folder().then_some(self.sort);
        let empty = self.root.is_empty() && self.error.is_none();
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
        let list = self.file_list(window, cx);
        let (date, size, kind) = columns(self.chrome.width);
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
            // The grid sorts from the View menu and Settings.
            .when(self.view == View::List && self.unavailable.is_none(), |d| {
                d.child(
                    theme::pane_bar(theme::ROW_HEIGHT)
                        .gap_3()
                        .border_b_1()
                        .border_color(content(0.06))
                        .text_size(px(theme::TEXT_SMALL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(content(0.65))
                        .child(sort_header(SortBy::Name, "Name", sort_shown, cx).flex_1())
                        .when(date, |d| {
                            d.child(
                                sort_header(SortBy::Modified, "Date Modified", sort_shown, cx)
                                    .w(px(180.)),
                            )
                        })
                        .when(size, |d| {
                            d.child(
                                sort_header(SortBy::Size, "Size", sort_shown, cx)
                                    .w(px(80.))
                                    .justify_end(),
                            )
                        })
                        .when(kind, |d| {
                            d.child(sort_header(SortBy::Kind, "Kind", sort_shown, cx).w(px(110.)))
                        }),
                )
            })
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
    }
}
