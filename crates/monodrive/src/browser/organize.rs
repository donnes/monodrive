//! Organize's review page, which takes the pane's list until it's left: one
//! section per destination, each with why, its files, and a switch to leave it
//! out. The sorting and the moving are `ops::organize`'s and the owner's.

use super::pane::Pane;
use super::{CancelRename, CommitRename, with_owner};
use crate::theme::{self, ButtonLook, content, danger, text_button};
use gpui::{
    AnyElement, Context, FocusHandle, FontWeight, MouseButton, Task, Window, div, prelude::*, px,
};
use monodrive_core::ops::organize::{Group, Proposal, propose};
use monodrive_core::ops::{Origin, Outcome, Request};
use std::collections::HashSet;
use std::path::PathBuf;

/// How many names a section lists; the page reads by group, not by file.
const SHOWN: usize = 6;

pub(super) struct Review {
    /// Where it was opened. Going anywhere else leaves it.
    place: (PathBuf, bool, String),
    keys: FocusHandle,
    pub(super) proposal: Option<Proposal>,
    /// The destinations switched off.
    left_out: HashSet<PathBuf>,
    pub(super) sending: bool,
    error: Option<String>,
    _reading: Option<Task<()>>,
}

impl Review {
    fn chosen(&self) -> Vec<Group> {
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
    pub(super) fn start_organize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.changing_names() {
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

    fn read_proposal(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn end_organize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.organizing.as_ref().is_some_and(|r| r.sending) {
            return;
        }
        if self.organizing.take().is_some() {
            self.focus_list(window, cx);
            cx.notify();
        }
    }

    /// Each frame: going elsewhere leaves the review.
    pub(super) fn follow_organize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .organizing
            .as_ref()
            .is_some_and(|review| !review.sending && review.place != self.place_now())
        {
            self.organizing = None;
            cx.defer_in(window, |this, window, cx| this.focus_list(window, cx));
        }
    }

    pub(super) fn run_organize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn organized(
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

    fn organize_section(&self, group: &Group, off: bool, cx: &mut Context<Self>) -> AnyElement {
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
    pub(super) fn organize_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
