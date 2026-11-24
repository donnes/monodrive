//! Renaming and reorganizing what's in a pane: a name edited in place;
//! Rename in the list, where a bar across the pane holds the rule and each
//! selected row shows its new name; and Organize's review page, one section
//! per destination with why, its files and a switch to leave it out. The
//! names, the problems, the sorting and the moving are `ops::rename`'s,
//! `ops::organize`'s and the owner's; this only asks for them and draws what
//! comes back.

use crate::browser::pane::Pane;
use crate::browser::{CancelRename, CommitRename, with_owner};
use crate::format::short_reason;
use crate::preferences::{Preferences, View};
use crate::session::Shows;
use crate::text_field::{self, TextField};
use crate::theme::{self, ButtonLook, accent, content, danger, text_button};
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    MouseButton, ScrollStrategy, Stateful, Subscription, Task, Window, div, px,
};
use monodrive_core::ops::organize::{Group, Proposal, propose};
use monodrive_core::ops::rename::{Place, Plan, Rule, Status};
use monodrive_core::ops::{Outcome, Request};
use monodrive_core::{Origin, naming};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The rule being edited and what it comes to for the selection.
pub(in crate::browser) struct BatchRename {
    /// Which opening of the bar this is. An Apply's answer only belongs to
    /// the one that sent it.
    session: u64,
    /// The selected paths, in the order shown: numbers count in this order.
    pub(in crate::browser) paths: Vec<PathBuf>,
    /// Where it was opened. Going anywhere else leaves it.
    place: (PathBuf, Shows, String),
    pub(in crate::browser) numbering: bool,
    /// Names read from each file, with what's been read so far.
    pub(in crate::browser) suggesting: bool,
    /// Each path's suggested name, edited or not, or why it has none.
    suggested: HashMap<PathBuf, Result<String, String>>,
    /// Reading what `suggested` lacks.
    reading: Option<Task<()>>,
    /// The suggested name being changed by hand.
    pub(in crate::browser) editing: Option<(PathBuf, Entity<TextField>)>,
    pub(in crate::browser) find: Entity<TextField>,
    pub(in crate::browser) with: Entity<TextField>,
    pub(in crate::browser) name: Entity<TextField>,
    pub(in crate::browser) start: Entity<TextField>,
    match_case: bool,
    whole_name: bool,
    number_first: bool,
    /// The bar's buttons, as Tab stops.
    stops: HashMap<&'static str, FocusHandle>,
    pub(in crate::browser) preview: Option<Preview>,
    /// The preview on its way. One runs at a time, since a folder's stamp
    /// walks it, and typing faster than that only keeps the last rule.
    previewing: Option<Task<()>>,
    /// The rule or the selection changed since `previewing` began, so what
    /// it finds is dropped and it runs again.
    again: bool,
    /// Sent to the owner and not yet answered. Until then the rule, the
    /// selection and the preview stay as they were sent.
    pub(in crate::browser) sending: bool,
    /// Holds the keys while the fields are frozen and aren't drawn, so they
    /// reach nothing.
    keys: FocusHandle,
    /// Why the owner refused the last Apply.
    pub(in crate::browser) error: Option<String>,
    _edits: [Subscription; 4],
}

pub(in crate::browser) struct Preview {
    plan: Plan,
    rows: HashMap<PathBuf, usize>,
    pub(in crate::browser) renames: usize,
    pub(in crate::browser) problems: usize,
}

impl Preview {
    pub(in crate::browser) fn new(plan: Plan) -> Self {
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
pub(in crate::browser) struct RowPreview {
    pub(in crate::browser) name: String,
    pub(in crate::browser) renamed: bool,
    pub(in crate::browser) problem: bool,
    /// The old name beside a new one, or why it can't have it.
    pub(in crate::browser) note: Option<String>,
}

impl BatchRename {
    pub(in crate::browser) fn rule(&self, cx: &App) -> Result<Rule, String> {
        if self.suggesting {
            return Ok(Rule::Names {
                names: self
                    .paths
                    .iter()
                    .map(|path| match self.suggested.get(path) {
                        Some(Ok(name)) => name.clone(),
                        _ => String::new(),
                    })
                    .collect(),
            });
        }
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
    pub(in crate::browser) fn first_field(&self, cx: &App) -> FocusHandle {
        if let Some((_, field)) = self.editing.as_ref().filter(|_| !self.sending) {
            field.focus_handle(cx)
        } else if self.sending || self.suggesting {
            self.keys.clone()
        } else if self.numbering {
            self.name.focus_handle(cx)
        } else {
            self.find.focus_handle(cx)
        }
    }

    pub(in crate::browser) fn fields(&self) -> [&Entity<TextField>; 4] {
        [&self.find, &self.with, &self.name, &self.start]
    }

    pub(in crate::browser) fn field_has_keys(&self, window: &Window, cx: &App) -> bool {
        self.fields()
            .iter()
            .any(|field| field.focus_handle(cx).is_focused(window))
    }

    /// Whether the keys are anywhere in the bar.
    pub(in crate::browser) fn has_keys(&self, window: &Window, cx: &App) -> bool {
        self.keys.is_focused(window)
            || self.field_has_keys(window, cx)
            || self
                .editing
                .as_ref()
                .is_some_and(|(_, field)| field.focus_handle(cx).is_focused(window))
            || self.stops.values().any(|s| s.is_focused(window))
    }

    pub(in crate::browser) fn stop(
        &mut self,
        id: &'static str,
        control: Stateful<Div>,
        cx: &App,
    ) -> Stateful<Div> {
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
    pub(in crate::browser) fn row(
        &self,
        path: &Path,
        old: &str,
        folder: &Path,
    ) -> Option<RowPreview> {
        let preview = self.preview.as_ref()?;
        let row = &preview.plan.rows[*preview.rows.get(path)?];
        // A rule can leave nothing, which has nothing to draw.
        let name = if row.name.is_empty() { old } else { &row.name };
        let (renamed, problem, note) = match &row.status {
            Status::Ok => (true, false, Some(old.to_string())),
            Status::Unchanged => {
                let why = match self.suggested.get(path).filter(|_| self.suggesting) {
                    Some(Err(why)) => Some(why.clone()),
                    _ => None,
                };
                (false, false, why)
            }
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
    pub(in crate::browser) fn status(&self, cx: &App) -> (String, bool) {
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
        if self.reading.is_some() {
            return ("Reading files…".into(), false);
        }
        if self.editing.is_some() {
            return ("Press Return to keep this name.".into(), false);
        }
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

/// What a read found, under any name typed while it ran.
fn merge_suggestions(
    suggested: &mut HashMap<PathBuf, Result<String, String>>,
    found: Vec<(PathBuf, Result<String, String>)>,
) {
    for (path, name) in found {
        suggested.entry(path).or_insert(name);
    }
}

impl Pane {
    /// Whether a name or a batch is being edited, or a folder reviewed, when
    /// nothing else may change files or take the list's keys.
    pub(in crate::browser) fn changing_names(&self) -> bool {
        self.renaming.is_some()
            || self.batch.is_some()
            || self.organizing.is_some()
            || self.preview_typing
    }

    pub(in crate::browser) fn place_now(&self) -> (PathBuf, Shows, String) {
        let stop = self.stop();
        (stop.folder.clone(), stop.shows, self.query.clone())
    }

    /// An Apply is waiting on the owner: nothing here may change under it.
    pub(in crate::browser) fn batch_sending(&self) -> bool {
        self.batch.as_ref().is_some_and(|b| b.sending)
    }

    /// Rename with several selected: the bar, with nothing changed yet.
    pub(in crate::browser) fn start_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_batch(false, window, cx);
    }

    /// Suggest Names: the bar, with names read from each file.
    pub(in crate::browser) fn suggest_names(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(batch) = self.batch.as_mut().filter(|b| !b.sending) {
            batch.suggesting = true;
            self.preview_batch(cx);
            return self.take_keys(window, cx);
        }
        if self.batch.is_none() && self.renaming.is_none() {
            self.open_batch(true, window, cx);
        }
    }

    pub(in crate::browser) fn open_batch(
        &mut self,
        suggesting: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            suggesting,
            suggested: HashMap::new(),
            reading: None,
            editing: None,
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
    pub(in crate::browser) fn reveal_after_layout_change(&mut self) {
        self.reveal_pending = self.view == View::Grid && self.selection.cursor().is_some();
    }

    /// Esc or Cancel, or the job done. Nothing has changed that the owner
    /// didn't do; the list takes the keys back unless they're elsewhere.
    pub(in crate::browser) fn end_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(in crate::browser) fn preview_batch(&mut self, cx: &mut Context<Self>) {
        self.read_suggestions(cx);
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

    /// Reads the names a selection lacks, off the UI thread. One read runs at
    /// a time, and when it lands the preview runs and asks for any since added.
    pub(in crate::browser) fn read_suggestions(&mut self, cx: &mut Context<Self>) {
        let style = if cx.global::<Preferences>().sentence_case_names {
            naming::Style::Sentence
        } else {
            naming::Style::Hyphens
        };
        let Some(batch) = self
            .batch
            .as_mut()
            .filter(|b| b.suggesting && b.reading.is_none())
        else {
            return;
        };
        let missing: Vec<PathBuf> = batch
            .paths
            .iter()
            .filter(|path| !batch.suggested.contains_key(*path))
            .cloned()
            .collect();
        if missing.is_empty() {
            return;
        }
        let work = cx.background_executor().spawn(async move {
            missing
                .into_iter()
                .map(|path| {
                    let name = naming::suggest(&path, style);
                    (path, name)
                })
                .collect::<Vec<_>>()
        });
        batch.reading = Some(cx.spawn(async move |this, cx| {
            let found = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(batch) = &mut this.batch else {
                    return;
                };
                batch.reading = None;
                merge_suggestions(&mut batch.suggested, found);
                this.preview_batch(cx);
                cx.notify();
            });
        }));
    }

    /// A click on a row's suggested name, to change it by hand.
    pub(in crate::browser) fn edit_suggestion(
        &mut self,
        path: PathBuf,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let field = cx.new(|cx| {
            let mut field = TextField::new("New Name", "New Name", cx);
            field.set_text(&name, name.len()..name.len(), cx);
            field
        });
        if let Some(batch) = self.batch.as_mut().filter(|b| b.suggesting && !b.sending) {
            window.focus(&field.focus_handle(cx), cx);
            // Moving on from another row keeps what was typed there.
            if let Some((path, typed)) = batch.editing.replace((path, field)) {
                let name = typed.read(cx).text().trim().to_string();
                batch.suggested.insert(path, Ok(name));
                self.preview_batch(cx);
            }
            cx.notify();
        }
    }

    /// ↩ keeps what was typed, and Esc what was there.
    pub(in crate::browser) fn end_edit(
        &mut self,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(batch) = &mut self.batch else {
            return;
        };
        if let Some((path, field)) = batch.editing.take()
            && keep
        {
            let name = field.read(cx).text().trim().to_string();
            batch.suggested.insert(path, Ok(name));
            batch.error = None;
        }
        self.take_keys(window, cx);
        self.preview_batch(cx);
        cx.notify();
    }

    /// The field in `path`'s row, while its suggested name is being changed.
    pub(in crate::browser) fn suggestion_field(
        &self,
        path: &Path,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (_, field) = self
            .batch
            .as_ref()?
            .editing
            .as_ref()
            .filter(|(editing, _)| editing == path)?;
        Some(
            theme::field(accent())
                .debug_selector(|| "suggestion-field".into())
                .key_context("Rename")
                .on_action(
                    cx.listener(|this, _: &CommitRename, window, cx| {
                        this.end_edit(true, window, cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &CancelRename, window, cx| {
                    this.end_edit(false, window, cx)
                }))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .w(px(width))
                .flex_shrink_0()
                .px(px(6.))
                .child(field.clone()),
        )
    }

    /// While suggesting, a click on a selected row's name edits it, and isn't
    /// a click on the row, which would shrink the selection the bar is for.
    pub(in crate::browser) fn suggested_name(
        &self,
        name: Div,
        path: &Path,
        shown: &str,
        cx: &mut Context<Self>,
    ) -> Div {
        if !self
            .batch
            .as_ref()
            .is_some_and(|b| b.suggesting && !b.sending)
        {
            return name;
        }
        let (path, shown) = (path.to_path_buf(), shown.to_string());
        let selector = format!(
            "suggested-{}",
            path.file_name().unwrap_or_default().display()
        );
        name.debug_selector(move || selector)
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.edit_suggestion(path.clone(), shown.clone(), window, cx);
                }),
            )
    }

    /// Each frame: going elsewhere leaves the bar, and a selection or an
    /// order that's changed makes the preview out of date.
    pub(in crate::browser) fn follow_batch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(in crate::browser) fn apply_batch(
        &mut self,
        skip: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(batch) = self.batch.as_ref().filter(|b| !b.sending) else {
            return;
        };
        if batch.previewing.is_some() || batch.reading.is_some() || batch.editing.is_some() {
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
    pub(in crate::browser) fn batch_applied(
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

    pub(in crate::browser) fn change_batch(
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

    pub(in crate::browser) fn batch_pill(
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
    pub(in crate::browser) fn batch_bar(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let covered = self.chrome.covered;
        let batch = self.batch.as_ref()?;
        let sending = batch.sending;
        let stops_on = !covered && !sending;
        let (numbering, suggesting, match_case, whole_name, number_first) = (
            batch.numbering,
            batch.suggesting,
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
        let bad_start = numbering && !suggesting && batch.rule(cx).is_err();
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
        let fields = if suggesting {
            div()
                .flex()
                .flex_1()
                .min_w(px(200.))
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child("Click a new name to change it.")
        } else if numbering {
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
        let ready = batch.preview.as_ref().filter(|_| {
            !bad_start && !batch.sending && batch.reading.is_none() && batch.editing.is_none()
        });
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
            !numbering && !suggesting,
            |b| {
                b.numbering = false;
                b.suggesting = false;
                b.editing = None;
            },
            cx,
        )?;
        let number = self.batch_pill(
            "batch-number",
            "Number",
            numbering && !suggesting,
            |b| {
                b.numbering = true;
                b.suggesting = false;
                b.editing = None;
            },
            cx,
        )?;
        let suggest = self.batch_pill(
            "batch-suggest",
            "Suggest",
            suggesting,
            |b| b.suggesting = true,
            cx,
        )?;
        let options: Vec<Stateful<Div>> = if suggesting {
            Vec::new()
        } else if numbering {
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
                    if this.batch.as_ref().is_some_and(|b| {
                        b.field_has_keys(window, cx) || (b.suggesting && b.keys.is_focused(window))
                    }) {
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
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(replace)
                                .child(number)
                                .child(suggest),
                        )
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

/// A name being edited in place.
pub(in crate::browser) struct Renaming {
    pub(in crate::browser) path: PathBuf,
    pub(in crate::browser) field: Entity<TextField>,
    /// Why the last name didn't take, shown in the title bar.
    pub(in crate::browser) error: Option<String>,
    /// Sent to the owner and not yet answered, so a blur meanwhile doesn't
    /// send it again.
    sending: bool,
    /// Its field is in the inspector, so the row shows the name as usual.
    pub(in crate::browser) in_inspector: bool,
    /// Clicking away commits.
    _blur: Subscription,
}

/// Why `path` couldn't take a name, short enough for the title bar:
/// "\"draft.txt\" is already taken."
pub(in crate::browser) fn rename_reason(why: &str, path: &Path) -> String {
    let why = short_reason(why, path.parent());
    match why.strip_suffix(" already exists.") {
        Some(name) => format!("\"{name}\" is already taken."),
        None => why,
    }
}

impl Pane {
    /// Selects what a New Folder or New File made and edits its name.
    pub(in crate::browser) fn rename_made(
        &mut self,
        done: Result<(Request, Outcome), String>,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let made = done
            .as_ref()
            .ok()
            .and_then(|(_, outcome)| outcome.items.first())
            .and_then(|item| item.result.clone().ok());
        self.settle(done, true, cx);
        if let Some(path) = made.filter(|_| self.has_the_window()) {
            self.start_rename(path, folder, window, cx);
        }
    }
    /// ↩: edits the selected item's name in place, or with several selected
    /// opens the bar. From the list while it's open, the keys go back to it.
    pub(in crate::browser) fn rename_selected(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Return applies a crop or trim in the preview, as nothing there is renamed.
        if let Some(view) = self.preview.clone().filter(|view| view.read(cx).cropping()) {
            return view.update(cx, |view, cx| view.apply_crop(window, cx));
        }
        if let Some(view) = self.preview.clone().filter(|view| view.read(cx).trimming()) {
            return view.update(cx, |view, cx| view.apply_trim(cx));
        }
        if self.batch.is_some() {
            return self.take_keys(window, cx);
        }
        if self.selection.len() > 1 {
            return self.start_batch(window, cx);
        }
        let chosen = self.selection.in_order(&self.entries);
        let [only] = chosen.as_slice() else {
            return;
        };
        let (path, folder) = (
            only.path.clone(),
            only.kind == monodrive_core::entries::Kind::Dir,
        );
        if let Some(ix) = self.entries.iter().position(|e| e.path == path) {
            self.reveal(ix, ScrollStrategy::Nearest);
        }
        self.start_rename(path, folder, window, cx);
    }
    /// The lone selected item's name, edited where the inspector shows it
    /// and not in its row. Its field can draw in one place at a time.
    pub(in crate::browser) fn rename_in_inspector(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The panel can still show the last item while the selection has moved.
        if self.selection.only() != Some(path) {
            return;
        }
        self.rename_selected(window, cx);
        if let Some(renaming) = &mut self.renaming {
            renaming.in_inspector = true;
        }
    }
    /// Edits `path`'s name in place, selected up to its extension as in
    /// Finder, or whole for a folder. It needn't be listed yet.
    pub(in crate::browser) fn start_rename(
        &mut self,
        path: PathBuf,
        folder: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = name
            .rfind('.')
            .filter(|dot| *dot > 0 && !folder)
            .unwrap_or(name.len());
        let prefs = cx.global::<Preferences>();
        let size = match self.layout() {
            View::List | View::Columns => prefs.row_size.metrics().2,
            View::Grid => prefs.grid_label.metrics().2,
        };
        let field = cx.new(|cx| {
            let mut field = TextField::new("Name", "", cx).with_font_size(size);
            field.set_text(&name, 0..stem, cx);
            field
        });
        let focus = field.focus_handle(cx);
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            this.commit_rename(window, cx)
        });
        window.focus(&focus, cx);
        self.renaming = Some(Renaming {
            path,
            field,
            error: None,
            sending: false,
            in_inspector: false,
            _blur: blur,
        });
        cx.notify();
    }
    /// ↩, or clicking away: renames through the owner. A name that can't be
    /// used keeps the field open with the reason; the same name just closes
    /// it.
    pub(in crate::browser) fn commit_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(renaming) = self.renaming.as_ref().filter(|r| !r.sending) else {
            return;
        };
        let path = renaming.path.clone();
        let name = renaming.field.read(cx).text().to_string();
        if path.file_name() == Some(name.as_ref()) {
            return self.end_rename(window, cx);
        }
        let request = Request::Rename {
            path: path.clone(),
            name,
        };
        let sent = with_owner(
            window,
            cx,
            move |owner| {
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            move |this, done, window, cx| {
                let failure = match &done {
                    Ok((_, outcome)) => outcome.failures().next().map(str::to_owned),
                    Err(e) => Some(e.clone()),
                };
                let failure = failure.map(|why| rename_reason(&why, &path));
                let has_the_window = this.has_the_window();
                let still = this.renaming.as_mut().filter(|r| r.path == path);
                match (failure, still) {
                    (Some(why), Some(renaming)) => {
                        renaming.sending = false;
                        renaming.error = Some(why);
                        if has_the_window {
                            window.focus(&renaming.field.focus_handle(cx), cx);
                        }
                    }
                    (Some(why), None) => this.toast(why, true, None, cx),
                    (None, still) => {
                        if still.is_some() {
                            this.end_rename(window, cx);
                        }
                        this.settle(done, false, cx);
                    }
                }
            },
        );
        match self.renaming.as_mut() {
            Some(renaming) if sent => renaming.sending = true,
            _ => self.end_rename(window, cx),
        }
    }
    /// Closes the name being edited; the list takes the keys back unless
    /// a click already sent them elsewhere.
    pub(in crate::browser) fn end_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(renaming) = self.renaming.take()
            && renaming.field.focus_handle(cx).is_focused(window)
        {
            self.focus_list(window, cx);
        }
        cx.notify();
    }
}

/// How many names a section lists; the page reads by group, not by file.
const SHOWN: usize = 6;

pub(in crate::browser) struct Review {
    /// Where it was opened. Going anywhere else leaves it.
    place: (PathBuf, Shows, String),
    keys: FocusHandle,
    pub(in crate::browser) proposal: Option<Proposal>,
    /// The destinations switched off.
    left_out: HashSet<PathBuf>,
    pub(in crate::browser) sending: bool,
    error: Option<String>,
    _reading: Option<Task<()>>,
}

impl Review {
    pub(in crate::browser) fn chosen(&self) -> Vec<Group> {
        self.proposal
            .iter()
            .flat_map(|proposal| &proposal.groups)
            .filter(|group| !self.left_out.contains(&group.folder))
            .cloned()
            .collect()
    }
}

impl Pane {
    /// Organize: the review, with nothing changed.
    pub(in crate::browser) fn start_organize(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.changing_names() || self.showing_page() {
            return;
        }
        let keys = cx.focus_handle();
        window.focus(&keys, cx);
        self.organizing = Some(Review {
            place: self.place_now(),
            keys,
            proposal: None,
            left_out: HashSet::new(),
            sending: false,
            error: None,
            _reading: None,
        });
        self.read_proposal(cx);
        cx.notify();
    }

    pub(in crate::browser) fn read_proposal(&mut self, cx: &mut Context<Self>) {
        let folder = self.folder().to_path_buf();
        let work = cx
            .background_executor()
            .spawn(async move { propose(&folder) });
        let reading = cx.spawn(async move |this, cx| {
            let found = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(review) = &mut this.organizing else {
                    return;
                };
                match found {
                    Ok(proposal) => review.proposal = Some(proposal),
                    Err(why) => review.error = Some(why),
                }
                cx.notify();
            });
        });
        if let Some(review) = &mut self.organizing {
            review._reading = Some(reading);
        }
    }

    /// Esc, Cancel or Back: nothing has changed.
    pub(in crate::browser) fn end_organize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.organizing.as_ref().is_some_and(|r| r.sending) {
            return;
        }
        if self.organizing.take().is_some() {
            self.focus_list(window, cx);
            cx.notify();
        }
    }

    /// Each frame: going elsewhere leaves the review.
    pub(in crate::browser) fn follow_organize(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .organizing
            .as_ref()
            .is_some_and(|review| !review.sending && review.place != self.place_now())
        {
            self.organizing = None;
            cx.defer_in(window, |this, window, cx| this.focus_list(window, cx));
        }
    }

    pub(in crate::browser) fn run_organize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self.organizing.as_ref().filter(|r| !r.sending) else {
            return;
        };
        let groups = review.chosen();
        if groups.is_empty() {
            return;
        }
        let request = Request::Organize { groups };
        let sent = with_owner(
            window,
            cx,
            move |owner| {
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            move |this, done, window, cx| this.organized(done, window, cx),
        );
        if let Some(review) = &mut self.organizing {
            review.sending = sent;
            review.error = None;
        }
        cx.notify();
    }

    /// A refusal stays, with the reason and the disk as it is now. Anything
    /// else is done: the page goes, and the toast offers Undo.
    pub(in crate::browser) fn organized(
        &mut self,
        done: Result<(Request, Outcome), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(review) = &mut self.organizing {
            review.sending = false;
            if let Err(why) = &done {
                review.error = Some(why.lines().next().unwrap_or(why).to_string());
                review.proposal = None;
                self.read_proposal(cx);
                return cx.notify();
            }
        }
        self.end_organize(window, cx);
        self.settle(done, false, cx);
    }

    pub(in crate::browser) fn organize_section(
        &self,
        group: &Group,
        off: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let folder = group.folder.clone();
        let name = group.name();
        let count = group.items.len();
        let names = group
            .items
            .iter()
            .take(SHOWN)
            .filter_map(|item| item.path.file_name())
            .map(|name| name.to_string_lossy().into_owned());
        let more = count.saturating_sub(SHOWN);
        div()
            .pb_3()
            .flex()
            .flex_col()
            .gap(px(theme::ROW_GAP))
            .child(
                div()
                    .id(gpui::SharedString::from(format!("organize-{name}")))
                    .debug_selector({
                        let name = name.clone();
                        move || format!("organize-{name}")
                    })
                    .h(px(theme::ROW_HEIGHT))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(px(theme::RADIUS_ROW))
                    .hover(|d| d.bg(content(0.06)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(review) = this.organizing.as_mut().filter(|r| !r.sending) {
                            if !review.left_out.remove(&folder) {
                                review.left_out.insert(folder.clone());
                            }
                            cx.notify();
                        }
                    }))
                    .child(theme::switch(!off))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(content(if off { 0.65 } else { 1. }))
                            .child(format!("{name}/")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(theme::TEXT_SMALL))
                            .text_color(content(0.65))
                            .child(group.reason.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SMALL))
                            .text_color(content(0.65))
                            .child(format!(
                                "{count} {}",
                                if count == 1 { "file" } else { "files" }
                            )),
                    ),
            )
            .children(names.map(|name| {
                div()
                    .pl(px(44.))
                    .truncate()
                    .text_color(content(0.65))
                    .child(name)
            }))
            .when(more > 0, |d| {
                d.child(
                    div()
                        .pl(px(44.))
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(format!("and {more} more")),
                )
            })
            .into_any_element()
    }

    /// The page, in place of the list, while reviewing.
    pub(in crate::browser) fn organize_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let review = self.organizing.as_ref()?;
        let keys = review.keys.clone();
        let sending = review.sending;
        let groups = review.chosen();
        let files: usize = groups.iter().map(|group| group.items.len()).sum();
        let (status, bad) = match (&review.error, &review.proposal) {
            (Some(why), _) => (why.clone(), true),
            (None, None) => ("Reading the folder…".to_string(), false),
            (None, Some(proposal)) if proposal.groups.is_empty() => {
                ("No rule places anything in this folder.".to_string(), false)
            }
            (None, Some(proposal)) => {
                let folders = groups.len();
                let mut said = format!(
                    "{files} {} into {folders} {}",
                    if files == 1 { "file" } else { "files" },
                    if folders == 1 { "folder" } else { "folders" },
                );
                if proposal.unplaced > 0 {
                    said += &format!(", {} left where they are", proposal.unplaced);
                }
                (said, false)
            }
        };
        let sections: Vec<AnyElement> = review
            .proposal
            .iter()
            .flat_map(|proposal| proposal.groups.clone())
            .map(|group| {
                let off = self
                    .organizing
                    .as_ref()
                    .is_some_and(|r| r.left_out.contains(&group.folder));
                self.organize_section(&group, off, cx)
            })
            .collect();
        let cancel = if sending {
            text_button("organize-cancel", "Cancel", ButtonLook::Disabled)
        } else {
            text_button("organize-cancel", "Cancel", ButtonLook::Secondary)
                .debug_selector(|| "organize-cancel".into())
                .on_click(cx.listener(|this, _, window, cx| this.end_organize(window, cx)))
        };
        let label = if sending {
            "Organizing…".to_string()
        } else {
            format!("Organize {files}")
        };
        let run = if files > 0 && !sending {
            text_button("organize-run", label, ButtonLook::Primary)
                .debug_selector(|| "organize-run".into())
                .on_click(cx.listener(|this, _, window, cx| this.run_organize(window, cx)))
        } else {
            text_button("organize-run", label, ButtonLook::Disabled)
        };
        Some(
            div()
                .debug_selector(|| "organize-page".into())
                .track_focus(&keys)
                .key_context("Rename")
                .on_action(
                    cx.listener(|this, _: &CommitRename, window, cx| this.run_organize(window, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &CancelRename, window, cx| this.end_organize(window, cx)),
                )
                // Not a press on the list, which would start a rectangle.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    theme::pane_bar(theme::TITLE_BAR_HEIGHT)
                        .gap_2()
                        .flex_shrink_0()
                        .border_b_1()
                        .border_color(content(0.06))
                        .child(
                            div()
                                .debug_selector(|| "organize-status".into())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(theme::TEXT_SMALL))
                                .text_color(if bad { danger() } else { content(0.65) })
                                .child(status),
                        )
                        .child(cancel)
                        .child(run),
                )
                .child(
                    div()
                        .id("organize-sections")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_2()
                        .text_size(px(theme::TEXT_BODY))
                        .children(sections),
                )
                .into_any_element(),
        )
    }
}

impl Pane {
    pub(in crate::browser) fn name_field(
        &self,
        renaming: &Renaming,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let covered = self.chrome.covered;
        // Solid over the selection's accent, whatever the pane's glass.
        let mut fill = theme::pane_fill();
        fill.a = 1.;
        Some(
            theme::field(if renaming.error.is_some() {
                danger()
            } else {
                accent()
            })
            .debug_selector(|| "rename-field".into())
            .key_context("Rename")
            .on_action(
                cx.listener(|this, _: &CommitRename, window, cx| this.commit_rename(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &CancelRename, window, cx| this.end_rename(window, cx)),
            )
            // A click in the name places the caret; it isn't a click on
            // the row or the list.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .w(px(width))
            .flex_shrink_0()
            .px(px(6.))
            .bg(fill)
            .text_color(content(1.))
            .child(renaming.field.clone())
            .children(renaming.error.clone().filter(|_| !covered).map(|why| {
                // Over the row below, so no row moves, and kept in the window.
                // It would draw over a sheet or a menu too, so it waits for them.
                div().absolute().left_0().top(gpui::relative(1.)).child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopLeft)
                            .offset(gpui::point(px(0.), px(2.)))
                            .snap_to_window_with_margin(px(8.))
                            .child(
                                div()
                                    .debug_selector(|| "rename-error".into())
                                    .max_w(px(360.))
                                    .px_2()
                                    .rounded_full()
                                    .bg(fill)
                                    .border_1()
                                    .border_color(content(0.12))
                                    .text_size(px(theme::TEXT_SMALL))
                                    .text_color(danger())
                                    .truncate()
                                    .child(why),
                            ),
                    )
                    .with_priority(1),
                )
            })),
        )
    }
    /// The name being edited in place, if it's `path`'s, outlined in red
    /// while the last name tried can't be used. The title bar says why.
    pub(in crate::browser) fn rename_field(
        &self,
        path: &Path,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let renaming = self.renaming.as_ref().filter(|r| r.path == path)?;
        if renaming.in_inspector {
            return None;
        }
        self.name_field(renaming, width, cx)
    }
    /// The name being edited in the inspector, if it's `path`'s.
    pub(in crate::browser) fn inspector_rename_field(
        &self,
        path: &Path,
        width: f32,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let renaming = self
            .renaming
            .as_ref()
            .filter(|r| r.in_inspector && r.path == path)?;
        self.name_field(renaming, width, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub(in crate::browser) fn a_name_typed_during_a_read_outlives_what_the_read_finds() {
        let (a, b) = (PathBuf::from("/a"), PathBuf::from("/b"));
        let mut suggested = HashMap::from([(a.clone(), Ok("typed".to_string()))]);
        merge_suggestions(
            &mut suggested,
            vec![
                (a.clone(), Ok("read".to_string())),
                (b.clone(), Ok("found".to_string())),
            ],
        );
        assert_eq!(suggested[&a], Ok("typed".to_string()));
        assert_eq!(suggested[&b], Ok("found".to_string()));
    }
}
