//! The tabs of a window and the top bar that shows them. A tab holds its
//! panes; only the active tab's are drawn, listed or told about the disk.

use super::drag::{Spot, Target, offers};
use super::pane::{Opening, Pane, Stop};
use super::{Browser, drag_area, pane_room};
use crate::preferences::Preferences;
use crate::theme::{self, accent, content, icon_button, row_icon_button};
use crate::traffic_lights;
use gpui::{
    App, Context, CursorStyle, DispatchPhase, Entity, Focusable, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Role, Stateful, Window, canvas, div, prelude::*,
    px,
};
use std::path::PathBuf;

/// The widest and narrowest a tab gets. Past that they scroll.
const TAB_WIDEST: f32 = 180.;
const TAB_NARROWEST: f32 = 96.;
/// What the top bar always leaves for dragging the window.
const DRAG_ROOM: f32 = 24.;
/// The search field's width in the top bar, while the sidebar is closed.
const SEARCH_WIDTH: f32 = 200.;
/// The space between the traffic lights and Back.
const LIGHTS_GAP: f32 = 8.;

/// The narrowest a pane gets while dragging the divider.
const NARROWEST_PANE: f32 = 240.;
/// The gap between two cards, which is the divider. Boxy's panes touch.
pub(super) const CARD_GAP: f32 = 8.;

pub(super) struct Tab {
    /// The left pane.
    pub(super) pane: Entity<Pane>,
    /// The right pane, with Split View on.
    pub(super) second: Option<Entity<Pane>>,
    /// The commands are for the right pane.
    pub(super) second_focused: bool,
    /// The left pane's share of the room.
    pub(super) ratio: f32,
}

impl Tab {
    pub(super) fn new(pane: Entity<Pane>) -> Self {
        Self {
            pane,
            second: None,
            second_focused: false,
            ratio: 0.5,
        }
    }

    /// The pane commands act on.
    pub(super) fn focused(&self) -> &Entity<Pane> {
        match &self.second {
            Some(second) if self.second_focused => second,
            _ => &self.pane,
        }
    }

    /// The pane commands don't act on, with Split View on.
    pub(super) fn other(&self) -> Option<&Entity<Pane>> {
        let second = self.second.as_ref()?;
        Some(if self.second_focused {
            &self.pane
        } else {
            second
        })
    }

    pub(super) fn panes(&self) -> impl Iterator<Item = &Entity<Pane>> {
        std::iter::once(&self.pane).chain(&self.second)
    }
}

/// How wide the left pane is, of `total` to share.
pub(super) fn left_width(ratio: f32, total: f32) -> f32 {
    (total * ratio).round().clamp(
        NARROWEST_PANE.min(total / 2.),
        (total - NARROWEST_PANE).max(total / 2.),
    )
}

/// A drag of the divider: the left pane's width less the pointer's x at the
/// press, so the divider stays under the pointer, and the room being shared.
#[derive(Clone, Copy)]
pub(super) struct SplitDrag {
    grab: f32,
    total: f32,
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
        let drag = self.drag.clone();
        let pane = cx.new(|cx| Pane::new(opening, index, drag, cx));
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
        self.tabs.insert(at, Tab::new(pane));
        self.show_tab(at, window, cx);
    }

    /// ⌥⌘S: opens a second pane on this folder and moves there, or closes the
    /// pane commands act on, and the keys go to the one left.
    pub(super) fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (stop, view, sort) = {
            let pane = self.pane().read(cx);
            (pane.stop().clone(), pane.view, pane.sort)
        };
        if self.tabs[self.active].second.is_some() {
            self.pane()
                .clone()
                .update(cx, |pane, cx| pane.commit_rename(window, cx));
            let tab = &mut self.tabs[self.active];
            if let Some(second) = tab.second.take()
                && !tab.second_focused
            {
                tab.pane = second;
            }
            tab.second_focused = false;
        } else {
            let second = self.add_pane(Opening::at(stop, view, sort), cx);
            second.update(cx, |pane, cx| {
                pane.set_shown(true);
                pane.reload(cx);
            });
            let tab = &mut self.tabs[self.active];
            tab.second = Some(second);
            tab.second_focused = true;
        }
        self.pane()
            .update(cx, |pane, cx| pane.focus_list(window, cx));
        self.mirror_pane(cx);
        self.save_session(cx);
        cx.notify();
    }

    /// Copy or Move to Other Pane: what's selected, into the other pane's
    /// folder, as a job.
    pub(super) fn send_to_other_pane(
        &mut self,
        moving: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let tab = &self.tabs[self.active];
        let Some(into) = tab
            .other()
            .map(|other| other.read(cx).folder().to_path_buf())
        else {
            return;
        };
        tab.focused().clone().update(cx, |pane, cx| {
            let paths = pane.chosen();
            pane.send(paths, into, moving, window, cx);
        });
    }

    /// The panes on screen, and what shares the room between two.
    pub(super) fn split_view(
        &self,
        boxy: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tab = &self.tabs[self.active];
        let room = pane_room(window, cx);
        let gap = if boxy { 0. } else { CARD_GAP };
        let card = |pane: &Entity<Pane>, focused: bool| {
            div()
                .relative()
                .h_full()
                .min_w_0()
                .overflow_hidden()
                .border_color(if focused && tab.second.is_some() {
                    accent()
                } else {
                    content(0.06)
                })
                .when(!boxy || tab.second.is_some(), |d| d.border_1())
                .when(!boxy, |d| {
                    d.rounded(px(theme::RADIUS_CARD)).bg(theme::pane_fill())
                })
                .child(pane.clone())
        };
        let row = div()
            .id("panes")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .gap(px(gap));
        let Some(second) = &tab.second else {
            return row.child(card(&tab.pane, true).flex_1());
        };
        let total = room - gap;
        let left = left_width(tab.ratio, total);
        row.child(
            card(&tab.pane, !tab.second_focused)
                .w(px(left))
                .flex_shrink_0(),
        )
        .child(card(second, tab.second_focused).flex_1())
        .child(
            div()
                .id("split-divider")
                .debug_selector(|| "split-divider".into())
                .role(Role::Splitter)
                .aria_label("Resize panes")
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(left + (gap - CARD_GAP) / 2.))
                .w(px(CARD_GAP))
                .cursor(CursorStyle::ResizeLeftRight)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if event.click_count == 2 {
                            this.tabs[this.active].ratio = 0.5;
                            this.save_session(cx);
                        } else {
                            this.split_drag = Some(SplitDrag {
                                grab: left - event.position.x.as_f32(),
                                total,
                            });
                        }
                        cx.notify();
                    }),
                ),
        )
    }

    /// Follows a drag of the divider through the window, as `sidebar_drag`
    /// does.
    pub(super) fn split_drag_listener(
        &self,
        drag: SplitDrag,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let this = cx.weak_entity();
        canvas(
            |_, _, _| {},
            move |_, _, window, _| {
                window.set_window_cursor_style(CursorStyle::ResizeLeftRight);
                let moved = this.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = moved.update(cx, |this, cx| {
                        if event.pressed_button == Some(MouseButton::Left) {
                            let left = event.position.x.as_f32() + drag.grab;
                            let left = left_width(left / drag.total, drag.total);
                            this.tabs[this.active].ratio = left / drag.total;
                        } else {
                            this.split_drag = None;
                        }
                        cx.notify();
                    });
                });
                let released = this.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        let _ = released.update(cx, |this, cx| {
                            this.split_drag = None;
                            this.save_session(cx);
                            cx.notify();
                        });
                    }
                });
            },
        )
        .absolute()
        .size_0()
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
        if at != self.active
            && let Some(left) = self.tabs.get(self.active)
        {
            let left: Vec<_> = left.panes().cloned().collect();
            for pane in left {
                pane.update(cx, |pane, _| pane.set_shown(false));
            }
        }
        self.active = at;
        let panes: Vec<_> = self.tabs[at].panes().cloned().collect();
        for pane in panes {
            pane.update(cx, |pane, cx| {
                pane.set_shown(true);
                pane.list(true, cx);
            });
        }
        self.tab_scroll.scroll_to_item(at);
        self.pane()
            .update(cx, |pane, cx| pane.take_keys(window, cx));
        self.mirror_pane(cx);
        self.save_session(cx);
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
        self.save_session(cx);
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
        self.save_session(cx);
        cx.notify();
    }

    /// A tab in the strip. Alone it's the bare folder name, in the same place.
    fn tab_pill(&self, ix: usize, title: String, cx: &mut Context<Self>) -> Stateful<gpui::Div> {
        let active = ix == self.active;
        let hovered = self.hovered_tab == Some(ix);
        let alone = self.tabs.len() == 1;
        // A drop goes to its folder, and hovering doesn't switch to it.
        let target = self.drag.active().then(|| &self.tabs[ix]).and_then(|tab| {
            let pane = tab.focused().read(cx);
            pane.takes_files(cx).then(|| Target::Folder {
                spot: Spot::Tab(ix),
                path: pane.folder().to_path_buf(),
                name: title.clone(),
            })
        });
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
            .when_some(target, |d, target| {
                let drag = self.drag.clone();
                offers(d, move |_| drag.offer(target.clone()))
            })
            .when(self.drag.outlines(&Spot::Tab(ix)), |d| {
                d.child(self.drag.pulsed(theme::drop_outline().rounded_full()))
            })
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
    pub(super) fn title_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let pane = self.pane().read(cx);
        let (back, forward) = (pane.at > 0, pane.at + 1 < pane.history.len());
        let closed = !cx.global::<Preferences>().show_sidebar;
        // With no sidebar to hold it, the field shows while it's in use.
        let searching =
            closed && (!pane.query.is_empty() || pane.search.focus_handle(cx).is_focused(window));
        let lights = (traffic_lights::REACH + LIGHTS_GAP) / window.content_zoom();
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
            // The traffic lights, where the sidebar would have them.
            .when(closed, |d| d.child(drag_area().w(px(lights)).h_full()))
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
            .when(closed, |d| d.child(self.sidebar_button(cx)))
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
            .when(searching, |d| {
                d.child(self.search_field(window, cx).w(px(SEARCH_WIDTH)))
            })
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
