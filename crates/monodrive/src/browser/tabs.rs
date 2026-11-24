//! The tabs of a window and the top bar that shows them. A tab holds its
//! panes; only the active tab's are drawn, listed or told about the disk.

use super::Browser;
use super::pane::{Opening, Pane, Stop};
use crate::theme::{self, accent, content};
use gpui::{
    App, Context, CursorStyle, DispatchPhase, Entity, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Role, Window, canvas, div, prelude::*, px,
};
use std::path::PathBuf;

/// The narrowest a pane gets while dragging the divider.
pub(super) const NARROWEST_PANE: f32 = 240.;
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
                pane.set_shown(true, cx);
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
        let (focused, Some(other)) = (tab.focused().clone(), tab.other().cloned()) else {
            return;
        };
        let from_server = focused.read(cx).showing_remote();
        let to_server = other.read(cx).remote_folder().cloned();
        // Move isn't offered across a server: there's no Trash to recover from.
        if moving && (from_server || to_server.is_some()) {
            return;
        }
        if from_server {
            if to_server.is_some() {
                return self.toast(
                    "Copy between a server and this Mac: one pane needs a folder here.".into(),
                    true,
                    None,
                    cx,
                );
            }
            let into = other.read(cx).folder().to_path_buf();
            return focused.update(cx, |pane, cx| pane.remote_download(into, window, cx));
        }
        focused.update(cx, |pane, cx| {
            let paths = pane.chosen();
            match to_server {
                Some(into) => pane.upload(paths, into, window, cx),
                None => pane.send(
                    paths,
                    other.read(cx).folder().to_path_buf(),
                    moving,
                    window,
                    cx,
                ),
            }
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
        let room = self.pane_room(window, tab.second.is_some(), cx);
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
            .min_w_0()
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
                pane.update(cx, |pane, cx| pane.set_shown(false, cx));
            }
        }
        self.active = at;
        let panes: Vec<_> = self.tabs[at].panes().cloned().collect();
        for pane in panes {
            pane.update(cx, |pane, cx| {
                pane.set_shown(true, cx);
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

    /// ⌘T: a new tab on what's on screen, with a history of its own.
    pub(super) fn open_tab_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stop = self.pane().read(cx).stop().clone();
        self.new_tab(stop, window, cx);
    }
}

impl Tab {
    /// What its pill says.
    pub(super) fn title(&self, cx: &App) -> String {
        self.focused().read(cx).title(cx)
    }
}
