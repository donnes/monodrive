//! Rename in the list: a bar across the pane holds the rule, and each selected
//! row shows its new name. The names, the problems and the renaming are
//! `ops::rename`'s; this only asks for them and draws what comes back.

use super::pane::Pane;
use super::{CancelRename, CommitRename, short_reason, with_owner};
use crate::preferences::View;
use crate::text_field::{self, TextField};
use crate::theme::{self, ButtonLook, accent, content, danger, text_button};
use gpui::{
    App, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, FontWeight, ScrollStrategy,
    Stateful, Subscription, Task, Window, div, prelude::*, px,
};
use monodrive_core::ops::rename::{Place, Plan, Rule, Status};
use monodrive_core::ops::{Origin, Outcome, Request};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The rule being edited and what it comes to for the selection.
pub(super) struct BatchRename {
    /// Which opening of the bar this is. An Apply's answer only belongs to
    /// the one that sent it.
    session: u64,
    /// The selected paths, in the order shown: numbers count in this order.
    pub(super) paths: Vec<PathBuf>,
    /// Where it was opened. Going anywhere else leaves it.
    place: (PathBuf, bool, String),
    pub(super) numbering: bool,
    pub(super) find: Entity<TextField>,
    pub(super) with: Entity<TextField>,
    pub(super) name: Entity<TextField>,
    pub(super) start: Entity<TextField>,
    match_case: bool,
    whole_name: bool,
    number_first: bool,
    /// The bar's buttons, as Tab stops.
    stops: HashMap<&'static str, FocusHandle>,
    pub(super) preview: Option<Preview>,
    /// The preview on its way. One runs at a time, since a folder's stamp
    /// walks it, and typing faster than that only keeps the last rule.
    previewing: Option<Task<()>>,
    /// The rule or the selection changed since `previewing` began, so what
    /// it finds is dropped and it runs again.
    again: bool,
    /// Sent to the owner and not yet answered. Until then the rule, the
    /// selection and the preview stay as they were sent.
    pub(super) sending: bool,
    /// Holds the keys while the fields are frozen and aren't drawn, so they
    /// reach nothing.
    keys: FocusHandle,
    /// Why the owner refused the last Apply.
    pub(super) error: Option<String>,
    _edits: [Subscription; 4],
}

pub(super) struct Preview {
    plan: Plan,
    rows: HashMap<PathBuf, usize>,
    pub(super) renames: usize,
    pub(super) problems: usize,
}

impl Preview {
    fn new(plan: Plan) -> Self {
        let rows = plan
            .rows
            .iter()
            .enumerate()
            .map(|(ix, row)| (row.path.clone(), ix))
            .collect();
        let renames = plan
            .rows
            .iter()
            .filter(|row| row.status == Status::Ok)
            .count();
        let problems = plan.problems().count();
        Self {
            plan,
            rows,
            renames,
            problems,
        }
    }
}

/// What a selected row shows in place of its name.
pub(super) struct RowPreview {
    pub(super) name: String,
    pub(super) renamed: bool,
    pub(super) problem: bool,
    /// The old name beside a new one, or why it can't have it.
    pub(super) note: Option<String>,
}

impl BatchRename {
    fn rule(&self, cx: &App) -> Result<Rule, String> {
        if !self.numbering {
            return Ok(Rule::Replace {
                find: self.find.read(cx).text().to_string(),
                with: self.with.read(cx).text().to_string(),
                case_sensitive: self.match_case,
                extension: self.whole_name,
            });
        }
        // How it's typed is how it's padded: 001 counts 001, 002.
        let typed = self.start.read(cx).text().trim();
        let start = match typed {
            "" => 1,
            typed => typed
                .parse()
                .map_err(|_| "Start At needs a whole number.".to_string())?,
        };
        Ok(Rule::Format {
            name: self.name.read(cx).text().to_string(),
            start,
            digits: typed.len(),
            place: if self.number_first {
                Place::Before
            } else {
                Place::After
            },
        })
    }

    /// Where the keys go in the bar: the rule's first field.
    pub(super) fn first_field(&self, cx: &App) -> FocusHandle {
        if self.sending {
            self.keys.clone()
        } else if self.numbering {
            self.name.focus_handle(cx)
        } else {
            self.find.focus_handle(cx)
        }
    }

    fn fields(&self) -> [&Entity<TextField>; 4] {
        [&self.find, &self.with, &self.name, &self.start]
    }

    fn field_has_keys(&self, window: &Window, cx: &App) -> bool {
        self.fields()
            .iter()
            .any(|field| field.focus_handle(cx).is_focused(window))
    }

    /// Whether the keys are anywhere in the bar.
    pub(super) fn has_keys(&self, window: &Window, cx: &App) -> bool {
        self.keys.is_focused(window)
            || self.field_has_keys(window, cx)
            || self.stops.values().any(|s| s.is_focused(window))
    }

    fn stop(&mut self, id: &'static str, control: Stateful<Div>, cx: &App) -> Stateful<Div> {
        let handle = self
            .stops
            .entry(id)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        control
            .track_focus(&handle)
            .focus_visible(|s| s.shadow(theme::focus_ring()))
    }

    /// What `path`'s row shows, once a preview has it.
    pub(super) fn row(&self, path: &Path, old: &str, folder: &Path) -> Option<RowPreview> {
        let preview = self.preview.as_ref()?;
        let row = &preview.plan.rows[*preview.rows.get(path)?];
        // A rule can leave nothing, which has nothing to draw.
        let name = if row.name.is_empty() { old } else { &row.name };
        let (renamed, problem, note) = match &row.status {
            Status::Ok => (true, false, Some(old.to_string())),
            Status::Unchanged => (false, false, None),
            Status::Invalid(why) | Status::Collision(why) => {
                (false, true, Some(short_reason(why, [folder])))
            }
        };
        Some(RowPreview {
            name: name.to_string(),
            renamed,
            problem,
            note,
        })
    }

    /// What it comes to, and whether that's a problem.
    fn status(&self, cx: &App) -> (String, bool) {
        if self.sending {
            return ("Renaming…".into(), false);
        }
        if let Some(why) = &self.error {
            return (why.clone(), true);
        }
        if let Err(why) = self.rule(cx) {
            return (why, true);
        }
        if self.paths.is_empty() {
            return ("No items selected.".into(), false);
        }
        let Some(preview) = &self.preview else {
            return (String::new(), false);
        };
        let (renames, problems) = (preview.renames, preview.problems);
        let names = if renames == 1 { "name" } else { "names" };
        match problems {
            0 if renames == 0 => ("No item gets a new name.".into(), false),
            0 => (format!("{renames} new {names}"), false),
            1 => (format!("{renames} new {names}, 1 problem"), true),
            _ => (format!("{renames} new {names}, {problems} problems"), true),
        }
    }
}

impl Pane {
    /// Whether a name or a batch is being edited, when nothing else may
    /// change files or take the list's keys.
    pub(super) fn changing_names(&self) -> bool {
        self.renaming.is_some() || self.batch.is_some()
    }

    fn place_now(&self) -> (PathBuf, bool, String) {
        let stop = self.stop();
        (stop.folder.clone(), stop.recents, self.query.clone())
    }

    /// An Apply is waiting on the owner: nothing here may change under it.
    pub(super) fn batch_sending(&self) -> bool {
        self.batch.as_ref().is_some_and(|b| b.sending)
    }

    /// Rename with several selected: the bar, with nothing changed yet.
    pub(super) fn start_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut field = |label: &'static str, placeholder: &'static str| {
            let field = cx.new(|cx| TextField::new(label, placeholder, cx));
            let edits = cx.subscribe(&field, |this, _, _: &text_field::Changed, cx| {
                if let Some(batch) = &mut this.batch {
                    batch.error = None;
                }
                this.preview_batch(cx);
                cx.notify();
            });
            (field, edits)
        };
        let (find, find_edits) = field("Find", "Find");
        let (with, with_edits) = field("Replace With", "Replace With");
        let (name, name_edits) = field("Name", "Name");
        let (start, start_edits) = field("Start At", "Start At 1");
        self.batch_sessions += 1;
        let batch = BatchRename {
            session: self.batch_sessions,
            keys: cx.focus_handle(),
            paths: self.chosen(),
            place: self.place_now(),
            numbering: false,
            find,
            with,
            name,
            start,
            match_case: false,
            whole_name: false,
            number_first: false,
            stops: HashMap::new(),
            preview: None,
            previewing: None,
            again: false,
            sending: false,
            error: None,
            _edits: [find_edits, with_edits, name_edits, start_edits],
        };
        window.focus(&batch.first_field(cx), cx);
        self.batch = Some(batch);
        self.reveal_after_layout_change();
        if let Some(ix) = self.cursor_ix() {
            self.reveal(ix, ScrollStrategy::Nearest);
        }
        self.preview_batch(cx);
        cx.notify();
    }

    /// A grid shows the bar's rows as a list, which scrolls elsewhere.
    fn reveal_after_layout_change(&mut self) {
        self.reveal_pending = self.view == View::Grid && self.selection.cursor().is_some();
    }

    /// Esc or Cancel, or the job done. Nothing has changed that the owner
    /// didn't do; the list takes the keys back unless they're elsewhere.
    pub(super) fn end_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.batch.as_ref().is_some_and(|b| b.sending) {
            return;
        }
        if let Some(batch) = self.batch.take() {
            if batch.has_keys(window, cx) {
                self.focus_list(window, cx);
            }
            self.reveal_after_layout_change();
            cx.notify();
        }
    }

    /// Works the plan out off the UI thread, as the disk may have to be walked.
    pub(super) fn preview_batch(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.batch.as_mut().filter(|b| !b.sending) else {
            return;
        };
        if batch.previewing.is_some() {
            batch.again = true;
            return;
        }
        batch.again = false;
        let Ok(rule) = batch.rule(cx) else {
            batch.preview = None;
            return;
        };
        let paths = batch.paths.clone();
        let work = cx
            .background_executor()
            .spawn(async move { Plan::new(&paths, &rule) });
        batch.previewing = Some(cx.spawn(async move |this, cx| {
            let plan = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(batch) = &mut this.batch else {
                    return;
                };
                batch.previewing = None;
                if batch.again {
                    return this.preview_batch(cx);
                }
                batch.preview = Some(Preview::new(plan));
                cx.notify();
            });
        }));
    }

    /// Each frame: going elsewhere leaves the bar, and a selection or an
    /// order that's changed makes the preview out of date.
    pub(super) fn follow_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(batch) = &self.batch else {
            return;
        };
        if batch.place != self.place_now() {
            let keys = batch.has_keys(window, cx);
            self.batch = None;
            self.reveal_after_layout_change();
            if keys {
                cx.defer_in(window, |this, window, cx| this.focus_list(window, cx));
            }
            return;
        }
        let chosen = self.chosen();
        if let Some(batch) = self
            .batch
            .as_mut()
            .filter(|b| !b.sending && b.paths != chosen)
        {
            batch.paths = chosen;
            batch.error = None;
            self.preview_batch(cx);
        }
    }

    /// Apply: what's on screen, as long as it's still what the owner works
    /// out. `skip` renames the rows without a problem, which only the button
    /// that says so asks for.
    pub(super) fn apply_batch(&mut self, skip: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(batch) = self.batch.as_ref().filter(|b| !b.sending) else {
            return;
        };
        if batch.previewing.is_some() {
            return;
        }
        let (Some(preview), Ok(rule)) = (&batch.preview, batch.rule(cx)) else {
            return;
        };
        if preview.renames == 0 || (preview.problems > 0 && !skip) {
            return;
        }
        let request = Request::RenameMany {
            paths: batch.paths.clone(),
            rule,
            preview: Some(preview.plan.fingerprint()),
            skip_problems: skip,
        };
        let session = batch.session;
        let sent = with_owner(
            window,
            cx,
            move |owner| {
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            move |this, done, window, cx| this.batch_applied(session, done, window, cx),
        );
        if let Some(batch) = &mut self.batch {
            batch.sending = sent;
            batch.error = None;
            if sent {
                window.focus(&batch.keys, cx);
            }
        }
        cx.notify();
    }

    /// A refusal stays, with the reason and the disk as it is now. Anything
    /// else is done: the bar goes, and the toast says how it went. If this bar
    /// has closed since it sent this, only the toast is left to do, and a bar
    /// opened since stays as it is.
    fn batch_applied(
        &mut self,
        session: u64,
        done: Result<(Request, Outcome), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let same = self.batch.as_ref().is_some_and(|b| b.session == session);
        if !same {
            return self.settle(done, false, cx);
        }
        if let Some(batch) = &mut self.batch {
            batch.sending = false;
            if let Err(why) = &done {
                // Under its first line, the refusal lists what the rows now show.
                let first = why.lines().next().unwrap_or(why);
                batch.error = Some(match first.strip_suffix(':') {
                    Some(first) => format!("{first}."),
                    None => first.to_string(),
                });
                if batch.keys.is_focused(window) {
                    window.focus(&batch.first_field(cx), cx);
                }
                return self.preview_batch(cx);
            }
        }
        self.end_batch(window, cx);
        self.settle(done, false, cx);
    }

    fn change_batch(
        &mut self,
        change: impl FnOnce(&mut BatchRename),
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(batch) = &mut self.batch {
            change(batch);
            batch.error = None;
            // After a press, ↩ should still apply.
            if !event.is_keyboard() {
                window.focus(&batch.first_field(cx), cx);
            }
        }
        self.preview_batch(cx);
        cx.notify();
    }

    fn batch_pill(
        &mut self,
        id: &'static str,
        label: &'static str,
        on: bool,
        change: fn(&mut BatchRename),
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        if self.batch_sending() {
            return Some(
                text_button(id, label, ButtonLook::Disabled).when(on, |d| d.bg(content(0.14))),
            );
        }
        let look = if on {
            ButtonLook::Secondary
        } else {
            ButtonLook::Ghost
        };
        let pill = text_button(id, label, look)
            .debug_selector(move || id.into())
            .when(on, |d| {
                d.bg(content(0.14))
                    .text_color(content(1.))
                    .font_weight(FontWeight::SEMIBOLD)
            })
            .on_click(cx.listener(move |this, event, window, cx| {
                this.change_batch(change, event, window, cx)
            }));
        Some(self.batch.as_mut()?.stop(id, pill, cx))
    }

    /// The bar across the list, in two lines: the rule, then what it comes
    /// to and the buttons. The first line wraps in a narrow pane.
    pub(super) fn batch_bar(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let covered = self.chrome.covered;
        let batch = self.batch.as_ref()?;
        let sending = batch.sending;
        let stops_on = !covered && !sending;
        let (numbering, match_case, whole_name, number_first) = (
            batch.numbering,
            batch.match_case,
            batch.whole_name,
            batch.number_first,
        );
        for field in batch.fields() {
            field.focus_handle(cx).tab_stop(stops_on);
        }
        for stop in batch.stops.values() {
            stop.clone().tab_stop(stops_on);
        }
        let bad_start = numbering && batch.rule(cx).is_err();
        let box_for = |field: &Entity<TextField>, bad: bool| {
            let focused = field.focus_handle(cx).is_focused(window);
            theme::field(if bad {
                danger()
            } else if focused {
                accent()
            } else {
                content(0.12)
            })
            .h(px(theme::FIELD_HEIGHT))
            .px_2()
            .map(|d| {
                if sending {
                    // What was sent, which no key or press can reach.
                    d.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TEXT_TITLE))
                            .text_color(content(0.65))
                            .child(field.read(cx).text().to_string()),
                    )
                } else {
                    d.child(div().flex_1().min_w_0().child(field.clone()))
                }
            })
        };
        let fields = if numbering {
            div()
                .flex()
                .flex_1()
                .min_w(px(200.))
                .gap_2()
                .child(box_for(&batch.name, false).flex_1().min_w_0())
                .child(box_for(&batch.start, bad_start).w(px(96.)).flex_shrink_0())
        } else {
            div()
                .flex()
                .flex_1()
                .min_w(px(200.))
                .gap_2()
                .child(box_for(&batch.find, false).flex_1().min_w_0())
                .child(box_for(&batch.with, false).flex_1().min_w_0())
        };
        let (status, bad) = batch.status(cx);
        let ready = batch
            .preview
            .as_ref()
            .filter(|_| !bad_start && !batch.sending);
        let renames = ready.map_or(0, |preview| preview.renames);
        let problems = ready.map_or(0, |preview| preview.problems);
        let label = if sending {
            "Applying…".to_string()
        } else if problems > 0 && renames > 0 {
            format!("Skip {problems} and Apply")
        } else {
            "Apply".to_string()
        };
        let apply = if renames > 0 {
            let button = text_button("batch-apply", label, ButtonLook::Primary)
                .debug_selector(|| "batch-apply".into())
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_batch(problems > 0, window, cx)
                }));
            self.batch.as_mut()?.stop("batch-apply", button, cx)
        } else {
            text_button("batch-apply", label, ButtonLook::Disabled)
        };
        let cancel = if sending {
            text_button("batch-cancel", "Cancel", ButtonLook::Disabled)
        } else {
            let cancel = text_button("batch-cancel", "Cancel", ButtonLook::Secondary)
                .debug_selector(|| "batch-cancel".into())
                .on_click(cx.listener(|this, _, window, cx| this.end_batch(window, cx)));
            self.batch.as_mut()?.stop("batch-cancel", cancel, cx)
        };
        let keys = self.batch.as_ref()?.keys.clone();
        let replace = self.batch_pill(
            "batch-replace",
            "Replace",
            !numbering,
            |b| b.numbering = false,
            cx,
        )?;
        let number = self.batch_pill(
            "batch-number",
            "Number",
            numbering,
            |b| b.numbering = true,
            cx,
        )?;
        let options: Vec<Stateful<Div>> = if numbering {
            vec![self.batch_pill(
                "batch-number-first",
                "Number First",
                number_first,
                |b| b.number_first = !b.number_first,
                cx,
            )?]
        } else {
            vec![
                self.batch_pill(
                    "batch-match-case",
                    "Match Case",
                    match_case,
                    |b| b.match_case = !b.match_case,
                    cx,
                )?,
                self.batch_pill(
                    "batch-whole-name",
                    "Include Extension",
                    whole_name,
                    |b| b.whole_name = !b.whole_name,
                    cx,
                )?,
            ]
        };
        Some(
            div()
                .debug_selector(|| "batch-bar".into())
                .track_focus(&keys)
                .key_context("Rename")
                .on_action(cx.listener(|this, _: &CommitRename, window, cx| {
                    // ↩ on a button is that button's.
                    if this
                        .batch
                        .as_ref()
                        .is_some_and(|b| b.field_has_keys(window, cx))
                    {
                        this.apply_batch(false, window, cx);
                    } else {
                        cx.propagate();
                    }
                }))
                .on_action(
                    cx.listener(|this, _: &CancelRename, window, cx| this.end_batch(window, cx)),
                )
                .flex_shrink_0()
                .pb_1()
                .border_b_1()
                .border_color(content(0.06))
                .child(
                    theme::pane_bar(theme::TITLE_BAR_HEIGHT)
                        .h_auto()
                        .min_h(px(theme::TITLE_BAR_HEIGHT))
                        .py(px(6.))
                        .flex_wrap()
                        .gap_2()
                        .child(div().flex().gap_1().child(replace).child(number))
                        .child(fields)
                        .child(div().flex().gap_1().children(options)),
                )
                .child(
                    theme::pane_bar(theme::ROW_HEIGHT)
                        .gap_2()
                        .text_size(px(theme::TEXT_SMALL))
                        .child(
                            div()
                                .debug_selector(move || {
                                    if sending {
                                        "batch-running".into()
                                    } else {
                                        "batch-status".into()
                                    }
                                })
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(if bad { danger() } else { content(0.65) })
                                .child(status),
                        )
                        .child(cancel)
                        .child(apply),
                ),
        )
    }
}
