//! The sidebar: Recents and the favorites from `places::Store`, the mounted
//! volumes with how full each is, and the Trash and Settings at its foot.
//! Everything shown is read off the UI thread and read again when a watch
//! hears a change, so nothing here polls.

use super::{
    Browser, FavoriteDown, FavoriteUp, FocusList, FocusSearch, OpenPlace, OpenSelected, PlaceNext,
    PlacePrevious, Sheet, drag_area, home, sidebar_width,
};
use crate::commands::{self, Scope};
use crate::preferences::Preferences;
use crate::theme::{self, accent, content, icon, icon_button, section_label};
use gpui::{
    Action, App, Context, CursorStyle, ElementId, Focusable, FontWeight, Global, KeyDownEvent,
    MouseButton, MouseDownEvent, Pixels, Point, Render, Role, Stateful, Task, Window, div,
    prelude::*, px,
};
use monodrive_core::ops::{self, Origin};
use monodrive_core::places::{Availability, Place, Store};
use monodrive_core::servers::{self, Cancel, Prompt};
use monodrive_core::volumes::{self, Volume};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the system does for the sidebar. Views reach it through `Places`, so
/// their tests list a made-up set of volumes and never mount or eject
/// anything real.
pub trait Drives: Send + Sync {
    /// What's mounted. Can wait on a slow share, so call it off the UI thread.
    fn list(&self) -> Vec<Volume>;
    fn eject(&self, mount: &Path, ops: &mut ops::Client) -> Result<String, String>;
    /// Waits for macOS and any prompt, so call it off the UI thread.
    fn mount(&self, address: &str, cancel: &Arc<Cancel>) -> Result<PathBuf, String>;
    /// Opens a file in its app.
    fn open(&self, path: &Path, cx: &App);
}

/// The real thing.
pub struct System;

impl Drives for System {
    fn list(&self) -> Vec<Volume> {
        volumes::list()
    }

    fn eject(&self, mount: &Path, ops: &mut ops::Client) -> Result<String, String> {
        volumes::eject(mount, &Origin::Window, ops)
    }

    fn mount(&self, address: &str, cancel: &Arc<Cancel>) -> Result<PathBuf, String> {
        servers::mount(address, Prompt::Allow, cancel)
    }

    fn open(&self, path: &Path, cx: &App) {
        cx.open_with_system(path);
    }
}

/// Where the window's places come from. `run` sets it; without it, as in
/// benchmarks, the sidebar lists nothing and reads nothing.
#[derive(Clone)]
pub struct Places {
    pub store: Store,
    /// The Trash the window shows and Empty Trash erases.
    pub trash: PathBuf,
    pub drives: Arc<dyn Drives>,
}

impl Global for Places {}

/// What the sidebar lists, as last read.
#[derive(Default)]
pub(super) struct Sidebar {
    pub(super) places: Vec<Place>,
    pub(super) volumes: Vec<Volume>,
    pub(super) servers: Vec<String>,
    /// Why the saved lists didn't read. They stay as last read, and nothing
    /// changes the file.
    error: Option<String>,
    /// Edit mode: each favorite shows a handle and a ✕.
    pub(super) editing: bool,
    load_volumes: Option<Task<()>>,
    load_lists: Option<Task<()>>,
    _heard: Vec<Task<()>>,
}

/// A row the sidebar's keys can land on, in order on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Row {
    Recents,
    Favorite(usize),
    Volume(usize),
}

/// A favorite being dragged by its handle.
#[derive(Clone)]
struct DraggedFavorite {
    ix: usize,
    label: String,
}

impl Render for DraggedFavorite {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(28.))
            .px_3()
            .rounded_md()
            .flex()
            .items_center()
            .text_size(px(13.))
            .text_color(content(0.9))
            .bg(content(0.14))
            .child(self.label.clone())
    }
}

const ROW_HEIGHT: f32 = 32.;
const DRIVE_HEIGHT: f32 = 40.;

fn label_of(path: &Path) -> String {
    if path == home() {
        return "Home".into();
    }
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn glyph_of(path: &Path) -> &'static str {
    if path == home().join("Downloads") {
        "arrow-down-circle"
    } else if path == Path::new("/Applications") {
        "app-window"
    } else {
        "folder-01"
    }
}

/// How full a volume is, from 0 to 1, if it says.
fn fullness(volume: &Volume) -> Option<f32> {
    (volume.total > 0)
        .then(|| (volume.total.saturating_sub(volume.free) as f64 / volume.total as f64) as f32)
}

/// A small button in a sidebar row. It takes the press from the row under it.
fn row_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &'static str,
    glyph: &'static str,
) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .role(Role::Button)
        .aria_label(label)
        .size(px(20.))
        .flex_shrink_0()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(content(0.12)))
        .child(icon(glyph, 12.).text_color(content(0.55)))
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
                    if this.showing_recents() {
                        this.list(true, cx);
                    }
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
        let listed = cx
            .background_executor()
            .spawn(async move { places.drives.list() });
        self.sidebar.load_volumes = Some(cx.spawn(async move |this, cx| {
            let volumes = listed.await;
            let _ = this.update(cx, |this, cx| {
                let gone = this.sidebar.volumes.iter().find(|old| {
                    this.folder().starts_with(&old.mount)
                        && !old.startup
                        && !volumes.iter().any(|new| new.mount == old.mount)
                });
                if let Some(name) = gone.map(|volume| volume.name.clone()) {
                    if this.toast.is_none() {
                        this.toast(format!("{name} was disconnected."), false, None, cx);
                    }
                    this.navigate(home(), cx);
                }
                this.sidebar.volumes = volumes;
                this.load_lists(cx);
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
        let volumes = self.sidebar.volumes.clone();
        let read = cx.background_executor().spawn(async move {
            let favorites = places.store.places(&volumes);
            (favorites, places.store.servers())
        });
        self.sidebar.load_lists = Some(cx.spawn(async move |this, cx| {
            let (favorites, saved) = read.await;
            let _ = this.update(cx, |this, cx| {
                let sidebar = &mut this.sidebar;
                sidebar.error = favorites.as_ref().err().or(saved.as_ref().err()).cloned();
                if let Ok(favorites) = favorites {
                    sidebar.places = favorites;
                }
                if let Ok(saved) = saved {
                    sidebar.servers = saved;
                }
                cx.notify();
            });
        }));
    }

    /// Changes a saved list off the UI thread, says why if it can't, and
    /// reads the lists again.
    fn change_places(
        &mut self,
        cx: &mut Context<Self>,
        change: impl FnOnce(&Store, &[Volume]) -> Result<(), String> + Send + 'static,
    ) {
        let Some(store) = cx.try_global::<Places>().map(|places| places.store.clone()) else {
            return;
        };
        let volumes = self.sidebar.volumes.clone();
        let changed = cx
            .background_executor()
            .spawn(async move { change(&store, &volumes) });
        cx.spawn(async move |this, cx| {
            let changed = changed.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = changed {
                    this.toast(why, true, None, cx);
                }
                this.load_lists(cx);
                if this.showing_recents() {
                    this.list(true, cx);
                }
            });
        })
        .detach();
    }

    /// Add to Sidebar: pins the folder selected, last.
    pub(super) fn add_to_sidebar(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.change_places(cx, move |store, volumes| {
            store.pin(&path, None, volumes).map(|_| ())
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
        self.change_places(cx, move |store, volumes| store.unpin(&path, volumes));
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
        self.change_places(cx, move |store, volumes| match place.at() {
            Some(path) => store.pin(path, Some(to), volumes).map(|_| ()),
            None => store.move_favorite(&place.favorite, to),
        });
    }

    pub(super) fn toggle_editing(&mut self, cx: &mut Context<Self>) {
        self.sidebar.editing = !self.sidebar.editing;
        cx.notify();
    }

    /// What the sidebar's keys are on.
    pub(super) fn row_at(&self, ix: usize) -> Option<Row> {
        let favorites = self.sidebar.places.len();
        match ix {
            0 => Some(Row::Recents),
            ix if ix <= favorites => Some(Row::Favorite(ix - 1)),
            ix => {
                let volume = ix - favorites - 1;
                (volume < self.sidebar.volumes.len()).then_some(Row::Volume(volume))
            }
        }
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

    /// The volume to eject for the folder on screen, if any.
    pub(super) fn ejectable_here(&self) -> Option<PathBuf> {
        volumes::holding(&self.sidebar.volumes, self.folder())
            .filter(|volume| !volume.startup)
            .map(|volume| volume.mount.clone())
    }

    /// Ejects off the UI thread; a refusal comes back with macOS's reason.
    pub(super) fn eject(&mut self, mount: PathBuf, window: &Window, cx: &mut Context<Self>) {
        let Some(drives) = cx
            .try_global::<Places>()
            .map(|places| places.drives.clone())
        else {
            return;
        };
        self.with_owner(
            window,
            cx,
            move |owner| drives.eject(&mount, owner),
            |this, done, _, cx| {
                match done {
                    Ok(said) => this.toast(said, false, None, cx),
                    Err(why) => this.toast(why, true, None, cx),
                }
                this.load_volumes(cx);
            },
        );
    }

    /// Opens the row the sidebar's keys are on.
    pub(super) fn open_place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.row_at(self.sidebar_cursor) {
            Some(Row::Recents) => self.open_recents(window, cx),
            Some(Row::Favorite(ix)) => self.open_favorite(ix, window, cx),
            Some(Row::Volume(ix)) => {
                if let Some(mount) = self.sidebar.volumes.get(ix).map(|v| v.mount.clone()) {
                    self.go(mount, window, cx);
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
            Availability::Available(path) => self.go(path, window, cx),
            Availability::Unavailable(why) => self.toast(why, true, None, cx),
        }
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
    pub(super) fn cursor_on_folder(&self) -> Option<usize> {
        if self.showing_recents() {
            return Some(0);
        }
        let folder = self.folder();
        let favorite = self
            .sidebar
            .places
            .iter()
            .position(|place| place.at() == Some(folder))
            .map(|ix| ix + 1);
        favorite.or_else(|| {
            let volume = self
                .sidebar
                .volumes
                .iter()
                .position(|v| v.mount == folder)?;
            Some(volume + self.sidebar.places.len() + 1)
        })
    }

    /// A row's frame: the keyboard cursor's border, its highlight, its right
    /// click and its click.
    fn row(
        &self,
        ix: usize,
        selected: bool,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        div()
            .id(("place", ix))
            .debug_selector(move || format!("place-{ix}"))
            // The sidebar's keyboard cursor. The border is always there, so
            // showing it doesn't shift the row.
            .border_1()
            .border_color(if cursor == Some(ix) {
                accent().opacity(0.7)
            } else {
                gpui::transparent_black()
            })
            .role(Role::Button)
            .aria_selected(selected)
            .flex_shrink_0()
            .px_2()
            .rounded_md()
            .text_size(px(14.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(content(if selected { 1. } else { 0.5 }))
            .when(selected, |d| d.bg(content(0.1)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.place_menu(ix, event.position, window, cx)
                }),
            )
    }

    /// What every row but an edited favorite does when clicked.
    fn clickable(
        row: Stateful<gpui::Div>,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<gpui::Div> {
        row.cursor_pointer()
            .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.sidebar_cursor = ix;
                this.open_place(window, cx);
            }))
    }

    fn recents_row(
        &self,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let selected = self.query.is_empty() && self.showing_recents();
        Self::clickable(self.row(0, selected, cursor, cx), 0, cx)
            .aria_label("Recents")
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .child(icon("clock", 16.).text_color(content(if selected { 0.7 } else { 0.35 })))
            .child(div().flex_1().truncate().child("Recents"))
    }

    fn favorite_row(
        &self,
        fav: usize,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let place = &self.sidebar.places[fav];
        let ix = fav + 1;
        let label = label_of(&place.favorite.path);
        let glyph = glyph_of(&place.favorite.path);
        let selected = self.showing_folder() && place.at() == Some(self.folder());
        let reason = match &place.availability {
            Availability::Available(_) => None,
            Availability::Unavailable(why) => Some(why.clone()),
        };
        let unavailable = reason.is_some();
        let editing = self.sidebar.editing;
        let row = self
            .row(ix, selected, cursor, cx)
            .aria_label(label.clone())
            .h(px(ROW_HEIGHT))
            .flex()
            .items_center()
            .gap_2()
            .when(unavailable, |d| d.text_color(content(0.3)));
        let row = if editing {
            row.on_drop(cx.listener(move |this, dragged: &DraggedFavorite, _, cx| {
                this.move_favorite(dragged.ix, fav, cx);
            }))
            .drag_over::<DraggedFavorite>(|s, _, _, _| s.bg(content(0.14)))
            .child(
                div()
                    .id(("favorite-handle", fav))
                    .debug_selector(move || format!("favorite-handle-{fav}"))
                    .role(Role::Button)
                    .aria_label("Drag to reorder")
                    .flex_shrink_0()
                    .cursor(CursorStyle::OpenHand)
                    .child(icon("grip-vertical", 14.).text_color(content(0.4)))
                    .on_drag(
                        DraggedFavorite {
                            ix: fav,
                            label: label.clone(),
                        },
                        |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                    ),
            )
        } else {
            Self::clickable(row, ix, cx)
        };
        row.child(icon(glyph, 16.).text_color(content(if unavailable {
            0.2
        } else if selected {
            0.7
        } else {
            0.35
        })))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().line_height(px(16.)).child(label))
                .children(reason.map(|why| {
                    div()
                        .truncate()
                        .line_height(px(13.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::NORMAL)
                        .child(why)
                })),
        )
        .when(editing, |d| {
            d.child(
                row_button(
                    ("favorite-remove", fav),
                    format!("favorite-remove-{fav}"),
                    "Remove from Sidebar",
                    "cancel-01",
                )
                .on_click(cx.listener(move |this, _, _, cx| this.remove_favorite(fav, cx))),
            )
        })
    }

    fn volume_row(
        &self,
        vol: usize,
        cursor: Option<usize>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let volume = &self.sidebar.volumes[vol];
        let ix = vol + 1 + self.sidebar.places.len();
        let selected = self.showing_folder() && self.folder() == volume.mount;
        let mount = volume.mount.clone();
        let glyph = if volume.network {
            "server"
        } else {
            "hard-drive"
        };
        Self::clickable(self.row(ix, selected, cursor, cx), ix, cx)
            .aria_label(volume.name.clone())
            .h(px(DRIVE_HEIGHT))
            .flex()
            .flex_col()
            .justify_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(24.))
                    .child(icon(glyph, 16.).text_color(content(if selected { 0.7 } else { 0.35 })))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(volume.name.clone()),
                    )
                    .when(volume.read_only, |d| {
                        d.child(
                            div()
                                .debug_selector(move || format!("read-only-{vol}"))
                                .flex_shrink_0()
                                .text_size(px(11.))
                                .font_weight(FontWeight::NORMAL)
                                .text_color(content(0.4))
                                .child("Read-only"),
                        )
                    })
                    .when(!volume.startup, |d| {
                        d.child(
                            row_button(
                                ("volume-eject", vol),
                                format!("volume-eject-{vol}"),
                                "Eject",
                                "eject",
                            )
                            .occlude()
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.eject(mount.clone(), window, cx);
                                },
                            )),
                        )
                    }),
            )
            .children(fullness(volume).map(|fullness| {
                div()
                    .debug_selector(move || format!("volume-bar-{vol}"))
                    .ml(px(24.))
                    .h(px(3.))
                    .flex_shrink_0()
                    .rounded_full()
                    .bg(content(0.14))
                    .child(
                        div()
                            .debug_selector(move || format!("volume-used-{vol}"))
                            .h_full()
                            .w(gpui::relative(fullness))
                            .rounded_full()
                            .bg(content(0.55)),
                    )
            }))
    }

    fn favorites_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let editing = self.sidebar.editing;
        let label = if editing { "Done" } else { "Edit" };
        section_label("Favorites")
            .flex_shrink_0()
            .justify_between()
            .child(
                div()
                    .id("favorites-edit")
                    .debug_selector(|| "favorites-edit".into())
                    .role(Role::Button)
                    .aria_label(label)
                    .px_1()
                    .rounded(px(5.))
                    .cursor_pointer()
                    .text_color(if editing { accent() } else { content(0.4) })
                    .hover(|s| s.bg(content(0.1)))
                    .child(label)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_editing(cx))),
            )
    }

    /// The Trash and Settings at the sidebar's foot.
    fn foot(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let in_trash = self.in_trash(cx);
        div()
            .h(px(40.))
            .flex_shrink_0()
            .px_1()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                icon_button("trash", "Trash", "trash", true)
                    .debug_selector(|| "open-trash".into())
                    .when(in_trash, |d| d.bg(content(0.1)))
                    .on_click(cx.listener(|this, _, window, cx| this.open_trash(window, cx))),
            )
            .children(self.index_bar(cx))
            .child(
                icon_button("settings", "Settings", "settings", true)
                    .track_focus(&self.settings_focus)
                    .focus_visible(|s| s.bg(content(0.1)).border_1().border_color(accent()))
                    .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
    }

    pub(super) fn sidebar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A click focuses the sidebar too; only keys move its cursor, so
        // only they show it.
        let cursor = (self.sidebar_focus.is_focused(window) && window.last_input_was_keyboard())
            .then_some(self.sidebar_cursor);
        let focused = self.search.focus_handle(cx).is_focused(window);
        let width = sidebar_width(cx.global::<Preferences>().sidebar_width, window);
        let at_cursor = self.row_at(self.sidebar_cursor);
        let last_row = self.sidebar.places.len() + self.sidebar.volumes.len();
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
                    .flex_shrink_0()
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
                            .when(self.sidebar.editing, |d| {
                                d.on_action(cx.listener(move |this, _: &FavoriteUp, _, cx| {
                                    if fav > 0 {
                                        this.move_favorite(fav, fav - 1, cx);
                                        this.sidebar_cursor -= 1;
                                    }
                                }))
                                .on_action(cx.listener(
                                    move |this, _: &FavoriteDown, _, cx| {
                                        if fav + 1 < this.sidebar.places.len() {
                                            this.move_favorite(fav, fav + 1, cx);
                                            this.sidebar_cursor += 1;
                                        }
                                    },
                                ))
                            })
                        },
                    )
                    .when_some(self.ejectable_at_cursor(), |d, mount| {
                        d.on_action(cx.listener(move |this, _: &super::Eject, window, cx| {
                            this.eject(mount.clone(), window, cx)
                        }))
                    })
                    .when(at_cursor == Some(Row::Recents), |d| {
                        d.on_action(cx.listener(|this, _: &super::ClearRecents, _, cx| {
                            this.clear_recents(cx)
                        }))
                    })
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .child(self.favorites_header(cx))
                    .child(self.recents_row(cursor, cx))
                    .children(self.sidebar.error.clone().map(|why| {
                        div()
                            .px_2()
                            .text_size(px(11.))
                            .text_color(theme::danger())
                            .child(why)
                    }))
                    .children(
                        (0..self.sidebar.places.len())
                            .map(|fav| self.favorite_row(fav, cursor, cx))
                            .collect::<Vec<_>>(),
                    )
                    .child(section_label("Locations").flex_shrink_0())
                    .children(
                        (0..self.sidebar.volumes.len())
                            .map(|vol| self.volume_row(vol, cursor, cx))
                            .collect::<Vec<_>>(),
                    ),
            )
            .child(self.foot(cx))
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

    /// ⌘K: the Connect to Server card, with the saved servers as last read.
    pub(super) fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Connect(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        self.remember_focus(window, cx);
        let saved = self.sidebar.servers.clone();
        let view = cx.new(|cx| crate::connect::ConnectView::new(places, saved, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, event: &crate::connect::ConnectEvent, window, cx| {
                this.close_sheet(window, cx);
                if let crate::connect::ConnectEvent::Connected { address, mount } = event {
                    this.toast(
                        format!("Connected {address} at {}", mount.display()),
                        false,
                        None,
                        cx,
                    );
                    this.load_volumes(cx);
                }
                this.load_lists(cx);
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::Connect(view)), cx);
    }
}
