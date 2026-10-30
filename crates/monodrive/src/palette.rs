//! The command palette (⌘K or ⇧⌘P). With nothing typed it offers what applies
//! now, by section; typing searches commands, places and recents, then the
//! index's files. A row with a chevron opens a page with its own filter:
//! Remote locations, Cloud storage, Drives and Favorites. It works from what
//! the browser last read, so it opens at once, and reads the rest in the
//! background.

use crate::browser::{self, Eject, Places, format_bytes};
use crate::commands::{self, Command, Scope};
use crate::menu;
use crate::preferences::Preferences;
use crate::text_field::{self, TextField};
use crate::theme::{self, ButtonLook, Elevation, content, icon, row_icon_button};
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding,
    KeyContext, MouseButton, Role, ScrollHandle, SharedString, Task, Window, actions, div, point,
    prelude::*, px,
};
use monodrive_core::cloud::CloudFolder;
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::index::Index;
use monodrive_core::paths;
use monodrive_core::places::{Availability, Place};
use monodrive_core::remote::Source;
use monodrive_core::servers;
use monodrive_core::volumes::Volume;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

actions!(palette, [Up, Down, Run, Dismiss, OpenPage, Leave, Forget]);

pub fn bind(cx: &mut App) {
    let context = Some("Palette");
    cx.bind_keys([
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("enter", Run, context),
        KeyBinding::new("escape", Dismiss, context),
        // These are the query's own keys, except on a row or page that gives
        // them a meaning.
        KeyBinding::new("right", OpenPage, Some("Palette && OnPageRow > TextField")),
        KeyBinding::new("left", Leave, Some("Palette && CanLeave > TextField")),
        KeyBinding::new(
            "cmd-backspace",
            Forget,
            Some("Palette && OnForgettable > TextField"),
        ),
    ]);
    // The Browser context, where Eject is bound, gives way to a sheet.
    for keys in commands::shortcuts_of(Eject.name(), cx) {
        cx.bind_keys([KeyBinding::new(&keys, Eject, context)]);
    }
}

pub enum PaletteEvent {
    Dismissed,
    /// Run this command once the palette has closed and focus is back.
    Run(Box<dyn Action>),
    /// Show this folder.
    Go(PathBuf),
    /// Open this file in its app.
    Open(PathBuf),
    Eject(PathBuf),
    /// Connect to this saved server through the Connect card.
    Connect(String),
    Say(String),
}

/// What the browser knew as the palette opened.
pub struct Now {
    /// The ids of the commands that would run from where the keys were.
    pub available: HashSet<&'static str>,
    pub scopes: Vec<Scope>,
    pub favorites: Vec<Place>,
    pub volumes: Vec<Volume>,
    pub servers: Vec<String>,
    pub index: Arc<OnceLock<Result<Index, String>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Page {
    Root,
    Remote,
    Cloud,
    Drives,
    Favorites,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Root => "Command palette",
            Page::Remote => "Remote locations",
            Page::Cloud => "Cloud storage",
            Page::Drives => "Drives",
            Page::Favorites => "Favorites",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Page::Root => "search",
            Page::Remote => "server",
            Page::Cloud => "cloud",
            Page::Drives => "hard-drive",
            Page::Favorites => "folder-01",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Page::Root => "Search commands, places, and files…",
            Page::Remote => "Filter, or type a host or ssh alias…",
            Page::Cloud | Page::Drives | Page::Favorites => "Filter…",
        }
    }
}

pub(crate) enum Target {
    Command(Box<dyn Action>),
    Page(Page),
    Folder(PathBuf),
    File(PathBuf),
    /// A saved SFTP source, by its host.
    Remote(String),
    AddRemote,
    /// A saved SMB address that isn't mounted.
    Server(String),
}

/// A saved server, which can be forgotten.
#[derive(Clone)]
enum Saved {
    Remote(String),
    Server(String),
}

/// What a row is, wherever it sits in the list.
#[derive(Debug, PartialEq)]
enum RowId {
    Command(&'static str),
    Page(Page),
    Path(PathBuf),
    Remote(String),
    Server(String),
    AddRemote,
}

pub(crate) struct Row {
    pub(crate) section: &'static str,
    glyph: &'static str,
    pub(crate) title: SharedString,
    pub(crate) subtitle: Option<SharedString>,
    keys: Option<String>,
    pub(crate) target: Target,
    /// A row that wouldn't do anything now stays visible, dimmed.
    pub(crate) enabled: bool,
    pub(crate) destructive: bool,
    checked: bool,
    /// Where ⌘E or the row's button ejects.
    eject: Option<PathBuf>,
    /// What ⌘⌫ or the row's button forgets.
    forget: Option<Saved>,
    /// What else it's found by, besides its title: a command's menu. Not a
    /// path, whose letters would match nearly anything in order.
    found_by: String,
}

impl Row {
    fn new(
        section: &'static str,
        glyph: &'static str,
        title: impl Into<SharedString>,
        target: Target,
    ) -> Self {
        Self {
            section,
            glyph,
            title: title.into(),
            subtitle: None,
            keys: None,
            target,
            enabled: true,
            destructive: false,
            checked: false,
            eject: None,
            forget: None,
            found_by: String::new(),
        }
    }

    fn id(&self) -> RowId {
        match (&self.forget, &self.target) {
            (Some(Saved::Remote(id)), _) => RowId::Remote(id.clone()),
            (Some(Saved::Server(address)), _) => RowId::Server(address.clone()),
            (None, Target::Command(action)) => RowId::Command(action.name()),
            (None, Target::Page(page)) => RowId::Page(*page),
            (None, Target::Folder(path) | Target::File(path)) => RowId::Path(path.clone()),
            (None, Target::Remote(host)) => RowId::Remote(host.clone()),
            (None, Target::Server(address)) => RowId::Server(address.clone()),
            (None, Target::AddRemote) => RowId::AddRemote,
        }
    }

    fn about(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }
}

const FILE_LIMIT: usize = 8;
/// So the index's files, which come after them, are never far down.
const RESULT_LIMIT: usize = 8;
const WIDTH: f32 = 576.;
/// A row with a second line.
const TALL_ROW: f32 = 40.;
const LIST_HEIGHT: f32 = 360.;
const FOOTER_HEIGHT: f32 = 32.;
const KEY_CAP_HEIGHT: f32 = 16.;

pub struct PaletteView {
    query: Entity<TextField>,
    now: Now,
    pub(crate) page: Page,
    /// What was typed before a page opened, back when it closes.
    root_query: String,
    pub(crate) rows: Vec<Row>,
    /// The row the keys or the pointer are on, never a dimmed one.
    pub(crate) active: Option<usize>,
    /// The row to land on after the next filter, by title.
    land_on: Option<SharedString>,
    scroll: ScrollHandle,
    recents: Vec<Entry>,
    remotes: Vec<Source>,
    cloud: Vec<CloudFolder>,
    /// What the index found for the query.
    files: Vec<Entry>,
    /// Why saving or forgetting a server failed.
    pub(crate) error: Option<String>,
    file_search: Task<()>,
    _reads: [Task<()>; 2],
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl Focusable for PaletteView {
    /// The query, so typing filters at once.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl PaletteView {
    pub fn new(now: Now, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextField::new("Command", Page::Root.placeholder(), cx));
        cx.subscribe(&query, |this, _, _: &text_field::Changed, cx| {
            this.filter(cx)
        })
        .detach();
        let reads = Self::read_lists(cx);
        let mut view = Self {
            query,
            now,
            page: Page::Root,
            root_query: String::new(),
            rows: Vec::new(),
            active: None,
            land_on: None,
            scroll: ScrollHandle::new(),
            recents: Vec::new(),
            remotes: Vec::new(),
            cloud: Vec::new(),
            files: Vec::new(),
            error: None,
            file_search: Task::ready(()),
            _reads: reads,
        };
        view.filter(cx);
        view
    }

    /// Reads what the browser doesn't keep. A slow share among the recents
    /// holds up only the recents.
    fn read_lists(cx: &mut Context<Self>) -> [Task<()>; 2] {
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return [Task::ready(()), Task::ready(())];
        };
        let lists = {
            let places = places.clone();
            cx.background_executor()
                .spawn(async move { (places.store.remotes(), places.drives.cloud()) })
        };
        let recents = cx.background_executor().spawn(async move {
            places
                .store
                .recents()
                .map(|recents| browser::recent_entries(recents).0)
        });
        [
            cx.spawn(async move |this, cx| {
                let (remotes, cloud) = lists.await;
                let _ = this.update(cx, |this, cx| {
                    this.remotes = remotes.unwrap_or_default();
                    this.cloud = cloud;
                    this.refresh(cx);
                });
            }),
            cx.spawn(async move |this, cx| {
                let recents = recents.await;
                let _ = this.update(cx, |this, cx| {
                    this.recents = recents.unwrap_or_default();
                    this.refresh(cx);
                });
            }),
        ]
    }

    fn typed(&self, cx: &App) -> String {
        self.query.read(cx).text().trim().to_owned()
    }

    /// The query changed: new rows, from the top.
    fn filter(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.search_files(cx);
        self.rows = self.build(cx);
        let wanted = self.land_on.take();
        self.active = wanted
            .and_then(|title| self.rows.iter().position(|row| row.title == title))
            .or_else(|| self.first_enabled());
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// What's behind the rows changed: new rows, staying on the same one.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let was = self.active_row().map(Row::id);
        self.rows = self.build(cx);
        let kept = was.and_then(|id| self.rows.iter().position(|r| r.enabled && r.id() == id));
        self.active = kept.or_else(|| self.first_enabled());
        if let Some(ix) = self.active {
            self.scroll.scroll_to_item(ix);
        }
        cx.notify();
    }

    fn first_enabled(&self) -> Option<usize> {
        self.rows.iter().position(|row| row.enabled)
    }

    /// Asks the index off the UI thread. It may not have started, which is
    /// no error: the section just isn't there, and nothing here starts it.
    fn search_files(&mut self, cx: &mut Context<Self>) {
        self.files.clear();
        let query = self.typed(cx);
        let places = cx.try_global::<Places>().cloned();
        let (Page::Root, false, Some(places)) = (self.page, query.is_empty(), places) else {
            self.file_search = Task::ready(());
            return;
        };
        let index = self.now.index.clone();
        let asked = query.clone();
        let found = cx
            .background_executor()
            .spawn(async move { places.drives.find_files(&index, &asked, FILE_LIMIT) });
        self.file_search = cx.spawn(async move |this, cx| {
            let found = found.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| this.land_files(&query, found, cx));
        });
    }

    /// Takes the index's answer to `query`, if that's still what's typed.
    pub(crate) fn land_files(&mut self, query: &str, found: Vec<Entry>, cx: &mut Context<Self>) {
        if self.page == Page::Root && self.typed(cx) == query {
            self.files = found;
            self.refresh(cx);
        }
    }

    fn build(&self, cx: &App) -> Vec<Row> {
        let query = self.typed(cx);
        match self.page {
            Page::Root if query.is_empty() => self.home_rows(cx),
            Page::Root => self.results(&query, cx),
            page => {
                let mut rows = self.page_rows(page);
                rows.retain(|row| rank(&query, row).is_some());
                if page == Page::Remote {
                    let at = rows.iter().take_while(|r| r.section == SFTP).count();
                    rows.insert(at, add_remote_row(&query));
                }
                rows
            }
        }
    }

    /// Nothing typed: what applies now, by section.
    fn home_rows(&self, cx: &App) -> Vec<Row> {
        let all = commands::all();
        let prefs = cx.global::<Preferences>();
        let ticked = commands::checked(prefs);
        let row = |section, command: &Command| self.command_row(section, command, prefs, &ticked);
        let named = |section: &'static str, titles: &[&str]| -> Vec<Row> {
            titles
                .iter()
                .filter_map(|title| all.iter().find(|c| c.title == *title))
                .map(|command| row(section, command))
                .collect()
        };
        // What the context menu would offer on the selection, most useful
        // first and Move to Trash last.
        let mut chosen: Vec<&Command> = Vec::new();
        if self.now.scopes.contains(&Scope::Selection) {
            let offered = commands::context_menu(&self.now.scopes, prefs, |action| {
                self.now.available.contains(action.name())
            });
            chosen = SELECTION_ORDER
                .iter()
                .filter(|title| offered.iter().any(|row| row.title == **title))
                .filter_map(|title| all.iter().find(|c| c.title == *title))
                .collect();
        }
        let mut rows: Vec<Row> = chosen.into_iter().map(|c| row(ACTIONS, c)).collect();
        rows.extend(named(
            ACTIONS,
            &[
                "New Folder",
                "New File",
                "Paste",
                "Go to Folder…",
                "Connect to Server…",
                "Undo",
                "Empty Trash…",
            ],
        ));
        let page = |page: Page| Row::new("Go to", page.glyph(), page.title(), Target::Page(page));
        rows.extend([
            page(Page::Remote),
            page(Page::Cloud),
            page(Page::Drives),
            page(Page::Favorites),
        ]);
        rows.extend(named("Go to", &["Recents", "Trash"]));
        rows.extend(named(
            "View",
            &[
                "as List",
                "as Grid",
                "Show Hidden Files",
                "Show Path Bar",
                "Sort by Name",
                "Sort by Date Modified",
                "Sort by Size",
                "Sort by Kind",
                "Reverse Sort Order",
            ],
        ));
        rows.extend(named("Settings", &["Settings…"]));
        rows
    }

    /// Something typed: commands, places and recents, best first, then the
    /// index's files that aren't already there.
    fn results(&self, query: &str, cx: &App) -> Vec<Row> {
        let all = commands::all();
        let prefs = cx.global::<Preferences>();
        let ticked = commands::checked(prefs);
        let palette = commands::OpenPalette.name();
        let mut pool: Vec<Row> = all
            .iter()
            .filter(|command| command.id() != palette)
            .map(|command| self.command_row(RESULTS, command, prefs, &ticked))
            .collect();
        for page in [Page::Remote, Page::Cloud, Page::Drives, Page::Favorites] {
            pool.push(Row::new(
                RESULTS,
                page.glyph(),
                page.title(),
                Target::Page(page),
            ));
            pool.extend(self.page_rows(page).into_iter().map(|row| Row {
                section: RESULTS,
                ..row
            }));
        }
        // The Go menu's own commands already find these, with their shortcuts.
        pool.retain(|row| !matches!(&row.target, Target::Folder(path) if has_go_command(path)));
        pool.extend(self.recents.iter().map(|entry| entry_row(RESULTS, entry)));
        let mut seen = HashSet::new();
        pool.retain(|row| path_of(row).is_none_or(|path| seen.insert(path.to_owned())));
        let mut ranked: Vec<(i32, usize, Row)> = pool
            .into_iter()
            .enumerate()
            .filter_map(|(ix, row)| Some((rank(query, &row)?, ix, row)))
            .collect();
        // Best first; then what would run; then the shorter title, as closer
        // to what was typed; then the table's order, which follows the menus.
        ranked.sort_by_key(|(score, ix, row)| (-score, !row.enabled, row.title.len(), *ix));
        let mut rows: Vec<Row> = ranked
            .into_iter()
            .take(RESULT_LIMIT)
            .map(|(_, _, row)| row)
            .collect();
        let shown: HashSet<PathBuf> = rows
            .iter()
            .filter_map(path_of)
            .map(Path::to_path_buf)
            .collect();
        rows.extend(
            self.files
                .iter()
                .filter(|entry| !shown.contains(&entry.path))
                .map(|entry| entry_row("Files", entry)),
        );
        rows
    }

    fn page_rows(&self, page: Page) -> Vec<Row> {
        match page {
            Page::Root => Vec::new(),
            Page::Remote => {
                let mut rows: Vec<Row> = self.remotes.iter().map(remote_row).collect();
                rows.extend(self.now.servers.iter().map(|a| self.server_row(a)));
                rows
            }
            Page::Cloud => self.cloud.iter().map(cloud_row).collect(),
            Page::Drives => self.now.volumes.iter().map(drive_row).collect(),
            Page::Favorites => self.now.favorites.iter().map(favorite_row).collect(),
        }
    }

    fn command_row(
        &self,
        section: &'static str,
        command: &Command,
        prefs: &Preferences,
        ticked: &[&str],
    ) -> Row {
        let keys = command.shortcuts(&prefs.shortcuts);
        Row {
            keys: keys.first().map(|keys| commands::label(keys)),
            enabled: self.now.available.contains(command.id()),
            destructive: command.destructive,
            checked: ticked.contains(&command.id()),
            found_by: format!("{}: {}", command.group.label(), command.title),
            ..Row::new(
                section,
                glyph_for(command),
                command.title,
                Target::Command((command.action)()),
            )
        }
    }

    /// A saved server: connected ones open where they're mounted.
    fn server_row(&self, address: &str) -> Row {
        let row = |target, about: String| Row {
            forget: Some(Saved::Server(address.to_owned())),
            ..Row::new(SMB, "server", address.to_owned(), target).about(about)
        };
        match servers::mounted(address, &self.now.volumes) {
            Some(volume) => row(
                Target::Folder(volume.mount.clone()),
                format!("Connected at {}", volume.mount.display()),
            ),
            None => row(Target::Server(address.to_owned()), "Not connected".into()),
        }
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let enabled: Vec<usize> = (0..self.rows.len())
            .filter(|&ix| self.rows[ix].enabled)
            .collect();
        let Some(last) = enabled.len().checked_sub(1) else {
            return;
        };
        let at = self
            .active
            .and_then(|active| enabled.iter().position(|&ix| ix == active));
        let next = match (at, down) {
            (None, true) => 0,
            (None, false) => last,
            (Some(at), true) => (at + 1) % enabled.len(),
            (Some(at), false) => (at + last) % enabled.len(),
        };
        self.active = Some(enabled[next]);
        self.scroll.scroll_to_item(enabled[next]);
        cx.notify();
    }

    fn run(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(ix).filter(|row| row.enabled) else {
            return;
        };
        let event = match &row.target {
            Target::Command(action) => PaletteEvent::Run(action.boxed_clone()),
            Target::Folder(path) => PaletteEvent::Go(path.clone()),
            Target::File(path) => PaletteEvent::Open(path.clone()),
            Target::Server(address) => PaletteEvent::Connect(address.clone()),
            Target::Remote(host) => {
                PaletteEvent::Say(format!("Browsing {host} isn't available yet."))
            }
            Target::Page(page) => {
                let page = *page;
                return self.open_page(page, cx);
            }
            Target::AddRemote => return self.add_remote(cx),
        };
        cx.emit(event);
    }

    fn open_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.root_query = self.query.read(cx).text().to_owned();
        self.page = page;
        self.query.update(cx, |field, cx| {
            field.set_placeholder(page.placeholder(), cx);
            field.set_text("", 0..0, cx);
        });
    }

    /// Back to the rows the page was opened from, on the row that opened it.
    pub(crate) fn leave_page(&mut self, cx: &mut Context<Self>) {
        self.land_on = Some(self.page.title().into());
        self.page = Page::Root;
        let typed = std::mem::take(&mut self.root_query);
        self.query.update(cx, |field, cx| {
            field.set_placeholder(Page::Root.placeholder(), cx);
            field.set_text(&typed, typed.len()..typed.len(), cx);
        });
    }

    /// Saves what's typed as a server. It connects only when it's opened.
    fn add_remote(&mut self, cx: &mut Context<Self>) {
        let host = self.typed(cx);
        let Some(store) = cx.try_global::<Places>().map(|p| p.store.clone()) else {
            return;
        };
        let added = cx
            .background_executor()
            .spawn(async move { store.add_remote(&host, None, None) });
        cx.spawn(async move |this, cx| {
            let added = added.await;
            let _ = this.update(cx, |this, cx| match added {
                Ok(source) => {
                    this.land_on = Some(source.host.clone().into());
                    if !this.remotes.contains(&source) {
                        this.remotes.push(source);
                    }
                    this.query.update(cx, |field, cx| field.clear(cx));
                }
                Err(why) => {
                    this.error = Some(why);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Forgets the server under the keys. Nothing on it changes, and a share
    /// it mounted stays mounted.
    fn forget(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(store) = cx.try_global::<Places>().map(|p| p.store.clone()) else {
            return;
        };
        let forgotten = match self.rows.get(ix).and_then(|row| row.forget.clone()) {
            Some(Saved::Remote(id)) => {
                self.remotes.retain(|source| source.id != id);
                cx.background_executor()
                    .spawn(async move { store.forget_remote(&id).map(drop) })
            }
            Some(Saved::Server(address)) => {
                self.now.servers.retain(|saved| *saved != address);
                cx.background_executor()
                    .spawn(async move { store.forget_server(&address) })
            }
            None => return,
        };
        self.refresh(cx);
        cx.spawn(async move |this, cx| {
            if let Err(why) = forgotten.await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(why);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn eject(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(mount) = self.rows.get(ix).and_then(|row| row.eject.clone()) {
            cx.emit(PaletteEvent::Eject(mount));
        }
    }

    fn active_row(&self) -> Option<&Row> {
        self.active.and_then(|ix| self.rows.get(ix))
    }

    fn row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.active == Some(ix);
        let first_of_section = ix == 0 || self.rows[ix - 1].section != row.section;
        let muted = if !row.enabled {
            content(0.3)
        } else if row.destructive {
            theme::destructive_ink(active)
        } else {
            theme::row_muted(active)
        };
        let enabled = row.enabled;
        let frame = if row.destructive && enabled {
            theme::destructive_row(active)
        } else {
            theme::row(active)
        };
        let button_ink = if active { muted } else { content(0.55) };
        let eject_button = row.eject.is_some().then(|| {
            row_icon_button(("palette-eject", ix), "Eject", "eject", button_ink)
                .occlude()
                .on_click(cx.listener(move |this, _, _, cx| this.eject(ix, cx)))
        });
        let forget_button = row.forget.is_some().then(|| {
            row_icon_button(
                ("palette-forget", ix),
                "Forget this server",
                "cancel-01",
                button_ink,
            )
            .occlude()
            .on_click(cx.listener(move |this, _, _, cx| this.forget(ix, cx)))
        });
        let label = (first_of_section && !row.section.is_empty())
            .then(|| theme::section_label(row.section).when(ix == 0, |label| label.mt(px(0.))));
        let title = div()
            .truncate()
            .when(!enabled, |d| d.text_color(muted))
            .child(row.title.clone());
        div().flex_shrink_0().children(label).child(
            theme::row_slot(ix)
                .role(Role::ListBoxOption)
                .aria_label(row.title.clone())
                .aria_selected(active)
                .when(!enabled, |d| d.cursor_default())
                .when(enabled, |d| {
                    d.on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.active != Some(ix) {
                            this.active = Some(ix);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.run(ix, cx)))
                })
                .child(
                    frame
                        .debug_selector(move || format!("palette-row-{ix}"))
                        .when(row.subtitle.is_some(), |d| d.h(px(TALL_ROW)))
                        .when(row.eject.is_some() || row.forget.is_some(), |d| d.pr_1())
                        .child(icon(row.glyph, 16.).flex_shrink_0().text_color(if enabled {
                            if active { muted } else { content(0.55) }
                        } else {
                            muted
                        }))
                        .child(match row.subtitle.clone() {
                            None => div().flex_1().min_w_0().child(title),
                            Some(subtitle) => div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(title)
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(theme::TEXT_CAPTION))
                                        .text_color(muted)
                                        .child(subtitle),
                                ),
                        })
                        .when(row.checked, |d| {
                            d.child(icon("check-hi", 12.).flex_shrink_0().text_color(muted))
                        })
                        .children(row.keys.clone().map(|keys| menu::shortcut(keys, muted)))
                        .children(eject_button)
                        .children(forget_button)
                        .when(matches!(row.target, Target::Page(_)), |d| {
                            d.child(icon("chevron-right", 12.).flex_shrink_0().text_color(muted))
                        }),
                ),
        )
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .w_full()
            .flex()
            .items_center()
            .gap_2()
            .child(
                icon("search", 14.)
                    .flex_shrink_0()
                    .text_color(content(0.55)),
            )
            .when(self.page != Page::Root, |d| {
                d.child(
                    theme::button("palette-back", ButtonLook::Secondary)
                        .debug_selector(|| "palette-back".into())
                        .aria_label("Back to the command palette")
                        .flex_shrink_0()
                        .gap_1()
                        .child(icon("chevron-left", 12.))
                        .child(self.page.title())
                        .on_click(cx.listener(|this, _, _, cx| this.leave_page(cx))),
                )
            })
            .child(div().flex_1().min_w_0().child(self.query.clone()))
    }

    fn footer(&self, cx: &App) -> impl IntoElement + use<> {
        let page_key = match self.page {
            Page::Remote => Some((commands::label("cmd-backspace"), "Forget")),
            Page::Drives => commands::shortcut_label(Eject.name(), cx).map(|keys| (keys, "Eject")),
            _ => None,
        };
        div()
            .h(px(FOOTER_HEIGHT))
            .px_4()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(content(0.06))
            .child(key_hint(&["↑", "↓"], "Navigate"))
            .child(key_hint(&["Enter"], "Select"))
            .child(key_hint(
                &["Esc"],
                if self.page == Page::Root {
                    "Close"
                } else {
                    "Back"
                },
            ))
            .children(page_key.map(|(keys, what)| key_hint(&[keys.as_str()], what)))
    }

    /// The one line where there are no rows.
    fn empty_line(&self) -> &'static str {
        match self.page {
            Page::Cloud if self.cloud.is_empty() => "No cloud folders on this Mac.",
            Page::Favorites if self.now.favorites.is_empty() => "No favorites.",
            _ => "No results.",
        }
    }
}

impl Render for PaletteView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = (0..self.rows.len())
            .map(|ix| {
                let row = &self.rows[ix];
                self.row(ix, row, cx).into_any_element()
            })
            .collect();
        let error = self.error.clone().map(|why| {
            div()
                .debug_selector(|| "palette-error".into())
                .px_4()
                .pt_2()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(theme::danger())
                .child(why)
        });
        let empty = rows.is_empty().then(|| {
            div()
                .h(px(theme::ROW_HEIGHT * 2.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_BODY))
                .text_color(content(0.65))
                .child(self.empty_line())
        });
        // The card sits 16% of the way down, with its header and footer.
        let room =
            window.viewport_size().height.as_f32() * 0.8 - theme::TITLE_BAR_HEIGHT - FOOTER_HEIGHT;
        let list = div()
            .id("palette-rows")
            .role(Role::ListBox)
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .max_h(px(LIST_HEIGHT.min(room)))
            .p_2()
            .flex()
            .flex_col()
            .children(rows)
            .children(empty);
        let mut context = KeyContext::default();
        context.add("Palette");
        if let Some(row) = self.active_row() {
            if matches!(row.target, Target::Page(_)) {
                context.add("OnPageRow");
            }
            if row.forget.is_some() {
                context.add("OnForgettable");
            }
        }
        if self.page != Page::Root && self.query.read(cx).text().is_empty() {
            context.add("CanLeave");
        }
        overlay("palette-overlay")
            .key_context(context)
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &Run, _, cx| {
                if let Some(ix) = this.active {
                    this.run(ix, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &OpenPage, _, cx| {
                if let Some(ix) = this.active {
                    this.run(ix, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Leave, _, cx| this.leave_page(cx)))
            .on_action(cx.listener(|this, _: &Forget, _, cx| {
                if let Some(ix) = this.active {
                    this.forget(ix, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Eject, _, cx| {
                if let Some(ix) = this.active {
                    this.eject(ix, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| {
                if this.page == Page::Root {
                    cx.emit(PaletteEvent::Dismissed);
                } else {
                    this.leave_page(cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismissed)),
            )
            .child(
                bare_card("palette", "Command palette", self.header(cx), {
                    div()
                        .flex()
                        .flex_col()
                        .children(error)
                        .child(list)
                        .child(self.footer(cx))
                })
                .w(px(WIDTH)),
            )
    }
}

/// The full-window layer behind a quick sheet (the palette, Go to Folder):
/// a light scrim, with the card near the top, where the eye already is.
pub fn overlay(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .flex_col()
        .items_center()
        .pt(gpui::relative(0.16))
        .px_6()
        .bg(theme::scrim(1.))
}

/// A quick sheet's card: its text field, or a title, on top, `body` below.
pub fn card(
    id: &'static str,
    label: &'static str,
    header: impl IntoElement,
    body: impl IntoElement,
) -> gpui::Stateful<gpui::Div> {
    bare_card(id, label, header, div().p_2().child(body))
}

/// A quick sheet's card with nothing around `below`, for what runs edge to edge.
fn bare_card(
    id: &'static str,
    label: &'static str,
    header: impl IntoElement,
    below: impl IntoElement,
) -> gpui::Stateful<gpui::Div> {
    theme::floating(Elevation::Sheet)
        .id(id)
        .role(Role::Dialog)
        .aria_label(label)
        .w(px(560.))
        .max_w_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(theme::TITLE_BAR_HEIGHT))
                        .px_4()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(content(0.08))
                        .text_size(px(theme::TEXT_TITLE))
                        .font_weight(FontWeight::MEDIUM)
                        .child(header),
                )
                .child(below),
        )
}

/// Keys and what they do, in a sheet's footer.
fn key_hint(keys: &[&str], what: &'static str) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_size(px(theme::TEXT_CAPTION))
        .text_color(content(0.65))
        .children(keys.iter().map(|key| {
            div()
                .h(px(KEY_CAP_HEIGHT))
                .min_w(px(KEY_CAP_HEIGHT))
                .px_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(content(0.08))
                .child(key.to_string())
        }))
        .child(what)
}

const ACTIONS: &str = "Actions";
const RESULTS: &str = "Results";
const SFTP: &str = "SFTP servers";
const SMB: &str = "SMB servers";

/// The commands on a selection, in the order worth offering them.
const SELECTION_ORDER: [&str; 13] = [
    "Open",
    "Quick Look",
    "Rename",
    "Duplicate",
    "Copy",
    "Copy Path",
    "Show Package Contents",
    "New Folder with Selection",
    "Add to Sidebar",
    "Reveal in Finder",
    "Open in Terminal",
    "Put Back",
    "Move to Trash",
];

fn path_of(row: &Row) -> Option<&Path> {
    match &row.target {
        Target::Folder(path) | Target::File(path) => Some(path),
        _ => None,
    }
}

/// Whether a command in the Go menu goes to `path`.
fn has_go_command(path: &Path) -> bool {
    let home = browser::home();
    path == home
        || path == Path::new("/")
        || path == Path::new("/Applications")
        || ["Desktop", "Documents", "Downloads"]
            .iter()
            .any(|folder| path == home.join(folder))
}

fn glyph_for(command: &Command) -> &'static str {
    use commands::Group;
    match command.title {
        "Move to Trash" | "Empty Trash…" | "Trash" | "Put Back" => "trash",
        "Recents" => "clock",
        "Connect to Server…" => "server",
        "Search" | "Reveal in Finder" => "search",
        "Eject" => "eject",
        "Open" => "external-link",
        "Quick Look" => "eye",
        "Rename" => "pencil",
        "Duplicate" | "Copy" => "copy",
        "Copy Path" => "link",
        "New Folder" | "New Folder with Selection" | "Add to Sidebar" | "Go to Folder…" => {
            "folder-01"
        }
        "Settings…" => "settings",
        "Undo" | "Redo" => "rotate-ccw",
        "Macintosh HD" => "hard-drive",
        "Downloads" | "Copies and Moves" => "arrow-down-circle",
        "Applications" => "app-window",
        "Open in Terminal" => "terminal",
        "New File" => "file",
        "New Tab" => "add-01",
        _ => match command.group {
            Group::App | Group::Window | Group::Help => "app-window",
            Group::File | Group::Edit => "file",
            Group::Go => "folder-01",
            Group::View => "sliders",
        },
    }
}

fn add_remote_row(query: &str) -> Row {
    if query.is_empty() {
        Row {
            enabled: false,
            ..Row::new(SFTP, "server", "Add Server…", Target::AddRemote)
                .about("Type a host or ssh alias.")
        }
    } else {
        Row::new(
            SFTP,
            "server",
            format!("Add \"{query}\""),
            Target::AddRemote,
        )
        .about("Saves it. Nothing connects until you open it.")
    }
}

fn remote_row(source: &Source) -> Row {
    Row {
        forget: Some(Saved::Remote(source.id.clone())),
        ..Row::new(
            SFTP,
            "server",
            source.host.clone(),
            Target::Remote(source.host.clone()),
        )
        .about("Not connected")
    }
}

fn cloud_row(folder: &CloudFolder) -> Row {
    Row::new(
        "",
        "cloud",
        folder.name.clone(),
        Target::Folder(folder.path.clone()),
    )
    .about(paths::abbreviate(&folder.path))
}

fn drive_row(volume: &Volume) -> Row {
    let row = Row::new(
        "",
        if volume.network {
            "server"
        } else {
            "hard-drive"
        },
        volume.name.clone(),
        Target::Folder(volume.mount.clone()),
    );
    let row = if volume.total > 0 {
        row.about(format!("{} free", format_bytes(volume.free)))
    } else {
        row
    };
    Row {
        eject: (!volume.startup).then(|| volume.mount.clone()),
        ..row
    }
}

fn favorite_row(place: &Place) -> Row {
    let path = &place.favorite.path;
    let row = Row::new(
        "",
        browser::glyph_of(path),
        browser::label_of(path),
        Target::Folder(place.at().unwrap_or(path).to_path_buf()),
    );
    match &place.availability {
        Availability::Available(at) => row.about(paths::abbreviate(at)),
        Availability::Unavailable(why) => Row {
            enabled: false,
            ..row.about(why.clone())
        },
    }
}

/// A recent or an indexed file. A folder shows in the window; anything else
/// opens in its app.
fn entry_row(section: &'static str, entry: &Entry) -> Row {
    let folder = entry.kind == Kind::Dir;
    let shown = entry
        .path
        .parent()
        .map_or_else(String::new, paths::abbreviate);
    Row::new(
        section,
        if folder { "folder-01" } else { "file" },
        entry.name.clone(),
        if folder {
            Target::Folder(entry.path.clone())
        } else {
            Target::File(entry.path.clone())
        },
    )
    .about(shown)
}

/// How well `query` finds `row`, if it does: its title counts over anything
/// else, and the start of its title most.
fn rank(query: &str, row: &Row) -> Option<i32> {
    match score(query, &row.title) {
        Some(score) => {
            let start = row.title.to_lowercase().starts_with(&query.to_lowercase());
            Some(score + 100 + if start { 50 } else { 0 })
        }
        None => score(query, &row.found_by),
    }
}

/// How well `query` matches `text` as a subsequence, ignoring case; `None` if
/// it doesn't. Letters at the start of a word and runs of letters count more,
/// so "sh" finds "Show Hidden Files" before "Search: Home".
fn score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut at = 0;
    let mut score = 0;
    let mut previous: Option<usize> = None;
    for wanted in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (at..text.len()).find(|&i| text[i] == wanted)?;
        let word_start = found == 0 || !text[found - 1].is_alphanumeric();
        score += 1
            + if word_start { 6 } else { 0 }
            + if previous == Some(found.wrapping_sub(1)) {
                4
            } else {
                0
            };
        previous = Some(found);
        at = found + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn rows_that_arrive_late_leave_the_keys_on_the_same_row(cx: &mut TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Preferences::default());
            commands::apply(cx);
        });
        let now = Now {
            available: HashSet::new(),
            scopes: Vec::new(),
            favorites: Vec::new(),
            volumes: Vec::new(),
            servers: vec!["smb://nas/media".into(), "smb://nas/photos".into()],
            index: Arc::default(),
        };
        let (view, cx) = cx.add_window_view(|_, cx| PaletteView::new(now, cx));
        view.update(cx, |view, cx| view.open_page(Page::Remote, cx));
        cx.run_until_parked();
        let under_keys =
            |view: &PaletteView, _: &App| view.active_row().map(|row| row.title.to_string());
        // The second SMB server, which is the second row here.
        view.update(cx, |view, cx| view.step(true, cx));
        let server = Some("smb://nas/photos".to_string());
        assert_eq!(view.read_with(cx, under_keys), server);
        // The saved SFTP servers land above it, where its index now is another's.
        view.update(cx, |view, cx| {
            view.remotes = vec![
                Source::new("a", "a.local", None).unwrap(),
                Source::new("b", "b.local", None).unwrap(),
            ];
            view.refresh(cx);
        });
        assert_eq!(view.read_with(cx, under_keys), server);
        // When it's gone, the keys go to the first row that would run.
        view.update(cx, |view, cx| {
            view.now.servers.pop();
            view.refresh(cx);
        });
        assert_eq!(view.read_with(cx, under_keys).as_deref(), Some("a.local"));
    }

    #[test]
    fn letters_in_order_match_and_word_starts_rank_first() {
        assert_eq!(score("", "Go: Back"), Some(0));
        assert!(score("gb", "Go: Back").is_some());
        assert!(score("bg", "Go: Back").is_none());
        assert!(score("BACK", "Go: Back").is_some());
        // Word starts beat letters buried inside words.
        let show_hidden = score("sh", "View: Show Hidden Files").unwrap();
        let mashed = score("sh", "Go: Macintosh HD").unwrap();
        assert!(show_hidden > mashed);
        // A run of letters beats the same letters scattered.
        assert!(score("zoom", "View: Zoom In") > score("zoom", "Go: Zap photo mix"));
    }
}
