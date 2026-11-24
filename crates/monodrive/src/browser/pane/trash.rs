//! The Trash and Recents as the file list shows them. The Trash is a folder
//! with a header bar, and Put Back acts only on what Monodrive's own history
//! says came from somewhere. Recents is a list of files, gone ones included.

use crate::Operations;
use crate::browser::EmptyTrash;
use crate::browser::pane::Pane;
use crate::empty_trash::is_hidden;
use crate::lock;
use crate::system::Places;
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{Context, FontWeight, Task, Window, div, prelude::*, px};
use monodrive_core::ops::{self, Request};
use monodrive_core::places::Store;
use std::path::PathBuf;

/// What the Trash's header shows, as last read.
#[derive(Default)]
pub(in crate::browser) struct TrashHeader {
    /// How many items the list shows, hidden ones not among them; `None` if
    /// it didn't read.
    pub(in crate::browser) count: Option<usize>,
    /// Whether Empty Trash would erase anything, hidden items included.
    erasable: bool,
    /// The selection the owner was last asked about, and its answer: where
    /// each item came from, if Monodrive knows.
    asked: Vec<PathBuf>,
    origins: Option<Result<Vec<Option<PathBuf>>, String>>,
    counting: Option<Task<()>>,
    asking: Option<Task<()>>,
}

/// The saved recents, for a listing off the UI thread.
pub(in crate::browser) fn read_recents(store: Option<&Store>) -> Result<Vec<PathBuf>, String> {
    store.map_or(Ok(Vec::new()), Store::recents)
}

impl Pane {
    pub(in crate::browser) fn clear_recents(&mut self, cx: &mut Context<Self>) {
        let Some(store) = cx.try_global::<Places>().map(|places| places.store.clone()) else {
            return;
        };
        let cleared = cx
            .background_executor()
            .spawn(async move { store.clear_recents() });
        cx.spawn(async move |this, cx| {
            let cleared = cleared.await;
            let _ = this.update(cx, |this, cx| {
                match cleared {
                    Ok(()) => this.toast("Cleared Recents".into(), false, None, cx),
                    Err(why) => this.toast(why, true, None, cx),
                }
                if this.showing_recents() {
                    this.list(true, cx);
                }
            });
        })
        .detach();
    }

    /// Counts what Empty Trash would erase, whenever the Trash has listed,
    /// and asks again about what's selected.
    pub(in crate::browser) fn refresh_trash(&mut self, cx: &mut Context<Self>) {
        self.trash_header.asked.clear();
        self.trash_header.origins = None;
        let Some(trash) = cx
            .try_global::<Places>()
            .map(|places| places.trash.clone())
            .filter(|_| self.in_trash(cx))
        else {
            return;
        };
        let counted = cx
            .background_executor()
            .spawn(async move { ops::trash_items(&trash) });
        self.trash_header.counting = Some(cx.spawn(async move |this, cx| {
            let counted = counted.await;
            let _ = this.update(cx, |this, cx| {
                let header = &mut this.trash_header;
                header.erasable = counted.as_ref().is_ok_and(|items| !items.is_empty());
                header.count = counted
                    .ok()
                    .map(|items| items.iter().filter(|item| !is_hidden(item)).count());
                cx.notify();
            });
        }));
        self.follow_trash_selection(cx);
    }

    /// In the Trash, asks the owner where what's selected came from whenever
    /// that changes. Only here, so browsing elsewhere starts no owner.
    pub(in crate::browser) fn follow_trash_selection(&mut self, cx: &mut Context<Self>) {
        if !self.in_trash(cx) {
            return;
        }
        let chosen = self.chosen();
        if chosen == self.trash_header.asked {
            return;
        }
        self.trash_header.origins = None;
        if self.trash_header.asking.is_some() {
            return;
        }
        self.trash_header.asked.clone_from(&chosen);
        let Some(owner) = cx.try_global::<Operations>().map(|ops| ops.0.clone()) else {
            return;
        };
        let question = chosen.clone();
        let answer = cx
            .background_executor()
            .spawn(async move { lock(&owner).origins(&chosen) });
        self.trash_header.asking = Some(cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                this.trash_header.asking = None;
                if this.trash_header.asked == question && this.chosen() == question {
                    this.trash_header.origins = Some(answer);
                } else {
                    this.trash_header.asked.clear();
                    this.follow_trash_selection(cx);
                }
                cx.notify();
            });
        }));
    }

    /// What's selected that Put Back can take, and if a selection can't all
    /// go, why. With nothing selected, the disabled button says enough.
    pub(in crate::browser) fn put_back(&self) -> (Vec<PathBuf>, Option<String>) {
        let header = &self.trash_header;
        if header.asked.is_empty() {
            return (Vec::new(), None);
        }
        let origins = match &header.origins {
            None => return (Vec::new(), None),
            Some(Err(why)) => return (Vec::new(), Some(why.clone())),
            Some(Ok(origins)) => origins,
        };
        let known: Vec<PathBuf> = header
            .asked
            .iter()
            .zip(origins)
            .filter(|(_, origin)| origin.is_some())
            .map(|(path, _)| path.clone())
            .collect();
        let why = match header.asked.len() - known.len() {
            0 => None,
            unknown if known.is_empty() => Some(if unknown == 1 {
                "Monodrive didn't move this to the Trash, so it doesn't know where it came from."
                    .to_string()
            } else {
                "Monodrive didn't move these to the Trash, so it doesn't know where they came from."
                    .to_string()
            }),
            unknown => Some(format!(
                "{unknown} of {} didn't come through Monodrive, so they stay.",
                header.asked.len()
            )),
        };
        (known, why)
    }

    /// Put Back: only what the owner said it knows, never a guess.
    pub(in crate::browser) fn put_back_selected(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let (paths, _) = self.put_back();
        if !paths.is_empty() {
            self.submit(Request::PutBack { paths }, window, cx);
        }
    }

    /// The bar over the Trash's list: its count, Put Back and Empty Trash.
    pub(in crate::browser) fn trash_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.in_trash(cx) {
            return None;
        }
        let title = self.trash_header.count.map_or_else(String::new, item_count);
        let (known, why) = self.put_back();
        let can_empty = self.trash_header.erasable;
        Some(
            theme::pane_bar(theme::TITLE_BAR_HEIGHT)
                .gap_2()
                .border_b_1()
                .border_color(content(0.06))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(theme::TEXT_BODY))
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .justify_end()
                        .children(why.map(|why| {
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(px(theme::TEXT_SMALL))
                                .text_color(content(0.65))
                                .child(why)
                        })),
                )
                .child(
                    text_button(
                        "put-back",
                        "Put Back",
                        if known.is_empty() {
                            ButtonLook::Disabled
                        } else {
                            ButtonLook::Secondary
                        },
                    )
                    .debug_selector(|| "put-back".into())
                    .when(!known.is_empty(), |d| {
                        d.on_click(
                            cx.listener(|this, _, window, cx| this.put_back_selected(window, cx)),
                        )
                    }),
                )
                .child(
                    text_button(
                        "empty-trash",
                        "Empty Trash",
                        if can_empty {
                            ButtonLook::Caution
                        } else {
                            ButtonLook::Disabled
                        },
                    )
                    .debug_selector(|| "empty-trash".into())
                    .when(can_empty, |d| {
                        d.on_click(|_, window, cx| window.dispatch_action(Box::new(EmptyTrash), cx))
                    }),
                ),
        )
    }

    /// The bar over Recents' list: its count and Clear Recents. Clearing only
    /// forgets paths, so it asks nothing.
    pub(in crate::browser) fn recents_bar(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.showing_recents() || !self.query.is_empty() || self.preview.is_some() {
            return None;
        }
        let count = self.entries.len();
        Some(
            theme::pane_bar(theme::TITLE_BAR_HEIGHT)
                .justify_between()
                .border_b_1()
                .border_color(content(0.06))
                .child(
                    div()
                        .text_size(px(theme::TEXT_BODY))
                        .font_weight(FontWeight::MEDIUM)
                        .child(item_count(count)),
                )
                .child(
                    text_button(
                        "clear-recents",
                        "Clear Recents",
                        if count == 0 {
                            ButtonLook::Disabled
                        } else {
                            ButtonLook::Secondary
                        },
                    )
                    .debug_selector(|| "clear-recents".into())
                    .when(count > 0, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.clear_recents(cx)))
                    }),
                ),
        )
    }
}

fn item_count(count: usize) -> String {
    match count {
        1 => "1 item".to_string(),
        n => format!("{n} items"),
    }
}
