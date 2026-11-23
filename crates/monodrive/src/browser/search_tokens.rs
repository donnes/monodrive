//! The search field's tokens: each finished filter as a pill before the words
//! typed, with keys and a menu to change it, edit it as text or remove it.
//! The tokens are a `Query` the pane holds, and `pane.query` is them and the
//! words as one text, which is what runs, what's kept and what agents type.

use super::Browser;
use super::pane::Pane;
use super::searches::{
    AcceptCompletion, DismissCompletions, LeaveChips, NextChip, NextCompletion, OpenChip,
    PreviousChip, PreviousCompletion, RemoveChip,
};
use crate::menu;
use crate::theme::{self, accent, content, icon, on_accent};
use gpui::{
    AccessibleAction, Context, Div, Focusable, KeyDownEvent, MouseButton, Role, Stateful, Window,
    div, point, prelude::*, px,
};
use monodrive_core::search::{self, Filter, Kind, Term};

const CHIP_HEIGHT: f32 = 20.;
const MENU_WIDTH: f32 = 240.;

/// What a row of a token's menu does.
enum Choice {
    Set(Term),
    Toggle(Kind),
    EditAsText,
    Remove,
}

struct ChoiceRow {
    title: String,
    /// Whether it's the value now, where the rows are values.
    checked: Option<bool>,
    keys: Option<&'static str>,
    choice: Choice,
}

impl Pane {
    /// The menu of the token the keys are on: its values, then Edit as Text
    /// and Remove Filter.
    fn choices(&self) -> Vec<ChoiceRow> {
        let Some(term) = self.chip.and_then(|ix| self.filters.terms.get(ix)) else {
            return Vec::new();
        };
        let mut rows: Vec<ChoiceRow> = match term {
            Term::Kind(kinds) => Kind::ALL
                .into_iter()
                .map(|kind| ChoiceRow {
                    title: kind.label().into(),
                    checked: Some(kinds.contains(&kind)),
                    keys: None,
                    choice: Choice::Toggle(kind),
                })
                .collect(),
            _ => Filter::of_term(term)
                .map(Filter::options)
                .unwrap_or_default()
                .into_iter()
                .map(|(option, label)| ChoiceRow {
                    title: label.into(),
                    checked: Some(option == *term),
                    keys: None,
                    choice: Choice::Set(option),
                })
                .collect(),
        };
        rows.push(ChoiceRow {
            title: "Edit as Text".into(),
            checked: None,
            keys: None,
            choice: Choice::EditAsText,
        });
        rows.push(ChoiceRow {
            title: "Remove Filter".into(),
            checked: None,
            keys: Some("⌫"),
            choice: Choice::Remove,
        });
        rows
    }

    pub(super) fn select_chip(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(last) = self.filters.terms.len().checked_sub(1) else {
            return;
        };
        let ix = ix.min(last);
        (self.chip, self.chip_menu, self.completing) = (Some(ix), None, None);
        self.chips_scroll.scroll_to_item(ix);
        window.focus(&self.chips_focus, cx);
        cx.notify();
    }

    /// ← or ⌫ at the start of the words: onto the last token, if any.
    pub(super) fn back_into_chips(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.filters.terms.is_empty() || !self.search.read(cx).at_start() {
            return false;
        }
        self.select_chip(usize::MAX, window, cx);
        true
    }

    /// → past the last token goes back to the words.
    pub(super) fn step_chip(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.chip else {
            return;
        };
        if !forward {
            return self.select_chip(ix.saturating_sub(1), window, cx);
        }
        if ix + 1 < self.filters.terms.len() {
            self.select_chip(ix + 1, window, cx);
        } else {
            self.leave_chips(true, window, cx);
        }
    }

    /// Back to typing, at the start of the words or after them.
    pub(super) fn leave_chips(
        &mut self,
        at_start: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        (self.chip, self.chip_menu) = (None, None);
        self.search.update(cx, |field, cx| {
            let text = field.text().to_string();
            let at = if at_start { 0 } else { text.len() };
            field.set_text(&text, at..at, cx);
            window.focus(&field.focus_handle(cx), cx);
        });
        cx.notify();
    }

    /// ⌫ on a token removes it, and the keys go to the one before.
    pub(super) fn remove_chip(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.chip.filter(|ix| *ix < self.filters.terms.len()) else {
            return;
        };
        self.filters.terms.remove(ix);
        if self.filters.terms.is_empty() {
            self.leave_chips(true, window, cx);
        } else {
            self.select_chip(ix.saturating_sub(1), window, cx);
        }
        self.sync_query(cx);
    }

    pub(super) fn open_chip(&mut self, cx: &mut Context<Self>) {
        let rows = self.choices();
        let at = rows.iter().position(|row| row.checked == Some(true));
        self.chip_menu = Some(at.unwrap_or(0));
        cx.notify();
    }

    pub(super) fn step_chip_menu(&mut self, down: bool, cx: &mut Context<Self>) {
        let len = self.choices().len();
        if let Some(active) = &mut self.chip_menu {
            *active = if down {
                (*active + 1) % len
            } else {
                (*active + len - 1) % len
            };
            cx.notify();
        }
    }

    pub(super) fn close_chip_menu(&mut self, cx: &mut Context<Self>) {
        self.chip_menu = None;
        cx.notify();
    }

    /// Row `ix` of the token's menu, or the one under the keys. A kind turns
    /// on or off and the menu stays, for the next.
    pub(super) fn pick_choice(
        &mut self,
        ix: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let row = ix
            .or(self.chip_menu)
            .and_then(|ix| self.choices().into_iter().nth(ix));
        let (Some(chip), Some(row)) = (self.chip, row) else {
            return;
        };
        match row.choice {
            Choice::Toggle(kind) => {
                let Some(Term::Kind(kinds)) = self.filters.terms.get_mut(chip) else {
                    return;
                };
                let toggled = search::toggle_kind(kinds, kind);
                if toggled.is_empty() {
                    return self.remove_chip(window, cx);
                }
                *kinds = toggled;
            }
            Choice::Set(term) => {
                if let Some(slot) = self.filters.terms.get_mut(chip) {
                    *slot = term;
                }
                self.chip_menu = None;
            }
            Choice::EditAsText => return self.edit_chip(chip, window, cx),
            Choice::Remove => return self.remove_chip(window, cx),
        }
        self.sync_query(cx);
        cx.notify();
    }

    /// Edit as Text: the token goes back to typed text, ahead of the words,
    /// with the caret at its end.
    fn edit_chip(&mut self, chip: usize, window: &mut Window, cx: &mut Context<Self>) {
        if chip >= self.filters.terms.len() {
            return;
        }
        let token = self.filters.terms.remove(chip).to_string();
        (self.chip, self.chip_menu) = (None, None);
        self.search.update(cx, |field, cx| {
            let words = field.text().trim_start().to_string();
            let text = if words.is_empty() {
                token.clone()
            } else {
                format!("{token} {words}")
            };
            field.set_text(&text, token.len()..token.len(), cx);
            window.focus(&field.focus_handle(cx), cx);
        });
    }
}

impl Browser {
    /// The tokens before the words. The keys reach them with ← or ⌫ at the
    /// start of the words, or a press. With none it takes no room, but stays
    /// drawn, so keys still reach the window when the last one goes while
    /// they're on it, as when Back leaves the search, and only a token the
    /// keys are on takes them.
    pub(super) fn search_chips(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let pane = self.pane().read(cx);
        let empty = pane.filters.terms.is_empty();
        let on_chips = pane.chips_focus.is_focused(window);
        let selected = pane.chip.filter(|_| on_chips);
        let mut menu = selected
            .and(pane.chip_menu)
            .map(|active| (pane.choices(), active));
        let menu_open = menu.is_some();
        let chips: Vec<(String, String)> = pane
            .filters
            .terms
            .iter()
            .map(|term| (term.label(), term.to_string()))
            .collect();
        let (focus, scroll) = (pane.chips_focus.clone(), pane.chips_scroll.clone());
        let strip = div()
            .id("search-chips")
            .debug_selector(|| "search-chips".into())
            .track_focus(&focus)
            .track_scroll(&scroll)
            .overflow_x_scroll()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1()
            .on_key_down(cx.listener(Self::type_past_chips))
            .children(chips.into_iter().enumerate().map(|(ix, (label, token))| {
                let on = selected == Some(ix);
                let menu = if on { menu.take() } else { None };
                self.chip(ix, label, token, on, menu, cx)
            }));
        let strip = div().min_w_0().flex().child(strip);
        let strip = if menu_open {
            strip
                .key_context("ChipMenu")
                .on_action(cx.listener(|this, _: &PreviousCompletion, _, cx| {
                    this.pane()
                        .update(cx, |pane, cx| pane.step_chip_menu(false, cx))
                }))
                .on_action(cx.listener(|this, _: &NextCompletion, _, cx| {
                    this.pane()
                        .update(cx, |pane, cx| pane.step_chip_menu(true, cx))
                }))
                .on_action(cx.listener(|this, _: &AcceptCompletion, window, cx| {
                    this.pane()
                        .update(cx, |pane, cx| pane.pick_choice(None, window, cx))
                }))
                .on_action(cx.listener(|this, _: &DismissCompletions, _, cx| {
                    this.pane().update(cx, |pane, cx| pane.close_chip_menu(cx))
                }))
        } else {
            strip
        };
        div()
            .when(selected.is_some(), |d| d.key_context("Chips"))
            .when(empty, |d| d.absolute())
            .min_w_0()
            .flex()
            .on_action(cx.listener(|this, _: &PreviousChip, window, cx| {
                this.pane()
                    .update(cx, |pane, cx| pane.step_chip(false, window, cx))
            }))
            .on_action(cx.listener(|this, _: &NextChip, window, cx| {
                this.pane()
                    .update(cx, |pane, cx| pane.step_chip(true, window, cx))
            }))
            .on_action(cx.listener(|this, _: &RemoveChip, window, cx| {
                this.pane()
                    .update(cx, |pane, cx| pane.remove_chip(window, cx))
            }))
            .on_action(cx.listener(|this, _: &OpenChip, _, cx| {
                this.pane().update(cx, |pane, cx| pane.open_chip(cx))
            }))
            .on_action(cx.listener(|this, _: &LeaveChips, window, cx| {
                this.pane()
                    .update(cx, |pane, cx| pane.leave_chips(false, window, cx))
            }))
            .child(strip)
    }

    /// Typing on a token types after the words.
    fn type_past_chips(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keys = &event.keystroke;
        if keys.modifiers.platform || keys.modifiers.control || keys.modifiers.function {
            return;
        }
        let Some(text) = keys.key_char.clone().filter(|text| !text.trim().is_empty()) else {
            return;
        };
        self.pane().update(cx, |pane, cx| {
            pane.leave_chips(false, window, cx);
            pane.search.update(cx, |field, cx| field.insert(&text, cx));
        });
        cx.stop_propagation();
    }

    fn chip(
        &self,
        ix: usize,
        label: String,
        token: String,
        on: bool,
        menu: Option<(Vec<ChoiceRow>, usize)>,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let pane = self.pane().clone();
        div()
            .id(("search-chip", ix))
            .debug_selector(move || format!("search-chip-{ix}"))
            .role(Role::Button)
            .aria_label(token)
            .aria_selected(on)
            .relative()
            .flex_shrink_0()
            .h(px(CHIP_HEIGHT))
            .px_2()
            .rounded_full()
            .flex()
            .items_center()
            .whitespace_nowrap()
            .text_size(px(theme::TEXT_SMALL))
            .bg(if on { accent() } else { content(0.14) })
            .text_color(if on { on_accent() } else { content(1.) })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.pane().update(cx, |pane, cx| {
                        pane.select_chip(ix, window, cx);
                        pane.open_chip(cx);
                    });
                }),
            )
            .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                pane.update(cx, |pane, cx| {
                    pane.select_chip(ix, window, cx);
                    pane.open_chip(cx);
                })
            })
            .child(label)
            .when_some(menu, |d, (rows, active)| {
                d.child(self.chip_menu(rows, active, cx))
            })
    }

    /// A token's menu, under it.
    fn chip_menu(&self, rows: Vec<ChoiceRow>, active: usize, cx: &mut Context<Self>) -> Div {
        let checks = rows.iter().any(|row| row.checked.is_some());
        let items = rows.into_iter().enumerate().map(|(ix, row)| {
            let on = ix == active;
            let pane = self.pane().clone();
            theme::row_slot(("search-chip-choice", ix))
                .debug_selector(move || format!("search-chip-choice-{ix}"))
                .role(Role::MenuItem)
                .aria_label(row.title.clone())
                .aria_selected(on)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.pane()
                            .update(cx, |pane, cx| pane.pick_choice(Some(ix), window, cx));
                    }),
                )
                .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                    pane.update(cx, |pane, cx| pane.pick_choice(Some(ix), window, cx))
                })
                .child(
                    theme::row(on)
                        .when(checks, |d| {
                            d.child(theme::lead((row.checked == Some(true)).then(|| {
                                icon("check-hi", 12.).text_color(theme::row_muted(on))
                            })))
                        })
                        .child(div().flex_1().min_w_0().truncate().child(row.title))
                        .children(
                            row.keys
                                .map(|keys| menu::shortcut(keys.into(), theme::row_muted(on))),
                        ),
                )
        });
        div().absolute().left_0().top(gpui::relative(1.)).child(
            gpui::deferred(
                gpui::anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .offset(point(px(0.), px(8.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        theme::floating(theme::Elevation::Popover)
                            .id("search-chip-menu")
                            .debug_selector(|| "search-chip-menu".into())
                            .role(Role::Menu)
                            .aria_label("Filter")
                            .w(px(MENU_WIDTH))
                            .child(
                                theme::inset_rows(div())
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .children(items),
                            ),
                    ),
            )
            .with_priority(1),
        )
    }
}
