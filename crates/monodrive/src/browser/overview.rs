//! The Overview page, which takes the pane's list while it's on screen: how
//! much is free and what fills the startup disk, what could free more, the
//! user's largest things across every location, and the other locations. Everything it says comes from
//! `monodrive_core::overview`, read off the UI thread; opening it draws what's
//! saved at once and measures what's due behind it, and leaving stops that.

use super::pane::Pane;
use super::sidebar::Places;
use super::{ConnectToServer, EmptyTrash, Offer, Operations, lock, with_owner};
use crate::theme::{self, ButtonLook, content, icon, row_icon_button, section_label, text_button};
use gpui::{
    AnyElement, Context, FontWeight, Hsla, MouseButton, Role, Task, Window, div, prelude::*, px,
    relative,
};
use monodrive_core::cloud::{self, CloudFolder};
use monodrive_core::ops::{self, AgentChange, Origin, Request};
use monodrive_core::overview::{
    self, Env, Group, GroupKind, Headline, Location, LocationKind, Measured, Overview, ScanRow,
    Spot, Suggestion,
};
use monodrive_core::paths;
use monodrive_core::storage::{self, Stop};
use monodrive_core::volumes::Volume;
use monodrive_core::worktrees::Git;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

/// Folders measured at once, whichever way the page was opened.
const MEASURING: usize = 2;
/// How far back the page says what agents changed, and how many.
const CHANGES_SINCE: u64 = 24 * 60 * 60;
const CHANGES_SHOWN: usize = 5;
/// Wider than this, the page stops growing, so sizes stay near their names.
const MAX_WIDTH: f32 = 880.;
/// A row holds a title over a detail, 16 pt each.
const ROW_HEIGHT: f32 = 34.;
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
    /// Tells the measuring threads to stop.
    stop: Option<Arc<storage::Progress>>,
    reading: Option<Task<()>>,
    /// The mount being scanned, because someone asked.
    scanning: Option<Scanning>,
    /// The groups in several places, listing each copy.
    open: HashSet<String>,
}

struct Scanning {
    mount: PathBuf,
    progress: Arc<storage::Progress>,
    _task: Task<()>,
}

/// What a read needs, each kept from the first read of a page: asking macOS
/// for volumes can wait on a slow share.
#[derive(Clone)]
struct Reader {
    env: Env,
    volumes: Arc<Vec<Volume>>,
    cloud: Arc<Vec<CloudFolder>>,
    ops: Option<Arc<Mutex<ops::Client>>>,
}

impl Reader {
    fn read(&self) -> (Overview, Vec<AgentChange>) {
        let now = overview::now();
        let about = overview::read(&self.env, &self.volumes, &self.cloud, &|_| false, now);
        let changes = self.ops.as_ref().map_or_else(Vec::new, |ops| {
            lock(ops).agent_changes(now.saturating_sub(CHANGES_SINCE), CHANGES_SHOWN)
        });
        (about, changes)
    }
}

impl Pane {
    /// Reads what's saved and draws it, then measures what's due, a row at a
    /// time. Connects to nothing: a scan or a server is someone's choice.
    pub(super) fn read_overview(&mut self, cx: &mut Context<Self>) {
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        if self.overview.reading.is_some() {
            return;
        }
        let env = Env::new(places.trash.clone(), places.store.clone());
        let ops = cx.try_global::<Operations>().map(|ops| ops.0.clone());
        let index = self.index.clone();
        let drives = places.drives.clone();
        let stop = Arc::new(storage::Progress::default());
        self.overview.stop = Some(stop.clone());
        self.overview.reading = Some(cx.spawn(async move |this, cx| {
            let background = cx.background_executor().clone();
            let (reader, first) = background
                .spawn(async move {
                    let reader = Reader {
                        volumes: Arc::new(places.drives.list()),
                        cloud: Arc::new(places.drives.cloud()),
                        env,
                        ops,
                    };
                    let first = reader.read();
                    (reader, first)
                })
                .await;
            let take = |this: &mut Pane, (about, changes), cx: &mut Context<Pane>| {
                this.overview.about = Some(about);
                this.overview.changes = changes;
                cx.notify();
            };
            let _ = this.update(cx, |this, cx| take(this, first, cx));
            let found = background.spawn({
                let reader = reader.clone();
                async move {
                    let git = Git::find().ok();
                    let _ = overview::discover(&reader.env, git.as_ref(), |name, limit| {
                        drives.find_files(&index, name, limit)
                    });
                    reader.read()
                }
            });
            let (about, changes) = found.await;
            let due = about.due.clone();
            let _ = this.update(cx, |this, cx| take(this, (about, changes), cx));

            let queue = Arc::new(Mutex::new(due));
            let (measured, heard) = async_channel::unbounded::<()>();
            let workers: Vec<Task<()>> = (0..MEASURING)
                .map(|_| {
                    let (queue, measured, stop, env) = (
                        queue.clone(),
                        measured.clone(),
                        stop.clone(),
                        reader.env.clone(),
                    );
                    background.spawn(async move {
                        loop {
                            let next = queue.lock().unwrap_or_else(|e| e.into_inner()).pop();
                            let Some(folder) = next else { return };
                            if stop.cancel.load(Ordering::Relaxed) {
                                return;
                            }
                            let now = overview::now();
                            if overview::measure(&env, &folder, &stop, None, now).is_some() {
                                let _ = measured.send(()).await;
                            }
                        }
                    })
                })
                .collect();
            drop(measured);
            while heard.recv().await.is_ok() {
                let reader = reader.clone();
                let read = background.spawn(async move { reader.read() }).await;
                let _ = this.update(cx, |this, cx| take(this, read, cx));
            }
            drop(workers);
            let _ = this.update(cx, |this, cx| {
                this.overview.reading = None;
                cx.notify();
            });
        }));
    }

    fn stop_measuring(&mut self) {
        if let Some(stop) = self.overview.stop.take() {
            stop.cancel.store(true, Ordering::Relaxed);
        }
        self.overview.reading = None;
    }

    /// The page is off screen: what it was measuring or scanning stops.
    pub(super) fn leave_overview(&mut self) {
        self.stop_measuring();
        if let Some(scanning) = self.overview.scanning.take() {
            scanning.progress.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn overview_env(&self, cx: &Context<Self>) -> Option<Env> {
        let places = cx.try_global::<Places>()?;
        Some(Env::new(places.trash.clone(), places.store.clone()))
    }

    /// Reads again after something changed what's saved.
    fn reread_overview(&mut self, cx: &mut Context<Self>) {
        self.stop_measuring();
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
        let env = Env::new(places.trash.clone(), places.store.clone());
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
        let measuring = self.overview.reading.is_some();
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
        let inside = (self.chrome.width - 2. * theme::PANE_INSET).min(MAX_WIDTH - 2. * ROW_INSET);
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
                                .w_full()
                                .max_w(px(MAX_WIDTH))
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
                .child(footer(&about, measuring, now))
                .into_any_element(),
        )
    }

    /// How much is free, in the page's one large figure, and the disk drawn
    /// by what fills it, with a key.
    fn headline(&self, about: &Overview, disk: &Headline) -> impl IntoElement + use<> {
        let parts = parts(about, disk);
        let key = parts.iter().map(|(part, bytes)| {
            let name = match part {
                Part::Kind(kind) => kind.name(),
                Part::Freeable => "Can be freed",
                Part::Other => "Other",
            };
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(swatch(*part))
                .child(div().text_color(content(0.65)).child(name))
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
                for spot in &group.spots {
                    let copy = self.spot_row(
                        spot.location.clone(),
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
        let places = on_startup
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
            .join(" · ");
        let selector = format!("overview-spot-{name}");
        let tint = Kind::of(group).tint();
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
                    (Kind::of(group).glyph(), tint),
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
                    Some(chevron.into_any_element()),
                )
                .group_hover(theme::ROW_GROUP, |s| s.bg(content(0.08))),
            )
            .into_any_element()
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
        let action = (group.kind == GroupKind::Cloud && spot.reachable).then(|| {
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
        let kind = Kind::of(group);
        let row = stuff_row(
            (kind.glyph(), kind.tint()),
            name,
            said,
            Bar {
                share,
                freeable,
                ink: kind.tint(),
            },
            size_label(spot.size.map(|size| size.on_disk), partial, measuring),
            action,
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
            action,
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

/// What fills the startup disk, by the kind of thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Documents,
    Projects,
    Pictures,
    Media,
    Cloud,
}

impl Kind {
    fn of(group: &Group) -> Self {
        match (group.kind, group.name.as_str()) {
            (GroupKind::Project, _) => Kind::Projects,
            (GroupKind::Cloud, _) => Kind::Cloud,
            (GroupKind::Folder, "Pictures") => Kind::Pictures,
            (GroupKind::Folder, "Movies" | "Music") => Kind::Media,
            (GroupKind::Folder, _) => Kind::Documents,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Documents => "Documents, Desktop and Downloads",
            Kind::Projects => "Projects",
            Kind::Pictures => "Pictures",
            Kind::Media => "Movies and music",
            Kind::Cloud => "Cloud",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Kind::Cloud => "cloud",
            _ => "folder-01",
        }
    }

    /// Fixed, not the accent, so no accent can make two kinds look alike.
    fn tint(self) -> Hsla {
        match self {
            Kind::Documents => theme::tint_blue(),
            Kind::Projects => theme::tint_pink(),
            Kind::Pictures => theme::tint_orange(),
            Kind::Media => theme::tint_purple(),
            Kind::Cloud => theme::tint_cyan(),
        }
    }
}

/// A part of the startup disk's bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Kind(Kind),
    /// What the suggestions and the Trash would give back.
    Freeable,
    /// Used, and in none of the above or not measured yet.
    Other,
}

/// What the startup disk's used space is made of, largest kind first, then
/// what can be freed, then the rest. Nothing is counted twice: a folder inside
/// another counts in the outer one, and what a suggestion frees leaves the
/// kind that holds it.
fn parts(about: &Overview, disk: &Headline) -> Vec<(Part, u64)> {
    let spots: Vec<(&Path, Kind, u64)> = about
        .stuff
        .iter()
        .flat_map(|group| group.spots.iter().map(move |spot| (group, spot)))
        .filter(|(_, spot)| spot.location == disk.name)
        .filter_map(|(group, spot)| {
            Some((spot.path.as_path(), Kind::of(group), spot.size?.on_disk))
        })
        .collect();
    let outer: Vec<_> = spots
        .iter()
        .filter(|(path, ..)| {
            !spots
                .iter()
                .any(|(other, ..)| other != path && path.starts_with(other))
        })
        .collect();
    let mut kinds: Vec<(Kind, u64)> = Vec::new();
    for (_, kind, bytes) in &outer {
        match kinds.iter_mut().find(|(k, _)| k == kind) {
            Some((_, total)) => *total += bytes,
            None => kinds.push((*kind, *bytes)),
        }
    }
    for suggestion in &about.suggestions {
        let holder = outer
            .iter()
            .find(|(path, ..)| suggestion.path.starts_with(path));
        if let Some((_, kind, _)) = holder
            && let Some((_, total)) = kinds.iter_mut().find(|(k, _)| k == kind)
        {
            *total = total.saturating_sub(suggestion.size.unshared);
        }
    }
    kinds.retain(|(_, bytes)| *bytes > 0);
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let freeable = about
        .suggestions
        .iter()
        .map(|suggestion| suggestion.size.unshared)
        .sum::<u64>()
        + disk.trash.map_or(0, |trash| trash.unshared);
    let used = disk.total.saturating_sub(disk.free);
    let known: u64 = kinds.iter().map(|(_, bytes)| bytes).sum();
    let other = used.saturating_sub(known + freeable);
    kinds
        .into_iter()
        .map(|(kind, bytes)| (Part::Kind(kind), bytes))
        .chain((freeable > 0).then_some((Part::Freeable, freeable)))
        .chain((other > 0).then_some((Part::Other, other)))
        .collect()
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
        Part::Kind(kind) => kind.tint(),
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

/// A row of the list: what it is, where it lives, a bar of its drive,
/// what it takes up, and its button. Every row keeps the button's room, so
/// the bars and sizes line up.
fn stuff_row(
    look: (&'static str, Hsla),
    title: String,
    detail: String,
    bar: Bar,
    size: String,
    action: Option<AnyElement>,
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
        .child(
            div()
                .w(px(ACTION_SLOT))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .children(action),
        )
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

/// A mark on its title's line, with the detail under the title.
fn marked(mark: impl IntoElement, title: String, detail: Detail) -> gpui::Div {
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .h(px(TITLE_LINE))
                .flex_shrink_0()
                .flex()
                .items_center()
                .child(mark),
        )
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
fn footer(about: &Overview, measuring: bool, now: u64) -> impl IntoElement + use<> {
    let startup = about.headline.as_ref().map(|disk| {
        let oldest = about
            .stuff
            .iter()
            .flat_map(|group| &group.spots)
            .filter(|spot| spot.location == disk.name)
            .filter_map(|spot| spot.size.map(|size| size.at))
            .min();
        match oldest {
            _ if measuring => format!("Measuring {}…", disk.name),
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

fn quoted(path: &Path) -> String {
    format!(
        "\"{}\"",
        path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned()
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use monodrive_core::overview::{Evidence, Measured};

    fn measured(bytes: u64) -> Measured {
        Measured {
            on_disk: bytes,
            unshared: bytes,
            at: 0,
            complete: true,
        }
    }

    fn group(name: &str, kind: GroupKind, path: &str, bytes: u64) -> Group {
        Group {
            name: name.into(),
            kind,
            spots: vec![Spot {
                location: "Macintosh HD".into(),
                path: path.into(),
                size: Some(measured(bytes)),
                reachable: true,
            }],
        }
    }

    #[test]
    fn the_disk_bar_counts_nothing_twice() {
        let disk = Headline {
            name: "Macintosh HD".into(),
            total: 100,
            free: 20,
            trash: Some(measured(4)),
            low: true,
        };
        let about = Overview {
            headline: Some(disk.clone()),
            suggestions: vec![Suggestion {
                path: "/h/app/target".into(),
                evidence: Evidence::BuildOutput {
                    project: "app".into(),
                },
                size: measured(5),
            }],
            stuff: vec![
                group("Documents", GroupKind::Folder, "/h/Documents", 30),
                // Inside Documents, so already in its 30.
                group("notes", GroupKind::Project, "/h/Documents/notes", 10),
                group("app", GroupKind::Project, "/h/app", 15),
            ],
            ..Overview::default()
        };
        assert_eq!(
            parts(&about, &disk),
            vec![
                (Part::Kind(Kind::Documents), 30),
                (Part::Kind(Kind::Projects), 10),
                (Part::Freeable, 9),
                (Part::Other, 31),
            ]
        );
    }
}
