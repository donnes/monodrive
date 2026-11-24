//! The search field, in the sidebar or the title bar: its tokens, their
//! menus, and the completions under it.

use super::pane::searches::{
    self, AcceptCompletion, COMPLETIONS_WIDTH, DismissCompletions, LeaveChips, NextChip,
    NextCompletion, OpenChip, PreviousChip, PreviousCompletion, RemoveChip,
};
use super::pane::searches::{Chips, ChoiceRow, SearchBar};
use super::{Browser, FocusSearch};
use crate::commands::{self};
use crate::menu;
use crate::text_field::{self};
use crate::theme::{self, accent, content, icon, on_accent, row_icon_button};
use gpui::prelude::*;
use gpui::{
    AccessibleAction, Action, Context, Div, KeyDownEvent, MouseButton, Role, Stateful, Window, div,
    point, px,
};
use monodrive_core::search::Completion;

/// The least the words keep beside the search field's tokens.
const TYPING_ROOM: f32 = 56.;
const CHIP_HEIGHT: f32 = 20.;
const MENU_WIDTH: f32 = 240.;

impl Browser {
    /// The search field of the pane commands act on.
    pub(super) fn search_field(&self, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let SearchBar {
            field: search,
            contents,
            plain,
            typed,
            focused,
            completing,
        } = self.pane().read(cx).search_bar(window, cx);
        theme::field(if focused { accent() } else { content(0.12) })
            .debug_selector(|| "search-field".into())
            .key_context("Search")
            .relative()
            .flex_shrink_0()
            .h(px(theme::FIELD_HEIGHT))
            .pl_1()
            .pr_1()
            .gap_1()
            .child(searches::track_anchor(self.search_anchor.clone()))
            .child(
                theme::tip_below(
                    row_icon_button(
                        "search-menu-button",
                        "Searches",
                        if contents { "file" } else { "search" },
                        content(if plain { 0.55 } else { 1. }),
                    ),
                    "Searches",
                )
                .debug_selector(|| "search-menu-button".into())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_searches(window, cx))),
            )
            .child(self.search_chips(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(TYPING_ROOM))
                    // ← or ⌫ at the start of the words steps onto the tokens.
                    .capture_action(cx.listener(|this, _: &text_field::Left, window, cx| {
                        if this
                            .pane()
                            .update(cx, |pane, cx| pane.back_into_chips(window, cx))
                        {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|this, _: &text_field::Backspace, window, cx| {
                        if this
                            .pane()
                            .update(cx, |pane, cx| pane.back_into_chips(window, cx))
                        {
                            cx.stop_propagation();
                        }
                    }))
                    .when(completing.is_some(), |d| {
                        d.key_context("Completing")
                            .on_action(cx.listener(|this, _: &PreviousCompletion, _, cx| {
                                this.pane()
                                    .update(cx, |pane, cx| pane.step_completion(false, cx))
                            }))
                            .on_action(cx.listener(|this, _: &NextCompletion, _, cx| {
                                this.pane()
                                    .update(cx, |pane, cx| pane.step_completion(true, cx))
                            }))
                            .on_action(cx.listener(|this, _: &AcceptCompletion, window, cx| {
                                this.pane()
                                    .update(cx, |pane, cx| pane.accept_completion(None, window, cx))
                            }))
                            .on_action(cx.listener(|this, _: &DismissCompletions, _, cx| {
                                this.pane()
                                    .update(cx, |pane, cx| pane.dismiss_completions(cx))
                            }))
                    })
                    .child(search),
            )
            .when_some(completing, |d, (items, active)| {
                d.child(self.completions(items, active, cx))
            })
            .when(typed, |d| {
                d.child(
                    theme::tip_below(
                        row_icon_button("clear-search", "Clear Search", "cancel-01", content(0.55)),
                        "Clear Search",
                    )
                    .debug_selector(|| "clear-search".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pane()
                            .update(cx, |pane, cx| pane.clear_search(window, cx));
                    })),
                )
            })
            .when_some(
                (!focused)
                    .then(|| commands::shortcut_label(FocusSearch.name(), cx))
                    .flatten(),
                |d, keys| {
                    d.child(
                        div()
                            .text_size(px(theme::TEXT_CAPTION))
                            .text_color(content(0.65))
                            .pr_1()
                            .child(keys),
                    )
                },
            )
    }

    /// The values that could finish the filter being typed, under the field.
    /// A press takes one at once, before the field could lose the keys.
    fn completions(
        &self,
        items: Vec<Completion>,
        active: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows = items.into_iter().enumerate().map(|(ix, item)| {
            let on = ix == active;
            let pane = self.pane().clone();
            theme::row_slot(("search-completion", ix))
                .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                    pane.update(cx, |pane, cx| pane.accept_completion(Some(ix), window, cx))
                })
                .debug_selector(move || format!("search-completion-{ix}"))
                .role(Role::MenuItem)
                .aria_label(item.text.clone())
                .aria_selected(on)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.pane()
                            .update(cx, |pane, cx| pane.accept_completion(Some(ix), window, cx));
                    }),
                )
                .child(
                    theme::row(on)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme::MONO_FONT)
                                .text_size(px(theme::TEXT_SMALL))
                                .child(item.text),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(theme::TEXT_CAPTION))
                                .text_color(theme::row_muted(on))
                                .child(item.label),
                        ),
                )
        });
        div().absolute().left_0().top(gpui::relative(1.)).child(
            gpui::deferred(
                gpui::anchored()
                    .anchor(gpui::Anchor::TopLeft)
                    .offset(gpui::point(px(-1.), px(4.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        theme::floating(theme::Elevation::Popover)
                            .id("search-completions")
                            .debug_selector(|| "search-completions".into())
                            .role(Role::Menu)
                            .aria_label("Completions")
                            .w(px(COMPLETIONS_WIDTH))
                            .child(
                                theme::inset_rows(div())
                                    .relative()
                                    .flex()
                                    .flex_col()
                                    .children(rows),
                            ),
                    ),
            )
            .with_priority(1),
        )
    }

    /// The tokens before the words. The keys reach them with ← or ⌫ at the
    /// start of the words, or a press. With none it takes no room, but stays
    /// drawn, so keys still reach the window when the last one goes while
    /// they're on it, as when Back leaves the search, and only a token the
    /// keys are on takes them.
    pub(super) fn search_chips(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let Chips {
            labels: chips,
            selected,
            mut menu,
            focus,
            scroll,
        } = self.pane().read(cx).chips(window);
        let empty = chips.is_empty();
        let menu_open = menu.is_some();
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
        self.pane()
            .update(cx, |pane, cx| pane.type_into_search(&text, window, cx));
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
