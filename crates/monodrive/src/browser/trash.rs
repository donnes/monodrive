//! The Trash and Recents as the file list shows them. The Trash is a folder
//! with a header bar, and Put Back acts only on what Monodrive's own history
//! says came from somewhere. Recents is a list of files, gone ones included.

use super::sidebar::Places;
use super::{Browser, Operations, Stop, lock};
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{Context, FontWeight, Task, Window, div, prelude::*, px};
use monodrive_core::cloud::Availability;
use monodrive_core::entries::{self, Entry, Kind};
use monodrive_core::ops::{self, Request};
use monodrive_core::places::Store;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What the Trash's header shows, as last read.
#[derive(Default)]
pub(super) struct TrashHeader {
    /// How many items Empty Trash would erase; `None` if it didn't read.
    pub(super) count: Option<usize>,
    /// The selection the owner was last asked about, and its answer: where
    /// each item came from, if Monodrive knows.
    asked: Vec<PathBuf>,
    origins: Option<Result<Vec<Option<PathBuf>>, String>>,
    counting: Option<Task<()>>,
    asking: Option<Task<()>>,
}

/// The rows for `recents`, newest first. One that's gone stays, marked.
pub(super) fn recent_entries(recents: Vec<PathBuf>) -> (Vec<Entry>, HashSet<PathBuf>) {
    let mut missing = HashSet::new();
    let rows = recents
        .into_iter()
        .map(|path| {
            entries::entry_at(&path).unwrap_or_else(|_| {
                missing.insert(path.clone());
                Entry {
                    name: path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    ),
                    path,
                    kind: Kind::Other,
                    size: 0,
                    modified: 0,
                    availability: Availability::Local,
                }
            })
        })
        .collect();
    (rows, missing)
}

/// The saved recents, for a listing off the UI thread.
pub(super) fn read_recents(store: Option<&Store>) -> Result<Vec<PathBuf>, String> {
    store.map_or(Ok(Vec::new()), Store::recents)
}

impl Browser {
    pub(super) fn showing_recents(&self) -> bool {
        self.history[self.at].recents
    }

    /// Whether the rows are a folder's, rather than search results or Recents.
    pub(super) fn showing_folder(&self) -> bool {
        self.query.is_empty() && !self.showing_recents()
    }

    pub(super) fn in_trash(&self, cx: &gpui::App) -> bool {
        self.showing_folder()
            && cx
                .try_global::<Places>()
                .is_some_and(|places| places.trash == self.folder())
    }

    /// Opens the Trash in the list, so Back returns from it.
    pub(super) fn open_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(trash) = cx.try_global::<Places>().map(|places| places.trash.clone()) {
            self.go(trash, window, cx);
        }
    }

    /// Opens the list of recently opened files, so Back returns from it. The
    /// folder behind it stays what Go to Folder and Open in Terminal start at.
    pub(super) fn open_recents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.showing_recents() {
            self.collapse_all();
            self.history.truncate(self.at + 1);
            self.history.push(Stop {
                folder: self.folder().to_path_buf(),
                recents: true,
            });
            self.at = self.history.len() - 1;
        }
        self.show_folder(cx);
        self.focus_list(window, cx);
    }

    pub(super) fn clear_recents(&mut self, cx: &mut Context<Self>) {
        let Some(store) = cx.try_global::<Places>().map(|places| places.store.clone()) else {
            return;
        };
        let cleared = cx
            .background_executor()
            .spawn(async move { store.clear_recents() });
        cx.spawn(async move |this, cx| {
            let cleared = cleared.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = cleared {
                    this.toast(why, true, None, cx);
                }
                if this.showing_recents() {
                    this.list(true, cx);
                }
            });
        })
        .detach();
    }

    /// Opens a file in its app and remembers that, or says it's gone.
    pub(super) fn open_file(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.missing.contains(path) {
            let said = format!("{} is no longer there.", crate::jobs::quoted(path));
            return self.toast(said, true, None, cx);
        }
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return cx.open_with_system(path);
        };
        places.drives.open(path, cx);
        let path = path.to_path_buf();
        let remembered = cx
            .background_executor()
            .spawn(async move { places.store.opened(&path) });
        cx.spawn(async move |this, cx| {
            if let Err(why) = remembered.await {
                let _ = this.update(cx, |this, cx| this.toast(why, true, None, cx));
            }
        })
        .detach();
    }

    /// Counts what Empty Trash would erase, whenever the Trash has listed,
    /// and asks again about what's selected.
    pub(super) fn refresh_trash(&mut self, cx: &mut Context<Self>) {
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
            .spawn(async move { ops::trash_items(&trash).map(|items| items.len()) });
        self.trash_header.counting = Some(cx.spawn(async move |this, cx| {
            let counted = counted.await;
            let _ = this.update(cx, |this, cx| {
                this.trash_header.count = counted.ok();
                cx.notify();
            });
        }));
        self.follow_trash_selection(cx);
    }

    /// In the Trash, asks the owner where what's selected came from whenever
    /// that changes. Only here, so browsing elsewhere starts no owner.
    pub(super) fn follow_trash_selection(&mut self, cx: &mut Context<Self>) {
        if !self.in_trash(cx) {
            return;
        }
        let chosen = self.chosen();
        if chosen == self.trash_header.asked {
            return;
        }
        self.trash_header.asked.clone_from(&chosen);
        self.trash_header.origins = None;
        let Some(owner) = cx.try_global::<Operations>().map(|ops| ops.0.clone()) else {
            return;
        };
        let answer = cx
            .background_executor()
            .spawn(async move { lock(&owner).origins(&chosen) });
        self.trash_header.asking = Some(cx.spawn(async move |this, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                this.trash_header.origins = Some(answer);
                cx.notify();
            });
        }));
    }

    /// What's selected that Put Back can take, and if a selection can't all
    /// go, why. With nothing selected, the disabled button says enough.
    pub(super) fn put_back(&self) -> (Vec<PathBuf>, Option<String>) {
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
    pub(super) fn put_back_selected(&mut self, window: &Window, cx: &mut Context<Self>) {
        let (paths, _) = self.put_back();
        if !paths.is_empty() {
            self.submit(Request::PutBack { paths }, window, cx);
        }
    }

    /// The bar over the Trash's list: its count, Put Back and Empty Trash.
    pub(super) fn trash_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.in_trash(cx) {
            return None;
        }
        let title = match self.trash_header.count {
            None => String::new(),
            Some(1) => "1 item".to_string(),
            Some(n) => format!("{n} items"),
        };
        let (known, why) = self.put_back();
        let can_empty = self.trash_header.count.is_some_and(|count| count > 0);
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
                        d.on_click(cx.listener(|this, _, window, cx| this.empty_trash(window, cx)))
                    }),
                ),
        )
    }
}
