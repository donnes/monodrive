//! The Overview page, which takes the pane's list while it's on screen: how
//! much is free and what fills the startup disk, what could free more, the
//! user's largest things across every location, and the other locations.
//! Everything it says comes from `monodrive_core::overview`, read off the UI
//! thread. Opening it draws what's saved at once. Measuring is the app's, not
//! the page's: it starts a little after launch and whenever a page opens, and
//! goes on whatever is on screen.

use super::pane::Pane;
use super::sidebar::Places;
use super::{ConnectToServer, EmptyTrash, Offer, Operations, lock, with_owner};
use crate::theme::{self, ButtonLook, content, icon, row_icon_button, section_label, text_button};
use gpui::{
    AnyElement, App, Context, FontWeight, Global, Hsla, MouseButton, Role, Task, Window, div,
    prelude::*, px, relative,
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

/// Folders measured at once.
const MEASURING: usize = 2;
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
/// The disk's bar, and each dot of its key.
const BAR_HEIGHT: f32 = 12.;
const SWATCH: f32 = 8.;
const CHIP_MIN_WIDTH: f32 = 330.;
const CHIP_HEIGHT: f32 = 48.;

/// What the page shows, as last read.
#[derive(Default)]
pub(in crate::browser) struct OverviewPage {
    about: Option<Overview>,
    changes: Vec<AgentChange>,
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
            let _ = this.update(cx, |this, cx| take(this, first, cx));
            let ops = cx.update(|cx| operations(cx));
            let found = background.spawn({
                let reader = reader.clone();
                async move {
                    let git = Git::find().ok();
                    let _ = overview::discover(&reader.env, git.as_ref(), |name, limit| {
                        drives.find_files(&index, name, limit)
                    });
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

    /// The page, when it's what's on screen.
    pub(super) fn overview_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.showing_overview() {
            return None;
        }
        let about = self.overview.about.clone().unwrap_or_default();
        let progress = cx.try_global::<Measurer>().and_then(Measurer::progress);
        let measuring = progress.is_some();
        let now = overview::now();

        let headline = about
            .headline
            .as_ref()
            .map(|disk| self.headline(&about, disk));
        // What the green part of the disk's bar is made of, so the section
        // adds up to it.
        let trash = about
            .headline
            .as_ref()
            .and_then(|disk| disk.trash)
            .filter(|trash| trash.unshared > 0);
        let free_up = (!about.suggestions.is_empty() || trash.is_some()).then(|| {
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
            div()
                .flex()
                .flex_col()
                .child(section_label(format!("Free up {total}")))
                .children(
                    about
                        .suggestions
                        .iter()
                        .map(|suggestion| self.suggestion_row(suggestion, cx)),
                )
                .children(trash.map(|trash| self.trash_row(trash, cx)))
        });
        let largest = self.largest(&about, measuring, now, cx);
        // As many tiles across as fit whole, each as wide as the others.
        let inside = self.chrome.width - 2. * theme::PANE_INSET;
        let across = ((inside / CHIP_MIN_WIDTH).floor() as u16).max(1);
        let locations = (!about.locations.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(section_label("Other locations"))
                .child(
                    div()
                        .mx(px(ROW_INSET))
                        .grid()
                        .grid_cols(across)
                        .gap_2()
                        .children(
                            about
                                .locations
                                .iter()
                                .map(|location| self.location_chip(location, now, cx)),
                        ),
                )
        });
        let changes = (!self.overview.changes.is_empty()).then(|| {
            div()
                .flex()
                .flex_col()
                .child(section_label("Changed by agents"))
                .children(
                    self.overview
                        .changes
                        .iter()
                        .map(|change| change_row(change, now)),
                )
        });
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
                .child(
                    div()
                        .id("overview")
                        .role(Role::Group)
                        .aria_label("Overview")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        // Rows pad themselves by 8 pt, so every text starts on
                        // the pane's inset, as the footer's does.
                        .px(px(theme::PANE_INSET - ROW_INSET))
                        .py_4()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .children(headline)
                                .children(free_up)
                                .child(largest)
                                .children(locations)
                                .children(changes),
                        ),
                )
                .child(footer(&about, progress, now))
                .into_any_element(),
        )
    }

    /// How much is free, in the page's one large figure, and the disk drawn
    /// by what fills it, with a key.
    fn headline(&self, about: &Overview, disk: &Headline) -> impl IntoElement + use<> {
        let parts = about.parts();
        let key = parts.iter().map(|(part, bytes)| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(swatch(*part))
                .child(div().text_color(content(0.65)).child(part.name()))
                .child(amount(*bytes))
        });
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
            .child(div().mt_3().child(disk_bar(&parts, disk)))
            .child(
                div()
                    .mt_2()
                    .flex()
                    .flex_wrap()
                    .gap_x(px(14.))
                    .gap_y_1()
                    .text_size(px(theme::TEXT_SMALL))
                    .children(key),
            )
    }

    /// A suggestion as a row: what it is and why it's safe, what it frees,
    /// then its button.
    fn suggestion_row(
        &self,
        suggestion: &Suggestion,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let evidence = &suggestion.evidence;
        let dir = suggestion.path.clone();
        let button = text_button(
            format!("overview-clean-{}", dir.display()),
            "Move to Trash",
            ButtonLook::Secondary,
        )
        .debug_selector(|| "overview-clean".into())
        .on_click(
            cx.listener(move |this, _, window, cx| this.clean_suggestion(dir.clone(), window, cx)),
        );
        still(
            free_row(
                "folder-01",
                evidence.title(),
                evidence.why(),
                size_of(suggestion.size),
                button,
            )
            .id(format!("overview-suggestion-{}", suggestion.path.display()))
            .role(Role::Group)
            .aria_label(evidence.title()),
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

    /// The largest things on the startup disk, then on each scanned drive,
    /// each list with bars against its own largest, since a backup's folders
    /// and the startup disk's don't share any space.
    fn largest(
        &self,
        about: &Overview,
        measuring: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
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
        let most = about.stuff.iter().filter_map(here).max().unwrap_or(0);
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
                .filter(|suggestion| suggestion.path.starts_with(&spot.path))
                .map(|suggestion| suggestion.size.unshared)
                .sum()
        };
        let mut ranked: Vec<(Option<u64>, AnyElement)> = Vec::new();
        for group in &about.stuff {
            let size = here(group);
            let freed = group.spots.iter().map(freeable).sum::<u64>();
            ranked.push((
                size,
                self.group_row(
                    group,
                    size,
                    (share(size, most), share(Some(freed), most)),
                    measuring,
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
                        measuring,
                        now,
                        cx,
                    );
                    ranked.push((size, div().pl(px(16.)).child(copy).into_any_element()));
                }
            }
        }
        // Stable, so a project's copies stay under it.
        ranked.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
        let title = startup.map_or("Largest".to_string(), |name| format!("Largest on {name}"));
        let drives = about.locations.iter().filter_map(|location| {
            let scan = location
                .scan
                .as_ref()
                .filter(|scan| !scan.rows.is_empty())?;
            let most = scan.rows.iter().map(|row| row.size.on_disk).max();
            Some(
                div()
                    .flex()
                    .flex_col()
                    .child(section_label(format!("Largest on {}", location.name)))
                    .children(scan.rows.iter().map(|row| {
                        let share = share(Some(row.size.on_disk), most.unwrap_or(0));
                        self.scan_row(row, location, share, now, cx)
                    })),
            )
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(section_label(title))
                    .children(ranked.into_iter().map(|(_, row)| row)),
            )
            .children(drives.collect::<Vec<_>>())
    }

    /// A group's row. With copies in several places it lists them on a press;
    /// otherwise it opens its folder.
    fn group_row(
        &self,
        group: &Group,
        here: Option<u64>,
        (share, freeable): (f32, f32),
        measuring: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let [spot] = group.spots.as_slice() {
            return self.spot_row(
                group.name.clone(),
                spot,
                group,
                (share, freeable),
                measuring,
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

    /// Whether a row of the startup disk's list has a button: a group's
    /// folders to list, or a cloud folder's download to remove.
    fn startup_buttons(&self) -> bool {
        self.overview.about.as_ref().is_some_and(|about| {
            about.stuff.iter().any(|group| {
                group.spots.len() > 1
                    || group.kind == Kind::Cloud && group.spots.iter().any(|spot| spot.reachable)
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn spot_row(
        &self,
        name: String,
        spot: &Spot,
        group: &Group,
        (share, freeable): (f32, f32),
        measuring: bool,
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
            None if self.startup_buttons() => End::Room,
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

    /// Another location as a tile: its name, how it stands in a line, and its
    /// button. The sidebar lists them too; this says what each holds.
    fn location_chip(
        &self,
        location: &Location,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let scanning = location.mount.as_ref().is_some_and(|mount| {
            self.overview
                .scanning
                .as_ref()
                .is_some_and(|scanning| &scanning.mount == mount)
        });
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
        let glyph = match location.kind {
            LocationKind::Share { .. } | LocationKind::Sftp { .. } => "server",
            LocationKind::Drive | LocationKind::Disconnected => "hard-drive",
        };
        let mark =
            icon(glyph, 16.).text_color(content(if location.connected { 0.55 } else { 0.3 }));
        div()
            .min_w_0()
            .h(px(CHIP_HEIGHT))
            .px_3()
            .rounded(px(theme::RADIUS_ROW))
            .border_1()
            .border_color(content(0.08))
            .bg(content(0.03))
            .flex()
            .items_center()
            .gap_2()
            .when(!location.connected, |d| d.text_color(content(0.65)))
            .child(marked(
                mark,
                location.name.clone(),
                Detail::Line(location_state(location, scanning, now)),
            ))
            .children(action.map(|action| action.flex_shrink_0()))
    }
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

/// The startup disk as one pill of touching parts: each part in its color,
/// then the free space as track. Only the pill's two ends are round, since
/// a parent can't clip its children to its corners.
fn disk_bar(parts: &[(Part, u64)], disk: &Headline) -> impl IntoElement + use<> {
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
            segment(*bytes, part_color(*part)).when(ix == 0, |d| d.rounded_l_full())
        }))
        .child(
            segment(disk.free, content(0.14))
                .rounded_r_full()
                .when(parts.is_empty(), |d| d.rounded_l_full()),
        )
}

fn part_color(part: Part) -> Hsla {
    match part {
        Part::Kind(kind) => tint(kind),
        Part::Freeable => theme::tint_green(),
        Part::Other => content(0.3),
    }
}

/// A part's dot in the bar's key.
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
