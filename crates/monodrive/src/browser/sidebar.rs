//! The sidebar: the Overview, Recents and the favorites from `places::Store`,
//! the cloud folders and the mounted volumes with how full each is, and the
//! Trash and Settings at its foot.
//! Everything shown is read off the UI thread and read again when a watch
//! hears a change, so nothing here polls.

mod rows;

use super::pane::Stop;
use super::pane::drag::{Spot, Target, offers};
use super::{
    Browser, FocusList, GoFavorite1, GoFavorite2, GoFavorite3, GoFavorite4, GoFavorite5,
    GoFavorite6, GoFavorite7, GoFavorite8, GoFavorite9, GoFavorite10, MovePlaceDown, MovePlaceUp,
    OpenInNewTab, OpenPlace, OpenSelected, PlaceNext, PlacePrevious, sidebar_width,
};
use crate::Operations;
use crate::assets::NIGHT_SKY;
use crate::commands::{self, OpenSettings, Scope};
use crate::feedback::{self, Haptic};
use crate::format::format_bytes;
use crate::home;
use crate::lock;
use crate::motion::Slide;
use crate::preferences::{
    self, Collapsed, Drive, HiddenDrive, Location, Preferences, SIDEBAR_WIDTHS,
};
use crate::session::RemoteAt;
use crate::session::Shows;
use crate::system::disconnect;
use crate::system::{Places, glyph_of, label_of};
use crate::text_field::TextField;
use crate::theme::drag_area;
use crate::theme::{self, ButtonLook, accent, content, icon, icon_button};
use crate::traffic_lights;
use gpui::{
    Action, AnyElement, App, Context, CursorStyle, ElementId, Entity, Focusable, FontWeight,
    ImageSource, KeyDownEvent, Modifiers, MouseButton, MouseDownEvent, ObjectFit, Pixels, Point,
    Render, Resource, Role, Stateful, Subscription, Task, Window, div, img, prelude::*, px,
};
use monodrive_core::cloud::{CloudFolder, Quota};
use monodrive_core::places::Source;
use monodrive_core::places::{Availability, Place, Store};
use monodrive_core::updates::Channel;
use monodrive_core::volumes::Volume;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The sky behind a nightly's sidebar, as drawn in its SVG.
const NIGHT_SKY_HEIGHT: f32 = 150.;

/// What the sidebar lists, as last read.
#[derive(Default)]
pub(super) struct Sidebar {
    pub(super) places: Vec<Place>,
    /// Every mounted volume and cloud folder, hidden ones too. Read
    /// together, which is also when the window comes forward.
    pub(super) all_volumes: Vec<Volume>,
    pub(super) all_cloud: Vec<CloudFolder>,
    /// What each cloud folder that reports free space said, once it has.
    /// Asked after the folders are listed, since iCloud Drive can take a
    /// moment.
    cloud_free: HashMap<PathBuf, Quota>,
    /// What Locations lists: those Settings doesn't hide, or all of them
    /// while Locations is edited.
    pub(super) volumes: Vec<Volume>,
    pub(super) cloud: Vec<CloudFolder>,
    pub(super) servers: Vec<String>,
    /// The saved SFTP servers.
    pub(super) remotes: Vec<Source>,
    /// Locations' rows in the order they're drawn, each a volume, cloud
    /// folder, server, AirDrop or LocalSend.
    pub(super) locations: Vec<Row>,
    /// Why the saved lists didn't read. They stay as last read, and nothing
    /// changes the file.
    error: Option<String>,
    /// Edit mode: each favorite shows a handle and a ✕.
    pub(super) editing_favorites: bool,
    /// Edit mode: each location shows a handle, the drives an eye, and the
    /// hidden ones come back, dimmed.
    pub(super) editing_locations: bool,
    /// ⌘ alone was held at the last draw, so the favorites show their keys.
    pub(super) command_held: bool,
    /// Worktrees has a row, after Recents, as Settings says.
    pub(super) worktrees: bool,
    /// LocalSend has a row in Locations once Settings turns it on.
    pub(super) localsend: bool,
    /// The cloud folder whose name is being typed in its row.
    pub(super) renaming: Option<CloudRenaming>,
    /// The sections folded under their headings, whose rows the keys skip.
    pub(super) collapsed: Collapsed,
    load_volumes: Option<Task<()>>,
    load_lists: Option<Task<()>>,
    _heard: Vec<Task<()>>,
}

impl Sidebar {
    /// Nothing read yet, with the Worktrees row and the sections as saved.
    pub(super) fn new(worktrees: bool, localsend: bool, collapsed: Collapsed) -> Self {
        Self {
            worktrees,
            localsend,
            collapsed,
            ..Self::default()
        }
    }
}

/// A cloud folder's sidebar name being typed in place of it.
pub(super) struct CloudRenaming {
    path: PathBuf,
    field: Entity<TextField>,
    _blur: Subscription,
}

/// What the sidebar calls `folder`: the name given to it, else its own.
fn cloud_name<'a>(folder: &'a CloudFolder, cx: &'a App) -> &'a str {
    cx.global::<Preferences>()
        .cloud_names
        .get(&folder.path)
        .map_or(&folder.name, |name| name)
}

/// A row the sidebar's keys can land on, in order on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Overview,
    Recents,
    Worktrees,
    Favorite(usize),
    Cloud(usize),
    Volume(usize),
    AirDrop,
    LocalSend,
    Server(usize),
}

/// What a sidebar row stands for, wherever the rows above it have gone.
#[derive(PartialEq)]
pub(super) enum RowTarget {
    Overview,
    Recents,
    Worktrees,
    Favorite(PathBuf),
    Location(Location),
}

/// Where the pane that commands act on is, for marking its row.
struct Here {
    folder: PathBuf,
    shows: Shows,
    /// The server the pane shows, by source id.
    server: Option<String>,
    searching: bool,
    in_trash: bool,
}

impl Here {
    fn showing_folder(&self) -> bool {
        !self.searching && self.shows == Shows::Folder
    }
}

/// A favorite being dragged by its handle.
#[derive(Clone)]
struct DraggedFavorite {
    ix: usize,
    label: String,
}

impl Render for DraggedFavorite {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        dragged_row(self.label.clone())
    }
}

/// A location being dragged by its handle, from position `at` in Locations.
#[derive(Clone)]
struct DraggedLocation {
    at: usize,
    label: String,
}

impl Render for DraggedLocation {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        dragged_row(self.label.clone())
    }
}

fn dragged_row(label: String) -> gpui::Div {
    div()
        .h(px(theme::ROW_HEIGHT))
        .px_3()
        .rounded(px(theme::RADIUS_ROW))
        .flex()
        .items_center()
        .text_size(px(theme::TEXT_BODY))
        .text_color(content(1.))
        .bg(content(0.14))
        .child(label)
}

/// The handle an edited row is dragged by.
fn drag_handle(id: impl Into<ElementId>, selector: String) -> Stateful<gpui::Div> {
    theme::tip(
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .role(Role::Button)
            .aria_label("Drag to reorder")
            .flex_shrink_0()
            .cursor(CursorStyle::OpenHand)
            .child(icon("grip-vertical", 14.).text_color(content(0.55))),
        "Drag to reorder",
    )
}

/// How far a drag handle and its gap push an edited row's contents.
const HANDLE_WIDTH: f32 = 14. + 8.;

const ROW_HEIGHT: f32 = theme::SIDEBAR_ROW_HEIGHT;
const DRIVE_HEIGHT: f32 = 40.;
/// How far into a favorite its edge counts as between rows, as in Finder.
const PIN_EDGE: f32 = 8.;

/// A location's icon ink: dimmed while it's hidden.
fn glyph_ink(selected: bool, hidden: bool) -> gpui::Hsla {
    content(if hidden {
        0.3
    } else if selected {
        1.
    } else {
        0.55
    })
}

/// How full a volume is, from 0 to 1, if it says.
fn fullness(volume: &Volume) -> Option<f32> {
    (volume.total > 0)
        .then(|| (volume.total.saturating_sub(volume.free) as f64 / volume.total as f64) as f32)
}

/// Whether ⌘ alone is held here. GPUI rereads the keys and redraws when the
/// window activates, so a ⌘ let go in another app leaves nothing behind.
pub(super) fn command_held(window: &Window) -> bool {
    window.is_window_active() && window.modifiers() == Modifiers::command()
}

/// A small button in a sidebar row. It takes the press from the row under it.
fn row_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &'static str,
    glyph: &'static str,
) -> Stateful<gpui::Div> {
    theme::tip(
        theme::row_icon_button(id, label, glyph, content(0.55)),
        label,
    )
    // Its glyph, not its circle, ends on the row's text edge.
    .mr(px(-4.))
    .debug_selector(move || selector.clone())
}

impl Browser {
    /// Listens for what `volumes` and `lists` say changed, for as long as
    /// the window lasts; `keep` is whatever keeps them talking.
    pub fn hear_places(
        &mut self,
        keep: impl Sized + 'static,
        volumes: async_channel::Receiver<()>,
        lists: async_channel::Receiver<()>,
        cx: &mut Context<Self>,
    ) {
        let heard_volumes = cx.spawn(async move |this, cx| {
            let _keep = keep;
            while volumes.recv().await.is_ok() {
                while volumes.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.load_volumes(cx)).is_err() {
                    return;
                }
            }
        });
        let heard_lists = cx.spawn(async move |this, cx| {
            while lists.recv().await.is_ok() {
                while lists.try_recv().is_ok() {}
                let reloaded = this.update(cx, |this, cx| {
                    this.load_lists(cx);
                    this.relist_recents(cx);
                });
                if reloaded.is_err() {
                    return;
                }
            }
        });
        self.sidebar._heard = vec![heard_volumes, heard_lists];
    }

    /// Lists the volumes off the UI thread, then the lists that depend on
    /// them. The folder on screen gives way if its volume has gone.
    pub fn load_volumes(&mut self, cx: &mut Context<Self>) {
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        let drives = places.drives.clone();
        let listed = cx
            .background_executor()
            .spawn(async move { (drives.list(), drives.cloud()) });
        self.sidebar.load_volumes = Some(cx.spawn(async move |this, cx| {
            let (volumes, cloud) = listed.await;
            let folders = cloud.clone();
            let _ = this.update(cx, |this, cx| {
                let gone: Vec<_> = this
                    .sidebar
                    .all_volumes
                    .iter()
                    .filter(|old| !old.startup && !volumes.iter().any(|new| new.mount == old.mount))
                    .cloned()
                    .collect();
                let panes: Vec<_> = this
                    .tabs
                    .iter()
                    .flat_map(|tab| tab.panes())
                    .cloned()
                    .collect();
                for pane in panes {
                    // A restored place hasn't opened, so its drive leaving changes nothing.
                    if pane.read(cx).unchecked() {
                        continue;
                    }
                    let here = pane.read(cx).folder().to_path_buf();
                    let Some(volume) = gone.iter().find(|old| here.starts_with(&old.mount)) else {
                        continue;
                    };
                    if this.toast.is_none() {
                        let said = format!("{} was disconnected.", volume.name);
                        this.toast(said, false, None, cx);
                    }
                    pane.update(cx, |pane, cx| pane.navigate(home(), cx));
                }
                this.sidebar.all_volumes = volumes;
                this.sidebar.all_cloud = cloud;
                this.arrange_locations(cx);
                this.load_lists(cx);
                this.save_session(cx);
                // A drive that's back may be what a restored pane waits for.
                for pane in this.shown_panes() {
                    pane.update(cx, |pane, cx| {
                        if pane.waits_for_a_drive() {
                            pane.reload(cx);
                        }
                    });
                }
                cx.notify();
            });
            let free = cx
                .background_executor()
                .spawn(async move {
                    folders
                        .iter()
                        .filter_map(|folder| {
                            let free = places.drives.cloud_free(folder)?;
                            Some((folder.path.clone(), free))
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.sidebar.cloud_free = free;
                cx.notify();
            });
        }));
    }

    /// Reads the favorites and saved servers off the UI thread, checking
    /// each favorite against the volumes as last listed.
    pub(super) fn load_lists(&mut self, cx: &mut Context<Self>) {
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        let volumes = self.sidebar.all_volumes.clone();
        let read = cx.background_executor().spawn(async move {
            let favorites = places.store.places(&volumes);
            (favorites, places.store.servers(), places.store.remotes())
        });
        self.sidebar.load_lists = Some(cx.spawn(async move |this, cx| {
            let (favorites, saved, remotes) = read.await;
            let _ = this.update(cx, |this, cx| {
                let kept = this.cursor_location();
                let sidebar = &mut this.sidebar;
                sidebar.error = favorites
                    .as_ref()
                    .err()
                    .or(saved.as_ref().err())
                    .or(remotes.as_ref().err())
                    .cloned();
                if let Ok(favorites) = favorites {
                    sidebar.places = favorites;
                }
                if let Ok(saved) = saved {
                    sidebar.servers = saved;
                }
                if let Ok(remotes) = remotes {
                    sidebar.remotes = remotes;
                }
                this.arrange_locations_keeping(kept, cx);
                cx.notify();
            });
        }));
    }

    /// Changes a saved list off the UI thread, says `said` once it has or
    /// why it couldn't, and reads the lists again.
    pub(super) fn change_places(
        &mut self,
        cx: &mut Context<Self>,
        said: Option<String>,
        change: impl FnOnce(&Store, &[Volume]) -> Result<(), String> + Send + 'static,
    ) {
        let Some(store) = cx.try_global::<Places>().map(|places| places.store.clone()) else {
            return;
        };
        let volumes = self.sidebar.all_volumes.clone();
        let changed = cx
            .background_executor()
            .spawn(async move { change(&store, &volumes) });
        cx.spawn(async move |this, cx| {
            let changed = changed.await;
            let _ = this.update(cx, |this, cx| {
                match (changed, said) {
                    (Err(why), _) => this.toast(why, true, None, cx),
                    (Ok(()), Some(said)) => this.toast(said, false, None, cx),
                    (Ok(()), None) => {}
                }
                this.load_lists(cx);
                this.relist_recents(cx);
            });
        })
        .detach();
    }

    /// Recents changed: lists it again where it's on screen.
    fn relist_recents(&mut self, cx: &mut Context<Self>) {
        for pane in self.shown_panes() {
            pane.update(cx, |pane, cx| {
                if pane.showing_recents() {
                    pane.list(true, cx);
                }
            });
        }
    }

    /// Add to Sidebar: pins the folder selected, last, and says so, as the
    /// sidebar may be hidden.
    pub(super) fn add_to_sidebar(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let said = format!("Added {} to the sidebar", crate::jobs::quoted(&path));
        self.change_places(cx, Some(said), move |store, volumes| {
            store.pin(&path, None, volumes).map(|_| ())
        });
    }

    /// A drop between favorites: pins `paths` before the row at position `at`.
    pub(super) fn pin_folders(&mut self, paths: Vec<PathBuf>, at: usize, cx: &mut Context<Self>) {
        self.change_places(cx, None, move |store, volumes| {
            store.pin_before(&paths, at, volumes)
        });
    }

    /// Forgets favorite `ix`. Only the bookmark goes.
    fn remove_favorite(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix >= self.sidebar.places.len() {
            return;
        }
        let place = self.sidebar.places.remove(ix);
        cx.notify();
        let path = place.at().unwrap_or(&place.favorite.path).to_path_buf();
        self.change_places(cx, None, move |store, volumes| store.unpin(&path, volumes));
    }

    /// Moves favorite `from` to position `to`, on screen at once. One with
    /// no folder has only its saved self to go by.
    pub(super) fn move_favorite(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let places = &mut self.sidebar.places;
        if from == to || from >= places.len() || to >= places.len() {
            return;
        }
        let place = places.remove(from);
        places.insert(to, place.clone());
        cx.notify();
        self.change_places(cx, None, move |store, volumes| match place.at() {
            Some(path) => store.pin(path, Some(to), volumes).map(|_| ()),
            None => store.move_favorite(&place.favorite, to),
        });
    }

    pub(super) fn toggle_editing_favorites(&mut self, cx: &mut Context<Self>) {
        self.sidebar.editing_favorites = !self.sidebar.editing_favorites;
        cx.notify();
    }

    /// Editing Locations brings back the hidden drives, so they can be shown.
    pub(super) fn toggle_editing_locations(&mut self, cx: &mut Context<Self>) {
        self.sidebar.editing_locations = !self.sidebar.editing_locations;
        self.arrange_locations(cx);
        cx.notify();
    }

    /// Ends both sections' Edit sessions, saying whether one was on.
    pub(super) fn stop_editing(&mut self, cx: &mut Context<Self>) -> bool {
        let sidebar = &mut self.sidebar;
        let editing = sidebar.editing_favorites || sidebar.editing_locations;
        if sidebar.editing_locations {
            sidebar.editing_locations = false;
            self.arrange_locations(cx);
        }
        self.sidebar.editing_favorites = false;
        cx.notify();
        editing
    }

    /// Folds the sections as saved, in every window. Folding a section ends
    /// its Edit session. The rows under the keys may have gone or moved, so
    /// the keys go to the open place.
    pub(super) fn fold_sections(&mut self, collapsed: Collapsed, cx: &mut Context<Self>) {
        if self.sidebar.collapsed == collapsed {
            return;
        }
        self.sidebar.collapsed = collapsed;
        self.sidebar.editing_favorites &= !collapsed.favorites;
        if collapsed.locations && self.sidebar.editing_locations {
            self.sidebar.editing_locations = false;
            self.arrange_locations(cx);
        }
        self.sidebar_cursor = self.cursor_on_folder(cx).unwrap_or(0);
        cx.notify();
    }

    /// The first favorite's row: after the Overview, Recents and, when
    /// Settings shows it, Worktrees.
    fn first_favorite(&self) -> usize {
        if self.sidebar.worktrees { 3 } else { 2 }
    }

    /// The row of Locations' first: the startup disk.
    fn first_location(&self) -> usize {
        let favorites = if self.sidebar.collapsed.favorites {
            0
        } else {
            self.sidebar.places.len()
        };
        self.first_favorite() + favorites
    }

    /// The sidebar's last row the keys reach.
    fn last_row(&self) -> usize {
        if self.sidebar.collapsed.locations {
            self.first_location() - 1
        } else {
            self.first_location() + self.sidebar.locations.len().saturating_sub(1)
        }
    }

    /// Lists in Locations every drive and cloud folder Settings doesn't hide,
    /// or all of them while Locations is edited, the saved servers, AirDrop
    /// and LocalSend, in the order Edit gave them. Otherwise the startup disk
    /// leads, then the cloud folders, the other drives, the servers, AirDrop
    /// and LocalSend. The sidebar's keys
    /// stay on their location wherever it lands, and on a row when the last
    /// one goes.
    pub(super) fn arrange_locations(&mut self, cx: &App) {
        self.arrange_locations_keeping(self.cursor_location(), cx);
    }

    /// The location the sidebar's keys are on, read before its list changes.
    pub(super) fn cursor_location(&self) -> Option<Location> {
        self.location(self.row_at(self.sidebar_cursor)?)
    }

    /// Arranges Locations with the keys back on `kept`.
    pub(super) fn arrange_locations_keeping(&mut self, kept: Option<Location>, cx: &App) {
        let prefs = cx.try_global::<Preferences>();
        let hidden = prefs.map_or(&[][..], |p| &p.hidden_drives);
        let order = prefs.map_or(&[][..], |p| &p.location_order);
        let editing = self.sidebar.editing_locations;
        let shown = |drive: Drive| editing || !hidden.iter().any(|h| h.drive == drive);
        let sidebar = &mut self.sidebar;
        sidebar.volumes = (sidebar.all_volumes.iter())
            .filter(|v| shown(Drive::of(v)))
            .cloned()
            .collect();
        sidebar.cloud = (sidebar.all_cloud.iter())
            .filter(|c| shown(Drive::Path(c.path.clone())))
            .cloned()
            .collect();
        let startup = sidebar.volumes.iter().take_while(|v| v.startup).count();
        let mut rows: Vec<Row> = (0..startup)
            .map(Row::Volume)
            .chain((0..sidebar.cloud.len()).map(Row::Cloud))
            .chain((startup..sidebar.volumes.len()).map(Row::Volume))
            .chain((0..sidebar.remotes.len()).map(Row::Server))
            .chain([Row::AirDrop])
            .chain(sidebar.localsend.then_some(Row::LocalSend))
            .collect();
        rows.sort_by_cached_key(|row| {
            let location = self.location(*row);
            order
                .iter()
                .position(|saved| Some(saved) == location.as_ref())
                .unwrap_or(usize::MAX)
        });
        self.sidebar.locations = rows;
        let at = kept.and_then(|kept| {
            (self.sidebar.locations.iter())
                .position(|row| self.location(*row) == Some(kept.clone()))
        });
        if let Some(at) = at {
            self.sidebar_cursor = self.first_location() + at;
        }
        self.sidebar_cursor = self.sidebar_cursor.min(self.last_row());
    }

    /// What Locations' saved order calls `row`.
    fn location(&self, row: Row) -> Option<Location> {
        let sidebar = &self.sidebar;
        Some(match row {
            Row::Volume(ix) => Location::Drive(Drive::of(sidebar.volumes.get(ix)?)),
            Row::Cloud(ix) => Location::Drive(Drive::Path(sidebar.cloud.get(ix)?.path.clone())),
            Row::Server(ix) => Location::Server(sidebar.remotes.get(ix)?.id.clone()),
            Row::AirDrop => Location::AirDrop,
            Row::LocalSend => Location::LocalSend,
            Row::Overview | Row::Recents | Row::Worktrees | Row::Favorite(_) => return None,
        })
    }

    /// Moves the location at position `from` in Locations to `to`, in every
    /// window. A drive that isn't mounted keeps its place after the rest.
    pub(super) fn move_location(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let len = self.sidebar.locations.len();
        if from == to || from >= len || to >= len {
            return;
        }
        let mut rows = self.sidebar.locations.clone();
        let row = rows.remove(from);
        rows.insert(to, row);
        let mut order: Vec<Location> = rows
            .into_iter()
            .filter_map(|row| self.location(row))
            .collect();
        preferences::update(cx, |p| {
            p.location_order.retain(|saved| !order.contains(saved));
            order.append(&mut p.location_order);
            p.location_order = order;
        });
    }

    /// The drive or cloud folder `row` stands for, to hide or show.
    fn hideable(&self, row: Row, cx: &App) -> Option<HiddenDrive> {
        match row {
            Row::Volume(ix) => self.sidebar.volumes.get(ix).map(|volume| HiddenDrive {
                name: volume.name.clone(),
                drive: Drive::of(volume),
            }),
            Row::Cloud(ix) => self.sidebar.cloud.get(ix).map(|folder| HiddenDrive {
                name: cloud_name(folder, cx).to_owned(),
                drive: Drive::Path(folder.path.clone()),
            }),
            Row::Server(_) | Row::AirDrop | Row::LocalSend => None,
            Row::Overview | Row::Recents | Row::Worktrees | Row::Favorite(_) => None,
        }
    }

    /// The eye on an edited drive's row: hides it, or shows it again.
    fn toggle_hidden(&mut self, row: Row, cx: &mut Context<Self>) {
        let Some(drive) = self.hideable(row, cx) else {
            return;
        };
        preferences::update(cx, |p| {
            let before = p.hidden_drives.len();
            p.hidden_drives.retain(|h| h.drive != drive.drive);
            if p.hidden_drives.len() == before {
                p.hidden_drives.push(drive);
            }
        });
    }

    /// What the sidebar's keys are on.
    pub(super) fn row_at(&self, ix: usize) -> Option<Row> {
        let first = self.first_favorite();
        let locations = self.first_location();
        match ix {
            0 => Some(Row::Overview),
            1 => Some(Row::Recents),
            2 if self.sidebar.worktrees => Some(Row::Worktrees),
            ix if ix < locations => Some(Row::Favorite(ix - first)),
            _ if self.sidebar.collapsed.locations => None,
            ix => self.sidebar.locations.get(ix - locations).copied(),
        }
    }

    /// What the row the sidebar's keys are on stands for.
    pub(super) fn sidebar_target(&self) -> Option<RowTarget> {
        Some(match self.row_at(self.sidebar_cursor)? {
            Row::Overview => RowTarget::Overview,
            Row::Recents => RowTarget::Recents,
            Row::Worktrees => RowTarget::Worktrees,
            Row::Favorite(ix) => {
                RowTarget::Favorite(self.sidebar.places.get(ix)?.favorite.path.clone())
            }
            row @ (Row::Cloud(_)
            | Row::Volume(_)
            | Row::Server(_)
            | Row::AirDrop
            | Row::LocalSend) => RowTarget::Location(self.location(row)?),
        })
    }

    /// The server the sidebar's keys are on, and whether it's connected.
    fn server_at_cursor(&self, cx: &App) -> Option<(String, bool)> {
        let Some(Row::Server(ix)) = self.row_at(self.sidebar_cursor) else {
            return None;
        };
        let id = self.sidebar.remotes.get(ix)?.id.clone();
        let connected = crate::system::link(&id, cx) == Some(&crate::system::Link::Connected);
        Some((id, connected))
    }

    /// The volume to eject for the row the sidebar's keys are on, if any.
    fn ejectable_at_cursor(&self) -> Option<PathBuf> {
        match self.row_at(self.sidebar_cursor) {
            Some(Row::Volume(ix)) => {
                let volume = self.sidebar.volumes.get(ix)?;
                (!volume.startup).then(|| volume.mount.clone())
            }
            _ => None,
        }
    }

    /// The drive or cloud folder the sidebar's keys are on, to hide, unless
    /// it's hidden already and shows only while Locations is edited.
    fn hideable_at_cursor(&self, cx: &App) -> Option<HiddenDrive> {
        let drive = self.hideable(self.row_at(self.sidebar_cursor)?, cx)?;
        let hidden = &cx.global::<Preferences>().hidden_drives;
        (!hidden.iter().any(|h| h.drive == drive.drive)).then_some(drive)
    }

    /// Hide from Sidebar: Locations leaves the drive out until Edit or
    /// Settings shows it again. Nothing is ejected, and its path opens as
    /// before.
    fn hide_drive(&mut self, hidden: HiddenDrive, cx: &mut Context<Self>) {
        let said = format!(
            "Hid {} from the sidebar. Edit Locations shows it again.",
            hidden.name
        );
        preferences::update(cx, |p| {
            if !p.hidden_drives.iter().any(|h| h.drive == hidden.drive) {
                p.hidden_drives.push(hidden);
            }
        });
        self.toast(said, false, None, cx);
    }

    /// Ejects off the UI thread; a refusal comes back with macOS's reason.
    pub(super) fn eject(&mut self, mount: PathBuf, cx: &mut Context<Self>) {
        let (Some(drives), Some(owner)) = (
            cx.try_global::<Places>()
                .map(|places| places.drives.clone()),
            cx.try_global::<Operations>().map(|ops| ops.0.clone()),
        ) else {
            return;
        };
        let done = cx
            .background_executor()
            .spawn(async move { drives.eject(&mount, &mut lock(&owner)) });
        cx.spawn(async move |this, cx| {
            let done = done.await;
            let _ = this.update(cx, |this, cx| {
                match done {
                    Ok(said) => this.toast(said, false, None, cx),
                    Err(why) => this.toast(why, true, None, cx),
                }
                this.load_volumes(cx);
            });
        })
        .detach();
    }

    /// Opens the row the sidebar's keys are on.
    pub(super) fn open_place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.row_at(self.sidebar_cursor) {
            Some(Row::Overview) => self
                .pane()
                .update(cx, |pane, cx| pane.open_page(Shows::Overview, window, cx)),
            Some(Row::Recents) => self
                .pane()
                .update(cx, |pane, cx| pane.open_recents(window, cx)),
            Some(Row::Worktrees) => self
                .pane()
                .update(cx, |pane, cx| pane.open_page(Shows::Worktrees, window, cx)),
            Some(Row::Favorite(ix)) => self.open_favorite(ix, window, cx),
            Some(Row::Cloud(ix)) => {
                if let Some(path) = self.sidebar.cloud.get(ix).map(|c| c.path.clone()) {
                    self.pane().update(cx, |pane, cx| pane.go(path, window, cx));
                }
            }
            Some(Row::Volume(ix)) => {
                if let Some(mount) = self.sidebar.volumes.get(ix).map(|v| v.mount.clone()) {
                    self.pane()
                        .update(cx, |pane, cx| pane.go(mount, window, cx));
                }
            }
            Some(Row::AirDrop) => self
                .pane()
                .update(cx, |pane, cx| pane.open_page(Shows::AirDrop, window, cx)),
            Some(Row::LocalSend) => self
                .pane()
                .update(cx, |pane, cx| pane.open_page(Shows::LocalSend, window, cx)),
            Some(Row::Server(ix)) => {
                if let Some(id) = self.sidebar.remotes.get(ix).map(|s| s.id.clone()) {
                    self.pane()
                        .update(cx, |pane, cx| pane.open_server(&id, window, cx));
                }
            }
            None => {}
        }
    }

    /// Opens favorite `ix` where it is now, or says why it can't. Its saved
    /// path may be another drive's now, so that's never where it goes.
    fn open_favorite(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(place) = self.sidebar.places.get(ix) else {
            return;
        };
        match place.availability.clone() {
            // A pinned app opens, as it does from the list.
            Availability::Available(path) if crate::workspace::is_package(&path) => {
                self.pane().update(cx, |pane, cx| pane.open_file(&path, cx))
            }
            Availability::Available(path) => {
                self.pane().update(cx, |pane, cx| pane.go(path, window, cx))
            }
            Availability::Unavailable(why) => self.toast(why, true, None, cx),
        }
    }

    /// Open in New Tab from the sidebar: the row the keys are on.
    fn open_place_in_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stop = match self.row_at(self.sidebar_cursor) {
            Some(Row::Overview) => {
                let mut stop = Stop::folder(self.pane().read(cx).disk_folder().to_path_buf());
                stop.shows = Shows::Overview;
                stop
            }
            Some(Row::Recents) => {
                let mut stop = Stop::folder(self.pane().read(cx).disk_folder().to_path_buf());
                stop.shows = Shows::Recents;
                stop
            }
            Some(Row::Worktrees) => {
                let mut stop = Stop::folder(self.pane().read(cx).folder().to_path_buf());
                stop.shows = Shows::Worktrees;
                stop
            }
            Some(Row::Favorite(ix)) => {
                let Some(place) = self.sidebar.places.get(ix) else {
                    return;
                };
                match place.availability.clone() {
                    Availability::Available(path) => Stop::folder(path),
                    Availability::Unavailable(why) => return self.toast(why, true, None, cx),
                }
            }
            Some(Row::Cloud(ix)) => {
                let Some(folder) = self.sidebar.cloud.get(ix) else {
                    return;
                };
                Stop::folder(folder.path.clone())
            }
            Some(Row::Volume(ix)) => {
                let Some(volume) = self.sidebar.volumes.get(ix) else {
                    return;
                };
                Stop::folder(volume.mount.clone())
            }
            Some(Row::LocalSend) => {
                let mut stop = Stop::folder(self.pane().read(cx).disk_folder().to_path_buf());
                stop.shows = Shows::LocalSend;
                stop
            }
            Some(Row::Server(ix)) => {
                let Some(source) = self.sidebar.remotes.get(ix) else {
                    return;
                };
                let folder = self.pane().read(cx).folder().to_path_buf();
                Stop::remote(folder, RemoteAt::Start(source.id.clone()))
            }
            Some(Row::AirDrop) => {
                let mut stop = Stop::folder(self.pane().read(cx).disk_folder().to_path_buf());
                stop.shows = Shows::AirDrop;
                stop
            }
            None => return,
        };
        self.new_tab(stop, window, cx);
    }

    /// A right-click on the sidebar's row `ix`.
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

    /// Tabbing into the sidebar starts on the place that's open, if any.
    pub(super) fn cursor_on_folder(&self, cx: &App) -> Option<usize> {
        let here = self.here(cx);
        (0..=self.last_row())
            .find(|ix| self.row_at(*ix).is_some_and(|row| self.is_open(row, &here)))
    }

    /// Where the pane that commands act on is.
    fn here(&self, cx: &App) -> Here {
        let pane = self.pane().read(cx);
        Here {
            folder: pane.folder().to_path_buf(),
            shows: pane.stop().shows,
            server: pane.remote_source().map(str::to_owned),
            searching: pane.searching(),
            in_trash: pane.in_trash(cx),
        }
    }

    /// Whether `row` is what the pane shows, which marks it and is where
    /// the keys start.
    fn is_open(&self, row: Row, here: &Here) -> bool {
        let sidebar = &self.sidebar;
        let page = |shows| !here.searching && here.shows == shows;
        let folder = |path: Option<&Path>| here.showing_folder() && path == Some(&here.folder);
        match row {
            Row::Overview => page(Shows::Overview),
            Row::Recents => page(Shows::Recents),
            Row::Worktrees => page(Shows::Worktrees),
            Row::AirDrop => page(Shows::AirDrop),
            Row::LocalSend => page(Shows::LocalSend),
            Row::Favorite(ix) => folder(sidebar.places.get(ix).and_then(|place| place.at())),
            Row::Cloud(ix) => folder(sidebar.cloud.get(ix).map(|c| c.path.as_path())),
            Row::Volume(ix) => folder(sidebar.volumes.get(ix).map(|v| v.mount.as_path())),
            Row::Server(ix) => {
                let id = sidebar.remotes.get(ix).map(|source| source.id.as_str());
                id.is_some() && here.server.as_deref() == id
            }
        }
    }

    /// Opens or closes the sidebar. It sits after the traffic lights, open or
    /// closed, so it holds still under the pointer while the sidebar slides.
    pub(super) fn sidebar_toggle(&self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .absolute()
            .top_0()
            .left_0()
            .h(px(theme::TITLE_BAR_HEIGHT))
            .flex()
            .items_center()
            .gap_1()
            .child(drag_area().w(px(lights(window))).h_full())
            .child(
                theme::tip_below(
                    icon_button("toggle-sidebar", "Show Sidebar", "sidebar-left", true),
                    "Show Sidebar",
                )
                .debug_selector(|| "toggle-sidebar".into())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_sidebar(window, cx))),
            )
    }

    /// Follows `open` with a slide, and samples how far it has got.
    pub(super) fn step_sidebar(&mut self, open: bool, window: &mut Window, cx: &App) {
        let now = Instant::now();
        if open != self.sidebar_open {
            self.sidebar_open = open;
            let to = if open { 1. } else { 0. };
            let width = sidebar_width(cx.global::<Preferences>().sidebar_width, window);
            self.sidebar_slide = (!cx.reduce_motion()).then(|| match self.sidebar_slide.take() {
                Some(slide) => slide.retarget(to, now),
                None => Slide::new(1. - to, to, width),
            });
        }
        let sample = self.sidebar_slide.as_ref().map(|slide| slide.at(now));
        self.sidebar_fraction = match sample {
            Some((fraction, false)) => {
                window.request_animation_frame();
                fraction
            }
            Some((fraction, true)) => {
                self.sidebar_slide = None;
                fraction
            }
            None if open => 1.,
            None => 0.,
        };
    }

    /// How wide the sidebar is drawn this frame.
    pub(super) fn sidebar_drawn(&self, window: &Window, cx: &App) -> f32 {
        sidebar_width(cx.global::<Preferences>().sidebar_width, window) * self.sidebar_fraction
    }

    pub(super) fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A click focuses the sidebar too; only keys move its cursor, so
        // only they show it.
        let cursor = (self.sidebar_focus.is_focused(window) && window.last_input_was_keyboard())
            .then_some(self.sidebar_cursor);
        let here = self.here(cx);
        let width = sidebar_width(cx.global::<Preferences>().sidebar_width, window);
        let at_cursor = self.row_at(self.sidebar_cursor);
        let last_row = self.last_row();
        let collapsed = self.sidebar.collapsed;
        div()
            .relative()
            .w(px(width))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .px_2()
            // Its ink is for a dark sidebar, and it's a dark picture.
            .when(
                Channel::default() == Channel::Nightly && !theme::is_light(),
                |d| {
                    // Pinned to the right, where its own edge fades out, and as
                    // wide as the sidebar gets, so dragging only uncovers more.
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w_full()
                            .h(px(NIGHT_SKY_HEIGHT))
                            .overflow_hidden()
                            .child(
                                img(ImageSource::Resource(Resource::Embedded(NIGHT_SKY.into())))
                                    .absolute()
                                    .top_0()
                                    .right_0()
                                    .w(px(*SIDEBAR_WIDTHS.end()))
                                    .h(px(NIGHT_SKY_HEIGHT))
                                    .object_fit(ObjectFit::Fill),
                            ),
                    )
                },
            )
            .child(
                div()
                    .h(px(theme::TITLE_BAR_HEIGHT))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(drag_area().flex_1().h_full()),
            )
            .child(self.search_field(window, cx).mb_1())
            .child(
                div()
                    .id("places")
                    .key_context("Sidebar")
                    .track_focus(&self.sidebar_focus)
                    .on_key_down(cx.listener(Self::type_to_search))
                    // On a row that isn't a favorite, Remove from Sidebar's
                    // key stops here: macOS would offer it to the menu bar.
                    .on_key_down(|event: &KeyDownEvent, _, cx| {
                        if commands::is_shortcut_of(
                            &event.keystroke,
                            super::RemoveFromSidebar.name(),
                            cx,
                        ) {
                            cx.stop_propagation();
                        }
                    })
                    .on_action(cx.listener(|this, _: &PlacePrevious, _, cx| {
                        this.move_sidebar_cursor(this.sidebar_cursor.saturating_sub(1), cx)
                    }))
                    .on_action(cx.listener(move |this, _: &PlaceNext, _, cx| {
                        this.move_sidebar_cursor((this.sidebar_cursor + 1).min(last_row), cx)
                    }))
                    .on_action(cx.listener(|this, _: &FocusList, window, cx| {
                        this.pane().update(cx, |pane, cx| {
                            pane.focus_list(window, cx);
                            pane.select_first_if_none(cx);
                        });
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
                    .on_action(cx.listener(|this, _: &OpenInNewTab, window, cx| {
                        this.open_place_in_tab(window, cx)
                    }))
                    .when_some(
                        match at_cursor {
                            Some(Row::Favorite(ix)) => Some(ix),
                            _ => None,
                        },
                        |d, fav| {
                            d.on_action(cx.listener(
                                move |this, _: &super::RemoveFromSidebar, _, cx| {
                                    this.remove_favorite(fav, cx)
                                },
                            ))
                            .when(
                                self.sidebar.editing_favorites,
                                |d| {
                                    d.on_action(cx.listener(move |this, _: &MovePlaceUp, _, cx| {
                                        if fav > 0 {
                                            this.move_favorite(fav, fav - 1, cx);
                                            this.sidebar_cursor -= 1;
                                        }
                                    }))
                                    .on_action(cx.listener(
                                        move |this, _: &MovePlaceDown, _, cx| {
                                            if fav + 1 < this.sidebar.places.len() {
                                                this.move_favorite(fav, fav + 1, cx);
                                                this.sidebar_cursor += 1;
                                            }
                                        },
                                    ))
                                },
                            )
                        },
                    )
                    .when_some(
                        (self.sidebar.editing_locations)
                            .then(|| self.sidebar_cursor.checked_sub(self.first_location()))
                            .flatten()
                            .filter(|at| *at < self.sidebar.locations.len()),
                        |d, at| {
                            d.on_action(cx.listener(move |this, _: &MovePlaceUp, _, cx| {
                                this.move_location(at, at.saturating_sub(1), cx)
                            }))
                            .on_action(cx.listener(
                                move |this, _: &MovePlaceDown, _, cx| {
                                    this.move_location(at, at + 1, cx)
                                },
                            ))
                        },
                    )
                    .when_some(
                        match at_cursor {
                            Some(Row::Cloud(at)) if self.sidebar.renaming.is_none() => Some(at),
                            _ => None,
                        },
                        |d, at| {
                            d.on_action(cx.listener(
                                move |this, _: &super::RenameLocation, window, cx| {
                                    this.rename_cloud(at, window, cx)
                                },
                            ))
                        },
                    )
                    .when_some(self.hideable_at_cursor(cx), |d, hidden| {
                        d.on_action(cx.listener(move |this, _: &super::HideFromSidebar, _, cx| {
                            this.hide_drive(hidden.clone(), cx)
                        }))
                    })
                    .when_some(self.ejectable_at_cursor(), |d, mount| {
                        d.on_action(cx.listener(move |this, _: &super::Eject, _, cx| {
                            this.eject(mount.clone(), cx)
                        }))
                    })
                    // A server's Eject disconnects it, and Remove from Sidebar
                    // forgets it, as Finder does for servers.
                    .when_some(self.server_at_cursor(cx), |d, (id, connected)| {
                        let forgotten = id.clone();
                        d.when(connected, |d| {
                            d.on_action(cx.listener(move |this, _: &super::Eject, _, cx| {
                                this.disconnect_server(&id, cx)
                            }))
                        })
                        .on_action(cx.listener(
                            move |this, _: &super::RemoveFromSidebar, _, cx| {
                                this.forget_server(&forgotten, cx)
                            },
                        ))
                    })
                    .when(at_cursor == Some(Row::Recents), |d| {
                        d.on_action(cx.listener(|this, _: &super::ClearRecents, _, cx| {
                            this.pane().update(cx, |pane, cx| pane.clear_recents(cx))
                        }))
                    })
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(Self::page_row(
                        0,
                        "Overview",
                        "hard-drive",
                        self.is_open(Row::Overview, &here),
                        cursor,
                        cx,
                    ))
                    .child(Self::page_row(
                        1,
                        "Recents",
                        "clock",
                        self.is_open(Row::Recents, &here),
                        cursor,
                        cx,
                    ))
                    .when(self.sidebar.worktrees, |d| {
                        d.child(Self::page_row(
                            2,
                            "Worktrees",
                            "git-branch",
                            self.is_open(Row::Worktrees, &here),
                            cursor,
                            cx,
                        ))
                    })
                    .child(self.favorites_header(cx))
                    .children(self.sidebar.error.clone().map(|why| {
                        div()
                            .px_2()
                            .text_size(px(theme::TEXT_CAPTION))
                            .text_color(theme::danger())
                            .child(why)
                    }))
                    .when(!collapsed.favorites, |d| {
                        d.children(
                            (0..self.sidebar.places.len())
                                .map(|fav| self.favorite_row(fav, &here, cursor, cx))
                                .collect::<Vec<_>>(),
                        )
                    })
                    .child(self.locations_header(cx))
                    .when(!collapsed.locations, |d| {
                        d.children(self.location_rows(&here, cursor, cx))
                    }),
            )
            .child(self.foot(&here, cx))
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
}

/// The space between the traffic lights and the sidebar's button.
const LIGHTS_GAP: f32 = 8.;

/// The blank room the traffic lights take.
fn lights(window: &Window) -> f32 {
    (traffic_lights::REACH + LIGHTS_GAP) / window.content_zoom()
}

/// Where Back can start in the top bar: past the traffic lights and the
/// sidebar's button, with the gaps around it.
pub(super) fn toggle_reach(window: &Window) -> f32 {
    lights(window) + 4. + theme::BUTTON_HEIGHT + 8.
}

impl Browser {
    /// Disconnects server `id`, and every pane showing it says so.
    pub(in crate::browser) fn disconnect_server(&mut self, id: &str, cx: &mut Context<Self>) {
        disconnect(id, cx);
        for pane in self
            .tabs
            .iter()
            .flat_map(|tab| tab.panes().cloned().collect::<Vec<_>>())
        {
            pane.update(cx, |pane, cx| pane.remote_disconnected(id, cx));
        }
        cx.notify();
    }

    /// Forgets server `id`: disconnects it and drops it from the saved
    /// list. Nothing on the server changes.
    pub(in crate::browser) fn forget_server(&mut self, id: &str, cx: &mut Context<Self>) {
        disconnect(id, cx);
        let kept = self.cursor_location();
        self.sidebar.remotes.retain(|source| source.id != id);
        self.arrange_locations_keeping(kept, cx);
        let id = id.to_owned();
        self.change_places(cx, None, move |store, _| store.forget_remote(&id).map(drop));
    }
}

impl Browser {
    pub(super) fn favorite_commands(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let root = Self::opens_favorite::<GoFavorite1>(root, 0, cx);
        let root = Self::opens_favorite::<GoFavorite2>(root, 1, cx);
        let root = Self::opens_favorite::<GoFavorite3>(root, 2, cx);
        let root = Self::opens_favorite::<GoFavorite4>(root, 3, cx);
        let root = Self::opens_favorite::<GoFavorite5>(root, 4, cx);
        let root = Self::opens_favorite::<GoFavorite6>(root, 5, cx);
        let root = Self::opens_favorite::<GoFavorite7>(root, 6, cx);
        let root = Self::opens_favorite::<GoFavorite8>(root, 7, cx);
        let root = Self::opens_favorite::<GoFavorite9>(root, 8, cx);
        Self::opens_favorite::<GoFavorite10>(root, 9, cx)
    }

    fn opens_favorite<A: Action>(root: gpui::Div, fav: usize, cx: &mut Context<Self>) -> gpui::Div {
        root.on_action(cx.listener(move |this, _: &A, window, cx| {
            this.open_favorite(fav, window, cx);
        }))
    }
}
