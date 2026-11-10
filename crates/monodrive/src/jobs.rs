//! Copies and moves in the title bar: a ring of how far they've got, shown
//! while one is queued, running, waiting for an answer or failed, and a
//! popover under it listing those (⌥⌘J). Agents' jobs show too. A job
//! waiting at a name that's taken is answered here, which continues the same
//! job. The window asks the owner about once a second, only while a job is
//! going or the popover is open.

use crate::browser::{Operations, format_bytes, lock, short_reason};
use crate::downloads::{self, State as DownloadState};
use crate::menu;
use crate::theme::{
    self, ButtonLook, accent, circle_button, content, danger, progress_fill, progress_track,
    row_icon_button, text_button,
};
use gpui::{
    App, Context, ElementId, EventEmitter, Hsla, KeyBinding, Role, Subscription, Task, actions,
    div, prelude::*, px,
};
use monodrive_core::cloud;
use monodrive_core::ops::{self, Conflict, Done, Job, Origin, Request, State, Transfer};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
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
    /// One of the window's own copies or moves finished, or its own
    /// extraction or compression was cancelled.
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
    /// Repaints a Download button as its download starts and ends.
    _downloads: Option<Subscription>,
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

    /// Checks the jobs each time `changes` says the owner's journal changed.
    pub fn hear(&mut self, changes: async_channel::Receiver<()>, cx: &mut Context<Self>) {
        self._heard = Some(cx.spawn(async move |this, cx| {
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
                    // Cancelling an extraction or compression says what it
                    // kept, and Undo can take that back.
                    State::Cancelled if matches!(job.title.as_str(), "Extract" | "Compress") => {
                        cx.emit(JobsEvent::Finished(job.clone()))
                    }
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
            theme::tip_below(
                circle_button("jobs-ring", "Copies and Moves", theme::BUTTON_HEIGHT),
                "Copies and Moves",
            )
            .relative()
            .cursor_pointer()
            .hover(|s| s.bg(content(0.08)))
            .child(theme::ring(progress(&shown), 14.))
            .children(badge.map(|color| {
                div()
                    .absolute()
                    .top(px(2.))
                    .right(px(2.))
                    .size(px(6.))
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

    /// Downloads `path`, then runs job `id` again, or says why it couldn't.
    fn download_then_retry(&mut self, path: PathBuf, id: u64, ours: bool, cx: &mut Context<Self>) {
        let jobs = cx.weak_entity();
        let then = Rc::new(move |_: &Path, how: Result<(), String>, cx: &mut App| {
            let _ = jobs.update(cx, |this, cx| match how {
                Ok(()) => this.ask(
                    Request::Retry {
                        job: id,
                        conflict: None,
                    },
                    ours,
                    cx,
                ),
                Err(why) => {
                    this.error = Some(why);
                    cx.notify();
                }
            });
        });
        downloads::get(cx).update(cx, |downloads, cx| downloads.download(vec![path], then, cx));
    }

    fn row(&mut self, job: &Job, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (id, run) = (job.id, job.run);
        let ours = job.origin == Origin::Window;
        let answer = |conflict| Request::Retry { job: id, conflict };
        // A copy refused because a source isn't downloaded offers to
        // download it, and runs again once it's here.
        let refused = match job.state {
            State::Failed | State::Interrupted => {
                job.tasks.iter().find_map(|task| match &task.done {
                    Some(Done::Failed(why)) => cloud::refused(why),
                    _ => None,
                })
            }
            _ => None,
        };
        let downloads = downloads::get(cx);
        if self._downloads.is_none() {
            self._downloads = Some(cx.observe(&downloads, |_, _, cx| cx.notify()));
        }
        let downloading = refused
            .as_ref()
            .is_some_and(|path| downloads.read(cx).state(path) == Some(&DownloadState::Running));
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
                        item_name(task),
                        folder_name(&job.into)
                    );
                    (taken, content(0.65))
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
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(theme::TEXT_BODY))
                    .child(div().flex_1().min_w_0().truncate().child(title(job)))
                    .children(close.map(|label| {
                        theme::tip(
                            row_icon_button(
                                ElementId::NamedInteger("job-close".into(), id),
                                label,
                                "cancel-01",
                                content(0.55),
                            ),
                            label,
                        )
                        // Its glyph, not its circle, ends on the bar's edge.
                        .mr(px(-4.))
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
                        .text_size(px(theme::TEXT_CAPTION))
                        .text_color(content(0.65))
                        .truncate()
                        .child(caption.join(" · ")),
                )
            })
            .when(bar, |d| {
                let fraction = progress(&[job]);
                d.child(progress_track().child(progress_fill(fraction)))
            })
            .children(detail.map(|(text, color)| {
                div()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(color)
                    .truncate()
                    .child(text)
            }))
            .when(!buttons.is_empty(), |d| {
                let download = refused.map(|path| {
                    let look = match downloading {
                        true => ButtonLook::Disabled,
                        false => ButtonLook::Secondary,
                    };
                    let label = match downloading {
                        true => "Downloading…",
                        false => "Download",
                    };
                    text_button(
                        ElementId::NamedInteger("job-download".into(), id),
                        label,
                        look,
                    )
                    .debug_selector(|| "job-download".into())
                    .when(!downloading, |d| {
                        d.on_click(cx.listener(move |this, _, _, cx| {
                            this.download_then_retry(path.clone(), id, ours, cx)
                        }))
                    })
                });
                d.child(
                    div()
                        .flex()
                        .gap_1()
                        .children(download)
                        .children(buttons.into_iter().map(|(label, request)| {
                            text_button(
                                ElementId::NamedInteger(label.into(), id),
                                label,
                                ButtonLook::Secondary,
                            )
                            .debug_selector(move || format!("job-{label}"))
                            .on_click(cx.listener(
                                move |this, _, _, cx| this.ask(request.clone(), ours, cx),
                            ))
                        })),
                )
            })
    }
}

impl Render for Jobs {
    /// The popover's card; the browser places it under the ring and closes it.
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown: Vec<Job> = self.shown().cloned().collect();
        let note = match (&self.error, shown.is_empty()) {
            (Some(error), _) => Some((error.clone(), danger())),
            (None, true) => Some(("No copies or moves right now.".to_string(), content(0.65))),
            (None, false) => None,
        };
        let rows: Vec<_> = shown.iter().map(|job| self.row(job, cx)).collect();
        let lines = note.map(|(text, color)| {
            div()
                .px_2()
                .py_2()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(color)
                .child(text)
                .into_any_element()
        });
        let height = window.viewport_size().height - px(theme::TITLE_BAR_HEIGHT + 16.);
        menu::card(
            WIDTH,
            height,
            lines
                .into_iter()
                .chain(rows.into_iter().map(IntoElement::into_any_element)),
            false,
        )
        .id("jobs")
        .role(Role::Dialog)
        .aria_label("Copies and Moves")
    }
}

/// "Copying 3 items to Inbox", or "Copied" once it's done: what it does,
/// to what, and where.
pub fn title(job: &Job) -> String {
    let (doing, done, stopped) = match job.title.as_str() {
        "Move" => ("Moving", "Moved", "Stopped moving"),
        "Compress" => ("Compressing", "Compressed", "Stopped compressing"),
        "Extract" => ("Extracting", "Extracted", "Stopped extracting"),
        _ => ("Copying", "Copied", "Stopped copying"),
    };
    let verb = match job.state {
        State::Completed => done,
        State::Cancelled => stopped,
        _ => doing,
    };
    if job.title == "Compress" {
        // Its one task is the item, or the folder of several.
        return match job.tasks.first() {
            Some(ops::Task {
                done: Some(Done::To(archive)),
                ..
            }) => format!("{verb} into {}", quoted(archive)),
            Some(task) if task.source != job.into => format!("{verb} {}", quoted(&task.source)),
            _ => format!("{verb} items in {}", folder_name(&job.into)),
        };
    }
    let what = match job.tasks.as_slice() {
        [one] => item_name(one),
        tasks => format!("{} items", tasks.len()),
    };
    // The server's side prints as `source:/path`.
    let on_server = |label: &Path| {
        let label = label.to_string_lossy();
        let (source, path) = label.split_once(':').unwrap_or((&label, ""));
        let name = path.rsplit('/').find(|part| !part.is_empty());
        (source.to_owned(), name.map(str::to_owned))
    };
    match job.transfer() {
        Some(Transfer::Upload) => match on_server(&job.into) {
            (source, Some(name)) => format!("{verb} {what} to {name} on {source}"),
            (source, None) => format!("{verb} {what} to {source}"),
        },
        Some(Transfer::Download) => {
            let source = job.tasks.first().map(|task| on_server(&task.source).0);
            let into = folder_name(&job.into);
            match source {
                Some(source) => format!("{verb} {what} from {source} to {into}"),
                None => format!("{verb} {what} to {into}"),
            }
        }
        None => format!("{verb} {what} to {}", folder_name(&job.into)),
    }
}

/// What a job's toast says once it ends: its title, and for a stopped one
/// what it left in the Trash.
pub fn report(job: &Job) -> String {
    let said = title(job);
    // Core's words for an unfinished item it trashed, the only way a task
    // says so.
    let trashed = job.tasks.iter().any(
        |task| matches!(&task.done, Some(Done::Failed(why)) if why.contains("went to the Trash")),
    );
    if job.state == State::Cancelled && trashed {
        format!("{said}. What was partway went to the Trash.")
    } else {
        said
    }
}

/// The name an item takes where it goes, quoted.
fn item_name(task: &ops::Task) -> String {
    match &task.name {
        Some(name) => format!("\"{name}\""),
        None => quoted(&task.source),
    }
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
                name: None,
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
