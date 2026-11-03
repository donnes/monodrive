//! The Overview page, which takes the pane's list while it's on screen: how
//! much is free and what could free more, the user's things across every
//! location, and what's happening now. Everything it says comes from
//! `monodrive_core::overview`, read off the UI thread; opening it draws what's
//! saved at once and measures what's due behind it, and leaving stops that.

use super::pane::Pane;
use super::sidebar::Places;
use super::{ConnectToServer, EmptyTrash, Operations, lock, with_owner};
use crate::jobs::JobLine;
use crate::theme::{
    self, ButtonLook, accent, content, dither_bar, icon, row_icon_button, section_label,
    text_button,
};
use chrono::{DateTime, Local};
use gpui::{
    AnyElement, Context, FontWeight, Hsla, MouseButton, Role, Task, Window, div, prelude::*, px,
};
use monodrive_core::cloud::{self, CloudFolder};
use monodrive_core::ops::{self, AgentChange, Origin, Request};
use monodrive_core::overview::{
    self, Env, Group, GroupKind, Headline, Location, LocationKind, Overview, ScanRow, Spot,
    Suggestion,
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
/// Narrower than this, "Right now" goes under "Your stuff".
const SIDE_BY_SIDE: f32 = 720.;
const RIGHT_NOW_WIDTH: f32 = 300.;
const STUFF_ROW_HEIGHT: f32 = 34.;
/// A row's title line, which a "Right now" mark centers on.
const TITLE_LINE: f32 = 16.;

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
                            Some(outcome.op),
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
                            Some(outcome.op),
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
        with_owner(
            window,
            cx,
            move |client| {
                let asked = cloud::remove_download(&path, &Origin::Window, client)?;
                overview::forget(&env, &path);
                Ok(asked)
            },
            move |this, removed, _, cx| {
                match removed {
                    Ok(_) => {
                        this.toast(format!("Removing the download of {name}"), false, None, cx)
                    }
                    Err(why) => this.toast(why, true, None, cx),
                }
                this.reread_overview(cx);
            },
        );
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
        let wide = self.chrome.width >= SIDE_BY_SIDE;

        let reclaimable: u64 = about.suggestions.iter().map(|s| s.size.unshared).sum();
        let partial = about.suggestions.iter().any(|s| !s.size.complete);
        let headline = about
            .headline
            .as_ref()
            .map(|disk| self.headline(disk, about.suggestions.len(), reclaimable, partial, cx));
        let cards = (!about.suggestions.is_empty()).then(|| {
            div().flex().when(!wide, |d| d.flex_col()).gap_3().children(
                about
                    .suggestions
                    .iter()
                    .map(|suggestion| self.suggestion_card(suggestion, now, cx)),
            )
        });
        let stuff = self.stuff(&about, measuring, now, cx);
        let right_now = self.right_now(&about, now, cx);
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
                        .px(px(theme::PANE_INSET))
                        .py_4()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .children(headline)
                        .children(cards)
                        .child(if wide {
                            div()
                                .flex()
                                .items_start()
                                .gap(px(24.))
                                .child(div().flex_1().min_w_0().child(stuff))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .w(px(RIGHT_NOW_WIDTH))
                                        .child(right_now),
                                )
                                .into_any_element()
                        } else {
                            div()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(stuff)
                                .child(right_now)
                                .into_any_element()
                        }),
                )
                .child(footer(&about, measuring, now))
                .into_any_element(),
        )
    }

    /// How much is free, in the page's one large line, what the suggestions
    /// would make of it, and the bar: solid for what's used, dithered in the
    /// accent for what the suggestions and the Trash could give back.
    fn headline(
        &self,
        disk: &Headline,
        suggested: usize,
        reclaimable: u64,
        partial: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let trash = disk.trash.filter(|trash| trash.unshared > 0);
        let in_trash = trash.map_or(0, |trash| trash.unshared);
        let share = |bytes: u64| bytes as f32 / disk.total.max(1) as f32;
        let used = disk.total.saturating_sub(disk.free);
        let conditional = (reclaimable + in_trash).min(used);
        let reach = amount(disk.free + reclaimable + in_trash);
        // A Trash that couldn't be read whole may hold more than it showed, even
        // when it showed nothing.
        let at_least = if partial || disk.trash.is_some_and(|trash| !trash.complete) {
            "at least "
        } else {
            ""
        };
        // Moving to the Trash frees nothing yet, so this says when it does.
        let promise = match (suggested, trash.is_some()) {
            (0, false) => "on this Mac.".to_string(),
            (0, true) => format!("on this Mac. Empty Trash gets you to {at_least}{reach}."),
            (1, _) => format!("on this Mac. This gets you to {at_least}{reach} after Empty Trash."),
            (n, _) => {
                format!("on this Mac. These {n} get you to {at_least}{reach} after Empty Trash.")
            }
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .debug_selector(|| "overview-headline".into())
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(theme::TEXT_DISPLAY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{} free", amount(disk.free))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TEXT_BODY))
                            .text_color(content(0.65))
                            .child(promise),
                    )
                    .when(trash.is_some(), |d| {
                        d.child(
                            text_button(
                                "overview-empty-trash",
                                "Empty Trash…",
                                ButtonLook::Caution,
                            )
                            .flex_shrink_0()
                            .on_click(cx.listener(
                                |_, _, window, cx| {
                                    window.dispatch_action(Box::new(EmptyTrash), cx);
                                },
                            )),
                        )
                    }),
            )
            .child(dither_bar(share(used - conditional), share(conditional)))
    }

    fn suggestion_card(
        &self,
        suggestion: &Suggestion,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dir = suggestion.path.clone();
        let path = &suggestion.path;
        let evidence = &suggestion.evidence;
        let mut size = amount(suggestion.size.unshared);
        if !suggestion.size.complete {
            size.push('+');
        }
        let mut why = evidence.why();
        if let Some(age) = suggestion.size.age(now) {
            why += &format!(" Measured {age}.");
        }
        div()
            .id(format!("overview-suggestion-{}", path.display()))
            .role(Role::Group)
            .aria_label(evidence.title())
            .flex_1()
            .min_w_0()
            .p_4()
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(content(0.12))
            .bg(content(0.03))
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_size(px(theme::TEXT_TITLE)).child(size))
            .child(
                div()
                    .truncate()
                    .text_size(px(theme::TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child(evidence.title()),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(why),
            )
            .child(
                div().pt_2().flex().child(
                    text_button(
                        format!("overview-clean-{}", path.display()),
                        evidence.action(),
                        ButtonLook::Primary,
                    )
                    .debug_selector(|| "overview-clean".into())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.clean_suggestion(dir.clone(), window, cx)
                    })),
                ),
            )
            .into_any_element()
    }

    /// Everything in one ranking, whichever location holds it: the startup
    /// disk's rows and each scanned drive's.
    fn stuff(
        &self,
        about: &Overview,
        measuring: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let mut ranked: Vec<(Option<u64>, AnyElement)> = Vec::new();
        for group in &about.stuff {
            let size = group.on_disk();
            ranked.push((size, self.group_row(group, measuring, now, cx)));
            if self.overview.open.contains(&group.name) {
                for spot in &group.spots {
                    let copy =
                        self.spot_row(spot.location.clone(), spot, group, measuring, now, cx);
                    ranked.push((size, div().pl(px(16.)).child(copy).into_any_element()));
                }
            }
        }
        for location in &about.locations {
            let Some(scan) = &location.scan else {
                continue;
            };
            for row in &scan.rows {
                ranked.push((
                    Some(row.size.on_disk),
                    self.scan_row(row, location, now, cx),
                ));
            }
        }
        // Stable, so a project's copies stay under it.
        ranked.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
        let rest = about.everything_else.filter(|rest| *rest > 0).map(|rest| {
            stuff_row(
                ("hard-drive", content(0.55)),
                "Everything else".to_string(),
                "What no row above explains.".to_string(),
                amount(rest),
            )
        });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .child(section_label("Your stuff, everywhere").px_0())
                    .child(section_label("Largest first").px_0()),
            )
            .children(ranked.into_iter().map(|(_, row)| row))
            .children(rest)
    }

    /// A group's row. With copies in several places it lists them on a press;
    /// otherwise it opens its folder.
    fn group_row(
        &self,
        group: &Group,
        measuring: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let [spot] = group.spots.as_slice() {
            return self.spot_row(group.name.clone(), spot, group, measuring, now, cx);
        }
        let name = group.name.clone();
        let places = group
            .spots
            .iter()
            .map(|spot| match spot.size {
                Some(size) => format!(
                    "{} on {}",
                    size_label(Some(size.on_disk), !size.complete, false),
                    spot.location
                ),
                None => format!("Not measured on {}", spot.location),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let selector = format!("overview-spot-{name}");
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
                    look(group),
                    group.name.clone(),
                    places,
                    size_label(
                        group.on_disk(),
                        group
                            .spots
                            .iter()
                            .any(|spot| spot.size.is_none_or(|size| !size.complete)),
                        measuring,
                    ),
                )
                .group_hover(theme::ROW_GROUP, |s| s.bg(content(0.08))),
            )
            .into_any_element()
    }

    fn spot_row(
        &self,
        name: String,
        spot: &Spot,
        group: &Group,
        measuring: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let where_ = paths::abbreviate(&spot.path);
        let age = spot.size.and_then(|size| size.age(now));
        // A copy under its project is already named for its location.
        let place = (name != spot.location).then(|| format!("on {}", spot.location));
        let said = [Some(where_), place, age]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let partial = spot.size.is_some_and(|size| !size.complete);
        let path = spot.path.clone();
        let label = name.clone();
        let id = format!("overview-spot-{}", path.display());
        let row = stuff_row(
            look(group),
            name,
            said,
            size_label(spot.size.map(|size| size.on_disk), partial, measuring),
        );
        let row = row.when(group.kind == GroupKind::Cloud && spot.reachable, |d| {
            let path = path.clone();
            d.child(
                row_icon_button(
                    format!("overview-remove-download-{}", path.display()),
                    "Remove Download",
                    "cloud",
                    content(0.55),
                )
                .occlude()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.remove_downloads(path.clone(), window, cx)
                })),
            )
        });
        if !spot.reachable {
            return row.into_any_element();
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

    /// A folder at the top of a drive's scan: what it took then, on which
    /// drive, and how long ago. A drive that's gone only says so.
    fn scan_row(
        &self,
        row: &ScanRow,
        location: &Location,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = row.path.clone();
        let mut said = if location.mount.is_some() {
            format!("On {}", location.name)
        } else {
            format!("{}, not connected", location.name)
        };
        if let Some(age) = row.size.age(now) {
            said += &format!(". Scanned {age}.");
        }
        let shown = stuff_row(
            ("folder-fill", content(0.55)),
            row.name.clone(),
            said,
            size_label(Some(row.size.on_disk), !row.size.complete, false),
        );
        let shown = shown.when(location.takes_trash, |d| {
            let path = path.clone();
            d.child(
                row_icon_button(
                    format!("overview-trash-{}", path.display()),
                    "Move to Trash",
                    "trash",
                    content(0.55),
                )
                .occlude()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.trash_scan_row(path.clone(), window, cx)
                })),
            )
        });
        if location.mount.is_none() {
            return shown.into_any_element();
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

    fn right_now(
        &self,
        about: &Overview,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let quiet = self.chrome.jobs.is_empty() && self.overview.changes.is_empty();
        div()
            .flex()
            .flex_col()
            .child(section_label("Right now").px_0())
            .when(quiet, |d| {
                d.child(now_line(
                    dot(content(0.2)),
                    "Nothing running".to_string(),
                    "No copies, moves or agent changes.".to_string(),
                ))
            })
            .children(self.chrome.jobs.iter().map(job_line))
            .children(
                self.overview
                    .changes
                    .iter()
                    .map(|change| change_line(change, now)),
            )
            .children(
                about
                    .locations
                    .iter()
                    .map(|location| self.location_line(location, now, cx)),
            )
    }

    /// A location: whether it's connected, what it can say, and its button.
    fn location_line(
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
                text_button("overview-cancel-scan", "Cancel Scan", ButtonLook::Secondary)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_scan(cx))),
            ),
            (_, Some(mount)) => {
                let mount = mount.clone();
                let label = if location.scan.is_some() {
                    "Scan Again"
                } else {
                    "Scan"
                };
                Some(
                    text_button(
                        format!("overview-scan-{}", mount.display()),
                        label,
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
        let mark = dot(if location.connected {
            theme::success()
        } else {
            content(0.2)
        });
        now_line(
            mark,
            location.name.clone(),
            location_state(location, scanning, now),
        )
        .children(action.map(|action| action.flex_shrink_0()))
    }
}

/// The glyph and tint of a group's row: a project or a kind of thing.
fn look(group: &Group) -> (&'static str, Hsla) {
    match (group.kind, group.name.as_str()) {
        (GroupKind::Cloud, _) => ("cloud", accent()),
        (GroupKind::Folder, "Pictures") => ("folder-fill", theme::tint_orange()),
        (GroupKind::Folder, "Movies") => ("folder-fill", theme::tint_purple()),
        (GroupKind::Folder, "Music") => ("folder-fill", theme::tint_pink()),
        _ => ("folder-fill", accent()),
    }
}

/// A row of "Your stuff": what it is, where it lives, and what it takes up.
fn stuff_row(look: (&'static str, Hsla), title: String, detail: String, size: String) -> gpui::Div {
    row_frame(STUFF_ROW_HEIGHT)
        .child(icon(look.0, 16.).flex_shrink_0().text_color(look.1))
        .child(two_lines(title, detail))
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(theme::TEXT_BODY))
                .child(size),
        )
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

/// What a location can say about itself, in a line.
fn location_state(location: &Location, scanning: bool, now: u64) -> String {
    let mut said = Vec::new();
    if let Some((total, free)) = location.capacity {
        said.push(format!("{} free of {}", amount(free), amount(total)));
    }
    if !location.connected {
        said.push("Not connected".to_string());
    }
    if scanning {
        said.push("Scanning…".to_string());
    } else {
        match &location.scan {
            Some(scan) => said.push(match scan.total.age(now) {
                Some(age) => format!("scanned {age}"),
                None => "scanned just now".to_string(),
            }),
            None if location.mount.is_some() => said.push("Not scanned".to_string()),
            None => {}
        }
    }
    said.join(" · ")
}

fn dot(ink: Hsla) -> gpui::Div {
    div().size(px(8.)).rounded_full().flex_shrink_0().bg(ink)
}

/// A line of "Right now": a mark, what it is, and how it stands, over a
/// hairline. Marks sit in one column so the text lines up.
fn now_line(mark: impl IntoElement, title: String, detail: String) -> gpui::Div {
    div()
        .py_2()
        .border_t_1()
        .border_color(content(0.06))
        .flex()
        .items_start()
        .gap_2()
        .child(
            div()
                .w(px(16.))
                .h(px(TITLE_LINE))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(mark),
        )
        .child(two_lines(title, detail))
}

fn job_line(job: &JobLine) -> impl IntoElement + use<> {
    let by = if job.agent { ", by an agent" } else { "" };
    now_line(
        theme::ring(job.done, 14.),
        job.title.clone(),
        format!("{:.0}%, {}{by}", job.done * 100., job.state),
    )
}

fn change_line(change: &AgentChange, now: u64) -> impl IntoElement + use<> {
    let items = match change.items {
        1 => "1 item".to_string(),
        n => format!("{n} items"),
    };
    let when = overview::age(change.at, now).unwrap_or_else(|| "just now".to_string());
    now_line(
        dot(accent()),
        change
            .client
            .clone()
            .unwrap_or_else(|| "An agent".to_string()),
        format!("{}, {items}, {when}", change.title),
    )
}

/// The bar under the page: how old what it's showing is, on each side.
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
    let scanned = about
        .locations
        .iter()
        .filter_map(|location| Some((location, location.scan.as_ref()?.total.at)))
        .max_by_key(|(_, at)| *at)
        .map(|(location, at)| {
            let day = DateTime::from_timestamp(at as i64, 0)
                .map(|at| at.with_timezone(&Local).format("%b %-d").to_string())
                .unwrap_or_default();
            format!("{} scanned {day}", location.name)
        });
    theme::pane_bar(theme::PATH_BAR_HEIGHT)
        .flex_shrink_0()
        .justify_between()
        .border_t_1()
        .border_color(content(0.06))
        .text_size(px(theme::TEXT_SMALL))
        .text_color(content(0.65))
        .child(div().children(startup))
        .child(div().children(scanned))
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

fn row_frame(height: f32) -> gpui::Div {
    div()
        .h(px(height))
        .flex_shrink_0()
        .px_2()
        .rounded(px(theme::RADIUS_ROW))
        .flex()
        .items_center()
        .gap_2()
}

fn two_lines(title: String, detail: String) -> impl IntoElement {
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
            div()
                .truncate()
                .line_height(px(TITLE_LINE))
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child(detail),
        )
}
