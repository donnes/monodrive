//! One pane: a location with its history, listing, selection, view and sort,
//! drawn as a card of files with its own path bar. A tab holds one or two.
//! What's window-wide (toasts, sheets, the jobs ring, the sidebar) belongs to
//! `Browser`, which a pane reaches through `PaneEvent`.

mod commands;
pub(super) mod rename;
pub(super) mod rows;
use rows::*;
pub(super) mod view;
use rename::Renaming;
use rows::Marquee;
use view::Address;
pub(super) mod airdrop;
pub(super) mod archive;
pub(super) mod columns;
pub(super) mod drag;
pub(super) mod duplicates;
pub(super) mod localsend;
pub(super) mod overview;
pub(super) mod remote;
pub(super) mod searches;
pub(super) mod trash;
pub(super) mod worktrees;

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
    SortByName, SortBySize, SuggestNames, Undo, ViewAsColumns, ViewAsGrid, ViewAsList, with_owner,
};
use crate::background::Backdrop;
use crate::commands::Scope;
use crate::downloads::Downloads;
use crate::feedback::{self, Sound};
use crate::format::short_reason;
use crate::jobs;
use crate::onboarding::IndexGate;
use crate::preferences::{self, Preferences, View};
use crate::quick_look::Preview;
use crate::selection::Selection;
use crate::session::{PaneState, RemoteAt, Shows, StopState};
use crate::system::Places;
use crate::text_field::{self, TextField};
use crate::theme::{self};
use crate::thumbnails::{self, Thumb};
use crate::workspace;
use columns::Columns;
use drag::Drag;
use duplicates::DuplicatesPage;
use gpui::{
    App, Bounds, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent,
    Pixels, Point, RenderImage, ScrollHandle, ScrollStrategy, Task, UniformListScrollHandle,
    Window, prelude::*, px,
};
use gpui::{MouseButton, MouseDownEvent, point};
use localsend::LocalSendPage;
use monodrive_core::Origin;
use monodrive_core::cloud::{self as icloud};
use monodrive_core::entries::{self, Entry, Kind, Sort, SortBy};
use monodrive_core::index::Index;
use monodrive_core::ops::{Outcome, Request};
use monodrive_core::places::Favorite;
use monodrive_core::places::{self, Availability};
use monodrive_core::search::{Looks, Query, Scope as SearchScope};
use overview::OverviewPage;
use remote::RemotePage;
use rename::BatchRename;
use rename::Review;
use searches::CONTENT_SETTLE;
use searches::{Completing, Matches};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use worktrees::WorktreesPage;

const SEARCH_LIMIT: usize = 200;
/// How long the disk must settle before a changed folder lists again.
pub(super) const REFRESH_DELAY: Duration = Duration::from_millis(150);
/// How long after a change beside one of its results a search runs again: a
/// result in a folder that never stops changing mustn't search all the time.
pub(super) const SEARCH_REFRESH_DELAY: Duration = Duration::from_secs(1);
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
        offer: Option<Offer>,
    },
    ClearToast,
    /// Follow this job of the pane's until it ends.
    Follow(u64),
    /// The context menu on the listing, at this point in the window.
    Menu {
        scopes: Vec<Scope>,
        at: Point<Pixels>,
    },
    Eject(PathBuf),
    /// Download this file on a server, then open it, or Quick Look it.
    Fetch {
        at: monodrive_core::remote::RemotePath,
        size: u64,
        look: bool,
    },
    /// What should be there on the next launch changed.
    Moved,
}

/// What a toast's button does.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Offer {
    /// Undo of this operation, which the owner refuses once anything else
    /// has changed files since.
    Undo(u64),
    /// Downloads these, under this label: Download, or Try Again.
    Download(&'static str, Vec<PathBuf>),
    /// Shows this in its folder, selected.
    Show(PathBuf),
}

/// Where a pane was before a drag opened folders in it.
pub(super) struct SpringReturn {
    history: Vec<Stop>,
    at: usize,
    expanded: HashSet<PathBuf>,
    selection: Selection,
    query: String,
    looks: Looks,
    search_scope: SearchScope,
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
    /// The folder the other pane shows, when it takes files: where Extract
    /// puts things.
    pub(super) extract_to: Option<PathBuf>,
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
    pub(super) index: Arc<OnceLock<Result<Index, String>>>,
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
    pub(super) expanded: HashSet<PathBuf>,
    /// The last listing of each expanded folder, and of each column's.
    pub(super) children: HashMap<PathBuf, Vec<Entry>>,
    /// Folder listings in flight for expanded folders and columns, dropped if
    /// superseded.
    pub(super) expanding: HashMap<PathBuf, Task<()>>,
    /// A listing or search is on its way, so the rows on screen are about to
    /// change: the tree's keys wait rather than act on rows that are leaving.
    pub(super) listing: bool,
    /// What on disk the listing follows.
    pub(super) watch: Watch,
    /// List, grid or columns, and the grid's icon size, as the preferences
    /// last said.
    pub(super) view: View,
    /// Column view's chain and its last column.
    pub(super) columns: Columns,
    /// The folder on screen is the Trash, which shows as a list.
    trash_here: bool,
    pub(super) grid_icon: u32,
    /// How far the pane's width said by the window was from the grid's as
    /// last laid out.
    grid_inset: Rc<Cell<f32>>,
    /// How many cells fit across and how wide each is, settled once per
    /// frame so the row count, the cells and scrolling agree.
    pub(super) across: usize,
    pub(super) cell_width: f32,
    /// Scroll to the selection once the layout settles (after a view switch).
    pub(super) reveal_pending: bool,
    /// Every thumbnail asked for and not yet given back, loaded or not.
    pub(super) thumbs: HashMap<Thumb, Option<Arc<RenderImage>>>,
    /// The thumbnails the last frame drew; the rest are given back.
    pub(super) drawn: HashSet<Thumb>,
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
    /// The path bar, edited as a path.
    pub(super) address: Option<Address>,
    /// Rename in the list: the bar, and the selected rows' new names.
    pub(super) batch: Option<BatchRename>,
    /// Organize's review, in place of the list.
    pub(super) organizing: Option<Review>,
    /// The preview, in place of the list. It leaves the keys on the list, so
    /// the arrows still move the selection it follows.
    pub(super) preview: Option<Entity<crate::preview::PreviewView>>,
    /// The preview's text tool has the keys, which the list must leave alone.
    pub(super) preview_typing: bool,
    /// How many times the bar has opened, which tells each opening apart.
    pub(super) batch_sessions: u64,
    /// What's selected stays when the next listing lands, as it was put back.
    pub(super) keep_selection: bool,
    /// What a change made, to select once the listing that has it lands.
    select_after: Vec<PathBuf>,
    /// The search as text, the tokens' then the words typed; empty while
    /// showing the folder.
    pub(super) query: String,
    /// The finished filters, shown as tokens before the words in the field.
    pub(super) filters: Query,
    /// The token the keys are on, and the row of its menu while that's open.
    pub(super) chip: Option<usize>,
    pub(super) chip_menu: Option<usize>,
    /// Where the keys go while they're on a token.
    pub(super) chips_focus: FocusHandle,
    pub(super) chips_scroll: ScrollHandle,
    /// What a search looks at. Leaving a search goes back to names.
    pub(super) looks: Looks,
    /// Where a search looks. Leaving a search goes back to everywhere.
    pub(super) search_scope: SearchScope,
    /// A content search's matching lines, and the rows that show them.
    pub(super) matches: Matches,
    /// Values that could finish the filter being typed, while the field shows them.
    pub(super) completing: Option<Completing>,
    /// How tall a row was last drawn.
    list_row_height: f32,
    /// Keeps the results on screen as a recent search, once they've stayed.
    pub(super) remember: Task<()>,
    pub(super) selection: Selection,
    pub(super) scroll: UniformListScrollHandle,
    /// The listing or search in flight. Replacing it drops the stale one.
    loading: Task<()>,
    /// Whether the listing on screen includes hidden files, and its order.
    pub(super) show_hidden: bool,
    /// This folder shows its hidden files anyway, because Go to Folder was
    /// asked for one of them. Cleared on leaving the folder.
    pub(super) peek_hidden: bool,
    pub(super) sort: Sort,
    /// A rectangle being dragged over the listing to select.
    marquee: Option<Marquee>,
    /// A rectangle was just dragged. GPUI still reports a click when it
    /// ends on the entry it began on; this says it was a drag.
    swept: bool,
    /// Where the listing was laid out last frame, in the window.
    pub(super) list_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// An Open waiting on the disk. Replacing it, or going elsewhere, drops it.
    opening: Task<()>,
    /// Whether this lone selected entry is a package, as last worked out.
    package: Option<(PathBuf, bool)>,
    /// Whether this lone selected entry is a folder, as last worked out.
    lone_folder: Option<(PathBuf, bool)>,
    /// What the Trash's bar shows while the Trash is on screen.
    pub(super) trash_header: trash::TrashHeader,
    /// Rows in Recents whose files are gone.
    pub(super) missing: HashSet<PathBuf>,
    /// Folders on screen that hold something not downloaded, as macOS last
    /// said. They get the cloud badge a placeholder gets from its flag.
    partly_downloaded: HashSet<PathBuf>,
    /// Asking macOS which folders those are. A new listing drops these.
    asking: Vec<Task<()>>,
    /// What's downloading, for every pane.
    pub(super) downloads: Entity<Downloads>,
    /// Placeholders that open in their app once they're downloaded.
    pub(super) open_after_download: HashSet<PathBuf>,
    /// The Overview, as last read.
    pub(super) overview: OverviewPage,
    /// The worktrees, as last scanned.
    pub(super) worktrees: WorktreesPage,
    /// Looking for LocalSend devices while its page is open.
    pub(super) localsend: LocalSendPage,
    /// What Find Duplicates found, as last looked through.
    pub(super) duplicates: DuplicatesPage,
    /// The archive on screen, as last read; dropped on leaving it.
    pub(super) archived: Option<archive::Archived>,
    /// The server folder, as last listed.
    pub(super) remote: RemotePage,
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
        cx.subscribe(&search, |this, _, _: &text_field::Changed, cx| {
            this.follow_typing(cx);
            this.sync_query(cx);
        })
        .detach();
        // Cleanup otherwise runs while drawing, and a closed tab, pane or window
        // doesn't draw again. GPUI keeps a loaded asset until it's removed.
        cx.on_release(|this, cx| {
            for (thumb, image) in this.thumbs.drain() {
                thumbnails::release(&thumb, image, None, cx);
            }
            if this.localsend.looking() {
                super::pane::localsend::stop_looking(cx);
            }
        })
        .detach();
        // Whatever moves the selection repaints, so the Trash's bar follows it.
        cx.observe_self(|this, cx| this.follow_trash_selection(cx))
            .detach();
        let downloads = Self::follow_downloads(cx);
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
            columns: Columns::default(),
            trash_here: false,
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
            address: None,
            batch: None,
            organizing: None,
            preview: None,
            preview_typing: false,
            batch_sessions: 0,
            keep_selection: false,
            select_after: Vec::new(),
            query: String::new(),
            filters: Query::default(),
            chip: None,
            chip_menu: None,
            chips_focus: cx.focus_handle(),
            chips_scroll: ScrollHandle::new(),
            looks: Looks::default(),
            search_scope: SearchScope::default(),
            matches: Matches::default(),
            completing: None,
            list_row_height: 0.,
            remember: Task::ready(()),
            selection: Selection::default(),
            scroll: UniformListScrollHandle::new(),
            loading: Task::ready(()),
            show_hidden,
            sort,
            peek_hidden: false,
            marquee: None,
            swept: false,
            list_bounds: Rc::default(),
            opening: Task::ready(()),
            package: None,
            lone_folder: None,
            trash_header: trash::TrashHeader::default(),
            missing: HashSet::new(),
            partly_downloaded: HashSet::new(),
            asking: Vec::new(),
            downloads,
            open_after_download: HashSet::new(),
            overview: OverviewPage::default(),
            worktrees: WorktreesPage::default(),
            localsend: LocalSendPage::default(),
            duplicates: DuplicatesPage::default(),
            archived: None,
            remote: RemotePage::default(),
            backdrop: cx.new(|_| Backdrop::default()),
            chrome: Chrome::default(),
            drag,
            grabbed: false,
        }
    }

    /// Whether its tab is on screen.
    pub(super) fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        if !shown {
            self.opening = Task::ready(());
            self.refresh = None;
            self.held.clear();
            self.marquee = None;
            self.leave_overview();
            self.leave_duplicates();
            self.leave_localsend(cx);
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
        match (&self.renaming, &self.batch) {
            (Some(renaming), _) => window.focus(&renaming.field.focus_handle(cx), cx),
            (None, Some(batch)) => window.focus(&batch.first_field(cx), cx),
            (None, None) => self.focus_list(window, cx),
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
            .map(|stop| stop.saved_folder().to_path_buf())
            .collect()
    }

    /// Keeps each stop with the volume its folder was first saved on.
    pub(super) fn place_stops(&mut self, placed: &HashMap<PathBuf, Favorite>) {
        for stop in &mut self.history {
            if stop.place.is_none() {
                stop.place = placed.get(stop.saved_folder()).cloned();
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
                    shows: match stop.shows {
                        Shows::Archive | Shows::Duplicates | Shows::AirDrop | Shows::LocalSend => {
                            Shows::Folder
                        }
                        shows => shows,
                    },
                    remote: stop.remote.clone(),
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

    pub(super) fn tidy_folders(&self, cx: &App) -> Vec<PathBuf> {
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
            match &self.search_scope {
                SearchScope::Everywhere => "Search results".to_string(),
                SearchScope::Folder(folder) => format!("Search results in {}", level_name(folder)),
            }
        } else if self.showing_overview() {
            "Overview".to_string()
        } else if self.showing_worktrees() {
            "Worktrees".to_string()
        } else if self.showing_airdrop() {
            "AirDrop".to_string()
        } else if self.showing_localsend() {
            "LocalSend".to_string()
        } else if self.showing_duplicates() {
            format!("Duplicates in {}", level_name(self.folder()))
        } else if self.showing_remote() {
            self.remote_title()
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
        offer: Option<Offer>,
        cx: &mut Context<Self>,
    ) {
        cx.emit(PaneEvent::Toast {
            text,
            failed,
            offer,
        });
    }

    pub(super) fn folder(&self) -> &Path {
        &self.history[self.at].folder
    }

    /// The folder on the disk: inside an archive, the one holding it. What
    /// leaves the pane for another app, a page or a search starts here.
    pub(super) fn disk_folder(&self) -> &Path {
        self.stop().saved_folder()
    }

    /// Goes to `folder`, which stays in the archive on screen when it's
    /// inside it.
    pub(super) fn navigate(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let stop = match &self.stop().archive {
            Some(archive) if folder.starts_with(archive) => Stop::inside(archive.clone(), folder),
            _ => Stop::folder(folder),
        };
        self.push_stop(stop, cx);
    }

    /// Makes `stop` what's on screen, after what was, dropping what was
    /// ahead of it.
    pub(super) fn push_stop(&mut self, stop: Stop, cx: &mut Context<Self>) {
        // A server's rows aren't this Mac's, so neither side has the other's
        // folder to select.
        let local = !self.showing_remote() && stop.shows != Shows::Remote;
        self.reselect = local.then(|| self.folder().to_path_buf());
        self.collapse_all();
        self.history.truncate(self.at + 1);
        self.history.push(stop);
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
            looks: self.looks,
            search_scope: self.search_scope.clone(),
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
        self.set_search_mode(to.looks, to.search_scope, cx);
        self.show_query(&to.query, cx);
        cx.emit(PaneEvent::Moved);
        self.reload(cx);
    }

    /// Back leaves Organize's review before it leaves the folder.
    pub(super) fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.organizing.is_some() {
            self.end_organize(window, cx);
        } else {
            self.step_history(-1, cx);
        }
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
        self.close_address(cx);
        self.query.clear();
        self.filters = Query::default();
        (self.chip, self.chip_menu) = (None, None);
        self.search.update(cx, |field, cx| field.clear(cx));
        self.reset_search_mode(cx);
        self.reload(cx);
    }

    /// Lists the folder and every folder expanded in it, or runs the search
    /// when there is a query, off the UI thread.
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.list(false, cx);
    }

    /// The layout on screen: a content search's lines and a batch's new
    /// names only fit the list.
    /// Search results, Recents and the Trash have no folders to chain, so
    /// column view shows them as a list.
    pub(super) fn layout(&self) -> View {
        match self.view {
            _ if !self.matches.is_empty() || self.batch.is_some() => View::List,
            // An archive's rows stay a list until the folder's listing
            // replaces them.
            View::Columns
                if !self.showing_folder() || self.trash_here || self.showing_archive() =>
            {
                View::List
            }
            view => view,
        }
    }

    /// What a listing that's just landed selects. Search selects its best hit
    /// so Enter opens it. A folder selects the one just left, else keeps its
    /// selection. Put back after a cancelled drag, the selection stays.
    pub(super) fn pick_selection(&mut self, left: Option<PathBuf>) {
        let enter_first = std::mem::take(&mut self.columns.enter_first);
        let first = if std::mem::take(&mut self.keep_selection) {
            None
        } else if self.query.is_empty() && !enter_first {
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

    /// The row the keys move from.
    pub(super) fn cursor_ix(&self) -> Option<usize> {
        self.selection.cursor_ix(&self.entries)
    }

    /// Lays the tree out as rows, keeping the selection on the same entry.
    pub(super) fn rebuild(&mut self) {
        self.relayout(true);
    }

    /// Whether row `ix` is a folder, and whether it's open. Links aren't
    /// opened in place: telling where they lead would read the disk here.
    /// Packages aren't either, which their extension says without a read.
    pub(super) fn is_open_folder(&self, ix: usize) -> Option<bool> {
        let entry = self.entries.get(ix)?;
        (entry.kind == Kind::Dir && !workspace::is_package(&entry.path))
            .then(|| self.expanded.contains(&entry.path))
    }

    /// Scrolls entry `ix` into view: in the grid, its row of the grid.
    pub(super) fn reveal(&self, ix: usize, strategy: ScrollStrategy) {
        let item = match self.layout() {
            View::Grid => ix / self.grid_columns(),
            // Going down brings a file's lines in with it; its own row stays
            // the one to show when it's above the view.
            View::List if !self.matches.is_empty() => {
                let first = self.matches.row_of(ix);
                let offset = self.scroll.0.borrow().base_handle.offset().y.as_f32();
                let above = first as f32 * self.list_row_height < -offset;
                if above || !matches!(strategy, ScrollStrategy::Nearest) {
                    first
                } else {
                    self.matches.last_row_of(ix)
                }
            }
            View::List | View::Columns => ix,
        };
        self.scroll.scroll_to_item(item, strategy);
    }

    /// A click on row `ix`: ⌘ toggles it, ⇧ selects the range from the
    /// anchor, and a plain click selects it alone. A double click opens.
    pub(super) fn click_entry(
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
            if event.click_count() == 2 && self.batch.is_none() {
                self.open_selection(window, cx);
            }
        }
        cx.notify();
    }

    /// What's selected, as far as an Open waiting on the disk cares.
    fn selected_now(&self) -> (Option<PathBuf>, usize) {
        (
            self.selection.only().map(Path::to_path_buf),
            self.selection.len(),
        )
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

    pub(super) fn focus_list(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.list_focus, cx);
    }

    /// Opens a place and moves focus to its files. Column view's chain starts
    /// again there.
    pub(super) fn go(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.columns.root = None;
        self.navigate(path, cx);
        self.focus_list(window, cx);
    }

    /// Shows `folder` with `select` selected, the way Go to Folder, a pasted
    /// path and another app's Show in Finder land.
    pub(super) fn go_to(
        &mut self,
        folder: PathBuf,
        select: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A hidden file asked for by name shows, though hidden files are off.
        let hidden = select
            .iter()
            .filter_map(|path| path.file_name())
            .any(|name| name.to_string_lossy().starts_with('.'));
        // Asked after any navigation, which clears the last folder's peek.
        let needs_peek = |this: &Self| hidden && !this.show_hidden && !this.peek_hidden;
        if folder == self.folder() && self.showing_folder() {
            let found: Vec<usize> = if self.listing {
                Vec::new()
            } else {
                select
                    .iter()
                    .filter_map(|path| self.entries.iter().position(|e| &e.path == path))
                    .collect()
            };
            match found.as_slice() {
                [] if !select.is_empty() => {
                    self.select_when_listed(select);
                    if needs_peek(self) {
                        self.peek_hidden = true;
                        self.reload(cx);
                    }
                }
                [] => {}
                [ix] => self.select_row(*ix, cx),
                [first, ..] => {
                    self.selection.clear();
                    for &ix in &found {
                        self.selection.toggle(&self.entries[ix]);
                    }
                    self.reveal(*first, ScrollStrategy::Nearest);
                    cx.notify();
                }
            }
        } else {
            // Column view's chain starts again at what was asked for.
            self.columns.root = None;
            self.navigate(folder, cx);
            self.select_when_listed(select);
            if needs_peek(self) {
                self.peek_hidden = true;
                self.reload(cx);
            }
        }
        self.focus_list(window, cx);
    }

    /// Selects `paths` once the listing that has them lands, the first with
    /// the cursor on it. Nothing asked keeps the selection the listing picks.
    fn select_when_listed(&mut self, mut paths: Vec<PathBuf>) {
        match paths.len() {
            0 => {}
            1 => self.reselect = paths.pop(),
            _ => {
                self.reselect = paths.first().cloned();
                self.select_after = paths;
            }
        }
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
        // Many items to the Trash: Activity follows the job, and its end
        // says how it went.
        if outcome.job {
            return cx.emit(PaneEvent::Follow(outcome.op));
        }
        if let Some(why) = outcome.failures().next() {
            // A copy of what isn't downloaded is refused: offer to download it.
            let download = icloud::refused(why).map(|path| Offer::Download("Download", vec![path]));
            let why = short_reason(why, [self.folder()]);
            let renamed = outcome.items.iter().filter(|i| i.result.is_ok()).count();
            if matches!(request, Request::RenameMany { .. }) && renamed > 0 {
                // A batch isn't atomic: what it did rename can still go back.
                let of = outcome.items.len();
                let said = format!("Renamed {renamed} of {of} items. {why}");
                self.toast(said, true, Some(Offer::Undo(outcome.op)), cx);
            } else {
                self.toast(why, true, download, cx);
            }
        } else if let Some(said) = said(&request, &outcome) {
            // Undo from the toast undoes this change or nothing. An undo or
            // redo leaves the rest to ⌘Z and ⌘⇧Z.
            let changed = !outcome.items.is_empty();
            let undoable = !matches!(
                request,
                Request::Undo | Request::UndoOf { .. } | Request::Redo
            );
            let undo = (changed && undoable).then_some(Offer::Undo(outcome.op));
            self.toast(said, false, undo, cx);
            if changed && matches!(request, Request::Trash { .. }) {
                feedback::sound(Sound::MoveToTrash, cx);
            }
        } else {
            // Whatever the last toast offered may be stale now.
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
        self.chosen_entries()
            .into_iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// The selected entries that are listed, in the order shown.
    pub(super) fn chosen_entries(&self) -> Vec<&Entry> {
        self.selection.in_order(&self.entries)
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
        Request::RenameMany { .. } => format!("Renamed {what}"),
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
        Request::SaveEdit { .. } => {
            let saved = outcome
                .items
                .iter()
                .rev()
                .find_map(|item| item.result.as_ref().ok());
            match saved {
                Some(path) => format!("Saved {}", jobs::quoted(path)),
                None => "Saved".to_string(),
            }
        }
        Request::Organize { groups } => {
            let files: usize = groups.iter().map(|group| group.items.len()).sum();
            format!(
                "Organized {files} {}",
                if files == 1 { "file" } else { "files" }
            )
        }
        Request::PutBack { .. } => format!("Put back {what}"),
        // Its folder first, unless only Git's registration of it was left.
        Request::RemoveWorktree { path, .. } if asked.first() == Some(&path.as_path()) => {
            format!("Moved {} to the Trash", jobs::quoted(path))
        }
        Request::RemoveWorktree { path, .. } => format!("Forgot {}", jobs::quoted(path)),
        // The owner titles these "Undo Rename", from the change they reverse.
        Request::Undo | Request::UndoOf { .. } | Request::Redo => {
            let of = outcome.title.strip_prefix(request.title()).unwrap_or("");
            let verb = match request {
                Request::Redo => "Redid",
                _ => "Undid",
            };
            format!("{verb}{of}")
        }
        _ => return None,
    })
}

/// A place in the history: a folder, or a page in front of one.
#[derive(Clone)]
pub(super) struct Stop {
    /// Inside an archive, the archive's path joined with the folder's
    /// names in it, which isn't on the disk.
    pub(super) folder: PathBuf,
    pub(super) shows: Shows,
    /// The ZIP archive `folder` is in, for `Shows::Archive`.
    pub(super) archive: Option<PathBuf>,
    /// The server folder a `Shows::Remote` stop shows in front of `folder`.
    pub(super) remote: Option<RemoteAt>,
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
            shows: Shows::Folder,
            archive: None,
            remote: None,
            place: None,
            unverified: false,
        }
    }

    /// `folder`, inside `archive` or the archive itself.
    pub(super) fn inside(archive: PathBuf, folder: PathBuf) -> Self {
        Self {
            folder,
            shows: Shows::Archive,
            archive: Some(archive),
            remote: None,
            place: None,
            unverified: false,
        }
    }

    pub(super) fn restored(state: StopState) -> Self {
        Self {
            folder: state.place.path.clone(),
            shows: state.shows,
            archive: None,
            remote: state.remote,
            place: Some(state.place),
            unverified: true,
        }
    }

    /// The folder on the disk it's saved as: the one holding its archive.
    pub(super) fn saved_folder(&self) -> &Path {
        match &self.archive {
            Some(archive) => archive.parent().unwrap_or(archive),
            None => &self.folder,
        }
    }
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

/// `dir` as FSEvents names it: links resolved (/var is /private/var).
/// Reads the disk.
/// Whether listing `dir` includes hidden files when the setting says `wanted`.
/// The Trash and Applications don't, wherever a link to them is opened from;
/// their subfolders do. Reads the disk, so call it off the UI thread.
pub(super) fn lists_hidden(dir: &Path, wanted: bool, tidy: &[PathBuf]) -> bool {
    wanted && {
        let dir = real(dir);
        !tidy.iter().any(|folder| real(folder) == dir)
    }
}

pub(super) fn real(dir: &Path) -> PathBuf {
    dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
}

/// More files than this at once ask before opening.
pub(super) const OPEN_WITHOUT_ASKING: usize = 20;

impl Pane {
    pub(super) fn showing_recents(&self) -> bool {
        self.history[self.at].shows == Shows::Recents
    }
    /// Whether the Overview is on screen: a search in front of it isn't.
    pub(super) fn showing_overview(&self) -> bool {
        self.query.is_empty() && self.history[self.at].shows == Shows::Overview
    }
    /// Whether the Worktrees page is on screen: a search in front of it isn't.
    pub(super) fn showing_worktrees(&self) -> bool {
        self.query.is_empty() && self.history[self.at].shows == Shows::Worktrees
    }
    /// Whether a page that isn't a listing, the Overview, Worktrees, AirDrop
    /// or Find Duplicates, is on screen.
    pub(super) fn showing_page(&self) -> bool {
        self.showing_overview()
            || self.showing_worktrees()
            || self.showing_airdrop()
            || self.showing_localsend()
            || self.showing_duplicates()
    }
    /// Whether the rows are a folder's, rather than search results or Recents.
    pub(super) fn showing_folder(&self) -> bool {
        self.query.is_empty() && self.history[self.at].shows == Shows::Folder
    }
    pub(super) fn in_trash(&self, cx: &gpui::App) -> bool {
        self.showing_folder()
            && cx
                .try_global::<Places>()
                .is_some_and(|places| places.trash == self.folder())
    }
    /// Opens the Trash in the list, so Back returns from it.
    pub(super) fn open_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(trash) = cx.try_global::<Places>().map(|places| places.trash.clone()) {
            self.go(trash, window, cx);
        }
    }
    /// Opens the list of recently opened files, so Back returns from it. The
    /// folder behind it stays what Go to Folder and Open in Terminal start at.
    pub(super) fn open_recents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(Shows::Recents, window, cx);
    }
    /// A page in front of the folder, as a place in the history.
    pub(super) fn open_page(&mut self, shows: Shows, window: &mut Window, cx: &mut Context<Self>) {
        self.worktrees.suggested.clear();
        if self.stop().shows != shows {
            self.collapse_all();
            self.history.truncate(self.at + 1);
            let mut stop = Stop::folder(self.disk_folder().to_path_buf());
            stop.shows = shows;
            self.history.push(stop);
            self.at = self.history.len() - 1;
        }
        self.show_folder(cx);
        self.focus_list(window, cx);
    }
    pub(super) fn showing_airdrop(&self) -> bool {
        self.query.is_empty() && self.stop().shows == Shows::AirDrop
    }
    pub(super) fn showing_localsend(&self) -> bool {
        self.query.is_empty() && self.stop().shows == Shows::LocalSend
    }
    /// Whether the page is on screen: a search in front of it isn't.
    pub(super) fn showing_duplicates(&self) -> bool {
        self.query.is_empty() && self.stop().shows == crate::session::Shows::Duplicates
    }
    /// Whether the rows on screen are an archive's. That follows the rows,
    /// not where the pane is going, so they stay read only until the
    /// listing that replaces them lands.
    pub(super) fn showing_archive(&self) -> bool {
        self.archived.is_some()
    }
    /// Whether a server folder is on screen: a search in front of it isn't.
    pub(super) fn showing_remote(&self) -> bool {
        self.query.is_empty() && self.stop().shows == Shows::Remote
    }
}

/// What on disk the listing follows, in FSEvents' real paths.
#[derive(Default)]
pub(super) struct Watch {
    /// The folder and each open folder in it, by the path shown.
    pub(super) folders: HashMap<PathBuf, PathBuf>,
    /// The folder's parent: renaming or deleting the folder changes it.
    parent: Option<PathBuf>,
}
impl Watch {
    pub(super) fn touches(&self, changed: &Path, below: bool) -> bool {
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
    pub(super) fn walk(
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

impl Pane {
    /// `reload`, but `quiet` keeps the view where it is: a refresh after the
    /// disk changed mustn't scroll.
    pub(super) fn list(&mut self, quiet: bool, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        if !quiet {
            self.remember = Task::ready(());
        }
        self.asking.clear();
        if self.showing_localsend() && cx.global::<Preferences>().localsend {
            self.enter_localsend(cx);
        } else {
            self.leave_localsend(cx);
        }
        if self.showing_remote() {
            // Nothing local is on screen, so nothing local can be chosen.
            self.leave_overview();
            self.leave_duplicates();
            self.loading = Task::ready(());
            self.listing = false;
            self.root.clear();
            self.children.clear();
            self.entries.clear();
            self.depths.clear();
            self.selection.clear();
            self.matches = Matches::default();
            self.error = None;
            self.unavailable = None;
            self.watch = Watch::default();
            self.renaming = None;
            self.archived = None;
            return self.list_remote(quiet, cx);
        }
        self.remote.leave();
        if !self.showing_folder() {
            self.partly_downloaded.clear();
        }
        if self.showing_overview() {
            self.read_overview(cx);
        } else {
            self.leave_overview();
        }
        if self.showing_worktrees() {
            self.scan_worktrees(cx);
        }
        if self.showing_airdrop() {
            self.read_airdrop(cx);
        }
        if self.showing_duplicates() {
            self.read_duplicates(cx);
        } else {
            self.leave_duplicates();
        }
        let mut folder = self.folder().to_path_buf();
        let query = self.query.clone();
        let search = self.current_search();
        // It reads files, so typing doesn't start one per key.
        let settle = (search.looks == Looks::Contents || query.contains("contains:"))
            .then(|| cx.background_executor().timer(CONTENT_SETTLE));
        let index = self.index.clone();
        let gate = cx.try_global::<IndexGate>().cloned();
        let overview = self.showing_page();
        let recents = self
            .showing_recents()
            .then(|| cx.try_global::<Places>().map(|places| places.store.clone()));
        // A restored place is checked once, before anything opens it.
        let check = self
            .stop()
            .place
            .clone()
            .filter(|_| self.stop().unverified && self.showing_folder());
        let drives = cx
            .try_global::<Places>()
            .map(|places| places.drives.clone());
        let searcher: Arc<dyn crate::system::Drives> = match &drives {
            Some(drives) => drives.clone(),
            None => Arc::new(crate::system::System),
        };
        let consented = std::mem::take(&mut self.consented);
        self.trash_here = self.in_trash(cx);
        let show_hidden = cx.global::<Preferences>().show_hidden;
        self.show_hidden = show_hidden;
        let sort = self.sort;
        let tidy = self.tidy_folders(cx);
        let peek = self.peek_hidden;
        let archive = self.stop().archive.clone().filter(|_| self.wants_archive());
        let cached = archive
            .as_deref()
            .and_then(|archive| self.cached_archive(archive));
        // The folder the keys move to in column view usually has a column
        // already, so its rows show now rather than the last folder's until
        // this listing lands.
        if !quiet
            && query.is_empty()
            && check.is_none()
            && self.layout() == View::Columns
            && let Some(listed) = self.children.get(&folder)
        {
            self.root = listed.clone();
            self.error = None;
            self.unavailable = None;
            self.relayout(false);
            let left = self.reselect.take();
            self.pick_selection(left);
            match self.cursor_ix() {
                Some(ix) => self.reveal(ix, ScrollStrategy::Center),
                None => self.scroll.scroll_to_item(0, ScrollStrategy::Top),
            }
        }
        // This listing covers the expanded folders too.
        self.expanding.clear();
        self.listing = true;
        let mut open: Vec<PathBuf> = self.expanded.iter().cloned().collect();
        if self.layout() == View::Columns {
            self.root_columns();
            open.extend(self.column_folders());
        }
        let work = cx.background_executor().spawn(async move {
            if !query.is_empty() {
                if let Some(settle) = settle {
                    settle.await;
                }
                #[expect(
                    clippy::disallowed_methods,
                    reason = "a search before launch started the index starts it"
                )]
                let started = || match &gate {
                    Some(gate) => gate.for_search(&index),
                    None => index
                        .get_or_init(Index::start)
                        .as_ref()
                        .map_err(Clone::clone),
                };
                let found = searcher.search(&started, &search, SEARCH_LIMIT)?;
                let (hits, matches) = Matches::of(found);
                return Ok((
                    hits,
                    HashMap::new(),
                    Watch::default(),
                    HashSet::new(),
                    None,
                    matches,
                    None,
                ));
            }
            if overview {
                return Ok((
                    Vec::new(),
                    HashMap::new(),
                    Watch::default(),
                    HashSet::new(),
                    None,
                    Matches::default(),
                    None,
                ));
            }
            if let Some(path) = archive {
                // Its name is enough: the path bar has the rest.
                let (open, stamp) = archive::read(&path, cached)
                    .map_err(|why| short_reason(&why, path.parent()))?;
                let inside = folder.strip_prefix(&path).unwrap_or(Path::new(""));
                let mut level = open.level(&inside.to_string_lossy());
                level
                    .entries
                    .retain(|entry| show_hidden || peek || !entry.name.starts_with('.'));
                entries::sort(&mut level.entries, sort);
                // The folder holding it changes when the archive does.
                let watch = Watch {
                    parent: path.parent().map(real),
                    folders: HashMap::new(),
                };
                let archived = archive::Archived {
                    refusal: open.refusal(),
                    claimed: open.claimed(),
                    open,
                    stamp,
                    problems: level.problems,
                };
                return Ok((
                    level.entries,
                    HashMap::new(),
                    watch,
                    HashSet::new(),
                    None,
                    Matches::default(),
                    Some(archived),
                ));
            }
            if let Some(store) = recents {
                let (rows, missing) =
                    crate::system::recent_entries(trash::read_recents(store.as_ref())?);
                return Ok((
                    rows,
                    HashMap::new(),
                    Watch::default(),
                    missing,
                    None,
                    Matches::default(),
                    None,
                ));
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
            let hidden = |dir: &Path| lists_hidden(dir, show_hidden || peek, &tidy);
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
            Ok((
                root,
                children,
                watch,
                HashSet::new(),
                verified,
                Matches::default(),
                None,
            ))
        });
        self.loading = cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.listing = false;
                match result {
                    Ok((root, children, watch, missing, verified, matches, archived)) => {
                        this.archived = archived;
                        if this.showing_folder() {
                            let listed = root.iter().chain(children.values().flatten());
                            this.ask_downloads(listed, cx);
                        }
                        this.root = root;
                        this.matches = matches;
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
                        this.archived = None;
                        this.root.clear();
                        this.matches = Matches::default();
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
                // The disk may have changed under the preview.
                this.preview_batch(cx);
                // A refresh leaves the view where it was.
                match this.cursor_ix() {
                    _ if quiet => {}
                    Some(ix) => this.reveal(ix, ScrollStrategy::Center),
                    None => this.scroll.scroll_to_item(0, ScrollStrategy::Top),
                }
                this.refresh_trash(cx);
                if !quiet {
                    this.remember_search(cx);
                }
                cx.notify();
            });
        });
    }
    /// Asks macOS, off the UI thread, which folders in `listed` hold
    /// something not downloaded, and badges them when it answers. An answer
    /// replaces only what it was asked, so a folder opened in place while a
    /// listing's answer is on its way keeps its own.
    pub(super) fn ask_downloads<'a>(
        &mut self,
        listed: impl IntoIterator<Item = &'a Entry>,
        cx: &mut Context<Self>,
    ) {
        let folders: Vec<Entry> = listed
            .into_iter()
            .filter(|entry| entry.kind == Kind::Dir)
            .cloned()
            .collect();
        let answer = cx.background_executor().spawn(async move {
            let partly = entries::partly_downloaded(&folders);
            (folders, partly)
        });
        self.asking.push(cx.spawn(async move |this, cx| {
            let (asked, partly) = answer.await;
            let _ = this.update(cx, |this, cx| {
                for folder in &asked {
                    this.partly_downloaded.remove(&folder.path);
                }
                this.partly_downloaded.extend(partly);
                cx.notify();
            });
        }));
    }
    /// Whether `entry` gets the cloud badge: it, or something in it, isn't
    /// downloaded.
    pub(super) fn in_cloud(&self, entry: &Entry) -> bool {
        entry.availability == icloud::Availability::Placeholder
            || self.partly_downloaded.contains(&entry.path)
    }
    /// Refreshes the listing shortly after the disk changes under a folder on
    /// screen, and runs a search again after a change beside one of its
    /// results.
    pub(super) fn disk_changed(&mut self, batch: &[(PathBuf, bool)], cx: &mut Context<Self>) {
        if !self.follows_disk() {
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
        if self.refresh.is_none() && self.touched(batch) {
            self.schedule_refresh(cx);
        }
    }
    /// Whether the rows on screen are read from the disk or the index, and
    /// so go stale when the disk changes.
    pub(super) fn follows_disk(&self) -> bool {
        self.showing_folder() || self.wants_archive() || !self.query.is_empty()
    }
    /// Whether any of `batch` changed a folder on screen, or the folder of
    /// one of a search's results.
    pub(super) fn touched(&self, batch: &[(PathBuf, bool)]) -> bool {
        let beside_results: HashSet<&Path> = if self.query.is_empty() {
            HashSet::new()
        } else {
            self.root
                .iter()
                .filter_map(|result| result.path.parent())
                .collect()
        };
        batch.iter().any(|(changed, below)| {
            self.watch.touches(changed, *below)
                || beside_results.contains(changed.as_path())
                || (*below && beside_results.iter().any(|dir| dir.starts_with(changed)))
        })
    }
    /// Checks the changes held during a listing against what it watches.
    pub(super) fn recheck_held(&mut self, cx: &mut Context<Self>) {
        if self.listing || !self.expanding.is_empty() {
            return;
        }
        let held = std::mem::take(&mut self.held);
        if self.touched(&held) {
            self.schedule_refresh(cx);
        }
    }
    /// One refresh `REFRESH_DELAY` after the first change of a burst, or
    /// `SEARCH_REFRESH_DELAY` for a search; changes meanwhile fold into it.
    pub(super) fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_some() {
            return;
        }
        let delay = if self.query.is_empty() {
            REFRESH_DELAY
        } else {
            SEARCH_REFRESH_DELAY
        };
        let wait = cx.background_executor().timer(delay);
        self.refresh = Some(cx.spawn(async move |this, cx| {
            wait.await;
            let _ = this.update(cx, |this, cx| {
                this.refresh = None;
                // A listing that started since reads the disk after the
                // change; it rechecks what it holds when it lands.
                if this.follows_disk() && !this.listing {
                    if !this.query.is_empty() {
                        this.keep_selection = true;
                    }
                    this.list(true, cx);
                }
            });
        }));
    }
}

impl Pane {
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
        let only = self.selection.only().filter(|_| !self.showing_archive())?;
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
        let only = self.selection.only().filter(|_| !self.showing_archive())?;
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
        let below = match self.layout() {
            View::List | View::Columns => {
                let height = self.row_pitch(cx);
                let row = self.matches.row_of(ix);
                point(px(60.), px(4. + height * (row as f32 + 1.)))
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
    pub(super) fn context_target<E: InteractiveElement>(
        element: E,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> E {
        element.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.context_click(Some(ix), event.position, window, cx)
            }),
        )
    }
    /// Opens a file in its app and remembers that, or says it's gone.
    pub(super) fn open_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.missing.contains(path) {
            let said = format!("{} is no longer there.", crate::jobs::quoted(path));
            return self.toast(said, true, None, cx);
        }
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return cx.open_with_system(path);
        };
        places.drives.open(path, cx);
        let path = path.to_path_buf();
        let remembered = cx
            .background_executor()
            .spawn(async move { places.store.opened(&path) });
        cx.spawn(async move |this, cx| {
            if let Err(why) = remembered.await {
                let _ = this.update(cx, |this, cx| this.toast(why, true, None, cx));
            }
        })
        .detach();
    }
    /// From one list row's top to the next. Column view's rows have the gap
    /// stacked rows elsewhere do; the list's touch, as in Finder.
    pub(super) fn row_pitch(&self, cx: &App) -> f32 {
        let height = cx.global::<Preferences>().row_size.metrics().0;
        match self.layout() {
            View::Columns => height + theme::ROW_GAP,
            _ => height,
        }
    }
}

/// What a level of a path is called.
pub(in crate::browser) fn level_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "Macintosh HD".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl Pane {
    /// Opens or closes a folder in place. Opening lists it off the UI thread.
    pub(in crate::browser) fn toggle_folder(&mut self, path: PathBuf, cx: &mut Context<Self>) {
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
            let hidden = lists_hidden(&dir, show_hidden || peek, &tidy);
            let list = entries::list_dir(&dir, hidden, sort).unwrap_or_default();
            (list, real(&dir))
        });
        let task = cx.spawn(async move |this, cx| {
            let (list, watched) = list.await;
            let _ = this.update(cx, |this, cx| {
                if this.expanding.remove(&path).is_some() && this.expanded.contains(&path) {
                    this.ask_downloads(&list, cx);
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

    /// Asks for `paths`, and says why any fails, with Try Again.
    pub(in crate::browser) fn download(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let then = Self::download_ended(cx);
        self.downloads
            .update(cx, |downloads, cx| downloads.download(paths, then, cx));
    }
}
