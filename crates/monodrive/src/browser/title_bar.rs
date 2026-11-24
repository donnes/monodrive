//! The title bar: the sidebar's toggle, the tabs and the search field, with
//! its tokens and their menus and the completions under it.

use super::Browser;
use super::pane::drag::{Spot, Target, offers};
use crate::feedback::{self, Haptic};
use crate::preferences::Preferences;
use crate::theme::{self, content, drag_area, icon_button, row_icon_button};
use gpui::prelude::*;
use gpui::{
    ClickEvent, Context, FontWeight, MouseButton, Render, Role, Stateful, Window, div, point, px,
};

/// The widest and narrowest a tab gets. Past that they scroll.
const TAB_WIDEST: f32 = 180.;
const TAB_NARROWEST: f32 = 96.;
/// What the top bar always leaves for dragging the window.
const DRAG_ROOM: f32 = 24.;
/// The search field's width in the top bar, while the sidebar is closed.
const SEARCH_WIDTH: f32 = 200.;

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
    /// One job: where am I, among which tabs. The folder's name is the first
    /// tab, so it stays put when tabs arrive.
    pub(super) fn title_bar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let pane = self.pane().read(cx);
        let (back, forward) = (pane.can_go_back(), pane.can_go_forward());
        let shareable = pane.shareable();
        // Until the sidebar has slid away, its field is still the one drawn.
        let closed = !cx.global::<Preferences>().show_sidebar && self.sidebar_fraction == 0.;
        // With no sidebar to hold it, the field shows while it's in use.
        let searching = closed && (pane.search_in_use(window, cx) || self.searches_open());
        // Back and Forward sit at the sidebar's edge, but never under its button.
        let lead = (super::sidebar::toggle_reach(window) - self.sidebar_drawn(window, cx)).max(0.);
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
            .pl(px(lead))
            .pr_3()
            .child(
                theme::tip_below(icon_button("back", "Back", "chevron-left", back), "Back")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pane().update(cx, |pane, cx| pane.go_back(window, cx));
                    })),
            )
            .child(
                theme::tip_below(
                    icon_button("forward", "Forward", "chevron-right", forward),
                    "Forward",
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.pane().update(cx, |pane, cx| pane.step_history(1, cx));
                })),
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
                    theme::tip_below(icon_button("new-tab", "New Tab", "add-01", true), "New Tab")
                        .debug_selector(|| "new-tab".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_tab_here(window, cx)),
                        ),
                )
            })
            .child(drag_area().flex_1().min_w(px(DRAG_ROOM)).h_full())
            .child(
                theme::tip_below(icon_button("share", "Share", "share", shareable), "Share").when(
                    shareable,
                    |d| {
                        d.on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                            let under = point(event.position().x, px(theme::TITLE_BAR_HEIGHT));
                            this.pane()
                                .update(cx, |pane, cx| pane.share(window, under, cx));
                        }))
                    },
                ),
            )
            .when(searching, |d| {
                d.child(self.search_field(window, cx).w(px(SEARCH_WIDTH)))
            })
            .children(self.jobs.read(cx).ring().map(|ring| {
                ring.on_hover(
                    cx.listener(|this, over: &bool, _, cx| this.peek_jobs(false, *over, cx)),
                )
                .on_click(cx.listener(|this, _, window, cx| this.toggle_jobs(window, cx)))
            }))
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
                            if dragged.ix != ix {
                                feedback::haptic(Haptic::Generic, cx);
                            }
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
                        theme::tip_below(
                            row_icon_button(
                                ("tab-close", ix),
                                "Close Tab",
                                "cancel-01",
                                content(0.55),
                            ),
                            "Close Tab",
                        )
                        .debug_selector(move || format!("tab-close-{ix}"))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.close_tab(ix, window, cx)),
                        ),
                    ),
                )
            })
    }
}
