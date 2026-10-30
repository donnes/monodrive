//! The command palette (⇧⌘P): every command in the registry, found by typing
//! a few letters of it, with its shortcut beside it.

use crate::commands::{self, Command};
use crate::preferences::Preferences;
use crate::text_field::{self, TextField};
use crate::theme::{self, Elevation, content};
use gpui::{
    Action, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding,
    MouseButton, Role, ScrollStrategy, UniformListScrollHandle, Window, actions, div, prelude::*,
    px, uniform_list,
};

actions!(palette, [Up, Down, Run, Dismiss]);

pub fn bind(cx: &mut App) {
    let context = Some("Palette");
    cx.bind_keys([
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("enter", Run, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

pub enum PaletteEvent {
    Dismissed,
    /// Run this command once the palette has closed and focus is back.
    Run(Box<dyn Action>),
}

/// A row and the gap under it, which `uniform_list` measures as one.
const ROW: f32 = theme::ROW_HEIGHT + theme::ROW_GAP;

pub struct PaletteView {
    query: Entity<TextField>,
    /// Indices into `commands::all()` that match, best first.
    matches: Vec<usize>,
    selected: usize,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<PaletteEvent> for PaletteView {}

impl Focusable for PaletteView {
    /// The query, so typing filters at once.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.focus_handle(cx)
    }
}

impl PaletteView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextField::new("Command", "Type a command", cx));
        cx.subscribe(&query, |this, _, _: &text_field::Changed, cx| {
            this.filter(cx)
        })
        .detach();
        let mut view = Self {
            query,
            matches: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
        };
        view.filter(cx);
        view
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).text().to_owned();
        let palette_id = commands::OpenPalette.name();
        let mut scored: Vec<(i32, usize)> = commands::all()
            .iter()
            .enumerate()
            .filter(|(_, command)| command.id() != palette_id)
            .filter_map(|(ix, command)| Some((score(&query, &haystack(command))?, ix)))
            .collect();
        // Best first; ties keep the table's order, which follows the menus.
        scored.sort_by_key(|(score, ix)| (-score, *ix));
        self.matches = scored.into_iter().map(|(_, ix)| ix).collect();
        self.selected = 0;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(last) = self.matches.len().checked_sub(1) else {
            return;
        };
        self.selected = if down {
            (self.selected + 1).min(last)
        } else {
            self.selected.saturating_sub(1)
        };
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn run(&mut self, ix: usize, cx: &mut Context<Self>) {
        let all = commands::all();
        if let Some(command) = self.matches.get(ix).and_then(|&ix| all.get(ix)) {
            cx.emit(PaletteEvent::Run((command.action)()));
        }
    }

    fn rows(
        &mut self,
        range: std::ops::Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let all = commands::all();
        let overrides = cx.global::<Preferences>().shortcuts.clone();
        range
            .filter_map(|row| {
                let command = all.get(*self.matches.get(row)?)?;
                let selected = row == self.selected;
                let keys = command
                    .shortcuts(&overrides)
                    .first()
                    .map(|keys| commands::label(keys));
                Some(
                    div()
                        .pb(px(theme::ROW_GAP))
                        .child(
                            theme::row(selected)
                                .id(row)
                                .role(Role::ListBoxOption)
                                .aria_label(command.title)
                                .aria_selected(selected)
                                .child(
                                    div()
                                        .text_color(theme::row_muted(selected))
                                        .child(command.group.label()),
                                )
                                .child(div().flex_1().truncate().child(command.title))
                                .children(keys.map(|keys| {
                                    div()
                                        .text_size(px(theme::TEXT_SMALL))
                                        .text_color(theme::row_muted(selected))
                                        .child(keys)
                                }))
                                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                    if this.selected != row {
                                        this.selected = row;
                                        cx.notify();
                                    }
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| this.run(row, cx))),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for PaletteView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = if self.matches.is_empty() {
            div()
                .h(px(ROW * 2.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_BODY))
                .text_color(content(0.65))
                .child("No matching command")
                .into_any_element()
        } else {
            uniform_list("commands", self.matches.len(), cx.processor(Self::rows))
                .track_scroll(&self.scroll)
                .h(px(ROW * (self.matches.len().min(10) as f32)))
                .into_any_element()
        };
        overlay("palette-overlay")
            .key_context("Palette")
            .on_action(cx.listener(|this, _: &Up, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &Run, _, cx| this.run(this.selected, cx)))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(PaletteEvent::Dismissed)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismissed)),
            )
            .child(card("palette", "Command palette", self.query.clone(), list))
    }
}

/// The full-window layer behind a quick sheet (the palette, Go to Folder):
/// a light scrim, with the card near the top, where the eye already is.
pub fn overlay(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .flex_col()
        .items_center()
        .pt(gpui::relative(0.16))
        .px_6()
        .bg(theme::scrim(1.))
}

/// A quick sheet's card: its text field, or a title, on top, `body` below.
pub fn card(
    id: &'static str,
    label: &'static str,
    header: impl IntoElement,
    body: impl IntoElement,
) -> gpui::Stateful<gpui::Div> {
    theme::floating(Elevation::Sheet)
        .id(id)
        .role(Role::Dialog)
        .aria_label(label)
        .w(px(560.))
        .max_w_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            div()
                .relative()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(theme::TITLE_BAR_HEIGHT))
                        .px_4()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(content(0.08))
                        .text_size(px(theme::TEXT_TITLE))
                        .font_weight(FontWeight::MEDIUM)
                        .child(header),
                )
                .child(div().p_2().child(body)),
        )
}

/// What a command is found by: "Go: Back".
fn haystack(command: &Command) -> String {
    format!("{}: {}", command.group.label(), command.title)
}

/// How well `query` matches `text` as a subsequence, ignoring case; `None` if
/// it doesn't. Letters at the start of a word and runs of letters count more,
/// so "sh" finds "Show Hidden Files" before "Search: Home".
fn score(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut at = 0;
    let mut score = 0;
    let mut previous: Option<usize> = None;
    for wanted in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (at..text.len()).find(|&i| text[i] == wanted)?;
        let word_start = found == 0 || !text[found - 1].is_alphanumeric();
        score += 1
            + if word_start { 6 } else { 0 }
            + if previous == Some(found.wrapping_sub(1)) {
                4
            } else {
                0
            };
        previous = Some(found);
        at = found + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_in_order_match_and_word_starts_rank_first() {
        assert_eq!(score("", "Go: Back"), Some(0));
        assert!(score("gb", "Go: Back").is_some());
        assert!(score("bg", "Go: Back").is_none());
        assert!(score("BACK", "Go: Back").is_some());
        // Word starts beat letters buried inside words.
        let show_hidden = score("sh", "View: Show Hidden Files").unwrap();
        let mashed = score("sh", "Go: Macintosh HD").unwrap();
        assert!(show_hidden > mashed);
        // A run of letters beats the same letters scattered.
        assert!(score("zoom", "View: Zoom In") > score("zoom", "Go: Zap photo mix"));
    }
}
