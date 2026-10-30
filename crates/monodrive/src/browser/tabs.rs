//! The tabs of a window and the top bar that shows them. A tab holds its
//! panes; only the active tab's are drawn, listed or told about the disk.

use super::pane::{Opening, Pane, Stop};
use super::{Browser, drag_area};
use crate::theme::{self, content, icon_button, row_icon_button};
use gpui::{
    App, Context, Entity, FontWeight, MouseButton, Role, Stateful, Window, div, prelude::*, px,
};
use std::path::PathBuf;

/// The widest and narrowest a tab gets. Past that they scroll.
const TAB_WIDEST: f32 = 180.;
const TAB_NARROWEST: f32 = 96.;
/// What the top bar always leaves for dragging the window.
const DRAG_ROOM: f32 = 24.;

pub(super) struct Tab {
    pub(super) pane: Entity<Pane>,
}

impl Tab {
    /// The pane commands act on.
    pub(super) fn focused(&self) -> &Entity<Pane> {
        &self.pane
    }

    pub(super) fn panes(&self) -> impl Iterator<Item = &Entity<Pane>> {
        std::iter::once(&self.pane)
    }
}

/// A tab being dragged to a new place in the strip.
#[derive(Clone)]
struct DraggedTab {
    ix: usize,
    label: String,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(theme::BUTTON_HEIGHT))
            .px_3()
            .rounded_full()
            .flex()
            .items_center()
            .font_family(theme::UI_FONT)
            .text_size(px(theme::TEXT_BODY))
            .text_color(content(1.))
            .bg(content(0.14))
            .child(self.label.clone())
    }
}

impl Browser {
    /// A new pane in this window, not yet listed.
    pub(super) fn add_pane(&mut self, opening: Opening, cx: &mut Context<Self>) -> Entity<Pane> {
        let index = self.index.clone();
        let pane = cx.new(|cx| Pane::new(opening, index, cx));
        cx.subscribe(&pane, Self::pane_event).detach();
        cx.observe(&pane, |_, _, cx| cx.notify()).detach();
        pane
    }

    /// Opens a tab on `stop` right after this one, with this pane's view and
    /// sort, and shows it.
    pub(super) fn new_tab(&mut self, stop: Stop, window: &mut Window, cx: &mut Context<Self>) {
        let (view, sort) = {
            let pane = self.pane().read(cx);
            (pane.view, pane.sort)
        };
        let pane = self.add_pane(Opening::at(stop, view, sort), cx);
        let at = self.active + 1;
        self.tabs.insert(at, Tab { pane });
        self.show_tab(at, window, cx);
    }

    /// Open in New Tab, on what may be a link, which reads the disk to tell
    /// where it leads.
    pub(super) fn open_in_new_tab(
        &mut self,
        path: PathBuf,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let target = path.clone();
        let is_dir = cx
            .background_executor()
            .spawn(async move { target.is_dir() });
        cx.spawn_in(window, async move |this, cx| {
            if is_dir.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.new_tab(Stop::folder(path), window, cx)
                });
            }
        })
        .detach();
    }

    /// Makes tab `at` the one on screen: its panes list again, and the one
    /// it left stops following the disk.
    pub(super) fn show_tab(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let left = (at != self.active)
            .then(|| self.tabs.get(self.active).map(|tab| tab.pane.clone()))
            .flatten();
        self.active = at;
        if let Some(left) = left {
            left.update(cx, |pane, _| pane.set_shown(false));
        }
        let panes: Vec<_> = self.tabs[at].panes().cloned().collect();
        for pane in panes {
            pane.update(cx, |pane, cx| {
                pane.set_shown(true);
                pane.list(true, cx);
            });
        }
        self.tab_scroll.scroll_to_item(at);
        self.pane()
            .update(cx, |pane, cx| pane.focus_list(window, cx));
        self.mirror_pane(cx);
        cx.notify();
    }

    /// ⌃Tab and ⌃⇧Tab: the next or previous tab, coming round from the ends.
    pub(super) fn step_tab(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        if count > 1 {
            let at = (self.active as isize + step).rem_euclid(count) as usize;
            self.show_tab(at, window, cx);
        }
    }

    /// Closes tab `at`; the last one closes the window. Jobs it started go on:
    /// they're the owner's, and the window follows them.
    pub(super) fn close_tab(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let panes: Vec<_> = self.tabs[at].panes().cloned().collect();
        for pane in panes {
            pane.update(cx, |pane, cx| pane.commit_rename(window, cx));
        }
        if self.tabs.len() == 1 {
            return window.remove_window();
        }
        self.tabs.remove(at);
        self.hovered_tab = None;
        // What's on screen stays, unless it's what closed.
        if at == self.active {
            self.active = at.min(self.tabs.len() - 1);
            let shown = self.active;
            // Nothing's left to hide, so this only shows it.
            self.show_tab(shown, window, cx);
        } else if at < self.active {
            self.active -= 1;
        }
        cx.notify();
    }

    /// Drops tab `from` where tab `to` is.
    pub(super) fn move_tab(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return;
        }
        let shown = self.tabs[self.active].pane.clone();
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self
            .tabs
            .iter()
            .position(|tab| tab.pane == shown)
            .unwrap_or(0);
        cx.notify();
    }

    /// A tab in the strip. Alone it's the bare folder name, in the same place.
    fn tab_pill(&self, ix: usize, title: String, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let active = ix == self.active;
        let hovered = self.hovered_tab == Some(ix);
        let alone = self.tabs.len() == 1;
        div()
            .id(("tab", ix))
            .debug_selector(move || format!("tab-{ix}"))
            .role(Role::Tab)
            .aria_label(title.clone())
            .aria_selected(active)
            .relative()
            .h(px(theme::BUTTON_HEIGHT))
            .px_3()
            .rounded_full()
            .flex()
            .items_center()
            .text_size(px(theme::TEXT_BODY))
            .when(active, |d| d.font_weight(FontWeight::SEMIBOLD))
            .map(|d| {
                if alone {
                    d.min_w_0()
                } else {
                    d.w(px(TAB_WIDEST))
                        .min_w(px(TAB_NARROWEST))
                        .flex_shrink(1.)
                        .cursor_pointer()
                        .map(|d| {
                            if active {
                                d.bg(content(0.14))
                            } else {
                                d.text_color(content(0.65))
                                    .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                            }
                        })
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.hovered_tab = Some(ix);
                            } else if this.hovered_tab == Some(ix) {
                                this.hovered_tab = None;
                            }
                            cx.notify();
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                if ix != this.active {
                                    this.show_tab(ix, window, cx);
                                }
                            }),
                        )
                        .on_drag(
                            DraggedTab {
                                ix,
                                label: title.clone(),
                            },
                            |dragged, _, _, cx| cx.new(|_| dragged.clone()),
                        )
                        .drag_over::<DraggedTab>(|s, _, _, _| s.bg(content(0.14)))
                        .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                            this.move_tab(dragged.ix, ix, cx);
                        }))
                }
            })
            // ✕ sits over the name's end, so showing it moves nothing.
            .when(hovered && !alone, |d| d.pr(px(24.)))
            .child(div().min_w_0().truncate().child(title))
            .when(hovered && !alone, |d| {
                d.child(
                    div().absolute().top(px(2.)).right(px(2.)).child(
                        row_icon_button(("tab-close", ix), "Close Tab", "cancel-01", content(0.55))
                            .debug_selector(move || format!("tab-close-{ix}"))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_tab(ix, window, cx)
                            })),
                    ),
                )
            })
    }

    /// One job: where am I, among which tabs. The folder's name is the first
    /// tab, so it stays put when tabs arrive.
    pub(super) fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pane = self.pane().read(cx);
        let (back, forward) = (pane.at > 0, pane.at + 1 < pane.history.len());
        let titles: Vec<String> = self.tabs.iter().map(|tab| tab.title(cx)).collect();
        let several = titles.len() > 1;
        let pills: Vec<_> = titles
            .into_iter()
            .enumerate()
            .map(|(ix, title)| self.tab_pill(ix, title, cx))
            .collect();
        div()
            .h(px(theme::TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .pr_3()
            .child(
                icon_button("back", "Back", "chevron-left", back).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.pane().update(cx, |pane, cx| pane.step_history(-1, cx));
                    },
                )),
            )
            .child(
                icon_button("forward", "Forward", "chevron-right", forward).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.pane().update(cx, |pane, cx| pane.step_history(1, cx));
                    },
                )),
            )
            .child(
                div()
                    .id("tabs")
                    .role(Role::TabList)
                    .min_w_0()
                    .h_full()
                    .flex()
                    .items_center()
                    .gap(px(theme::ROW_GAP))
                    .overflow_x_scroll()
                    .track_scroll(&self.tab_scroll)
                    .children(pills),
            )
            .when(several, |d| {
                d.child(
                    icon_button("new-tab", "New Tab", "add-01", true)
                        .debug_selector(|| "new-tab".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_tab_here(window, cx)),
                        ),
                )
            })
            .child(drag_area().flex_1().min_w(px(DRAG_ROOM)).h_full())
            .children(self.jobs.read(cx).ring().map(|ring| {
                ring.on_click(cx.listener(|this, _, window, cx| this.toggle_jobs(window, cx)))
            }))
    }

    /// ⌘T: a new tab on what's on screen, with a history of its own.
    pub(super) fn open_tab_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stop = self.pane().read(cx).stop().clone();
        self.new_tab(stop, window, cx);
    }
}

impl Tab {
    /// What its pill says.
    fn title(&self, cx: &App) -> String {
        self.focused().read(cx).title(cx)
    }
}
