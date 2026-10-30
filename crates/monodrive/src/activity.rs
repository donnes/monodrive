//! Activity (⌥⌘J): the copies and moves the window and agents started, how
//! far each has got, and what failed. A job still going can be cancelled, one
//! waiting at a conflict settled with Skip, Keep Both or Replace, and a
//! failed or interrupted one retried. Esc or a click outside closes it.

use crate::browser::{Operations, format_bytes};
use crate::palette;
use crate::theme::{accent, content};
use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Role, Task,
    Window, actions, div, prelude::*, px,
};
use monodrive_core::ops::{Client, Conflict, Done, Job, Origin, Request, State};
use monodrive_core::paths;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

actions!(activity, [Dismiss]);

pub fn bind(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("Activity"))]);
}

/// Asks the browser to close the sheet.
pub struct Dismissed;

/// How often the open list asks the owner again. It repaints only when
/// something changed.
const REFRESH: Duration = Duration::from_secs(1);

pub struct ActivityView {
    focus: FocusHandle,
    ops: Option<Arc<Mutex<Client>>>,
    jobs: Vec<Job>,
    error: Option<String>,
    _refresh: Task<()>,
}

impl EventEmitter<Dismissed> for ActivityView {}

impl Focusable for ActivityView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ActivityView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let ops = cx.try_global::<Operations>().map(|ops| ops.0.clone());
        let refresh = match ops.clone() {
            None => Task::ready(()),
            Some(ops) => cx.spawn(async move |this, cx| {
                loop {
                    let ops = ops.clone();
                    let jobs = cx
                        .background_executor()
                        .spawn(async move { lock(&ops).jobs() })
                        .await;
                    let alive = this.update(cx, |this, cx| {
                        let (jobs, error) = match jobs {
                            Ok(jobs) => (jobs, None),
                            Err(e) => (Vec::new(), Some(e)),
                        };
                        if (&jobs, &error) != (&this.jobs, &this.error) {
                            this.jobs = jobs;
                            this.error = error;
                            cx.notify();
                        }
                    });
                    if alive.is_err() {
                        return;
                    }
                    cx.background_executor().timer(REFRESH).await;
                }
            }),
        };
        Self {
            focus: cx.focus_handle(),
            ops,
            jobs: Vec::new(),
            error: None,
            _refresh: refresh,
        }
    }

    /// Cancels or retries a job; the next refresh shows the result.
    fn ask(&mut self, request: Request, cx: &mut Context<Self>) {
        let Some(ops) = self.ops.clone() else { return };
        let asked = cx
            .background_executor()
            .spawn(async move { lock(&ops).submit(Origin::Window, request) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = asked.await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(e);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn row(&self, job: &Job, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let done = job.tasks.iter().filter(|task| task.done.is_some()).count();
        let who = match &job.origin {
            Origin::Window => "Monodrive".to_string(),
            Origin::Agent { client: Some(name) } => format!("Agent {name}"),
            Origin::Agent { client: None } => "Agent".to_string(),
        };
        let items = match job.tasks.len() {
            1 => "1 item".to_string(),
            n => format!("{n} items"),
        };
        let progress = format!(
            "{done} of {items} · {} of {} · {who}",
            format_bytes(job.bytes),
            format_bytes(job.total_bytes)
        );
        let failure = job.tasks.iter().find_map(|task| match &task.done {
            Some(Done::Failed(why)) => Some(why.clone()),
            _ => None,
        });
        let fraction = if job.total_bytes > 0 {
            job.bytes as f32 / job.total_bytes as f32
        } else if job.tasks.is_empty() {
            1.
        } else {
            done as f32 / job.tasks.len() as f32
        };
        let id = job.id;
        let retry = |conflict| Request::Retry { job: id, conflict };
        // What can be done with the job now: stop it while it's going,
        // choose how a waiting one settles its conflict, or try again.
        let actions: Vec<(&'static str, Request)> = match job.state {
            State::Queued | State::Running => vec![("Cancel", Request::Cancel { job: id })],
            State::Waiting => vec![
                ("Skip", retry(Some(Conflict::Skip))),
                ("Keep Both", retry(Some(Conflict::KeepBoth))),
                ("Replace", retry(Some(Conflict::Replace))),
            ],
            State::Failed | State::Interrupted => vec![("Retry", retry(None))],
            State::Completed | State::Cancelled => Vec::new(),
        };
        div()
            .px_3()
            .py_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(13.))
                    .child(div().flex_1().min_w_0().truncate().child(format!(
                        "{} {items} to {}",
                        job.title,
                        job.into.file_name().map_or_else(
                            || paths::abbreviate(&job.into),
                            |name| { name.to_string_lossy().into_owned() }
                        )
                    )))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(content(0.5))
                            .child(capitalized(job.state.label())),
                    )
                    .children(actions.into_iter().map(|(label, request)| {
                        div()
                            .id(gpui::ElementId::NamedInteger(label.into(), id))
                            .role(Role::Button)
                            .aria_label(label)
                            .flex_shrink_0()
                            .h(px(22.))
                            .px_2()
                            .rounded(px(6.))
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .bg(content(0.08))
                            .cursor_pointer()
                            .child(label)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.ask(request.clone(), cx)),
                            )
                    })),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(content(0.5))
                    .child(progress),
            )
            .when(job.state.active(), |d| {
                d.child(
                    div().h(px(4.)).rounded_full().bg(content(0.12)).child(
                        div()
                            .h_full()
                            .w(gpui::relative(fraction.clamp(0., 1.)))
                            .rounded_full()
                            .bg(accent()),
                    ),
                )
            })
            .children(failure.map(|why| {
                div()
                    .text_size(px(11.))
                    .text_color(accent())
                    .truncate()
                    .child(why)
            }))
    }
}

fn lock(ops: &Mutex<Client>) -> std::sync::MutexGuard<'_, Client> {
    ops.lock().unwrap_or_else(PoisonError::into_inner)
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

impl Render for ActivityView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<_> = self.jobs.iter().map(|job| self.row(job, cx)).collect();
        let note = match (&self.error, rows.is_empty()) {
            (Some(error), _) => Some((error.clone(), accent())),
            (None, true) => Some(("No copies or moves yet.".to_string(), content(0.5))),
            (None, false) => None,
        };
        let body = div()
            .id("activity-jobs")
            .max_h(px(360.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .children(note.map(|(text, color)| {
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(12.))
                    .text_color(color)
                    .child(text)
            }))
            .children(rows);
        palette::overlay("activity-overlay")
            .key_context("Activity")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(Dismissed)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(Dismissed)),
            )
            .child(palette::card("activity", "Activity", "Activity", body))
    }
}
