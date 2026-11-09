//! The Worktrees page, which takes the pane's list while it's on screen:
//! every repository with a linked worktree that the index finds, and every
//! registered one, its worktrees under it. Git answers for all of it, off the
//! UI thread, through `monodrive_core::worktrees`. The selected row's changes
//! load on their own, and removing one asks first in the window's card.

use super::pane::Pane;
use super::sidebar::Places;
use crate::remove_worktree::Ask;
use crate::theme::{self, ButtonLook, content, icon, on_accent, row_icon_button, text_button, tip};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, MouseButton, PathPromptOptions, Role,
    ScrollStrategy, Task, UniformListScrollHandle, Window, actions, div, prelude::*, px,
    uniform_list,
};
use monodrive_core::worktrees::{self, Found, Git, Inventory, State, Status, Worktree};
use std::path::{Path, PathBuf};

actions!(worktrees, [AskRemoveWorktree]);

/// Wider than this, the page stops growing, so a row's end stays near its name.
const MAX_WIDTH: f32 = 880.;
/// A row holds a title over a detail, 16 pt each.
const ROW_HEIGHT: f32 = 34.;
const LINE: f32 = 16.;
const CHIP_HEIGHT: f32 = 18.;
/// How many repositories that couldn't be read show under the list.
const PROBLEMS_SHOWN: usize = 3;

/// What the page shows, as last scanned.
#[derive(Default)]
pub(in crate::browser) struct WorktreesPage {
    found: Option<Found>,
    /// The registered repositories' common Git folders, which can be
    /// forgotten.
    registered: Vec<PathBuf>,
    scanning: Option<Task<()>>,
    /// The selected worktree's path, which stays selected across scans.
    selected: Option<PathBuf>,
    /// The selected worktree's changes, once read, for the path they're of.
    status: Option<(PathBuf, Result<Status, String>)>,
    reading: Option<Task<()>>,
    /// What the window's card asks next.
    pub(in crate::browser) asking: Option<Ask>,
    scroll: UniformListScrollHandle,
}

/// One line of the list: a repository's title, or one of its worktrees.
/// Both are a row's height, so the list stays a `uniform_list`.
#[derive(Clone, Copy, PartialEq)]
enum Item {
    Repository(usize),
    Worktree(usize, usize),
}

impl WorktreesPage {
    pub(in crate::browser) fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// Every row in order, with its repository.
    fn rows(&self) -> impl Iterator<Item = (&Inventory, &Worktree)> {
        self.found.iter().flat_map(|found| {
            found
                .repositories
                .iter()
                .flat_map(|repo| repo.worktrees.iter().map(move |w| (repo, w)))
        })
    }

    fn row_of(&self, path: &Path) -> Option<(&Inventory, &Worktree)> {
        self.rows().find(|(_, w)| w.path == path)
    }

    fn items(&self) -> Vec<Item> {
        let repositories = self.found.iter().flat_map(|found| &found.repositories);
        repositories
            .enumerate()
            .flat_map(|(repo, inventory)| {
                std::iter::once(Item::Repository(repo))
                    .chain((0..inventory.worktrees.len()).map(move |w| Item::Worktree(repo, w)))
            })
            .collect()
    }

    fn worktree_at(&self, repo: usize, w: usize) -> Option<(&Inventory, &Worktree)> {
        let inventory = self.found.as_ref()?.repositories.get(repo)?;
        Some((inventory, inventory.worktrees.get(w)?))
    }
}

impl Pane {
    /// Scans again unless a scan is under way. Starts nothing: an index
    /// that isn't running yet finds nothing, and its error is shown.
    pub(super) fn scan_worktrees(&mut self, cx: &mut Context<Self>) {
        if self.worktrees.scanning.is_some() {
            return;
        }
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        let index = self.index.clone();
        let scanned = cx.background_executor().spawn(async move {
            let registered = places
                .store
                .repositories()
                .unwrap_or_default()
                .into_iter()
                .map(|repo| repo.common_dir)
                .collect();
            let found = match Git::find() {
                Ok(git) => worktrees::scan(&git, &places.store, |query, limit| {
                    places.drives.find_files(&index, query, limit)
                }),
                Err(why) => Found {
                    problems: vec![why],
                    ..Found::default()
                },
            };
            (found, registered)
        });
        self.worktrees.scanning = Some(cx.spawn(async move |this, cx| {
            let (found, registered) = scanned.await;
            let _ = this.update(cx, |this, cx| {
                let page = &mut this.worktrees;
                page.found = Some(found);
                page.registered = registered;
                page.scanning = None;
                // What was selected may have gone; its changes are read again.
                let kept = page
                    .selected
                    .clone()
                    .filter(|path| page.row_of(path).is_some());
                this.select_worktree(kept, cx);
            });
        }));
        cx.notify();
    }

    fn rescan_worktrees(&mut self, cx: &mut Context<Self>) {
        self.worktrees.scanning = None;
        self.scan_worktrees(cx);
    }

    /// Selects a row and reads its changes, dropping a read for another row.
    fn select_worktree(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        let page = &mut self.worktrees;
        page.selected = path.clone();
        page.status = None;
        page.reading = None;
        let readable = path.filter(|path| {
            page.row_of(path)
                .is_some_and(|(_, w)| w.state == State::Ok && !w.bare)
        });
        if let Some(path) = readable {
            let read = cx.background_executor().spawn({
                let path = path.clone();
                async move { Git::find().and_then(|git| worktrees::status(&git, &path)) }
            });
            page.reading = Some(cx.spawn(async move |this, cx| {
                let status = read.await;
                let _ = this.update(cx, |this, cx| {
                    if this.worktrees.selected.as_ref() == Some(&path) {
                        this.worktrees.status = Some((path, status));
                        this.worktrees.reading = None;
                        cx.notify();
                    }
                });
            }));
        }
        cx.notify();
    }

    /// ↑ and ↓ on the page.
    pub(super) fn step_worktree(&mut self, down: bool, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self.worktrees.rows().map(|(_, w)| w.path.clone()).collect();
        let at = self
            .worktrees
            .selected
            .as_ref()
            .and_then(|path| paths.iter().position(|p| p == path));
        let next = match (at, down) {
            (None, true) => paths.first(),
            (None, false) => paths.last(),
            (Some(at), true) => paths.get(at + 1).or(paths.get(at)),
            (Some(at), false) => paths.get(at.saturating_sub(1)),
        };
        self.select_worktree(next.cloned(), cx);
        // In view, and its repository's title with it when going up to it.
        let items = self.worktrees.items();
        let page = &self.worktrees;
        let at = items.iter().position(|item| match item {
            Item::Worktree(repo, w) => page
                .worktree_at(*repo, *w)
                .is_some_and(|(_, worktree)| Some(&worktree.path) == page.selected.as_ref()),
            Item::Repository(_) => false,
        });
        if let Some(at) = at {
            let title_above = at > 0 && matches!(items[at - 1], Item::Repository(_));
            let target = if !down && title_above { at - 1 } else { at };
            self.worktrees
                .scroll
                .scroll_to_item(target, ScrollStrategy::Nearest);
        }
    }

    /// ↩: the selected worktree's folder, in the pane, so Back returns here.
    pub(super) fn open_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, worktree)) = self
            .worktrees
            .selected
            .as_deref()
            .and_then(|path| self.worktrees.row_of(path))
        else {
            return;
        };
        match &worktree.state {
            State::Ok | State::Replaced => {
                let path = worktree.path.clone();
                self.go(path, window, cx);
            }
            State::Missing => {
                let why = format!("{} no longer exists.", worktree.path.display());
                self.toast(why, true, None, cx);
            }
            State::Stale => {
                let why = format!(
                    "{} is no longer linked to its repository.",
                    worktree.path.display()
                );
                self.toast(why, true, None, cx);
            }
            State::Unreadable(why) => {
                let why = format!("{}: {why}", worktree.path.display());
                self.toast(why, true, None, cx);
            }
        }
    }

    /// ⌘⌫ or a row's button: works out what removing the selected worktree
    /// would take, off the UI thread, then asks in the window's card.
    pub(super) fn ask_remove_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((repo, path)) = self
            .worktrees
            .selected
            .as_deref()
            .and_then(|path| self.worktrees.row_of(path))
            .map(|(repo, w)| (repo.common_dir.clone(), w.path.clone()))
        else {
            return;
        };
        let asked = cx.background_executor().spawn(async move {
            match Git::find().and_then(|git| worktrees::removal(&git, &repo, &path)) {
                Ok(removal) => Ask::Remove {
                    repo,
                    removal: Box::new(removal),
                },
                Err(why) => Ask::Refused { path, why },
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let ask = asked.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.worktrees.asking = Some(ask);
                window.dispatch_action(Box::new(AskRemoveWorktree), cx);
            });
        })
        .detach();
    }

    /// Add Repository…: remembers the repository a chosen folder is in.
    fn add_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = cx.try_global::<Places>().map(|places| places.store.clone()) else {
            return;
        };
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(dir) = paths.into_iter().next() else {
                return;
            };
            let added = cx
                .background_executor()
                .spawn(async move {
                    Git::find().and_then(|git| worktrees::register(&git, &store, &dir))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = added {
                    this.toast(why, true, None, cx);
                }
                this.rescan_worktrees(cx);
            });
        })
        .detach();
    }

    /// Forgets a registered repository: only Monodrive's bookmark goes.
    fn forget_repository(&mut self, common_dir: PathBuf, cx: &mut Context<Self>) {
        let Some(store) = cx.try_global::<Places>().map(|places| places.store.clone()) else {
            return;
        };
        let forgot = cx
            .background_executor()
            .spawn(async move { store.forget_repository(&common_dir) });
        cx.spawn(async move |this, cx| {
            let forgot = forgot.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(why) = forgot {
                    this.toast(why, true, None, cx);
                }
                this.rescan_worktrees(cx);
            });
        })
        .detach();
    }

    /// The page, when it's what's on screen.
    pub(super) fn worktrees_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.showing_worktrees() {
            return None;
        }
        let page = &self.worktrees;
        let scanning = page.scanning.is_some();
        let summary = match &page.found {
            None => "Looking for worktrees…".to_string(),
            Some(_) if scanning => "Looking again…".to_string(),
            Some(found) => {
                let worktrees: usize = found.repositories.iter().map(|r| r.worktrees.len()).sum();
                let repositories = found.repositories.len();
                format!(
                    "{} in {}",
                    counted(worktrees, "worktree"),
                    counted(repositories, "repository")
                )
            }
        };
        let bar = theme::pane_bar(theme::TITLE_BAR_HEIGHT)
            .gap_2()
            .border_b_1()
            .border_color(content(0.06))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::MEDIUM)
                    .child(summary),
            )
            .child(
                text_button(
                    "scan-worktrees",
                    "Scan Again",
                    if scanning {
                        ButtonLook::Disabled
                    } else {
                        ButtonLook::Ghost
                    },
                )
                .when(!scanning, |d| {
                    d.on_click(cx.listener(|this, _, _, cx| this.rescan_worktrees(cx)))
                }),
            )
            .child(
                text_button("add-repository", "Add Repository…", ButtonLook::Secondary)
                    .on_click(cx.listener(|this, _, window, cx| this.add_repository(window, cx))),
            );

        let found = page.found.clone().unwrap_or_default();
        let body = if found.repositories.is_empty() {
            let (title, detail) = if page.found.is_none() {
                ("Looking for worktrees…", String::new())
            } else if found.problems.is_empty() {
                (
                    "No worktrees found",
                    "Monodrive finds linked worktrees through the index. Add a repository to list it here without one."
                        .to_string(),
                )
            } else {
                (
                    "No worktrees found",
                    problem_lines(&found.problems).join("\n"),
                )
            };
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .px_6()
                .child(
                    div()
                        .text_size(px(theme::TEXT_TITLE))
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div()
                        .max_w(px(420.))
                        .text_center()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(detail),
                )
                .into_any_element()
        } else {
            let items = page.items().len();
            let problems = problem_lines(&found.problems).into_iter().map(|line| {
                div()
                    .px(px(theme::PANE_INSET))
                    .pb_2()
                    .truncate()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(theme::danger())
                    .child(line)
            });
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    uniform_list("worktrees-list", items, cx.processor(Self::worktree_items))
                        .track_scroll(&self.worktrees.scroll)
                        .flex_1()
                        .px(px(theme::PANE_INSET - 8.))
                        .pb_2(),
                )
                .children(problems)
                .into_any_element()
        };
        Some(
            div()
                .debug_selector(|| "worktrees-page".into())
                .id("worktrees")
                .role(Role::Group)
                .aria_label("Worktrees")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                // No rectangle to drag here, and no menu of folder commands.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.focus_list(window, cx);
                        cx.stop_propagation();
                    }),
                )
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(bar)
                .child(body)
                .into_any_element(),
        )
    }

    /// The list's lines in `range`.
    fn worktree_items(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let keys_here = self.list_focus.contains_focused(window, cx);
        let items = self.worktrees.items();
        items
            .get(range)
            .unwrap_or_default()
            .iter()
            .filter_map(|item| {
                let shown = match *item {
                    Item::Repository(repo) => {
                        let (inventory, _) = self.worktrees.worktree_at(repo, 0)?;
                        self.repository_header(inventory, cx).into_any_element()
                    }
                    Item::Worktree(repo, w) => {
                        let (_, worktree) = self.worktrees.worktree_at(repo, w)?;
                        self.worktree_row(worktree, keys_here, cx)
                            .into_any_element()
                    }
                };
                Some(
                    div()
                        .w_full()
                        .max_w(px(MAX_WIDTH))
                        .child(shown)
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// A repository's name, where it is, and Forget when it's registered.
    fn repository_header(
        &self,
        repo: &Inventory,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let main = repo.worktrees.first().map(|w| w.path.clone());
        let name = main
            .as_deref()
            .or(repo.common_dir.parent())
            .and_then(Path::file_name)
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let shown = main.map_or_else(String::new, |path| tilde(&path));
        let registered = self.worktrees.registered.contains(&repo.common_dir);
        let common_dir = repo.common_dir.clone();
        // A row's height and gap, its text at the bottom, so it sits by the
        // rows under it.
        div()
            .h(px(ROW_HEIGHT + theme::ROW_GAP))
            .pb(px(theme::ROW_GAP))
            .px_2()
            .flex()
            .items_end()
            .gap_2()
            .flex_shrink_0()
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(shown),
            )
            .when(registered, |d| {
                d.child(
                    text_button(
                        gpui::ElementId::Name(
                            format!("forget-{}", repo.common_dir.display()).into(),
                        ),
                        "Forget",
                        ButtonLook::Ghost,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.forget_repository(common_dir.clone(), cx)
                    })),
                )
            })
    }

    fn worktree_row(
        &self,
        worktree: &Worktree,
        keys_here: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let page = &self.worktrees;
        let selected = page.selected.as_ref() == Some(&worktree.path);
        let accented = selected && keys_here;
        let (ink, muted) = if accented {
            (on_accent(), on_accent())
        } else {
            (content(1.), content(0.65))
        };
        let name = worktree.path.file_name().map_or_else(
            || worktree.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let mut detail = match (&worktree.branch, &worktree.head, worktree.bare) {
            (Some(branch), _, _) => branch.clone(),
            (None, _, true) => "bare".to_string(),
            (None, Some(head), false) => format!("detached at {}", short(head)),
            (None, None, false) => "no commits".to_string(),
        };
        let status = page
            .status
            .as_ref()
            .filter(|(path, _)| selected && *path == worktree.path)
            .map(|(_, status)| status);
        detail.push_str(" · ");
        detail.push_str(&match status {
            Some(Ok(status)) => changes(status),
            Some(Err(why)) => why.clone(),
            None if worktree.main => "main worktree".to_string(),
            None => worktree.path.parent().map_or_else(String::new, tilde),
        });
        let mut chips: Vec<(String, bool)> = Vec::new();
        if worktree.main {
            chips.push(("Main".into(), false));
        }
        if worktree.locked.is_some() {
            chips.push(("Locked".into(), false));
        }
        match &worktree.state {
            State::Ok => {}
            State::Missing => chips.push(("Missing".into(), true)),
            State::Stale => chips.push(("Stale".into(), true)),
            State::Replaced => chips.push(("Replaced".into(), true)),
            State::Unreadable(_) => chips.push(("Unreadable".into(), true)),
        }
        let path = worktree.path.clone();
        let id = |what: &str| -> gpui::ElementId {
            gpui::ElementId::Name(format!("{what}-{}", path.display()).into())
        };
        let (select, copy, trash) = (path.clone(), path.clone(), path.clone());
        let selector = format!("worktree-{name}");
        theme::row_slot(id("worktree"))
            .debug_selector(move || selector.clone())
            .role(Role::ListBoxOption)
            .aria_label(name.clone())
            .aria_selected(selected)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.focus_list(window, cx);
                this.select_worktree(Some(select.clone()), cx);
                if event.click_count() == 2 {
                    this.open_worktree(window, cx);
                }
            }))
            .child(
                div()
                    .h(px(ROW_HEIGHT))
                    .px_2()
                    .rounded(px(theme::RADIUS_ROW))
                    .flex()
                    .items_center()
                    .gap_2()
                    .map(|d| {
                        if accented {
                            d.bg(theme::accent())
                        } else if selected {
                            d.bg(content(0.14))
                        } else {
                            d.group_hover(theme::ROW_GROUP, |s| s.bg(content(0.08)))
                        }
                    })
                    .child(icon("folder-01", 16.).text_color(if accented {
                        on_accent()
                    } else {
                        content(0.55)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .h(px(LINE))
                                    .line_height(px(LINE))
                                    .text_size(px(theme::TEXT_BODY))
                                    .text_color(ink)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .h(px(LINE))
                                    .line_height(px(LINE))
                                    .text_size(px(theme::TEXT_SMALL))
                                    .text_color(muted)
                                    .child(detail),
                            ),
                    )
                    .children(
                        chips
                            .into_iter()
                            .map(|(label, bad)| chip(label, bad, accented)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .flex_shrink_0()
                            .when(!selected, |d| {
                                d.opacity(0.)
                                    .group_hover(theme::ROW_GROUP, |s| s.opacity(1.))
                            })
                            .child(tip(
                                row_icon_button(id("copy"), "Copy Path", "copy", muted).on_click(
                                    cx.listener(move |_, _, _, cx| {
                                        cx.stop_propagation();
                                        let text = copy.display().to_string();
                                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                            text,
                                        ));
                                    }),
                                ),
                                "Copy Path",
                            ))
                            .child(tip(
                                row_icon_button(id("trash"), "Move to Trash", "trash", muted)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.select_worktree(Some(trash.clone()), cx);
                                        this.ask_remove_worktree(window, cx);
                                    })),
                                "Move to Trash",
                            )),
                    ),
            )
    }
}

/// What a scan couldn't read, a few lines of it, so a disk of unreadable
/// repositories can't push the list off the page.
fn problem_lines(problems: &[String]) -> Vec<String> {
    let mut lines: Vec<String> = problems
        .iter()
        .take(PROBLEMS_SHOWN)
        .map(|why| format!("Couldn't read {why}"))
        .collect();
    if problems.len() > PROBLEMS_SHOWN {
        lines.push(format!("and {} more", problems.len() - PROBLEMS_SHOWN));
    }
    lines
}

/// A badge at a row's end: what's special about it, in `danger` when Git's
/// view and the disk's disagree.
fn chip(label: String, bad: bool, accented: bool) -> impl IntoElement {
    let ink: Hsla = match (accented, bad) {
        (true, _) => on_accent(),
        (false, true) => theme::danger(),
        (false, false) => content(0.65),
    };
    div()
        .h(px(CHIP_HEIGHT))
        .px_2()
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .bg(if accented {
            on_accent().opacity(0.2)
        } else {
            content(0.08)
        })
        .text_size(px(theme::TEXT_CAPTION))
        .text_color(ink)
        .child(label)
}

/// A worktree's changes, as its row's detail says them.
fn changes(status: &Status) -> String {
    let changed = status.paths - status.untracked - status.conflicted;
    let mut parts: Vec<String> = [
        (changed, "changed"),
        (status.conflicted, "conflicted"),
        (status.untracked, "untracked"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    match status.ahead_behind {
        Some((0, 0)) | None => {}
        Some((ahead, behind)) => parts.push(format!("{ahead} ahead, {behind} behind")),
    }
    if parts.is_empty() {
        "no changes".to_string()
    } else {
        parts.join(", ")
    }
}

fn counted(n: usize, what: &str) -> String {
    match (n, what) {
        (1, _) => format!("1 {what}"),
        (_, "repository") => format!("{n} repositories"),
        _ => format!("{n} {what}s"),
    }
}

fn short(head: &str) -> String {
    head.chars().take(7).collect()
}

/// `path` with the home folder as `~`, as a person would type it.
fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
