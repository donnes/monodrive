//! The file browser window: a sidebar of places, a folder listing, and
//! search over the whole-disk index.

mod pane;
mod sidebar;
mod trash;

#[cfg(test)]
mod places_tests;
#[cfg(test)]
mod tests;

pub use pane::format_bytes;
pub use sidebar::{Drives, Places, System};
pub(crate) use sidebar::{glyph_of, label_of};
pub(crate) use trash::recent_entries;

use crate::commands::{self, OpenPalette, Scope};
use crate::connect::ConnectView;
use crate::empty_trash::{self, EmptyTrashView};
use crate::goto::{GotoEvent, GotoView};
use crate::jobs::{self, Jobs, JobsEvent};
use crate::menu::{ContextMenu, MenuEvent};
use crate::palette::{Now, PaletteEvent, PaletteView};
use crate::preferences::{self, Preferences, SIDEBAR_WIDTHS, View, WorkspaceStyle};
use crate::quick_look::{self, Preview};
use crate::settings::{self, SettingsView};
use crate::text_field;
use crate::theme::{self, ButtonLook, Elevation, accent, content, danger, icon_button};
use crate::traffic_lights;
use gpui::{
    App, Context, CursorStyle, DispatchPhase, Entity, FocusHandle, Focusable, FontWeight, Global,
    KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Task, Window, WindowAppearance, WindowControlArea, actions, canvas, div, prelude::*, px,
};
use monodrive_core::entries::SortBy;
use monodrive_core::index::{Index, Progress, Status};
use monodrive_core::ops::{self, Done, Job, Request};
use pane::{Chrome, Pane, PaneEvent, sorted_by};
use std::path::{Path, PathBuf};
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
        TogglePathBar,
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
        QuickLook,
        ShowContextMenu,
        AddToSidebar,
        RemoveFromSidebar,
        Eject,
        ClearRecents,
        PutBack,
        GoRecents,
        GoTrash,
        ConnectToServer,
        EditFavorites,
        FavoriteUp,
        FavoriteDown,
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
        // In Edit mode, the keys' way to do what dragging a handle does.
        KeyBinding::new("alt-up", FavoriteUp, Some("Sidebar")),
        KeyBinding::new("alt-down", FavoriteDown, Some("Sidebar")),
        // → steps from the sidebar into the files, as it does into a folder.
        KeyBinding::new("right", FocusList, Some("Sidebar")),
        // The context menu on the selection, as on other desktops. GPUI's
        // macOS keys have no name for a PC keyboard's Menu key.
        KeyBinding::new("shift-f10", ShowContextMenu, Some("FileList")),
    ]);
}

/// How long a window move or resize must settle before its frame is kept.
const FRAME_SETTLE: Duration = Duration::from_millis(300);
/// How often the sidebar checks on an index that is still building.
const INDEX_POLL: Duration = Duration::from_millis(500);
/// How long a toast stays.
const TOAST_FOR: Duration = Duration::from_secs(4);
/// The pane's margin and border across when it's a card.
const PANE_CARD_INSET: f32 = 10.;

pub struct Browser {
    /// Keeps the browser's keys working wherever focus is.
    focus: FocusHandle,
    sidebar_focus: FocusHandle,
    settings_focus: FocusHandle,
    /// The place the sidebar's keys are on, apart from the folder shown.
    sidebar_cursor: usize,
    /// What had focus when a sheet (Settings or the palette) opened.
    focus_before_sheet: Option<FocusHandle>,
    /// What waits for the first frame drawn without the sheet.
    after_sheet: Option<AfterSheet>,
    /// What commands would act on as the palette opened.
    palette_target: Option<PaletteTarget>,
    pane: Entity<Pane>,
    /// Started with the window; searches wait on it.
    index: Arc<OnceLock<Result<Index, String>>>,
    /// Saves the window's frame once a move or resize settles.
    frame_save: Task<()>,
    /// Hands disk changes from fsearch's thread to this view.
    _changes: Task<()>,
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
    /// One sheet at a time: opening one replaces the other.
    sheet: Option<Sheet>,
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
    /// The context menu, while it has the keys.
    menu: Option<OpenMenu>,
    /// A context menu asked for, opened once the next frame has drawn.
    menu_request: Option<MenuRequest>,
    /// What Quick Look was last asked to show, while it's open.
    preview: Option<Preview>,
    /// The Quick Look panel, from the first time it opens.
    quick_look: Option<quick_look::Panel>,
    /// What the sidebar lists.
    sidebar: sidebar::Sidebar,
}

impl Browser {
    pub fn new(folder: PathBuf, cx: &mut Context<Self>) -> Self {
        let jobs = cx.new(|_| Jobs::new());
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
        // Whatever moves the selection repaints, so Quick Look follows it here.
        cx.observe_self(|this, cx| this.follow_selection(cx))
            .detach();
        let index: Arc<OnceLock<Result<Index, String>>> = Arc::default();
        let pane = {
            let index = index.clone();
            cx.new(|cx| Pane::new(folder, index, cx))
        };
        cx.subscribe(&pane, Self::pane_event).detach();
        cx.observe(&pane, |_, _, cx| cx.notify()).detach();
        let mut browser = Self {
            focus: cx.focus_handle(),
            sidebar_focus: cx.focus_handle().tab_stop(true),
            settings_focus: cx.focus_handle().tab_stop(true),
            sidebar_cursor: 0,
            focus_before_sheet: None,
            after_sheet: None,
            palette_target: None,
            pane,
            index,
            frame_save: Task::ready(()),
            _changes: Task::ready(()),
            toast: None,
            jobs,
            jobs_focus: cx.focus_handle(),
            jobs_wanted: false,
            sheet: None,
            glass: None,
            index_status: None,
            _index_watch: Task::ready(()),
            index_hovered: false,
            resizing_sidebar: None,
            menu: None,
            menu_request: None,
            preview: None,
            quick_look: None,
            sidebar: sidebar::Sidebar::default(),
        };
        browser.load_volumes(cx);
        browser
    }

    /// The pane commands act on.
    fn pane(&self) -> &Entity<Pane> {
        &self.pane
    }

    /// What the window tells `pane` this frame.
    fn chrome_for(&self, pane: Entity<Pane>, width: f32, cx: &App) -> Chrome {
        let pane = pane.read(cx);
        let volume = pane
            .showing_folder()
            .then(|| monodrive_core::volumes::holding(&self.sidebar.volumes, pane.folder()))
            .flatten();
        Chrome {
            width,
            covered: self.sheet_open() || self.menu.is_some(),
            menu_here: self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.origin == pane.list_focus),
            space: volume
                .filter(|volume| volume.total > 0)
                .map(|volume| format!("{} free", format_bytes(volume.free))),
            ejectable: self.ejectable_here(cx),
        }
    }

    /// What a pane asks of its window.
    fn pane_event(&mut self, _: Entity<Pane>, event: &PaneEvent, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Toast { text, failed, undo } => self.toast(text.clone(), *failed, *undo, cx),
            PaneEvent::ClearToast => self.toast = None,
            PaneEvent::Follow(job) => self.jobs.update(cx, |jobs, cx| jobs.follow(*job, cx)),
            PaneEvent::VolumesChanged => self.load_volumes(cx),
            PaneEvent::Menu { scopes, at } => {
                let origin = self.pane().read(cx).list_focus.clone();
                self.request_menu(scopes.clone(), *at, origin, cx);
            }
            PaneEvent::Eject(mount) => self.eject(mount.clone(), cx),
        }
    }

    /// A listing or search is on its way.
    pub fn is_listing(&self, cx: &App) -> bool {
        self.pane().read(cx).listing
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
                                .update(cx, |this, cx| {
                                    this.pane()
                                        .update(cx, |pane, cx| pane.disk_changed(&batch, cx))
                                })
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
                        if ready {
                            this.pane().update(cx, |pane, cx| {
                                if !pane.query.is_empty() {
                                    pane.reload(cx);
                                }
                            });
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
                            .text_size(px(theme::TEXT_CAPTION))
                            .line_height(px(14.))
                            .text_color(content(0.65))
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
                                        .text_color(content(0.65))
                                        .child("No Full Disk Access"),
                                )
                            }),
                    )
                } else {
                    d.child(
                        theme::progress_track()
                            .w(px(72.))
                            .children(status.fraction().map(theme::progress_fill)),
                    )
                }
            });
        Some(bar)
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
        if let Some(ix) = self.cursor_on_folder(cx) {
            self.move_sidebar_cursor(ix, cx);
        }
    }

    /// Space and ⌘Y: Quick Look on the selection, or closes it.
    fn toggle_quick_look(&mut self, window: &mut Window, cx: &App) {
        if self.close_quick_look() {
            return;
        }
        let pane = self.pane().read(cx);
        let Some(preview) = Preview::of(&pane.selection, &pane.entries) else {
            return;
        };
        if self.quick_look.is_none() {
            self.quick_look = quick_look::Panel::new(window);
        }
        if let Some(panel) = &self.quick_look {
            panel.show(&preview);
        }
        self.preview = Some(preview);
    }

    /// Closes Quick Look, saying whether it was open.
    fn close_quick_look(&mut self) -> bool {
        // The panel also closes itself, on Esc or its close button.
        let open = self.preview.take().is_some()
            && self.quick_look.as_ref().is_none_or(|panel| panel.is_open());
        if open && let Some(panel) = &self.quick_look {
            panel.close();
        }
        open
    }

    /// While Quick Look is open it shows what's selected now, and closes
    /// when nothing is, as on going into another folder.
    fn follow_selection(&mut self, cx: &App) {
        let Some(shown) = &self.preview else {
            return;
        };
        let pane = self.pane().read(cx);
        let preview = Preview::of(&pane.selection, &pane.entries);
        if preview.as_ref() == Some(shown) {
            return;
        }
        // Not if the panel closed itself meanwhile.
        let open = self.quick_look.as_ref().is_none_or(|panel| panel.is_open());
        match preview.filter(|_| open) {
            Some(preview) => {
                if let Some(panel) = &self.quick_look {
                    panel.show(&preview);
                }
                self.preview = Some(preview);
            }
            None => {
                self.close_quick_look();
            }
        }
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

    /// Typing in the sidebar searches, as in the list.
    fn type_to_search(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pane()
            .update(cx, |pane, cx| pane.type_to_search(event, window, cx));
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
            .unwrap_or_else(|| self.pane().read(cx).list_focus.clone());
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

    fn open_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Goto(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self.remember_focus(window, cx);
        let folder = self.pane().read(cx).folder().to_path_buf();
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
                this.pane()
                    .update(cx, |pane, cx| pane.go_to(folder, select, window, cx));
            }
        })
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::Goto(view)), cx);
    }

    /// Opens the palette on what would run now, which is read off the frame
    /// drawn last. Another sheet closes first, and the palette waits for it.
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Palette(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        if self.sheet_open() {
            self.close_sheet(window, cx);
            self.after_sheet = Some(AfterSheet::OpenPalette);
            return;
        }
        self.remember_focus(window, cx);
        let origin = self
            .focus_before_sheet
            .clone()
            .unwrap_or_else(|| self.pane().read(cx).list_focus.clone());
        // Quit and the like listen on the app, where only this list finds them.
        let anywhere = window.available_actions(cx);
        let available = commands::all()
            .iter()
            .map(|command| (command.action)())
            .filter(|action| {
                window.is_action_available_in(&**action, &origin)
                    || anywhere.iter().any(|a| a.name() == action.name())
            })
            .map(|action| action.name())
            .collect();
        let now = Now {
            available,
            scopes: self.pane().update(cx, |pane, _| pane.scopes()),
            favorites: self.sidebar.places.clone(),
            volumes: self.sidebar.volumes.clone(),
            servers: self.sidebar.servers.clone(),
            index: self.index.clone(),
        };
        self.palette_target = Some(self.target(window, cx));
        let view = cx.new(|cx| PaletteView::new(now, cx));
        cx.subscribe_in(&view, window, Self::palette_event).detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::Palette(view)), cx);
    }

    /// What commands would act on now.
    fn target(&self, window: &Window, cx: &App) -> PaletteTarget {
        let pane = self.pane().read(cx);
        let mut chosen = pane.chosen();
        chosen.sort();
        PaletteTarget {
            folder: pane.folder().to_path_buf(),
            chosen,
            sidebar: self
                .sidebar_focus
                .is_focused(window)
                .then(|| self.sidebar_target())
                .flatten(),
        }
    }

    /// Runs a command chosen in the palette, unless it would act on something
    /// other than what was there when the palette opened: a job that finishes
    /// meanwhile selects what it made, and that isn't what the person chose for.
    fn run_from_palette(
        &mut self,
        action: Box<dyn gpui::Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opened_on = self.palette_target.take();
        if commands::acts_on_target(action.name())
            && opened_on.is_some_and(|target| target != self.target(window, cx))
        {
            let said = "The selection changed while the palette was open.";
            return self.toast(said.into(), true, None, cx);
        }
        window.dispatch_action(action, cx);
    }

    /// What the palette asked for, once it has closed and focus is back.
    fn palette_event(
        &mut self,
        _: &Entity<PaletteView>,
        event: &PaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_sheet(window, cx);
        match event {
            PaletteEvent::Dismissed => {}
            PaletteEvent::Run(action) => {
                self.after_sheet = Some(AfterSheet::Run(action.boxed_clone()));
            }
            PaletteEvent::Go(folder) => self
                .pane()
                .update(cx, |pane, cx| pane.go_to(folder.clone(), None, window, cx)),
            PaletteEvent::Open(file) => self.pane().update(cx, |pane, cx| pane.open_file(file, cx)),
            PaletteEvent::Eject(mount) => self.eject(mount.clone(), cx),
            PaletteEvent::Connect(address) => self.connect_to(address, window, cx),
            PaletteEvent::Say(text) => self.toast(text.clone(), false, None, cx),
        }
    }

    /// A paste or Move Here finished: what it made is selected, if it's
    /// here, and a toast offers Undo.
    fn job_finished(&mut self, job: Job, cx: &mut Context<Self>) {
        let undo = job.undoable.then_some(job.run);
        self.toast(jobs::title(&job), false, undo, cx);
        // A job's bytes may have moved between volumes.
        self.load_volumes(cx);
        self.pane().update(cx, |pane, cx| {
            if pane.showing_folder() && job.into == pane.folder() {
                let made = job
                    .tasks
                    .into_iter()
                    .filter_map(|task| match task.done {
                        Some(Done::To(path)) => Some(path),
                        _ => None,
                    })
                    .collect();
                pane.select_on_listing(made);
                pane.list(false, cx);
            }
        });
    }

    /// Empty Trash: erases what's in the Trash for good, once a person has
    /// confirmed how many items that is.
    fn empty_trash(&mut self, window: &Window, cx: &mut Context<Self>) {
        with_owner(
            window,
            cx,
            |owner| owner.trash_items(),
            |this, items, window, cx| match items {
                Ok(items) if items.is_empty() => {
                    this.toast("The Trash is empty.".into(), false, None, cx)
                }
                Ok(items) => this.ask_empty_trash(items, window, cx),
                Err(e) => this.toast(e, true, None, cx),
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
                    this.pane().update(cx, |pane, cx| {
                        pane.submit(Request::EmptyTrash { items }, window, cx)
                    });
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::EmptyTrash(view)), cx);
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

    /// One job: where am I, among which tabs. The folder's name is where the
    /// first tab will go, so it stays put when tabs arrive.
    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let pane = self.pane().read(cx);
        let title = pane.title(cx);
        let (back, forward) = (pane.at > 0, pane.at + 1 < pane.history.len());
        div()
            .h(px(theme::TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .pr_3()
            .child(
                icon_button("back", "Back", "chevron-left", back).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.pane().update(cx, |pane, cx| pane.step_history(-1, cx));
                    },
                )),
            )
            .child(
                icon_button("forward", "Forward", "chevron-right", forward).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.pane().update(cx, |pane, cx| pane.step_history(1, cx));
                    },
                )),
            )
            .child(
                div()
                    .ml_1()
                    .px(px(6.))
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(drag_area().flex_1().h_full())
            .children(self.jobs.read(cx).ring().map(|ring| {
                ring.on_click(cx.listener(|this, _, window, cx| this.toggle_jobs(window, cx)))
            }))
    }

    /// The toast at the bottom of the files, with its Undo.
    fn toast_view(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let toast = self.toast.as_ref()?;
        // Above the path bar, if it's showing.
        let bar = if cx.global::<Preferences>().show_path_bar {
            theme::PATH_BAR_HEIGHT
        } else {
            0.
        };
        let ink = if toast.failed { danger() } else { content(1.) };
        Some(
            div()
                .absolute()
                .bottom(px(16. + bar))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    theme::floating(Elevation::Popover)
                        .id("toast")
                        .occlude()
                        .max_w(px(520.))
                        .mx_4()
                        .h(px(36.))
                        .pl_3()
                        .pr(px(6.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .when(toast.failed, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .rounded(px(theme::RADIUS_CARD - 1.))
                                    .bg(danger().opacity(0.12)),
                            )
                        })
                        .child(
                            div()
                                .relative()
                                .min_w_0()
                                .truncate()
                                .text_size(px(theme::TEXT_BODY))
                                .text_color(ink)
                                .child(toast.text.clone()),
                        )
                        .when_some(toast.undo, |d, op| {
                            d.child(
                                theme::text_button("toast-undo", "Undo", ButtonLook::Ghost)
                                    .debug_selector(|| "toast-undo".into())
                                    .relative()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(accent())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.toast = None;
                                        this.pane().update(cx, |pane, cx| {
                                            pane.submit(Request::UndoOf { op }, window, cx)
                                        });
                                    })),
                            )
                        })
                        .when(toast.undo.is_none(), |d| d.pr_3()),
                ),
        )
    }

    /// Says `text` at the bottom of the files for `TOAST_FOR`, in red if
    /// it's a failure, with Undo of operation `undo` if given. It replaces
    /// the last one.
    fn toast(&mut self, text: String, failed: bool, undo: Option<u64>, cx: &mut Context<Self>) {
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
}

impl Focusable for Browser {
    /// The file list, so the arrows work as soon as the window opens; typing
    /// there still searches.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.pane().read(cx).list_focus.clone()
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
        if let Some(next) = self.after_sheet.take() {
            cx.defer_in(window, move |this, window, cx| match next {
                AfterSheet::Run(action) => this.run_from_palette(action, window, cx),
                AfterSheet::OpenPalette => this.open_palette(window, cx),
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
        // Only the sheet's controls take Tab while it's open.
        let sheet = self.sheet_open();
        let regions = !sheet;
        self.sidebar_focus.clone().tab_stop(regions);
        self.settings_focus.clone().tab_stop(regions);
        let width = pane_room(window, cx);
        let chrome = self.chrome_for(self.pane().clone(), width, cx);
        let (lone_folder, renaming, selected) = self.pane().update(cx, |pane, cx| {
            pane.chrome = chrome;
            pane.search.focus_handle(cx).tab_stop(regions);
            pane.list_focus.clone().tab_stop(regions);
            (
                pane.lone_folder(),
                pane.renaming.is_some(),
                !pane.selection.is_empty(),
            )
        });
        let root = div()
            // A sheet or the context menu takes the keys while it's open:
            // the menu's commands act on what it was opened on.
            .when(!sheet && self.menu.is_none(), |d| d.key_context("Browser"))
            .track_focus(&self.focus);
        // What acts on a pane goes to the one the commands are for.
        let root = self
            .pane()
            .update(cx, |pane, cx| pane.commands(root, sheet, cx));
        root
            // While a sheet is open, its commands (from the menu bar too) wait.
            .when(!sheet, |d| {
                d.on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                    window.focus_next(cx);
                    this.sync_sidebar_cursor(window, cx);
                }))
                .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| {
                    window.focus_prev(cx);
                    this.sync_sidebar_cursor(window, cx);
                }))
                .on_action(cx.listener(|this, _: &ConnectToServer, window, cx| {
                    this.open_connect(window, cx)
                }))
                .on_action(cx.listener(|this, _: &EditFavorites, _, cx| this.toggle_editing(cx)))
                // Esc closes Quick Look first, then leaves search, then lets
                // go of the selection.
                .on_action(cx.listener(|this, _: &ClearSearch, window, cx| {
                    this.toast = None;
                    if this.close_quick_look() {
                        return;
                    }
                    if this.sidebar.editing {
                        return this.toggle_editing(cx);
                    }
                    this.pane()
                        .update(cx, |pane, cx| pane.clear_search(window, cx));
                }))
                .on_action(
                    cx.listener(|this, _: &EmptyTrash, window, cx| this.empty_trash(window, cx)),
                )
            })
            .when(!sheet && !renaming, |d| {
                d.when_some(lone_folder, |d, path| {
                    d.on_action(cx.listener(move |this, _: &AddToSidebar, _, cx| {
                        this.add_to_sidebar(path.clone(), cx)
                    }))
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
                // Not while a name is edited, so Space types there.
                .when(!renaming && selected, |d| {
                    d.on_action(cx.listener(|this, _: &QuickLook, window, cx| {
                        this.toggle_quick_look(window, cx)
                    }))
                })
                .on_action(cx.listener(
                    |this, _: &ShowCopiesAndMoves, window, cx| this.toggle_jobs(window, cx),
                ))
            })
            .on_action(|_: &ToggleHiddenFiles, _, cx| {
                preferences::update(cx, |p| p.show_hidden = !p.show_hidden)
            })
            .on_action(|_: &TogglePathBar, _, cx| {
                preferences::update(cx, |p| p.show_path_bar = !p.show_path_bar)
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
                                        .rounded(px(theme::RADIUS_CARD))
                                        .border_1()
                                        .bg(theme::pane_fill())
                                }
                            })
                            .overflow_hidden()
                            .flex()
                            .flex_col()
                            .child(self.pane().clone())
                            .children(self.toast_view(cx)),
                    ),
            )
            .children(self.sheet.as_ref().map(|sheet| match sheet {
                Sheet::Settings(view) => view.clone().into_any_element(),
                Sheet::Palette(view) => view.clone().into_any_element(),
                Sheet::Goto(view) => view.clone().into_any_element(),
                Sheet::Jobs => self.jobs_popover(cx).into_any_element(),
                Sheet::EmptyTrash(view) => view.clone().into_any_element(),
                Sheet::Connect(view) => view.clone().into_any_element(),
            }))
            .children(self.menu.as_ref().map(|menu| menu.view.clone()))
            // Last, so its window listeners run before any element's.
            .children(
                self.resizing_sidebar
                    .map(|offset| self.sidebar_drag(offset, cx)),
            )
    }
}

/// How wide the panes' area is, inside the sidebar and the card's margin and
/// borders.
fn pane_room(window: &Window, cx: &App) -> f32 {
    let prefs = cx.global::<Preferences>();
    let margins = match prefs.workspace_style {
        WorkspaceStyle::Rounded => PANE_CARD_INSET,
        WorkspaceStyle::Boxy => 1.,
    };
    window.viewport_size().width.as_f32() - sidebar_width(prefs.sidebar_width, window) - margins
}

/// Runs `work` with the operation owner off the UI thread, then `done` with
/// what it returned. Without an owner, as in benchmarks, nothing runs and it
/// returns false.
fn with_owner<V: 'static, T: Send + 'static>(
    window: &Window,
    cx: &mut Context<V>,
    work: impl FnOnce(&mut ops::Client) -> Result<T, String> + Send + 'static,
    done: impl FnOnce(&mut V, Result<T, String>, &mut Window, &mut Context<V>) + 'static,
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
    /// Offers Undo of this operation, which the owner refuses once
    /// anything else has changed files since.
    undo: Option<u64>,
    _expire: Task<()>,
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
    Connect(Entity<ConnectView>),
}

/// What a command chosen in the palette would act on.
#[derive(PartialEq)]
struct PaletteTarget {
    folder: PathBuf,
    /// What's selected, whatever order the rows are in.
    chosen: Vec<PathBuf>,
    /// The sidebar's row, while the sidebar has the keys.
    sidebar: Option<sidebar::RowTarget>,
}

/// What needs the browser's commands back. A frame with a sheet holds them
/// off, and running a command or checking what would run reads the drawn frame.
enum AfterSheet {
    Run(Box<dyn gpui::Action>),
    OpenPalette,
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

pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}
