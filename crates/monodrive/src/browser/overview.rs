//! The Overview page, which takes the pane's list while it's on screen: how
//! much is free and what could free more, the user's things across every
//! location, and where each location stands. Everything it says comes from
//! `monodrive_core::overview`, read off the UI thread; opening it draws what's
//! saved at once and measures what's due behind it, and leaving stops that.

use super::pane::Pane;
use super::sidebar::Places;
use super::{ConnectToServer, EmptyTrash, Operations, lock, with_owner};
use crate::theme::{
    self, ButtonLook, accent, content, dither_bar, icon, row_icon_button, section_label,
    text_button,
};
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
/// From this pane width the suggestions sit side by side as cards, and
/// from the next the locations take a column beside the list.
const CARDS_ACROSS: f32 = 720.;
const SIDE_COLUMN: f32 = 820.;
const SIDE_WIDTH: f32 = 280.;
/// Wider than this, the page stops growing, so sizes stay near their names.
const MAX_WIDTH: f32 = 1040.;
const STUFF_ROW_HEIGHT: f32 = 34.;
const TITLE_LINE: f32 = 16.;
/// A row's own side padding, which puts its text on the pane's inset.
const ROW_INSET: f32 = 8.;
/// The trailing columns every row of the list keeps, filled or not.
const SIZE_SLOT: f32 = 72.;
const ACTION_SLOT: f32 = 20.;
/// A narrow pane's suggestion rows lead with what each frees.
const RECLAIMS_SLOT: f32 = 52.;

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
        let width = self.chrome.width;

        let reclaimable: u64 = about.suggestions.iter().map(|s| s.size.unshared).sum();
        let partial = about.suggestions.iter().any(|s| !s.size.complete);
        let headline = about
            .headline
            .as_ref()
            .map(|disk| self.headline(disk, about.suggestions.len(), reclaimable, partial, cx));
        // The cards' edges sit on the text's edge, with the headline and bar.
        let suggestions = (!about.suggestions.is_empty()).then(|| {
            if width >= CARDS_ACROSS {
                div()
                    .mx(px(ROW_INSET))
                    .flex()
                    .items_stretch()
                    .gap_3()
                    .children(
                        about
                            .suggestions
                            .iter()
                            .map(|suggestion| self.suggestion_card(suggestion, now, cx)),
                    )
                    .into_any_element()
            } else {
                card()
                    .mx(px(ROW_INSET))
                    .flex()
                    .flex_col()
                    .children(
                        about
                            .suggestions
                            .iter()
                            .enumerate()
                            .map(|(ix, suggestion)| {
                                self.suggestion_row(suggestion, now, cx)
                                    .when(ix > 0, |d| d.border_t_1().border_color(content(0.08)))
                            }),
                    )
                    .into_any_element()
            }
        });
        let stuff = self.stuff(&about, measuring, now, cx);
        let side = self.side(&about, now, cx);
        let body = match side {
            Some(side) if width >= SIDE_COLUMN => div()
                .flex()
                .items_start()
                .gap(px(24.))
                .child(div().flex_1().min_w_0().child(stuff))
                .child(div().flex_shrink_0().w(px(SIDE_WIDTH)).child(side))
                .into_any_element(),
            side => div()
                .flex()
                .flex_col()
                .gap_4()
                .child(stuff)
                .children(side)
                .into_any_element(),
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
                                .gap_4()
                                .children(headline)
                                .children(suggestions)
                                .child(body),
                        ),
                )
                .child(footer(&about, measuring, now))
                .into_any_element(),
        )
    }

    /// How much is free, in the page's one large figure, what the suggestions
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
        let name = &disk.name;
        // Moving to the Trash frees nothing yet, so this says when it does.
        let promise = match (suggested, trash.is_some()) {
            (0, false) => format!("On {name}."),
            (0, true) => format!("On {name}. Emptying the Trash gets you to {at_least}{reach}."),
            (1, _) => {
                format!("On {name}. This gets you to {at_least}{reach} after emptying the Trash.")
            }
            (n, _) => format!(
                "On {name}. These {n} get you to {at_least}{reach} after emptying the Trash."
            ),
        };
        div()
            .px(px(ROW_INSET))
            .flex()
            .flex_col()
            .gap_1()
            .debug_selector(|| "overview-headline".into())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TEXT_DISPLAY))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("{} free", amount(disk.free))),
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
            .child(
                div()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(content(0.65))
                    .child(promise),
            )
            .child(
                div()
                    .pt_2()
                    .child(dither_bar(share(used - conditional), share(conditional))),
            )
    }

    /// A suggestion as a card of its own, for when three fit side by side.
    /// The buttons sit at the cards' feet, so they line up.
    fn suggestion_card(
        &self,
        suggestion: &Suggestion,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let evidence = &suggestion.evidence;
        card()
            .id(format!("overview-suggestion-{}", suggestion.path.display()))
            .role(Role::Group)
            .aria_label(evidence.title())
            .flex_1()
            .min_w_0()
            .p_4()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(theme::TEXT_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(reclaims(suggestion)),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::MEDIUM)
                    .child(evidence.title()),
            )
            .child(
                div()
                    .flex_1()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(why(suggestion, now)),
            )
            .child(div().pt_2().flex().child(self.clean_button(suggestion, cx)))
            .into_any_element()
    }

    /// A suggestion as a row of the one card they share in a narrow pane:
    /// what it frees, what it is and why it's safe, then its button.
    fn suggestion_row(
        &self,
        suggestion: &Suggestion,
        now: u64,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let evidence = &suggestion.evidence;
        div()
            .id(format!("overview-suggestion-{}", suggestion.path.display()))
            .role(Role::Group)
            .aria_label(evidence.title())
            .px_3()
            .py_2()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(RECLAIMS_SLOT))
                    .flex_shrink_0()
                    .flex()
                    .justify_end()
                    .text_size(px(theme::TEXT_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(reclaims(suggestion)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(theme::TEXT_BODY))
                            .font_weight(FontWeight::MEDIUM)
                            .child(evidence.title()),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SMALL))
                            .text_color(content(0.65))
                            .child(why(suggestion, now)),
                    ),
            )
            .child(self.clean_button(suggestion, cx).flex_shrink_0())
    }

    fn clean_button(
        &self,
        suggestion: &Suggestion,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let dir = suggestion.path.clone();
        text_button(
            format!("overview-clean-{}", dir.display()),
            suggestion.evidence.action(),
            ButtonLook::Primary,
        )
        .debug_selector(|| "overview-clean".into())
        .on_click(
            cx.listener(move |this, _, window, cx| this.clean_suggestion(dir.clone(), window, cx)),
        )
    }

    /// Everything in one ranking, largest first, whichever location holds
    /// it: the startup disk's rows and each scanned drive's.
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
        // Only the startup disk is measured whole, so the rest is its rest,
        // apart from the ranking.
        let rest = about
            .everything_else
            .filter(|rest| *rest > 0)
            .zip(about.headline.as_ref())
            .map(|(rest, disk)| {
                div()
                    .flex()
                    .flex_col()
                    .child(hairline().my_1())
                    .child(still(stuff_row(
                        ("hard-drive", content(0.55)),
                        "Everything else".to_string(),
                        format!("On {}, outside the folders above.", disk.name),
                        amount(rest),
                        None,
                    )))
            });
        div()
            .flex()
            .flex_col()
            .child(section_label("Your stuff, everywhere"))
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
                    None,
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
        let row = stuff_row(
            look(group),
            name,
            said,
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
            said += &format!(" · scanned {age}");
        }
        // A drive that's gone can't be opened, so its rows go faint.
        let ink = if location.mount.is_some() {
            accent()
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

    /// Beside the list, or under it in a narrow pane: every location with
    /// how it stands and its button, then what agents changed lately. Copies
    /// and moves stay in the title bar's ring. `None` with nothing to show.
    fn side(&self, about: &Overview, now: u64, cx: &mut Context<Self>) -> Option<gpui::Div> {
        if about.locations.is_empty() && self.overview.changes.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_col()
                .when(!about.locations.is_empty(), |d| {
                    d.child(section_label("Locations")).children(
                        about
                            .locations
                            .iter()
                            .map(|location| self.location_row(location, now, cx)),
                    )
                })
                .when(!self.overview.changes.is_empty(), |d| {
                    d.child(section_label("Changed by agents")).children(
                        self.overview
                            .changes
                            .iter()
                            .map(|change| change_row(change, now)),
                    )
                }),
        )
    }

    /// A location: how it stands, a line for each part so none is lost to
    /// a wrap, and its button.
    fn location_row(
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
                text_button("overview-cancel-scan", "Cancel Scan", ButtonLook::Ghost)
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_scan(cx))),
            ),
            (_, Some(mount)) => {
                let mount = mount.clone();
                Some(
                    text_button(
                        format!("overview-scan-{}", mount.display()),
                        "Scan",
                        ButtonLook::Ghost,
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
                    ButtonLook::Ghost,
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
        let mark = icon(glyph, 16.).text_color(if location.connected {
            theme::success()
        } else {
            content(0.3)
        });
        // Top-aligned with the list's rows, so names line up across the
        // columns. The mark and the button center on the first two lines, as
        // a list row's do.
        let beside = || {
            div()
                .h(px(2. * TITLE_LINE))
                .flex_shrink_0()
                .flex()
                .items_center()
        };
        still(
            div()
                .min_h(px(STUFF_ROW_HEIGHT))
                .px(px(ROW_INSET))
                .py(px((STUFF_ROW_HEIGHT - 2. * TITLE_LINE) / 2.))
                .flex()
                .items_start()
                .gap_2()
                .child(beside().child(mark))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(title_line(location.name.clone()))
                        .children(location_state(location, scanning, now).into_iter().map(
                            |line| {
                                div()
                                    .truncate()
                                    .line_height(px(TITLE_LINE))
                                    .text_size(px(theme::TEXT_SMALL))
                                    .text_color(content(0.65))
                                    .child(line)
                            },
                        )),
                )
                .children(action.map(|action| beside().child(action))),
        )
    }
}

/// A rule between rows, from the text's edge to the size's.
fn hairline() -> gpui::Div {
    div().mx(px(ROW_INSET)).h(px(1.)).bg(content(0.08))
}

/// The frame both kinds of suggestion share.
fn card() -> gpui::Div {
    div()
        .rounded(px(theme::RADIUS_CARD))
        .border_1()
        .border_color(content(0.12))
        .bg(content(0.03))
}

/// What a suggestion frees, with a + when part of it couldn't be read.
fn reclaims(suggestion: &Suggestion) -> String {
    let mut size = amount(suggestion.size.unshared);
    if !suggestion.size.complete {
        size.push('+');
    }
    size
}

/// Why a suggestion is safe, and how old its size is when that matters.
fn why(suggestion: &Suggestion, now: u64) -> String {
    let mut why = suggestion.evidence.why();
    if let Some(age) = suggestion.size.age(now) {
        why += &format!(" Measured {age}.");
    }
    why
}

/// A group's glyph and its tint: the accent, or a kind of folder's own color,
/// as Finder tells Pictures and Movies apart.
fn look(group: &Group) -> (&'static str, Hsla) {
    match (group.kind, group.name.as_str()) {
        (GroupKind::Cloud, _) => ("cloud", accent()),
        (GroupKind::Folder, "Pictures") => ("folder-01", theme::tint_orange()),
        (GroupKind::Folder, "Movies") => ("folder-01", theme::tint_purple()),
        (GroupKind::Folder, "Music") => ("folder-01", theme::tint_pink()),
        _ => ("folder-01", accent()),
    }
}

/// A row of "Your stuff": what it is, where it lives, what it takes up, and
/// its button. Every row keeps the button's room, so the sizes line up.
fn stuff_row(
    look: (&'static str, Hsla),
    title: String,
    detail: String,
    size: String,
    action: Option<AnyElement>,
) -> gpui::Div {
    entry_row(look, title, detail)
        .child(
            div()
                .w(px(SIZE_SLOT))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .text_size(px(theme::TEXT_BODY))
                .child(size),
        )
        .child(
            div()
                .w(px(ACTION_SLOT))
                .flex_shrink_0()
                .flex()
                .justify_center()
                .children(action),
        )
}

/// A row's mark, title and detail, on the list's grid.
fn entry_row(look: (&'static str, Hsla), title: String, detail: String) -> gpui::Div {
    div()
        .h(px(STUFF_ROW_HEIGHT))
        .flex_shrink_0()
        .px(px(ROW_INSET))
        .rounded(px(theme::RADIUS_ROW))
        .flex()
        .items_center()
        .gap_2()
        .child(icon(look.0, 16.).flex_shrink_0().text_color(look.1))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(title_line(title))
                .child(
                    div()
                        .truncate()
                        .line_height(px(TITLE_LINE))
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(detail),
                ),
        )
}

/// A row nothing opens, spaced as a pressable one is.
fn still(row: gpui::Div) -> gpui::Div {
    div().flex_shrink_0().pb(px(theme::ROW_GAP)).child(row)
}

fn title_line(title: String) -> gpui::Div {
    div()
        .truncate()
        .h(px(TITLE_LINE))
        .line_height(px(TITLE_LINE))
        .text_size(px(theme::TEXT_BODY))
        .child(title)
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

/// What a location can say about itself, a line each: whether it's away,
/// its space, and its scan. The mark already says a connected one is.
fn location_state(location: &Location, scanning: bool, now: u64) -> Vec<String> {
    let mut said = Vec::new();
    if !location.connected {
        said.push("Not connected".to_string());
    }
    if let Some((total, free)) = location.capacity {
        said.push(format!("{} free of {}", amount(free), amount(total)));
    }
    if scanning {
        said.push("Scanning…".to_string());
    } else {
        match &location.scan {
            Some(scan) => said.push(match scan.total.age(now) {
                Some(age) => format!("Scanned {age}"),
                None => "Scanned just now".to_string(),
            }),
            None if location.mount.is_some() => said.push("Not scanned".to_string()),
            None => {}
        }
    }
    said
}

fn change_row(change: &AgentChange, now: u64) -> impl IntoElement + use<> {
    let items = match change.items {
        1 => "1 item".to_string(),
        n => format!("{n} items"),
    };
    let when = overview::age(change.at, now).unwrap_or_else(|| "just now".to_string());
    still(entry_row(
        ("bot", accent()),
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
