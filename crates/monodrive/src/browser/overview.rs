//! The Overview page, which takes the pane's list while it's on screen: how
//! much is free and what fills the startup disk, what could free more, the
//! user's largest things across every location, and the other locations.
//! Everything it says comes from `monodrive_core::overview`, read off the UI
//! thread. Opening it draws what's saved at once. Measuring is the app's, not
//! the page's: it starts a little after launch and whenever a page opens, and
//! goes on whatever is on screen.

use super::pane::Pane;
use super::sidebar::Places;
use super::{ConnectToServer, EmptyTrash, Offer, Operations, SharedIndex, lock, with_owner};
use crate::notify;
use crate::theme::{self, ButtonLook, content, icon, row_icon_button, section_label, text_button};
use gpui::{
    AnyElement, App, Context, FontWeight, Global, Hsla, MouseButton, Role, ScrollHandle, Task,
    Window, div, prelude::*, px, relative,
};
use monodrive_core::cloud::{self, CloudFolder};
use monodrive_core::ops::{self, AgentChange, Origin, Request};
use monodrive_core::overview::{
    self, Env, Group, Headline, Kind, Location, LocationKind, Measured, Overview, Part, ScanRow,
    Spot, Suggestion,
};
use monodrive_core::paths;
use monodrive_core::storage::{self, Stop};
use monodrive_core::volumes::Volume;
use monodrive_core::worktrees::Git;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// Folders measured at once.
const MEASURING: usize = 2;
/// How often the storage notifications look again.
const WATCH_EVERY: Duration = Duration::from_secs(3 * 60 * 60);
/// How many of a group's folders it lists when opened, largest first.
const SPOTS_SHOWN: usize = 12;
/// How far back the page says what agents changed, and how many.
const CHANGES_SINCE: u64 = 24 * 60 * 60;
const CHANGES_SHOWN: usize = 5;
/// A row's title and detail, 16 pt each, in the app's row with a second line.
const ROW_HEIGHT: f32 = theme::TALL_ROW_HEIGHT;
const TITLE_LINE: f32 = 16.;
/// A row's own side padding, which puts its text on the pane's inset.
const ROW_INSET: f32 = 8.;
/// The trailing columns every row of the list keeps, filled or not.
const SIZE_SLOT: f32 = 72.;
const ACTION_SLOT: f32 = 20.;
/// Wide enough for "Free up"'s widest button.
const FREE_ACTION_SLOT: f32 = 112.;
/// The disk's bar, and the dot that marks each of its parts.
const BAR_HEIGHT: f32 = 12.;
const SWATCH: f32 = 8.;
/// What the parts the bar doesn't have picked fade to while one is.
const FADED: f32 = 0.4;
/// The left column's share of the pane, and the least it keeps. A pane with
/// less than both minimums puts the list under the picks.
const PICKER_SHARE: f32 = 0.3;
const PICKER_MIN_WIDTH: f32 = 220.;
const LIST_MIN_WIDTH: f32 = 420.;
const MARK_SLOT: f32 = 16.;

/// What the left column picked, which the right lists: a part of the startup
/// disk, another location, or what agents changed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pick {
    Part(Part),
    /// By where it's mounted, since two drives can share a name, and by name
    /// while it isn't.
    Location {
        mount: Option<PathBuf>,
        name: String,
    },
    Changes,
}

impl Pick {
    fn location(location: &Location) -> Self {
        Pick::Location {
            mount: location.mount.clone(),
            name: location.name.clone(),
        }
    }

    /// Unique among the picks, for its row and the right column's scroll.
    fn key(&self) -> String {
        match self {
            Pick::Part(part) => part.name().to_string(),
            Pick::Location {
                mount: Some(mount), ..
            } => format!("on-{}", mount.display()),
            Pick::Location { name, .. } => format!("named-{name}"),
            Pick::Changes => "changes".to_string(),
        }
    }
}

/// What the page shows, as last read.
#[derive(Default)]
pub(in crate::browser) struct OverviewPage {
    about: Option<Overview>,
    changes: Vec<AgentChange>,
    /// What the left column has picked. `None` until someone picks, and then
    /// the page leads with what can be freed or the largest kind.
    pick: Option<Pick>,
    /// The left column's scroll, and whether it scrolls the pick into view at
    /// the next draw, after a key moved it.
    picker_scroll: ScrollHandle,
    reveal: bool,
    /// Reads what's saved, then again each time a folder is measured, while
    /// the page is on screen.
    reading: Option<Task<()>>,
    /// The mount being scanned, because someone asked.
    scanning: Option<Scanning>,
    /// The groups in several places, listing each of them.
    open: HashSet<String>,
}

struct Scanning {
    mount: PathBuf,
    progress: Arc<storage::Progress>,
    _task: Task<()>,
}

/// Where the Overview reads and saves, for this app's places.
fn env_of(places: &Places) -> Env {
    Env {
        root: places.root.clone(),
        ..Env::new(places.trash.clone(), places.store.clone())
    }
}

/// What a read needs, each kept from the first read of a page: asking macOS
/// for volumes can wait on a slow share.
#[derive(Clone)]
struct Reader {
    env: Env,
    volumes: Arc<Vec<Volume>>,
    cloud: Arc<Vec<CloudFolder>>,
}

impl Reader {
    /// Asks macOS for the volumes and cloud folders, so call it off the UI
    /// thread.
    fn new(places: &Places) -> Self {
        Self {
            env: env_of(places),
            volumes: Arc::new(places.drives.list()),
            cloud: Arc::new(places.drives.cloud()),
        }
    }

    fn read(&self) -> Overview {
        overview::read(
            &self.env,
            &self.volumes,
            &self.cloud,
            &|_| false,
            overview::now(),
        )
    }

    /// The Overview and what agents changed lately, through `ops`, which the
    /// read lets go of so the owner can exit when the window does.
    fn read_with(&self, ops: Option<Arc<Mutex<ops::Client>>>) -> (Overview, Vec<AgentChange>) {
        let since = overview::now().saturating_sub(CHANGES_SINCE);
        let changes = ops.map_or_else(Vec::new, |ops| {
            lock(&ops).agent_changes(since, CHANGES_SHOWN)
        });
        (self.read(), changes)
    }
}

/// The owner's client, for one read.
fn operations(cx: &App) -> Option<Arc<Mutex<ops::Client>>> {
    cx.try_global::<Operations>().map(|ops| ops.0.clone())
}

/// Measures what the Overview has due for the whole app, so it goes on
/// whichever page is on screen, and tells each open page as each folder
/// finishes.
#[derive(Clone, Default)]
pub(crate) struct Measurer(Arc<Mutex<Measuring>>);

#[derive(Default)]
struct Measuring {
    /// Folders measured so far in this run, and how many it has, while one
    /// runs.
    run: Option<(usize, usize)>,
    /// What's due changed during the run, so another follows it.
    again: bool,
    heard: Vec<async_channel::Sender<()>>,
}

impl Global for Measurer {}

impl Measurer {
    fn state(&self) -> std::sync::MutexGuard<'_, Measuring> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn say(&self) {
        self.state().heard.retain(|page| page.try_send(()).is_ok());
    }

    fn listen(&self) -> async_channel::Receiver<()> {
        let (send, receive) = async_channel::unbounded();
        self.state().heard.push(send);
        receive
    }

    /// Folders measured and how many there are, while a run is underway.
    fn progress(&self) -> Option<(usize, usize)> {
        self.state().run
    }

    /// Starts a run, or asks for another once this one ends. False when one is
    /// already underway.
    fn begin(&self) -> bool {
        let mut state = self.state();
        if state.run.is_some() {
            state.again = true;
            return false;
        }
        state.run = Some((0, 0));
        true
    }

    fn set(&self, run: Option<(usize, usize)>) {
        self.state().run = run;
        self.say();
    }

    fn measured_one(&self) {
        if let Some((done, _)) = &mut self.state().run {
            *done += 1;
        }
        self.say();
    }

    /// Ends a run, unless what's due changed while it ran.
    fn end(&self) -> bool {
        let mut state = self.state();
        if std::mem::take(&mut state.again) {
            return false;
        }
        state.run = None;
        drop(state);
        self.say();
        true
    }
}

/// Measures every folder the Overview has due, `MEASURING` at a time, saving
/// each total as it finishes. When a run is already underway, another
/// follows it, for what became due since.
pub(crate) fn measure_overview(cx: &mut App) {
    let Some(places) = cx.try_global::<Places>().cloned() else {
        return;
    };
    let measurer = cx.default_global::<Measurer>().clone();
    if !measurer.begin() {
        return;
    }
    let background = cx.background_executor().clone();
    cx.background_executor()
        .spawn(async move {
            loop {
                let reader = Reader::new(&places);
                let due: VecDeque<PathBuf> = reader.read().due.into();
                measurer.set(Some((0, due.len())));
                let queue = Arc::new(Mutex::new(due));
                let workers: Vec<Task<()>> = (0..MEASURING)
                    .map(|_| {
                        let (queue, measurer, env) =
                            (queue.clone(), measurer.clone(), reader.env.clone());
                        background.spawn(async move {
                            let progress = storage::Progress::default();
                            loop {
                                let next = queue
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .pop_front();
                                let Some(folder) = next else { return };
                                overview::measure(&env, &folder, &progress, None, overview::now());
                                measurer.measured_one();
                            }
                        })
                    })
                    .collect();
                for worker in workers {
                    worker.await;
                }
                if measurer.end() {
                    return;
                }
            }
        })
        .detach();
}

/// Looks at the startup disk and the clean worktrees now and every few hours,
/// for the notifications Settings wants. Asks Git and measures only what a
/// switch that's on needs.
pub(crate) fn watch_storage(cx: &mut App) {
    cx.spawn(async |cx| {
        loop {
            let wanted = cx.update(|cx| {
                let places = cx.try_global::<Places>().cloned()?;
                let index = cx.try_global::<SharedIndex>()?.0.clone();
                let low = notify::wanted(notify::Kind::LowDisk, cx);
                let worktrees = notify::wanted(notify::Kind::Worktrees, cx);
                (low || worktrees).then_some((places, index, low, worktrees))
            });
            if let Some((places, index, low, worktrees)) = wanted {
                let found = cx
                    .background_spawn(
                        async move { look_at_storage(&places, &index, low, worktrees) },
                    )
                    .await;
                if let Some(said) = cx.update(|cx| notify::storage(found, overview::now(), cx)) {
                    cx.background_spawn(async move { said.save() }).await;
                }
            }
            cx.background_executor().timer(WATCH_EVERY).await;
        }
    })
    .detach();
}

/// What the storage notifications would say about: the startup disk if it's
/// low, and the clean worktrees, asked about and measured again first.
fn look_at_storage(
    places: &Places,
    index: &std::sync::OnceLock<Result<monodrive_core::index::Index, String>>,
    low: bool,
    worktrees: bool,
) -> notify::Storage {
    let reader = Reader::new(places);
    let clean = worktrees
        .then(|| {
            let git = Git::find().ok()?;
            let search = |name: &str, limit| places.drives.find_files(index, name, limit);
            let _ = overview::check_worktrees(&reader.env, &git, search);
            let (_, due) = overview::clean_worktrees(&reader.env, &reader.volumes, overview::now());
            let progress = storage::Progress::default();
            for folder in due {
                overview::measure(&reader.env, &folder, &progress, None, overview::now());
            }
            overview::clean_worktrees(&reader.env, &reader.volumes, overview::now()).0
        })
        .flatten();
    let disk = low
        .then(|| {
            let about = reader.read();
            let headline = about.headline.filter(|disk| disk.low)?;
            let trash = headline.trash.map_or(0, |trash| trash.unshared);
            let freeable = about
                .suggestions
                .iter()
                .map(|suggestion| suggestion.size.unshared)
                .sum::<u64>()
                + trash;
            Some((headline, freeable))
        })
        .flatten();
    notify::Storage {
        low: disk,
        worktrees: clean,
        said: notify::Said::read(),
        allowed: notify::allowed(),
    }
}

impl Pane {
    /// Reads what's saved and draws it, then has what's due measured, and
    /// reads again as each folder finishes. Connects to nothing: a scan or a
    /// server is someone's choice.
    pub(super) fn read_overview(&mut self, cx: &mut Context<Self>) {
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        if self.overview.reading.is_some() {
            return;
        }
        let ops = operations(cx);
        let index = self.index.clone();
        let heard = cx.default_global::<Measurer>().listen();
        self.overview.reading = Some(cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let drives = places.drives.clone();
            let (reader, first) = background
                .spawn(async move {
                    let reader = Reader::new(&places);
                    let first = reader.read_with(ops);
                    (reader, first)
                })
                .await;
            let take = |this: &mut Pane, (about, changes), cx: &mut Context<Pane>| {
                this.overview.about = Some(about);
                this.overview.changes = changes;
                cx.notify();
            };
            // Worktrees are worth asking Git about only when suggestions show.
            let low = first.0.headline.as_ref().is_some_and(|disk| disk.low);
            let _ = this.update(cx, |this, cx| take(this, first, cx));
            let ops = cx.update(|cx| operations(cx));
            let found = background.spawn({
                let reader = reader.clone();
                async move {
                    let git = Git::find().ok();
                    let search = |name: &str, limit| drives.find_files(&index, name, limit);
                    let _ = overview::discover(&reader.env, git.as_ref(), search);
                    if let (true, Some(git)) = (low, &git) {
                        let _ = overview::check_worktrees(&reader.env, git, search);
                    }
                    reader.read_with(ops)
                }
            });
            let found = found.await;
            let _ = this.update(cx, |this, cx| take(this, found, cx));
            cx.update(measure_overview);
            while heard.recv().await.is_ok() {
                // One read for every folder finished since the last.
                while heard.try_recv().is_ok() {}
                let (reader, ops) = (reader.clone(), cx.update(|cx| operations(cx)));
                let read = background.spawn(async move { reader.read_with(ops) }).await;
                let _ = this.update(cx, |this, cx| take(this, read, cx));
            }
        }));
    }

    /// The page is off screen: it stops reading, and what it was scanning
    /// stops. Measuring goes on.
    pub(super) fn leave_overview(&mut self) {
        self.overview.reading = None;
        if let Some(scanning) = self.overview.scanning.take() {
            scanning.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn overview_env(&self, cx: &Context<Self>) -> Option<Env> {
        cx.try_global::<Places>().map(env_of)
    }

    /// Reads again after something changed what's saved.
    fn reread_overview(&mut self, cx: &mut Context<Self>) {
        self.overview.reading = None;
        self.read_overview(cx);
    }

    /// A suggestion's one button: checks its proof again, then the Trash, with
    /// the usual Undo. Refused while a build is running.
    fn clean_suggestion(&mut self, dir: PathBuf, window: &Window, cx: &mut Context<Self>) {
        let Some(env) = self.overview_env(cx) else {
            return;
        };
        let name = quoted(&dir);
        with_owner(
            window,
            cx,
            move |client| {
                let outcome = overview::clean(&env, &dir, Origin::Window, client)?;
                overview::forget(&env, &dir);
                Ok(outcome)
            },
            move |this, cleaned, _, cx| {
                match cleaned {
                    Ok(outcome) => match outcome.failures().next() {
                        Some(why) => this.toast(why.to_string(), true, None, cx),
                        None => this.toast(
                            format!("Moved {name} to the Trash"),
                            false,
                            Some(Offer::Undo(outcome.op)),
                            cx,
                        ),
                    },
                    Err(why) => this.toast(why, true, None, cx),
                }
                this.reread_overview(cx);
            },
        );
    }

    /// A row of a drive's scan to the Trash, which its scan then leaves out.
    fn trash_scan_row(&mut self, path: PathBuf, window: &Window, cx: &mut Context<Self>) {
        let Some(env) = self.overview_env(cx) else {
            return;
        };
        let name = quoted(&path);
        with_owner(
            window,
            cx,
            move |client| {
                let outcome = client.submit(
                    Origin::Window,
                    Request::Trash {
                        paths: vec![path.clone()],
                    },
                )?;
                if outcome.failures().next().is_none() {
                    let _ = overview::forget_scan_row(&env, &path);
                }
                Ok(outcome)
            },
            move |this, trashed, _, cx| {
                match trashed {
                    Ok(outcome) => match outcome.failures().next() {
                        Some(why) => this.toast(why.to_string(), true, None, cx),
                        None => this.toast(
                            format!("Moved {name} to the Trash"),
                            false,
                            Some(Offer::Undo(outcome.op)),
                            cx,
                        ),
                    },
                    Err(why) => this.toast(why, true, None, cx),
                }
                this.reread_overview(cx);
            },
        );
    }

    /// A cloud folder's button: the provider keeps it and frees the blocks.
    fn remove_downloads(&mut self, path: PathBuf, window: &Window, cx: &mut Context<Self>) {
        let Some(env) = self.overview_env(cx) else {
            return;
        };
        let name = quoted(&path);
        let removed = cx.background_executor().spawn(async move {
            let asked = cloud::remove_download(&path)?;
            overview::forget(&env, &path);
            Ok::<_, String>(asked)
        });
        cx.spawn_in(window, async move |this, cx| {
            let removed = removed.await;
            let _ = this.update(cx, |this, cx| {
                match removed {
                    Ok(_) => {
                        this.toast(format!("Removing the download of {name}"), false, None, cx)
                    }
                    Err(why) => this.toast(why, true, None, cx),
                }
                this.reread_overview(cx);
            });
        })
        .detach();
    }

    /// Scans a mounted drive or share, which only ever happens on request, and
    /// keeps its totals as its last scan.
    fn scan_location(&mut self, mount: PathBuf, cx: &mut Context<Self>) {
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        let env = env_of(&places);
        let progress = Arc::new(storage::Progress::default());
        let scanned = cx.background_executor().spawn({
            let (progress, mount) = (progress.clone(), mount.clone());
            async move {
                let report = storage::scan(&mount, &progress, None)?;
                if report.stopped == Some(Stop::Cancelled) {
                    return Ok(());
                }
                let volumes = places.drives.list();
                let volume = overview::volume_at(&volumes, &mount)
                    .ok_or_else(|| format!("{} is no longer mounted.", mount.display()))?;
                overview::record_scan(&env, volume, &report, overview::now()).map(|_| ())
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let scanned: Result<(), String> = scanned.await;
            let _ = this.update(cx, |this, cx| {
                this.overview.scanning = None;
                if let Err(why) = scanned {
                    this.toast(why, true, None, cx);
                }
                this.reread_overview(cx);
                cx.notify();
            });
        });
        self.overview.scanning = Some(Scanning {
            mount,
            progress,
            _task: task,
        });
        cx.notify();
    }

    fn cancel_scan(&mut self, cx: &mut Context<Self>) {
        if let Some(scanning) = self.overview.scanning.take() {
            scanning.progress.cancel.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// The left column's picks, top to bottom: the startup disk's parts, the
    /// other locations, then what agents changed, when they changed anything.
    fn picks(&self, about: &Overview, listed: &[(Part, Option<u64>)]) -> Vec<Pick> {
        listed
            .iter()
            .map(|(part, _)| Pick::Part(*part))
            .chain(about.locations.iter().map(Pick::location))
            .chain((!self.overview.changes.is_empty()).then_some(Pick::Changes))
            .collect()
    }

    /// What the right lists: the pick, while it's still there to pick, or else
    /// what can be freed on a low disk, or else the largest kind.
    fn picked(&self, about: &Overview, listed: &[(Part, Option<u64>)]) -> Option<Pick> {
        let picks = self.picks(about, listed);
        if let Some(pick) = self.overview.pick.as_ref().filter(|p| picks.contains(p)) {
            return Some(pick.clone());
        }
        let freeable = Pick::Part(Part::Freeable);
        if about.headline.as_ref().is_some_and(|disk| disk.low) && picks.contains(&freeable) {
            return Some(freeable);
        }
        picks
            .iter()
            .find(|pick| matches!(pick, Pick::Part(Part::Kind(_))))
            .or(picks.first())
            .cloned()
    }

    fn pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        self.overview.pick = Some(pick);
        cx.notify();
    }

    /// ↑ and ↓ move the left column's pick.
    pub(super) fn step_overview(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(about) = &self.overview.about else {
            return;
        };
        let listed = disk_parts(about, &about.parts());
        let picks = self.picks(about, &listed);
        let at = self
            .picked(about, &listed)
            .and_then(|pick| picks.iter().position(|p| *p == pick));
        let next = match at {
            Some(at) if down => (at + 1).min(picks.len().saturating_sub(1)),
            Some(at) => at.saturating_sub(1),
            None => 0,
        };
        if let Some(pick) = picks.into_iter().nth(next) {
            self.overview.reveal = true;
            self.pick(pick, cx);
        }
    }

    /// The page, when it's what's on screen: how much is free and the disk's
    /// bar across the top, then the startup disk's parts and the other
    /// locations on the left, and what the picked one holds on the right.
    pub(super) fn overview_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.showing_overview() {
            return None;
        }
        let about = self.overview.about.clone().unwrap_or_default();
        let progress = cx.try_global::<Measurer>().and_then(Measurer::progress);
        let measuring = progress.is_some();
        let now = overview::now();
        let parts = about.parts();
        let listed = disk_parts(&about, &parts);
        let pick = self.picked(&about, &listed);

        let headline = about
            .headline
            .as_ref()
            .map(|disk| self.headline(&parts, disk, pick.as_ref(), cx));
        let (picker, at) = self.picker(&about, &listed, pick.as_ref(), measuring, cx);
        if std::mem::take(&mut self.overview.reveal)
            && let Some(at) = at
        {
            self.overview.picker_scroll.scroll_to_item(at);
        }
        let picked = pick
            .as_ref()
            .map(|pick| self.picked_page(&about, &listed, pick, measuring, now, cx));
        // Rows pad themselves by 8 pt, so every text starts on the pane's
        // inset, as the footer's does.
        let inset = px(theme::PANE_INSET - ROW_INSET);
        let columns = if self.chrome.width < PICKER_MIN_WIDTH + LIST_MIN_WIDTH {
            // Too narrow for two: the picks, then what's picked, under them.
            div()
                .id("overview")
                .role(Role::Group)
                .aria_label("Overview")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .track_scroll(&self.overview.picker_scroll)
                .border_t_1()
                .border_color(content(0.06))
                .px(inset)
                .pb_4()
                .children(picker)
                .children(picked)
        } else {
            // A scroll of its own for each pick, so a new one starts at the top.
            let scroll = format!(
                "overview-{}",
                pick.as_ref().map_or_else(String::new, Pick::key)
            );
            div()
                .id("overview-columns")
                .flex_1()
                .min_h_0()
                .flex()
                .border_t_1()
                .border_color(content(0.06))
                .child(
                    div()
                        .id("overview-picker")
                        .role(Role::ListBox)
                        .aria_label("What fills the disk")
                        .w(relative(PICKER_SHARE))
                        .min_w(px(PICKER_MIN_WIDTH))
                        .flex_shrink_0()
                        .overflow_y_scroll()
                        .track_scroll(&self.overview.picker_scroll)
                        .border_r_1()
                        .border_color(content(0.06))
                        .px(inset)
                        .pb_2()
                        .children(picker),
                )
                .child(
                    div()
                        .id(scroll)
                        .role(Role::Group)
                        .aria_label("Overview")
                        .flex_1()
                        .min_w_0()
                        .overflow_y_scroll()
                        .px(inset)
                        .pb_4()
                        .children(picked),
                )
        };
        Some(
            div()
                .debug_selector(|| "overview-page".into())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                // No rectangle to drag here, and no menu of folder commands.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.focus_list(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .children(headline.map(|headline| {
                    div()
                        .flex_shrink_0()
                        .px(inset)
                        .pt_4()
                        .pb_3()
                        .child(headline)
                }))
                .child(columns)
                .child(footer(&about, progress, now))
                .into_any_element(),
        )
    }

    /// The left column, one element a row or label: the startup disk's parts
    /// with what each takes, then the other locations and what agents changed.
    /// Also where the pick is among them, to scroll it into view.
    fn picker(
        &self,
        about: &Overview,
        listed: &[(Part, Option<u64>)],
        pick: Option<&Pick>,
        measuring: bool,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Option<usize>) {
        let parts: Vec<AnyElement> = listed
            .iter()
            .map(|&(part, bytes)| {
                let this = Pick::Part(part);
                let picked = pick == Some(&this);
                let trail = Trail::Size(size_label(bytes, false, measuring));
                let mark = swatch(part).into_any_element();
                self.pick_row(this, mark, part.name().to_string(), trail, picked, cx)
            })
            .collect();
        let locations: Vec<AnyElement> = about
            .locations
            .iter()
            .map(|location| {
                let this = Pick::location(location);
                let picked = pick == Some(&this);
                let note = match location.capacity {
                    _ if !location.connected => "Not connected".to_string(),
                    Some((_, free)) => format!("{} free", amount(free)),
                    None => String::new(),
                };
                let mark = location_mark(location).into_any_element();
                let name = location.name.clone();
                self.pick_row(this, mark, name, Trail::Note(note), picked, cx)
            })
            .collect();
        let changes = (!self.overview.changes.is_empty()).then(|| {
            let count = self.overview.changes.len().to_string();
            let mark = icon("bot", 16.)
                .text_color(content(0.55))
                .into_any_element();
            let picked = pick == Some(&Pick::Changes);
            let name = "Changed by agents".to_string();
            self.pick_row(Pick::Changes, mark, name, Trail::Note(count), picked, cx)
        });
        let mut items: Vec<AnyElement> = Vec::new();
        if let Some(disk) = &about.headline {
            items.push(section_label(disk.name.clone()).into_any_element());
        }
        items.extend(parts);
        if !locations.is_empty() {
            items.push(section_label("Other locations").into_any_element());
            items.extend(locations);
        }
        if let Some(changes) = changes {
            items.push(section_label("Agents").into_any_element());
            items.push(changes);
        }
        // Each label stands before its rows, so the picks are in order among
        // the items that aren't labels.
        let picks = self.picks(about, listed);
        let at = pick
            .and_then(|pick| picks.iter().position(|p| p == pick))
            .map(|at| {
                let labels = usize::from(about.headline.is_some())
                    + usize::from(at >= listed.len() && !about.locations.is_empty())
                    + usize::from(at >= listed.len() + about.locations.len());
                at + labels
            });
        (items, at)
    }

    /// A row of the left column, which picks what the right lists.
    fn pick_row(
        &self,
        pick: Pick,
        mark: AnyElement,
        name: String,
        trail: Trail,
        picked: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let trail = match trail {
            Trail::Size(size) => div().flex_shrink_0().child(size),
            Trail::Note(note) => div()
                .flex_shrink_0()
                .text_size(px(theme::TEXT_CAPTION))
                .text_color(content(0.65))
                .child(note),
        };
        let selector = format!("overview-pick-{name}");
        theme::row_slot(format!("overview-pick-{}", pick.key()))
            .role(Role::ListBoxOption)
            .aria_label(name.clone())
            .aria_selected(picked)
            .debug_selector(move || selector)
            .on_click(cx.listener(move |this, _, _, cx| this.pick(pick.clone(), cx)))
            .child(
                div()
                    .h(px(theme::SIDEBAR_ROW_HEIGHT))
                    .px(px(ROW_INSET))
                    .rounded(px(theme::RADIUS_ROW))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(theme::TEXT_BODY))
                    .map(|row| {
                        if picked {
                            row.bg(content(0.14))
                        } else {
                            row.group_hover(theme::ROW_GROUP, |s| s.bg(content(0.08)))
                        }
                    })
                    .child(mark_slot(mark))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(trail),
            )
            .into_any_element()
    }

    /// The right column: what the picked part or location holds, under its
    /// name and what it is.
    fn picked_page(
        &self,
        about: &Overview,
        listed: &[(Part, Option<u64>)],
        pick: &Pick,
        measuring: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (header, rows) = match pick {
            Pick::Part(part) => {
                let bytes = listed
                    .iter()
                    .find(|(listed, _)| listed == part)
                    .and_then(|(_, bytes)| *bytes);
                let share = bytes.map(|bytes| format!("{} of used", percent(bytes, used(about))));
                let (size, rows) = match part {
                    Part::Freeable => {
                        let (total, rows) = self.free_up(about, cx);
                        match bytes {
                            Some(_) => (total, rows),
                            None => (size_label(None, false, measuring), rows),
                        }
                    }
                    Part::Kind(kind) => (
                        size_label(bytes, false, measuring),
                        self.kind_rows(about, *kind, measuring, now, cx),
                    ),
                    Part::Other => (size_label(bytes, false, measuring), Vec::new()),
                };
                let header = page_header(
                    swatch(*part).into_any_element(),
                    part.name().to_string(),
                    described(*part).to_string(),
                    figure(size, share),
                );
                (header, rows)
            }
            Pick::Location { .. } => {
                let Some(location) = about
                    .locations
                    .iter()
                    .find(|location| Pick::location(location) == *pick)
                else {
                    return div().into_any_element();
                };
                let scanning = location.mount.as_ref().is_some_and(|mount| {
                    self.overview
                        .scanning
                        .as_ref()
                        .is_some_and(|scanning| &scanning.mount == mount)
                });
                let action = self
                    .location_action(location, scanning, cx)
                    .unwrap_or_else(|| div().into_any_element());
                let header = page_header(
                    location_mark(location).into_any_element(),
                    location.name.clone(),
                    location_state(location, scanning, now),
                    action,
                );
                let mut rows = Vec::new();
                if let Some(scan) = &location.scan {
                    let most = scan
                        .rows
                        .iter()
                        .map(|row| row.size.on_disk)
                        .max()
                        .unwrap_or(0);
                    for row in &scan.rows {
                        let share = row.size.on_disk as f32 / most.max(1) as f32;
                        rows.push(self.scan_row(row, location, share, now, cx));
                    }
                }
                (header, rows)
            }
            Pick::Changes => {
                let header = page_header(
                    icon("bot", 16.)
                        .text_color(content(0.55))
                        .into_any_element(),
                    "Changed by agents".to_string(),
                    "What agents changed in the last day.".to_string(),
                    div().into_any_element(),
                );
                let rows = self
                    .overview
                    .changes
                    .iter()
                    .map(|change| change_row(change, now).into_any_element())
                    .collect();
                (header, rows)
            }
        };
        div()
            .flex()
            .flex_col()
            .child(header)
            .children(rows)
            .into_any_element()
    }

    /// What the suggestions and the Trash free, together, and a row for each.
    /// They add up to the green part of the disk's bar.
    fn free_up(&self, about: &Overview, cx: &mut Context<Self>) -> (String, Vec<AnyElement>) {
        let trash = about
            .headline
            .as_ref()
            .and_then(|disk| disk.trash)
            .filter(|trash| trash.unshared > 0);
        let sizes: Vec<Measured> = about
            .suggestions
            .iter()
            .map(|suggestion| suggestion.size)
            .chain(trash)
            .collect();
        let mut total = amount(sizes.iter().map(|size| size.unshared).sum());
        if sizes.iter().any(|size| !size.complete) {
            total.push('+');
        }
        let mut rows: Vec<AnyElement> = about
            .suggestions
            .iter()
            .map(|suggestion| self.suggestion_row(suggestion, cx).into_any_element())
            .collect();
        rows.extend(trash.map(|trash| self.trash_row(trash, cx).into_any_element()));
        (total, rows)
    }

    /// How much is free, in the page's one large figure, and the disk drawn
    /// by what fills it.
    fn headline(
        &self,
        parts: &[(Part, u64)],
        disk: &Headline,
        pick: Option<&Pick>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let picked = match pick {
            Some(Pick::Part(part)) => Some(*part),
            _ => None,
        };
        let bar = self.disk_bar(parts, disk, picked, cx);
        div()
            .px(px(ROW_INSET))
            .flex()
            .flex_col()
            .debug_selector(|| "overview-headline".into())
            .child(
                // GPUI lines up the boxes' bottoms, not the baselines, so
                // each line is as tall as its text and the smaller one rises
                // by what the larger one's descent adds.
                div()
                    .min_w_0()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::TEXT_DISPLAY))
                            .line_height(px(theme::TEXT_DISPLAY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{} free", amount(disk.free))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .mb(px(2.))
                            .text_size(px(theme::TEXT_BODY))
                            .line_height(px(theme::TEXT_BODY))
                            .text_color(content(0.65))
                            .child(format!("of {} on {}", amount(disk.total), disk.name)),
                    ),
            )
            .child(div().mt_3().child(bar))
    }

    /// The startup disk as one pill of touching parts: each part in its color,
    /// then the free space as track. A press on a part picks it, and while one
    /// is picked the others fade. Only the pill's two ends are round, since a
    /// parent can't clip its children to its corners.
    fn disk_bar(
        &self,
        parts: &[(Part, u64)],
        disk: &Headline,
        picked: Option<Part>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let segment = |grow: u64, color: Hsla| {
            div()
                .h_full()
                .flex_basis(px(0.))
                .flex_grow(grow as f32)
                .min_w(px(3.))
                .bg(color)
        };
        div()
            .h(px(BAR_HEIGHT))
            .flex()
            .children(parts.iter().enumerate().map(|(ix, (part, bytes))| {
                let part = *part;
                segment(*bytes, part_color(part))
                    .when(ix == 0, |d| d.rounded_l_full())
                    .when(picked.is_some_and(|picked| picked != part), |d| {
                        d.opacity(FADED)
                    })
                    .id(format!("overview-part-{}", part.name()))
                    .role(Role::Button)
                    .aria_label(part.name())
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(Pick::Part(part), cx)))
            }))
            .child(
                segment(disk.free, content(0.14))
                    .rounded_r_full()
                    .when(parts.is_empty(), |d| d.rounded_l_full()),
            )
    }

    /// A suggestion as a row: what it is and why it's safe, what it frees,
    /// then its button.
    fn suggestion_row(
        &self,
        suggestion: &Suggestion,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let title = suggestion.title();
        let (glyph, button, id) = match suggestion.trash() {
            Some(dir) => {
                let dir = dir.to_path_buf();
                let id = format!("overview-suggestion-{}", dir.display());
                let button = text_button(
                    format!("overview-clean-{}", dir.display()),
                    "Move to Trash",
                    ButtonLook::Secondary,
                )
                .debug_selector(|| "overview-clean".into())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.clean_suggestion(dir.clone(), window, cx)
                }));
                ("folder-01", button, id)
            }
            // Nothing here removes a worktree: plan 22's page reviews each.
            None => {
                let worktrees = suggestion.worktrees().to_vec();
                let button = text_button(
                    "overview-review-worktrees",
                    "Review…",
                    ButtonLook::Secondary,
                )
                .debug_selector(|| "overview-review-worktrees".into())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.review_worktrees(worktrees.clone(), window, cx)
                }));
                (
                    "git-branch",
                    button,
                    "overview-suggestion-worktrees".to_string(),
                )
            }
        };
        still(
            free_row(
                glyph,
                title.clone(),
                suggestion.why(),
                size_of(suggestion.size),
                button,
            )
            .id(id)
            .role(Role::Group)
            .aria_label(title),
        )
    }

    /// What's in the Trash, the last thing to free: trashing a suggestion
    /// moves its size here.
    fn trash_row(&self, trash: Measured, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let button = text_button("overview-empty-trash", "Empty Trash…", ButtonLook::Caution)
            .on_click(cx.listener(|_, _, window, cx| {
                window.dispatch_action(Box::new(EmptyTrash), cx);
            }));
        still(
            free_row(
                "trash",
                "Items in the Trash".to_string(),
                "Emptying the Trash gives their space back.".to_string(),
                size_of(trash),
                button,
            )
            .id("overview-trash")
            .role(Role::Group)
            .aria_label("Items in the Trash"),
        )
    }

    /// A kind's things on the startup disk, largest first, with bars against
    /// the list's largest. macOS's own volumes lead the system's list, since
    /// no folder holds them.
    fn kind_rows(
        &self,
        about: &Overview,
        kind: Kind,
        measuring: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let startup = about.headline.as_ref().map(|disk| disk.name.as_str());
        // A project with copies elsewhere ranks by what it takes here; its
        // detail says what the others take.
        let here = |group: &Group| {
            group
                .spots
                .iter()
                .filter(|spot| Some(spot.location.as_str()) == startup)
                .filter_map(|spot| spot.size.map(|size| size.on_disk))
                .reduce(|all, size| all + size)
        };
        let groups: Vec<&Group> = about
            .stuff
            .iter()
            .filter(|group| group.kind == kind)
            .collect();
        let volumes = about
            .headline
            .as_ref()
            .and_then(|disk| disk.system)
            .filter(|_| kind == Kind::System);
        let most = groups
            .iter()
            .filter_map(|group| here(group))
            .chain(volumes)
            .max()
            .unwrap_or(0);
        // A list with a button anywhere keeps its room on every row.
        let room = groups.iter().any(|group| {
            group.spots.len() > 1
                || group.kind == Kind::Cloud && group.spots.iter().any(|spot| spot.reachable)
        });
        let share =
            |size: Option<u64>, most: u64| size.map_or(0., |size| size as f32 / most.max(1) as f32);
        // What "Free up" counts inside a spot on the startup disk, so its bar
        // shows why the disk's key gives its kind less than it takes.
        let freeable = |spot: &Spot| -> u64 {
            if Some(spot.location.as_str()) != startup {
                return 0;
            }
            about
                .suggestions
                .iter()
                .flat_map(|suggestion| &suggestion.folders)
                .filter(|(path, _)| path.starts_with(&spot.path))
                .map(|(_, size)| size.unshared)
                .sum()
        };
        let mut ranked: Vec<(Option<u64>, AnyElement)> = Vec::new();
        if let Some(bytes) = volumes {
            let row = stuff_row(
                ("hard-drive", tint(kind)),
                "macOS".to_string(),
                "Its own volumes".to_string(),
                Bar {
                    share: share(Some(bytes), most),
                    freeable: 0.,
                    ink: tint(kind),
                },
                amount(bytes),
                if room { End::Room } else { End::Size },
            );
            ranked.push((Some(bytes), still(row).into_any_element()));
        }
        for group in groups {
            let size = here(group);
            let freed = group.spots.iter().map(freeable).sum::<u64>();
            ranked.push((
                size,
                self.group_row(
                    group,
                    size,
                    (share(size, most), share(Some(freed), most)),
                    (measuring, room),
                    now,
                    cx,
                ),
            ));
            if self.overview.open.contains(&group.name) {
                // A project's copies are named for where they are; the rest
                // of the disk's folders for themselves.
                let shown = match group.kind {
                    Kind::Projects => group.spots.len(),
                    _ => SPOTS_SHOWN,
                };
                for spot in group.spots.iter().take(shown) {
                    let name = match group.kind {
                        Kind::Projects => spot.location.clone(),
                        _ => file_name(&spot.path),
                    };
                    let copy = self.spot_row(
                        name,
                        spot,
                        group,
                        (
                            share(spot.size.map(|size| size.on_disk), most),
                            share(Some(freeable(spot)), most),
                        ),
                        (measuring, room),
                        now,
                        cx,
                    );
                    ranked.push((size, div().pl(px(16.)).child(copy).into_any_element()));
                }
            }
        }
        // Stable, so a project's copies stay under it.
        ranked.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
        ranked.into_iter().map(|(_, row)| row).collect()
    }

    /// A group's row. With copies in several places it lists them on a press;
    /// otherwise it opens its folder.
    fn group_row(
        &self,
        group: &Group,
        here: Option<u64>,
        (share, freeable): (f32, f32),
        (measuring, room): (bool, bool),
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let [spot] = group.spots.as_slice() {
            return self.spot_row(
                group.name.clone(),
                spot,
                group,
                (share, freeable),
                (measuring, room),
                now,
                cx,
            );
        }
        let name = group.name.clone();
        // Where it is on the startup disk, whose list this is and whose size
        // the row shows, then what each other copy takes.
        let startup = self
            .overview
            .about
            .as_ref()
            .and_then(|about| about.headline.as_ref())
            .map(|disk| disk.name.as_str());
        let (on_startup, elsewhere): (Vec<&Spot>, Vec<&Spot>) = group
            .spots
            .iter()
            .partition(|spot| Some(spot.location.as_str()) == startup);
        let places = if group.kind == Kind::Projects {
            on_startup
                .iter()
                .map(|spot| paths::abbreviate(&spot.path))
                .chain(elsewhere.iter().map(|spot| match spot.size {
                    Some(size) => format!(
                        "{} on {}",
                        size_label(Some(size.on_disk), !size.complete, false),
                        spot.location
                    ),
                    None => format!("Not measured on {}", spot.location),
                }))
                .collect::<Vec<_>>()
                .join(" · ")
        } else {
            // Its folders, largest first.
            group
                .spots
                .iter()
                .map(|spot| file_name(&spot.path))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let selector = format!("overview-spot-{name}");
        let tint = tint(group.kind);
        let open = self.overview.open.contains(&name);
        let chevron = icon(
            if open {
                "chevron-down-hi"
            } else {
                "chevron-right"
            },
            12.,
        )
        .text_color(content(0.55));
        theme::row_slot(format!("overview-group-{name}"))
            .role(Role::Button)
            .debug_selector(move || selector)
            .aria_label(name.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.overview.open.remove(&name) {
                    this.overview.open.insert(name.clone());
                }
                cx.notify();
            }))
            .child(
                stuff_row(
                    (glyph(group.kind), tint),
                    group.name.clone(),
                    places,
                    Bar {
                        share,
                        freeable,
                        ink: tint,
                    },
                    size_label(
                        here,
                        group
                            .spots
                            .iter()
                            .any(|spot| spot.size.is_none_or(|size| !size.complete)),
                        measuring,
                    ),
                    End::Button(chevron.into_any_element()),
                )
                .group_hover(theme::ROW_GROUP, |s| s.bg(content(0.08))),
            )
            .into_any_element()
    }

    /// A folder's row, which opens it. `room` keeps a button's room at its end
    /// for a list where another row has one.
    #[allow(clippy::too_many_arguments)]
    fn spot_row(
        &self,
        name: String,
        spot: &Spot,
        group: &Group,
        (share, freeable): (f32, f32),
        (measuring, room): (bool, bool),
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let where_ = paths::abbreviate(&spot.path);
        let age = spot.size.and_then(|size| size.age(now));
        // A copy under its project is already named for its location, and
        // the list is the startup disk's.
        let startup = self
            .overview
            .about
            .as_ref()
            .and_then(|about| about.headline.as_ref())
            .is_some_and(|disk| disk.name == spot.location);
        let place = (name != spot.location && !startup).then(|| format!("on {}", spot.location));
        let said = [Some(where_), place, age]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let partial = spot.size.is_some_and(|size| !size.complete);
        let path = spot.path.clone();
        let label = name.clone();
        let id = format!("overview-spot-{}", path.display());
        let action = (group.kind == Kind::Cloud && spot.reachable).then(|| {
            let path = path.clone();
            theme::tip(
                row_icon_button(
                    format!("overview-remove-download-{}", path.display()),
                    "Remove Download",
                    "cloud",
                    content(0.55),
                ),
                "Remove Download",
            )
            .occlude()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.remove_downloads(path.clone(), window, cx)
            }))
            .into_any_element()
        });
        let end = match action {
            Some(button) => End::Button(button),
            None if room => End::Room,
            None => End::Size,
        };
        let row = stuff_row(
            (glyph(group.kind), tint(group.kind)),
            name,
            said,
            Bar {
                share,
                freeable,
                ink: tint(group.kind),
            },
            size_label(spot.size.map(|size| size.on_disk), partial, measuring),
            end,
        );
        if !spot.reachable {
            return still(row).into_any_element();
        }
        let selector = format!("overview-spot-{label}");
        theme::row_slot(id)
            .role(Role::Button)
            .debug_selector(move || selector)
            .aria_label(label)
            .on_click(cx.listener(move |this, _, window, cx| this.go(path.clone(), window, cx)))
            .child(row.group_hover(theme::ROW_GROUP, |s| s.bg(content(0.08))))
            .into_any_element()
    }

    /// A folder at the top of a drive's scan: what it took then, and how long
    /// ago. Not on the startup disk, so it's drawn in plain ink, and fainter
    /// when the drive is gone.
    fn scan_row(
        &self,
        row: &ScanRow,
        location: &Location,
        share: f32,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = row.path.clone();
        let mut said = if location.mount.is_some() {
            paths::abbreviate(&row.path)
        } else {
            "Not connected".to_string()
        };
        if let Some(age) = row.size.age(now) {
            said += &format!(" · scanned {age}");
        }
        let ink = if location.mount.is_some() {
            content(0.55)
        } else {
            content(0.3)
        };
        let action =
            location.takes_trash.then(|| {
                let path = path.clone();
                theme::tip(
                    row_icon_button(
                        format!("overview-trash-{}", path.display()),
                        "Move to Trash",
                        "trash",
                        content(0.55),
                    ),
                    "Move to Trash",
                )
                .occlude()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.trash_scan_row(path.clone(), window, cx)
                }))
                .into_any_element()
            });
        let shown = stuff_row(
            ("folder-01", ink),
            row.name.clone(),
            said,
            Bar {
                share,
                freeable: 0.,
                ink,
            },
            size_label(Some(row.size.on_disk), !row.size.complete, false),
            action.map_or(End::Size, End::Button),
        );
        if location.mount.is_none() {
            return still(shown).into_any_element();
        }
        let selector = format!("overview-row-{}", row.name);
        theme::row_slot(format!("overview-row-{}", path.display()))
            .role(Role::Button)
            .debug_selector(move || selector)
            .aria_label(row.name.clone())
            .on_click(cx.listener(move |this, _, window, cx| this.go(path.clone(), window, cx)))
            .child(shown.group_hover(theme::ROW_GROUP, |s| s.bg(content(0.08))))
            .into_any_element()
    }

    /// Another location's button: scan it, stop the scan, or connect a saved
    /// share that isn't mounted.
    fn location_action(
        &self,
        location: &Location,
        scanning: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let action = match (&location.kind, &location.mount) {
            (_, Some(_)) if scanning => Some(
                text_button("overview-cancel-scan", "Cancel", ButtonLook::Secondary)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_scan(cx))),
            ),
            (_, Some(mount)) => {
                let mount = mount.clone();
                Some(
                    text_button(
                        format!("overview-scan-{}", mount.display()),
                        "Scan",
                        ButtonLook::Secondary,
                    )
                    .debug_selector(|| "overview-scan".into())
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.scan_location(mount.clone(), cx)),
                    ),
                )
            }
            (LocationKind::Share { address: Some(_) }, None) => Some(
                text_button(
                    format!("overview-connect-{}", location.name),
                    "Connect…",
                    ButtonLook::Secondary,
                )
                .on_click(cx.listener(|_, _, window, cx| {
                    window.dispatch_action(Box::new(ConnectToServer), cx);
                })),
            ),
            _ => None,
        };
        action.map(IntoElement::into_any_element)
    }
}

/// The startup disk's `parts` as the left column lists them, each with what
/// it takes once measured: what can be freed first, then each kind, largest
/// first, then the kinds with nothing of their own left once a suggestion is
/// counted or with nothing measured yet, then the rest. A low disk always
/// lists what can be freed, so the page can lead with it while it's measured.
fn disk_parts(about: &Overview, parts: &[(Part, u64)]) -> Vec<(Part, Option<u64>)> {
    let Some(disk) = &about.headline else {
        return Vec::new();
    };
    let of = |wanted: fn(&Part) -> bool| {
        parts
            .iter()
            .filter(move |(part, _)| wanted(part))
            .map(|(part, bytes)| (*part, Some(*bytes)))
    };
    let mut listed: Vec<(Part, Option<u64>)> = of(|part| *part == Part::Freeable)
        .chain(of(|part| matches!(part, Part::Kind(_))))
        .collect();
    if disk.low
        && listed
            .first()
            .is_none_or(|(part, _)| *part != Part::Freeable)
    {
        // Nothing to free is only known once nothing is left to measure.
        listed.insert(0, (Part::Freeable, about.due.is_empty().then_some(0)));
    }
    for group in &about.stuff {
        let part = Part::Kind(group.kind);
        let spots: Vec<&Spot> = group
            .spots
            .iter()
            .filter(|spot| spot.location == disk.name)
            .collect();
        if spots.is_empty() || listed.iter().any(|(listed, _)| *listed == part) {
            continue;
        }
        let measured = spots.iter().any(|spot| spot.size.is_some());
        listed.push((part, measured.then_some(0)));
    }
    listed.extend(of(|part| *part == Part::Other));
    listed
}

/// The startup disk's used space.
fn used(about: &Overview) -> u64 {
    about
        .headline
        .as_ref()
        .map_or(0, |disk| disk.total.saturating_sub(disk.free))
}

/// `bytes` as a share of `of`, rounded, and never 0% when there's something.
fn percent(bytes: u64, of: u64) -> String {
    let share = bytes as f64 * 100. / of.max(1) as f64;
    if bytes > 0 && share < 1. {
        "<1%".to_string()
    } else {
        format!("{share:.0}%")
    }
}

/// What a part of the disk is, under its name on the right.
fn described(part: Part) -> &'static str {
    match part {
        Part::Freeable => {
            "What the suggestions and the Trash give back. Each suggestion goes to the Trash first."
        }
        Part::Kind(Kind::Documents) => {
            "Desktop, Documents, Downloads and the other folders in Home."
        }
        Part::Kind(Kind::Projects) => {
            "Git repositories on this Mac. A copy on another drive lists under its project."
        }
        Part::Kind(Kind::Pictures) => "Your Pictures folder.",
        Part::Kind(Kind::Media) => "Your Movies and Music folders.",
        Part::Kind(Kind::Cloud) => {
            "What cloud folders keep downloaded. Placeholders take no space."
        }
        Part::Kind(Kind::Apps) => "Applications, and what apps keep in Library.",
        Part::Kind(Kind::Developer) => {
            "Developer tools, and the hidden folders in Home, which are mostly theirs."
        }
        Part::Kind(Kind::System) => "macOS's own volumes and the system's folders.",
        Part::Other => "Used, and in none of the above or not measured yet.",
    }
}

/// What a row of the left column ends in.
enum Trail {
    /// A part's size. The right column's title says its share.
    Size(String),
    /// How a location stands, or how many changes there are.
    Note(String),
}

/// A row's mark, centered in the column every mark of the list keeps.
fn mark_slot(mark: impl IntoElement) -> gpui::Div {
    div()
        .w(px(MARK_SLOT))
        .flex_shrink_0()
        .flex()
        .justify_center()
        .child(mark)
}

/// A location's mark, fainter while it's away.
fn location_mark(location: &Location) -> gpui::Svg {
    let glyph = match location.kind {
        LocationKind::Share { .. } | LocationKind::Sftp { .. } => "server",
        LocationKind::Drive | LocationKind::Disconnected => "hard-drive",
    };
    icon(glyph, 16.).text_color(content(if location.connected { 0.55 } else { 0.3 }))
}

/// The right column's title: the pick's mark and name, what it is, and its
/// size or button at the end.
fn page_header(mark: AnyElement, title: String, detail: String, end: AnyElement) -> gpui::Div {
    div()
        .px(px(ROW_INSET))
        .pt_4()
        .pb_2()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(mark_slot(mark))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(theme::TEXT_TITLE))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        ),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_BODY))
                        .text_color(content(0.65))
                        .child(detail),
                ),
        )
        .child(div().flex_shrink_0().child(end))
}

/// A part's size over its share of the used space, at the header's end.
fn figure(size: String, share: Option<String>) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .items_end()
        .gap_1()
        .child(
            div()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(FontWeight::SEMIBOLD)
                .child(size),
        )
        .children(share.map(|share| {
            div()
                .text_size(px(theme::TEXT_CAPTION))
                .text_color(content(0.65))
                .child(share)
        }))
        .into_any_element()
}

/// A kind's icon in its rows.
fn glyph(kind: Kind) -> &'static str {
    match kind {
        Kind::Cloud => "cloud",
        _ => "folder-01",
    }
}

/// Fixed, not the accent, so no accent can make two kinds look alike.
fn tint(kind: Kind) -> Hsla {
    match kind {
        Kind::Documents => theme::tint_blue(),
        Kind::Projects => theme::tint_pink(),
        Kind::Pictures => theme::tint_orange(),
        Kind::Media => theme::tint_purple(),
        Kind::Cloud => theme::tint_cyan(),
        Kind::Apps => theme::tint_indigo(),
        Kind::Developer => theme::tint_yellow(),
        Kind::System => content(0.55),
    }
}

fn part_color(part: Part) -> Hsla {
    match part {
        Part::Kind(kind) => tint(kind),
        Part::Freeable => theme::tint_green(),
        Part::Other => content(0.3),
    }
}

/// A part's dot in the left column and over its list.
fn swatch(part: Part) -> impl IntoElement {
    div()
        .size(px(SWATCH))
        .flex_shrink_0()
        .rounded_full()
        .bg(part_color(part))
}

/// What trashing something frees, with a + when part of it couldn't be read.
fn size_of(measured: Measured) -> String {
    let mut size = amount(measured.unshared);
    if !measured.complete {
        size.push('+');
    }
    size
}

/// A row of "Free up": what it is and why, what it frees, and its button,
/// which takes the same room on every row so the sizes line up.
fn free_row(
    glyph: &'static str,
    title: String,
    detail: String,
    size: String,
    button: impl IntoElement,
) -> gpui::Div {
    // Its detail is why it's safe, so it wraps rather than lose its end.
    div()
        .min_h(px(ROW_HEIGHT))
        .flex_shrink_0()
        .px(px(ROW_INSET))
        .py(px((ROW_HEIGHT - 2. * TITLE_LINE) / 2.))
        .rounded(px(theme::RADIUS_ROW))
        .flex()
        .items_center()
        .gap_2()
        .child(marked(
            icon(glyph, 16.).text_color(theme::tint_green()),
            title,
            Detail::Wraps(detail),
        ))
        .child(size_slot(size))
        .child(
            div()
                .w(px(FREE_ACTION_SLOT))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .child(button),
        )
}

/// A row's bar: its `share` of the list's largest, in `ink`, ending in the
/// `freeable` share that "Free up" counts, in that section's green.
struct Bar {
    share: f32,
    freeable: f32,
    ink: Hsla,
}

/// What a row of the list ends in. A list with a button anywhere keeps its
/// room on every row, so the bars and sizes line up; one without ends at
/// the size, on the same inset as the row's start.
enum End {
    Size,
    Room,
    Button(AnyElement),
}

/// A row of the list: what it is, where it lives, a bar of its drive,
/// what it takes up, and its button.
fn stuff_row(
    look: (&'static str, Hsla),
    title: String,
    detail: String,
    bar: Bar,
    size: String,
    end: End,
) -> gpui::Div {
    let share = bar.share.clamp(0., 1.);
    let freeable = bar.freeable.clamp(0., share);
    entry_row(look, title, detail)
        .child(
            theme::progress_track()
                .w(relative(0.24))
                .max_w(px(200.))
                .flex_shrink_0()
                .child(
                    div()
                        .h_full()
                        .w(relative(share))
                        .when(share > 0., |d| d.min_w(px(3.)))
                        .rounded_full()
                        .bg(bar.ink)
                        .flex()
                        .justify_end()
                        .when(freeable > 0., |d| {
                            d.child(
                                div()
                                    .h_full()
                                    .w(relative(freeable / share))
                                    .bg(theme::tint_green())
                                    .rounded_r_full()
                                    .when(freeable >= share, |d| d.rounded_l_full()),
                            )
                        }),
                ),
        )
        .child(size_slot(size))
        .map(|row| {
            let button = match end {
                End::Size => return row,
                End::Room => None,
                End::Button(button) => Some(button),
            };
            // The button's glyph, not its circle, ends on the row's inset,
            // as in every row that ends in buttons.
            row.pr_1().child(
                div()
                    .w(px(ACTION_SLOT))
                    .flex_shrink_0()
                    .flex()
                    .justify_center()
                    .children(button),
            )
        })
}

/// A row's size, flush right in its column.
fn size_slot(size: String) -> gpui::Div {
    div()
        .w(px(SIZE_SLOT))
        .flex_shrink_0()
        .flex()
        .justify_end()
        .text_size(px(theme::TEXT_BODY))
        .child(size)
}

/// A row's mark, title and detail, on the list's grid.
fn entry_row(look: (&'static str, Hsla), title: String, detail: String) -> gpui::Div {
    div()
        .h(px(ROW_HEIGHT))
        .flex_shrink_0()
        .px(px(ROW_INSET))
        .rounded(px(theme::RADIUS_ROW))
        .flex()
        .items_center()
        .gap_2()
        .child(marked(
            icon(look.0, 16.).text_color(look.1),
            title,
            Detail::Line(detail),
        ))
}

/// The muted text under a title.
enum Detail {
    /// Cut short at the row's end.
    Line(String),
    /// On as many lines as it takes.
    Wraps(String),
}

/// A mark centered on its title and the detail under it, as the bar and
/// size beside them are.
fn marked(mark: impl IntoElement, title: String, detail: Detail) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_shrink_0().flex().child(mark))
        .child(two_lines(title, detail))
}

/// A title, cut short rather than wrapped, over its detail.
fn two_lines(title: String, detail: Detail) -> gpui::Div {
    let detail = match detail {
        Detail::Line(text) => div().truncate().h(px(TITLE_LINE)).child(text),
        Detail::Wraps(text) => div().child(text),
    };
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(
            div()
                .truncate()
                .h(px(TITLE_LINE))
                .line_height(px(TITLE_LINE))
                .text_size(px(theme::TEXT_BODY))
                .child(title),
        )
        .child(
            detail
                .line_height(px(TITLE_LINE))
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65)),
        )
}

/// A row nothing opens, spaced as a pressable one is.
fn still(row: impl IntoElement) -> gpui::Div {
    div().flex_shrink_0().pb(px(theme::ROW_GAP)).child(row)
}

fn size_label(on_disk: Option<u64>, partial: bool, measuring: bool) -> String {
    match on_disk {
        Some(on_disk) if partial => format!("{}+", amount(on_disk)),
        Some(on_disk) => amount(on_disk),
        None if measuring => "Measuring…".to_string(),
        None => "Not measured".to_string(),
    }
}

/// A size worth reading at a glance: 44 GB past a hundred, 12.4 GB under it.
fn amount(bytes: u64) -> String {
    let n = bytes as f64;
    let (value, unit) = if n >= 1e12 {
        (n / 1e12, "TB")
    } else if n >= 1e9 {
        (n / 1e9, "GB")
    } else if n >= 1e6 {
        (n / 1e6, "MB")
    } else {
        (n / 1e3, "KB")
    };
    if value >= 100. || value.fract() < 0.05 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

/// How a location stands, in one line: whether it's away or its space, then
/// its scan. The mark already says a connected one is.
fn location_state(location: &Location, scanning: bool, now: u64) -> String {
    let stands = match location.capacity {
        _ if !location.connected => Some("Not connected".to_string()),
        Some((_, free)) => Some(format!("{} free", amount(free))),
        None => None,
    };
    let scan = if scanning {
        Some("scanning…".to_string())
    } else {
        match &location.scan {
            Some(scan) => Some(match scan.total.age(now) {
                Some(age) => format!("scanned {age}"),
                None => "scanned just now".to_string(),
            }),
            None if location.mount.is_some() => Some("not scanned".to_string()),
            None => None,
        }
    };
    let said = [stands, scan]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    let mut chars = said.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => said,
    }
}

fn change_row(change: &AgentChange, now: u64) -> impl IntoElement + use<> {
    let items = match change.items {
        1 => "1 item".to_string(),
        n => format!("{n} items"),
    };
    let when = overview::age(change.at, now).unwrap_or_else(|| "just now".to_string());
    still(entry_row(
        ("bot", content(0.55)),
        change
            .client
            .clone()
            .unwrap_or_else(|| "An agent".to_string()),
        format!("{}, {items}, {when}", change.title),
    ))
}

/// The bar under the page: how old the startup disk's sizes are. Each other
/// location says its own scan's age.
fn footer(
    about: &Overview,
    progress: Option<(usize, usize)>,
    now: u64,
) -> impl IntoElement + use<> {
    let startup = about.headline.as_ref().map(|disk| {
        let oldest = about
            .stuff
            .iter()
            .flat_map(|group| &group.spots)
            .filter(|spot| spot.location == disk.name)
            .filter_map(|spot| spot.size.map(|size| size.at))
            .min();
        match oldest {
            _ if progress.is_some_and(|(_, of)| of > 0) => {
                let (done, of) = progress.unwrap_or_default();
                format!("Measuring {}, {done} of {of} folders…", disk.name)
            }
            _ if progress.is_some() => format!("Measuring {}…", disk.name),
            Some(at) => format!(
                "{} measured {}",
                disk.name,
                overview::age(at, now).unwrap_or_else(|| "just now".to_string())
            ),
            None => format!("{} not measured yet", disk.name),
        }
    });
    theme::pane_bar(theme::PATH_BAR_HEIGHT)
        .flex_shrink_0()
        .border_t_1()
        .border_color(content(0.06))
        .text_size(px(theme::TEXT_SMALL))
        .text_color(content(0.65))
        .child(div().min_w_0().truncate().children(startup))
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn quoted(path: &Path) -> String {
    format!("\"{}\"", file_name(path))
}
