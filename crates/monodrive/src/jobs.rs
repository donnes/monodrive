//! Copies and moves in the title bar: a ring of how far they've got, shown
//! while one is queued, running, waiting for an answer or failed, and a
//! popover under it listing those (⌥⌘J). Agents' jobs show too. A job
//! waiting at a name that's taken is answered here, which continues the same
//! job. The window asks the owner about once a second, only while a job is
//! going or the popover is open.

use crate::browser::{Operations, format_bytes, lock, short_reason};
use crate::menu;
use crate::theme::{accent, content, danger};
use gpui::{
    App, Context, ElementId, EventEmitter, Hsla, KeyBinding, PathBuilder, Role, Task, actions,
    canvas, div, point, prelude::*, px,
};
use monodrive_core::ops::{Conflict, Done, Job, Origin, Request, State, Watch};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

actions!(jobs, [Dismiss]);

pub fn bind(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("Jobs"))]);
}

/// How often the owner is asked while a job is going or the popover is open.
pub const CHECK: Duration = Duration::from_secs(1);
/// How long a finished job stays in the list.
pub const LINGER: Duration = Duration::from_secs(3);
const WIDTH: f32 = 320.;

pub enum JobsEvent {
    /// One of the window's own copies or moves finished.
    Finished(Job),
    /// One of the window's own jobs stopped at a name that's taken, or failed.
    NeedsYou,
}

#[derive(Default)]
pub struct Jobs {
    /// The owner's jobs, newest first, as it last said.
    jobs: Vec<Job>,
    error: Option<String>,
    /// When the window opened, in seconds since 1970. A failed or finished
    /// job shows only if its latest run started since, so last week's
    /// failures don't.
    since: u64,
    /// Runs, by id, that failed and were dismissed.
    dismissed: HashSet<u64>,
    /// Finished runs on their way out of the list, each with its timer.
    leaving: HashMap<u64, Task<()>>,
    /// Finished runs that have left it.
    gone: HashSet<u64>,
    /// The window's own copies and moves, followed until they end.
    followed: HashSet<u64>,
    open: bool,
    /// Hears the owner's journal change, which may be an agent's new job.
    _heard: Option<Task<()>>,
    /// The owner is asked again each `CHECK`.
    watching: bool,
    watch: Option<Task<()>>,
}

impl EventEmitter<JobsEvent> for Jobs {}

impl Jobs {
    pub fn new() -> Self {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Self {
            since,
            ..Self::default()
        }
    }

    /// Checks the jobs each time `changes` says the owner's journal
    /// changed, for as long as `watch` is kept.
    pub fn hear(
        &mut self,
        watch: Watch,
        changes: async_channel::Receiver<()>,
        cx: &mut Context<Self>,
    ) {
        self._heard = Some(cx.spawn(async move |this, cx| {
            let _watch = watch;
            while changes.recv().await.is_ok() {
                if this.update(cx, |this, cx| this.poke(cx)).is_err() {
                    return;
                }
            }
        }));
    }

    /// Follows a job the window started, and asks about it now.
    pub fn follow(&mut self, job: u64, cx: &mut Context<Self>) {
        self.followed.insert(job);
        self.watch(cx);
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.open = open;
        if open {
            self.watch(cx);
        }
    }

    /// Something changed the owner's journal, maybe an agent's new job.
    fn poke(&mut self, cx: &mut Context<Self>) {
        if !self.watching {
            self.watch(cx);
        }
    }

    /// Asks the owner now, then each `CHECK` while a job is going or the
    /// popover is open. The owner is taken afresh each time, so a gone one
    /// ends it.
    fn watch(&mut self, cx: &mut Context<Self>) {
        self.watching = true;
        self.watch = Some(cx.spawn(async move |this, cx| {
            loop {
                let ops = cx.update(|cx| cx.try_global::<Operations>().map(|ops| ops.0.clone()));
                let Some(ops) = ops else {
                    let _ = this.update(cx, |this, _| this.watching = false);
                    return;
                };
                let jobs = cx
                    .background_executor()
                    .spawn(async move { lock(&ops).jobs() })
                    .await;
                let more = this.update(cx, |this, cx| {
                    this.take(jobs, cx);
                    this.watching = this.open || this.jobs.iter().any(|job| job.state.active());
                    this.watching
                });
                if !matches!(more, Ok(true)) {
                    return;
                }
                cx.background_executor().timer(CHECK).await;
            }
        }));
    }

    /// Takes what the owner said, and repaints only if it changed.
    pub fn take(&mut self, jobs: Result<Vec<Job>, String>, cx: &mut Context<Self>) {
        let (jobs, error) = match jobs {
            Ok(jobs) => (jobs, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        for job in &jobs {
            let id = job.id;
            if job.state.active() {
                continue;
            }
            if self.followed.remove(&id) {
                match job.state {
                    State::Completed => cx.emit(JobsEvent::Finished(job.clone())),
                    State::Waiting | State::Failed | State::Interrupted => {
                        cx.emit(JobsEvent::NeedsYou)
                    }
                    _ => {}
                }
            }
            let run = job.run;
            let finished = matches!(job.state, State::Completed | State::Cancelled);
            if finished
                && self.new_run(job)
                && !self.gone.contains(&run)
                && !self.leaving.contains_key(&run)
            {
                let leave = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(LINGER).await;
                    let _ = this.update(cx, |this, cx| {
                        this.leaving.remove(&run);
                        this.gone.insert(run);
                        cx.notify();
                    });
                });
                self.leaving.insert(run, leave);
            }
        }
        if (&jobs, &error) != (&self.jobs, &self.error) {
            self.jobs = jobs;
            self.error = error;
            cx.notify();
        }
    }

    /// Whether the job's latest run started while the window was open,
    /// whether or not a check saw it going.
    fn new_run(&self, job: &Job) -> bool {
        job.started >= self.since
    }

    /// The jobs the ring and the popover show, newest first.
    fn shown(&self) -> impl Iterator<Item = &Job> {
        self.jobs.iter().filter(|job| {
            let run = job.run;
            match job.state {
                State::Queued | State::Running | State::Waiting => true,
                State::Failed | State::Interrupted => {
                    self.new_run(job) && !self.dismissed.contains(&run)
                }
                State::Completed | State::Cancelled => self.leaving.contains_key(&run),
            }
        })
    }

    /// The title bar's ring: how far the jobs shown have got, with a dot
    /// when one waits for an answer or failed. `None` with nothing to show.
    pub fn ring(&self) -> Option<gpui::Stateful<gpui::Div>> {
        let shown: Vec<&Job> = self.shown().collect();
        if shown.is_empty() {
            return None;
        }
        let failed = shown
            .iter()
            .any(|job| matches!(job.state, State::Failed | State::Interrupted));
        let waiting = shown.iter().any(|job| job.state == State::Waiting);
        let badge = (failed || waiting).then(|| if failed { danger() } else { accent() });
        Some(
            div()
                .id("jobs-ring")
                .role(Role::Button)
                .aria_label("Copies and Moves")
                .relative()
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(content(0.1)))
                .child(arc(progress(&shown)))
                .children(badge.map(|color| {
                    div()
                        .absolute()
                        .top(px(3.))
                        .right(px(3.))
                        .size(px(7.))
                        .rounded_full()
                        .bg(color)
                })),
        )
    }

    /// Cancels or continues a job; the next check shows how it's going.
    fn ask(&mut self, request: Request, follow: bool, cx: &mut Context<Self>) {
        let Some(ops) = cx.try_global::<Operations>().map(|ops| ops.0.clone()) else {
            return;
        };
        let asked = cx
            .background_executor()
            .spawn(async move { lock(&ops).submit(Origin::Window, request) });
        cx.spawn(async move |this, cx| {
            let asked = asked.await;
            let _ = this.update(cx, |this, cx| match asked {
                Ok(outcome) => {
                    if follow {
                        this.followed.insert(outcome.op);
                    }
                    this.watch(cx);
                }
                Err(e) => {
                    this.error = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn row(&self, job: &Job, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (id, run) = (job.id, job.run);
        let ours = job.origin == Origin::Window;
        let answer = |conflict| Request::Retry { job: id, conflict };
        let buttons: Vec<(&'static str, Request)> = match job.state {
            State::Waiting => vec![
                ("Skip", answer(Some(Conflict::Skip))),
                ("Keep Both", answer(Some(Conflict::KeepBoth))),
                ("Replace", answer(Some(Conflict::Replace))),
            ],
            State::Failed | State::Interrupted => vec![("Retry", answer(None))],
            _ => Vec::new(),
        };
        let close = match job.state {
            State::Queued | State::Running | State::Waiting => Some("Cancel"),
            State::Failed | State::Interrupted => Some("Dismiss"),
            _ => None,
        };
        let detail: Option<(String, Hsla)> = match job.state {
            State::Waiting => job
                .tasks
                .iter()
                .rev()
                .find(|task| task.done.is_some())
                .map(|task| {
                    let taken = format!(
                        "{} is already in {}",
                        quoted(&task.source),
                        folder_name(&job.into)
                    );
                    (taken, content(0.6))
                }),
            State::Failed | State::Interrupted => job
                .tasks
                .iter()
                .find_map(|task| match &task.done {
                    Some(Done::Failed(why)) => Some(why),
                    _ => None,
                })
                .map(|why| {
                    let folders = job
                        .tasks
                        .iter()
                        .filter_map(|task| task.source.parent())
                        .chain([job.into.as_path()]);
                    (short_reason(why, folders), danger())
                }),
            _ => None,
        };
        let mut caption = Vec::new();
        if job.state.active() && job.total_bytes > 0 {
            caption.push(format!(
                "{} of {}",
                format_bytes(job.bytes),
                format_bytes(job.total_bytes)
            ));
        }
        if let Origin::Agent { client } = &job.origin {
            caption.push(client.clone().unwrap_or_else(|| "An agent".into()));
        }
        let bar = matches!(job.state, State::Queued | State::Running | State::Completed);
        div()
            .px_2()
            .py(px(6.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .child(div().flex_1().min_w_0().truncate().child(title(job)))
                    .children(close.map(|label| {
                        div()
                            .id(ElementId::NamedInteger("job-close".into(), id))
                            .role(Role::Button)
                            .aria_label(label)
                            .flex_shrink_0()
                            .size(px(18.))
                            .rounded(px(5.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(content(0.1)))
                            .child(crate::theme::icon("cancel-01", 11.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if label == "Cancel" {
                                    this.ask(Request::Cancel { job: id }, false, cx);
                                } else {
                                    this.dismissed.insert(run);
                                    cx.notify();
                                }
                            }))
                    })),
            )
            .when(!caption.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(11.))
                        .text_color(content(0.5))
                        .truncate()
                        .child(caption.join(" · ")),
                )
            })
            .when(bar, |d| {
                let fraction = progress(&[job]);
                d.child(
                    div().h(px(4.)).rounded_full().bg(content(0.12)).child(
                        div()
                            .h_full()
                            .w(gpui::relative(fraction))
                            .rounded_full()
                            .bg(accent()),
                    ),
                )
            })
            .children(detail.map(|(text, color)| {
                div()
                    .text_size(px(12.))
                    .text_color(color)
                    .truncate()
                    .child(text)
            }))
            .when(!buttons.is_empty(), |d| {
                d.child(div().flex().gap_1().children(buttons.into_iter().map(
                    |(label, request)| {
                        div()
                            .id(ElementId::NamedInteger(label.into(), id))
                            .debug_selector(move || format!("job-{label}"))
                            .role(Role::Button)
                            .aria_label(label)
                            .h(px(22.))
                            .px_2()
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .bg(content(0.08))
                            .hover(|s| s.bg(content(0.14)))
                            .cursor_pointer()
                            .child(label)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.ask(request.clone(), ours, cx)
                            }))
                    },
                )))
            })
    }
}

impl Render for Jobs {
    /// The popover's card; the browser places it under the ring and closes it.
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown: Vec<Job> = self.shown().cloned().collect();
        let note = match (&self.error, shown.is_empty()) {
            (Some(error), _) => Some((error.clone(), danger())),
            (None, true) => Some(("No copies or moves right now.".to_string(), content(0.5))),
            (None, false) => None,
        };
        let rows: Vec<_> = shown.iter().map(|job| self.row(job, cx)).collect();
        let lines = note.map(|(text, color)| {
            div()
                .px_2()
                .py(px(6.))
                .text_size(px(12.))
                .text_color(color)
                .child(text)
                .into_any_element()
        });
        let height = window.viewport_size().height - px(crate::theme::TITLE_BAR_HEIGHT + 16.);
        menu::card(
            WIDTH,
            height,
            lines
                .into_iter()
                .chain(rows.into_iter().map(IntoElement::into_any_element)),
        )
        .id("jobs")
        .role(Role::Dialog)
        .aria_label("Copies and Moves")
    }
}

/// "Copying 3 items to Inbox", or "Copied" once it's done.
pub fn title(job: &Job) -> String {
    let moving = job.title == "Move";
    let verb = match (job.state, moving) {
        (State::Completed, false) => "Copied",
        (State::Completed, true) => "Moved",
        (State::Cancelled, false) => "Stopped copying",
        (State::Cancelled, true) => "Stopped moving",
        (_, false) => "Copying",
        (_, true) => "Moving",
    };
    let what = match job.tasks.as_slice() {
        [one] => quoted(&one.source),
        tasks => format!("{} items", tasks.len()),
    };
    format!("{verb} {what} to {}", folder_name(&job.into))
}

pub fn quoted(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    format!("\"{name}\"")
}

fn folder_name(folder: &Path) -> String {
    folder.file_name().map_or_else(
        || monodrive_core::paths::abbreviate(folder),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// How far `jobs` have got together: by bytes, or by items when there are
/// no bytes to count.
fn progress(jobs: &[&Job]) -> f32 {
    let (bytes, total) = jobs.iter().fold((0, 0), |(bytes, total), job| {
        (bytes + job.bytes, total + job.total_bytes)
    });
    let fraction = if total > 0 {
        bytes as f32 / total as f32
    } else {
        let (done, items) = jobs.iter().fold((0, 0), |(done, items), job| {
            let finished = job.tasks.iter().filter(|task| task.done.is_some()).count();
            (done + finished, items + job.tasks.len())
        });
        if items == 0 {
            1.
        } else {
            done as f32 / items as f32
        }
    };
    fraction.clamp(0., 1.)
}

/// A circle's track with `fraction` of it drawn from the top, clockwise.
/// It's painted once per frame drawn, which happens only when jobs change.
fn arc(fraction: f32) -> impl IntoElement {
    let (track, ink) = (content(0.18), accent());
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = bounds.size.width.min(bounds.size.height) / 2. - px(1.5);
            let at = |turn: f32| {
                let angle = turn * std::f32::consts::TAU;
                point(
                    center.x + radius * angle.sin(),
                    center.y - radius * angle.cos(),
                )
            };
            let radii = point(radius, radius);
            let mut circle = PathBuilder::stroke(px(2.));
            circle.move_to(at(0.));
            circle.arc_to(radii, px(0.), false, true, at(0.5));
            circle.arc_to(radii, px(0.), false, true, at(0.));
            if let Ok(path) = circle.build() {
                window.paint_path(path, track);
            }
            if fraction > 0. {
                let end = fraction.min(0.999);
                let mut done = PathBuilder::stroke(px(2.));
                done.move_to(at(0.));
                done.arc_to(radii, px(0.), end > 0.5, true, at(end));
                if let Ok(path) = done.build() {
                    window.paint_path(path, ink);
                }
            }
        },
    )
    .size(px(14.))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use monodrive_core::ops::Task as Item;

    fn job(id: u64, state: State, started: u64) -> Job {
        run(id, id, state, started)
    }

    /// Run `run` of job `id`.
    fn run(id: u64, run: u64, state: State, started: u64) -> Job {
        Job {
            id,
            title: "Copy".into(),
            origin: Origin::Agent { client: None },
            into: "/Users/me/Inbox".into(),
            state,
            tasks: vec![Item {
                source: "/Users/me/draft.txt".into(),
                done: None,
            }],
            bytes: 0,
            total_bytes: 0,
            started,
            run,
            undoable: false,
        }
    }

    #[gpui::test]
    fn the_ring_shows_new_jobs_until_they_have_finished(cx: &mut TestAppContext) {
        let jobs = cx.new(|_| Jobs {
            since: 100,
            ..Jobs::default()
        });
        let shown = |cx: &mut TestAppContext| jobs.read_with(cx, |jobs, _| jobs.ring().is_some());
        let take = |list: Vec<Job>, cx: &mut TestAppContext| {
            jobs.update(cx, |jobs, cx| jobs.take(Ok(list), cx));
        };
        // A failure from before the window opened isn't news.
        take(vec![job(1, State::Failed, 50)], cx);
        assert!(!shown(cx));
        take(vec![job(2, State::Running, 150)], cx);
        assert!(shown(cx));
        // One that failed before any check saw it going stays, as does one
        // that finished: until it's dismissed, and for a few seconds.
        take(vec![job(3, State::Failed, 150)], cx);
        cx.executor().advance_clock(LINGER);
        assert!(shown(cx));
        take(vec![job(4, State::Completed, 150)], cx);
        assert!(shown(cx));
        cx.executor().advance_clock(LINGER);
        assert!(!shown(cx));
        // Continued in the same second, it's another run, with its own stay.
        take(vec![run(4, 9, State::Completed, 150)], cx);
        assert!(shown(cx));
        cx.executor().advance_clock(LINGER);
        assert!(!shown(cx));
    }
}
