//! Go to Folder (⇧⌘G): type or paste a path. A folder opens; a file opens
//! its folder with it selected, so a path from an agent's output lands on
//! the right file. Completions come from the disk as you type; Tab takes one.

use crate::palette;
use crate::preferences::Preferences;
use crate::text_field::{self, TextField};
use crate::theme::{self, content, danger};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyBinding, MouseButton, Role,
    Task, Window, actions, div, prelude::*, px,
};
use monodrive_core::paths::{self, Destination};
use std::path::PathBuf;

actions!(goto, [Up, Down, Complete, Go, Dismiss]);

pub fn bind(cx: &mut App) {
    let context = Some("GoTo");
    cx.bind_keys([
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("tab", Complete, context),
        KeyBinding::new("enter", Go, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

pub enum GotoEvent {
    Dismissed,
    Go(Destination),
}

const LIMIT: usize = 8;

pub struct GotoView {
    field: Entity<TextField>,
    /// What the field started with, selected so typing replaces it.
    prefill: String,
    /// The folder on screen, which a relative path starts from.
    base: PathBuf,
    completions: Vec<PathBuf>,
    /// The highlighted completion; `None` means what was typed.
    selected: Option<usize>,
    error: Option<String>,
    /// The completion listing or path check in flight.
    work: Task<()>,
}

impl EventEmitter<GotoEvent> for GotoView {}

impl Focusable for GotoView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.field.focus_handle(cx)
    }
}

impl GotoView {
    /// Starts at `folder`, so typing a name completes inside it.
    pub fn new(folder: PathBuf, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| TextField::new("Path", "Type or paste a path", cx));
        let mut start = paths::abbreviate(&folder);
        if !start.ends_with('/') {
            start.push('/');
        }
        field.update(cx, |field, cx| field.set_text(&start, 0..start.len(), cx));
        cx.subscribe(&field, |this, _, _: &text_field::Changed, cx| {
            this.start_over(cx);
            this.complete(cx)
        })
        .detach();
        let mut view = Self {
            field,
            prefill: start,
            base: folder,
            completions: Vec::new(),
            selected: None,
            error: None,
            work: Task::ready(()),
        };
        view.complete(cx);
        view
    }

    /// A path of its own typed after what the field started with, with the
    /// selection let go of, takes over from it: `~/here//System` is `/System`.
    fn start_over(&mut self, cx: &mut Context<Self>) {
        let typed = self.field.read(cx).text();
        let Some(rest) = typed
            .strip_prefix(self.prefill.as_str())
            .filter(|rest| rest.starts_with('/') || rest.starts_with("~/"))
            .map(str::to_owned)
        else {
            return;
        };
        let end = rest.len();
        self.field
            .update(cx, |field, cx| field.set_text(&rest, end..end, cx));
    }

    /// Lists what the typed path could complete to, off the UI thread.
    fn complete(&mut self, cx: &mut Context<Self>) {
        let typed = self.field.read(cx).text().to_owned();
        let show_hidden = cx.global::<Preferences>().show_hidden;
        self.error = None;
        // What was offered for the old text can't be taken for the new.
        self.completions.clear();
        self.selected = None;
        cx.notify();
        let found = cx
            .background_executor()
            .spawn(async move { paths::completions(&typed, show_hidden, LIMIT) });
        self.work = cx.spawn(async move |this, cx| {
            let found = found.await;
            let _ = this.update(cx, |this, cx| {
                this.completions = found;
                this.selected = None;
                cx.notify();
            });
        });
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(last) = self.completions.len().checked_sub(1) else {
            return;
        };
        self.selected = match (self.selected, down) {
            (None, true) => Some(0),
            (Some(ix), true) => Some((ix + 1).min(last)),
            (Some(0), false) | (None, false) => None,
            (Some(ix), false) => Some(ix - 1),
        };
        cx.notify();
    }

    /// Puts the highlighted completion, or the only one, into the field.
    fn take_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = match self.selected {
            Some(ix) => self.completions.get(ix),
            None if self.completions.len() == 1 => self.completions.first(),
            None => None,
        };
        if let Some(path) = pick.map(|path| paths::abbreviate(path)) {
            self.field.update(cx, |field, cx| {
                field.select_all(&text_field::SelectAll, window, cx);
                field.insert(&path, cx);
            });
        }
    }

    /// Goes to the highlighted completion, or else what was typed, once the
    /// disk says it exists.
    fn go(&mut self, cx: &mut Context<Self>) {
        let target = match self.selected.and_then(|ix| self.completions.get(ix)) {
            Some(path) => path.display().to_string(),
            None => self.field.read(cx).text().to_owned(),
        };
        let base = self.base.clone();
        let resolved = cx
            .background_executor()
            .spawn(async move { paths::destination(&target, &base) });
        self.work = cx.spawn(async move |this, cx| {
            let resolved = resolved.await;
            let _ = this.update(cx, |this, cx| match resolved {
                Ok(destination) => cx.emit(GotoEvent::Go(destination)),
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        });
    }
}

impl Render for GotoView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.completions.iter().enumerate().map(|(ix, path)| {
            let selected = self.selected == Some(ix);
            theme::row_slot(ix)
                .role(Role::ListBoxOption)
                .aria_selected(selected)
                .child(theme::row(selected).child(div().truncate().child(paths::abbreviate(path))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = Some(ix);
                    this.go(cx);
                }))
        });
        let body = div()
            .debug_selector(|| "goto-body".into())
            .flex()
            .flex_col()
            .children(self.error.clone().map(|error| {
                // A long name wraps inside the card.
                div()
                    .debug_selector(|| "goto-error".into())
                    .min_h(px(theme::ROW_HEIGHT))
                    .w_full()
                    .px_2()
                    .py(px(6.))
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(danger())
                    .child(error)
            }))
            .children(rows)
            .when(self.completions.is_empty() && self.error.is_none(), |d| {
                d.child(
                    div()
                        .h(px(theme::ROW_HEIGHT))
                        .px_2()
                        .flex()
                        .items_center()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child("Return opens it. A file opens its folder with it selected."),
                )
            });
        palette::overlay("goto-overlay")
            .key_context("GoTo")
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(true, cx)))
            .on_action(
                cx.listener(|this, _: &Complete, window, cx| this.take_completion(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Go, _, cx| this.go(cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(GotoEvent::Dismissed)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(GotoEvent::Dismissed)),
            )
            .child(palette::card(
                "goto",
                "Go to Folder",
                self.field.clone(),
                body,
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands;
    use gpui::{TestAppContext, VisualTestContext};

    fn open<'a>(
        folder: &std::path::Path,
        cx: &'a mut TestAppContext,
    ) -> (Entity<GotoView>, &'a mut VisualTestContext) {
        cx.update(|cx| {
            cx.set_global(Preferences::default());
            commands::apply(cx);
        });
        let folder = folder.to_path_buf();
        let (view, cx) = cx.add_window_view(|_, cx| GotoView::new(folder, cx));
        cx.update(|window, cx| {
            window.activate_window();
            let field = view.read(cx).focus_handle(cx);
            window.focus(&field, cx);
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn typed(view: &Entity<GotoView>, cx: &mut VisualTestContext) -> String {
        view.read_with(cx, |view, cx| view.field.read(cx).text().to_owned())
    }

    /// Lets go of what's selected, with the caret at the end.
    fn to_the_end(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("right");
        cx.run_until_parked();
    }

    #[gpui::test]
    fn typing_replaces_what_it_started_with(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = open(dir.path(), cx);
        assert!(typed(&view, cx).starts_with('/') && typed(&view, cx).ends_with('/'));
        cx.simulate_input("n");
        assert_eq!(typed(&view, cx), "n");
    }

    #[gpui::test]
    fn an_absolute_path_after_what_it_started_with_takes_over(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = open(dir.path(), cx);
        to_the_end(cx);
        cx.simulate_input("/");
        assert_eq!(typed(&view, cx), "/");
        cx.simulate_input("System");
        assert_eq!(typed(&view, cx), "/System");
    }

    #[gpui::test]
    fn the_home_folder_after_what_it_started_with_takes_over(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = open(dir.path(), cx);
        to_the_end(cx);
        cx.simulate_input("~/Library");
        assert_eq!(typed(&view, cx), "~/Library");
    }

    #[gpui::test]
    fn a_name_starting_with_a_tilde_adds_to_what_it_started_with(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = open(dir.path(), cx);
        let start = typed(&view, cx);
        to_the_end(cx);
        cx.simulate_input("~backup");
        assert_eq!(typed(&view, cx), format!("{start}~backup"));
    }

    #[gpui::test]
    fn a_long_error_names_the_item_and_wraps_inside_the_card(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (view, cx) = open(dir.path(), cx);
        let name = "b ".repeat(200);
        let name = name.trim();
        cx.simulate_input(&format!("/nowhere/at/all/{name}"));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let said = view.read_with(cx, |view, _| view.error.clone()).unwrap();
        assert_eq!(said, format!("\"{name}\" doesn't exist."));
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let error = cx.debug_bounds("goto-error").unwrap();
        let card = cx.debug_bounds("goto-body").unwrap();
        assert!(error.size.height.as_f32() > theme::ROW_HEIGHT, "{error:?}");
        assert!(error.right() <= card.right(), "{error:?} in {card:?}");
    }
}
