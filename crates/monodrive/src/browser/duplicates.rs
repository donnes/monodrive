//! Find Duplicates, a page in the pane's history: the files in one folder
//! that are copies of each other, through `monodrive_core::storage`, read
//! off the UI thread. Only copies read whole and found the same can be
//! chosen for the Trash, every copy starts as kept, and nothing moves until
//! the window's card has listed each path. The owner checks each copy again
//! as it moves it, in one operation that one Undo reverses.

use super::pane::{Pane, format_bytes};
use crate::review_duplicates::Review;
use crate::theme::{self, ButtonLook, content, icon, on_accent, row_icon_button, text_button, tip};
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, Hsla, ListAlignment, ListState, MouseButton, Role,
    Task, Toggled, Window, actions, div, list, prelude::*, px,
};
use monodrive_core::ops::{Origin, Outcome, Request};
use monodrive_core::storage::{self, Duplicates, Match, Progress, Stop};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

actions!(duplicates, [ReviewDuplicates]);

/// Smaller files are left out until asked for: a folder of small files
/// takes long to compare and frees little.
const FLOOR: u64 = 1_000_000;
/// A row holds a title over a detail, 16 pt each.
const ROW_HEIGHT: f32 = 34.;
const LINE: f32 = 16.;
const CHIP_HEIGHT: f32 = 18.;
/// A copy's row starts under its group's name.
const COPY_INDENT: f32 = 24.;

/// What the page shows, for the folder it last looked through.
pub(in crate::browser) struct DuplicatesPage {
    /// The folder and smallest size looked for, once a scan has started.
    of: Option<(PathBuf, u64)>,
    /// Files under `FLOOR` are compared too.
    smaller: bool,
    scanning: Option<Scanning>,
    found: Option<Result<Found, String>>,
    /// The groups shown open, by their first copy's path.
    open: HashSet<PathBuf>,
    /// The copies chosen for the Trash.
    trash: BTreeSet<PathBuf>,
    /// The line the keys are on.
    selected: Option<Line>,
    lines: ListState,
    /// What the window's card lists next.
    pub(in crate::browser) asking: Option<Review>,
}

impl Default for DuplicatesPage {
    fn default() -> Self {
        Self {
            of: None,
            smaller: false,
            scanning: None,
            found: None,
            open: HashSet::new(),
            trash: BTreeSet::new(),
            selected: None,
            lines: ListState::new(0, ListAlignment::Top, px(400.)),
            asking: None,
        }
    }
}

struct Scanning {
    progress: Arc<Progress>,
    /// The scan has met every file and is reading them.
    reading: Arc<AtomicBool>,
    _task: Task<()>,
    _redraw: Task<()>,
}

struct Found {
    duplicates: Duplicates,
    /// Files the scan met, all compared that are big enough.
    files: u64,
    /// What the scan couldn't read, with why.
    unreadable: Vec<String>,
    /// Why the scan or the reading stopped early, if either did.
    stopped: Option<Stop>,
}

/// One line of the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Group(usize),
    Copy(usize, usize),
    /// The title over the groups that weren't read whole.
    Unverified,
}

impl DuplicatesPage {
    fn groups(&self) -> &[storage::Group] {
        match &self.found {
            Some(Ok(found)) => &found.duplicates.groups,
            _ => &[],
        }
    }

    fn items(&self) -> Vec<Line> {
        let mut items = Vec::new();
        for (g, group) in self.groups().iter().enumerate() {
            if group.matched != Match::Content
                && (g == 0 || self.groups()[g - 1].matched == Match::Content)
            {
                items.push(Line::Unverified);
            }
            items.push(Line::Group(g));
            if self.open.contains(&group.members[0].path) {
                items.extend((0..group.members.len()).map(|m| Line::Copy(g, m)));
            }
        }
        items
    }

    /// Lays the list out again after its lines changed, where it was.
    fn relines(&mut self) {
        let top = self.lines.logical_scroll_top();
        let count = self.items().len();
        self.lines.reset(count);
        self.lines.scroll_to(top);
    }

    /// Whether the copy can change between Keep and Trash: it was verified,
    /// and trashing it would leave another copy.
    fn choosable(&self, g: usize, m: usize) -> bool {
        let Some(group) = self.groups().get(g) else {
            return false;
        };
        let path = &group.members[m].path;
        group.matched == Match::Content
            && (self.trash.contains(path)
                || group
                    .members
                    .iter()
                    .any(|other| other.path != *path && !self.trash.contains(&other.path)))
    }

    /// The chosen copies and what moving them frees, at least.
    fn chosen(&self) -> (usize, u64) {
        let copies = self
            .groups()
            .iter()
            .flat_map(|group| &group.members)
            .filter(|member| self.trash.contains(&member.path));
        copies.fold((0, 0), |(n, frees), member| {
            (n + 1, frees + member.unshared)
        })
    }
}

impl Pane {
    /// Whether the page is on screen: a search in front of it isn't.
    pub(super) fn showing_duplicates(&self) -> bool {
        self.query.is_empty() && self.stop().shows == crate::session::Shows::Duplicates
    }

    /// Find Duplicates: the lone selected folder, else the one shown.
    pub(super) fn find_duplicates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = self.selection.in_order(&self.entries);
        let folder = match chosen.as_slice() {
            [only] if only.kind == monodrive_core::entries::Kind::Dir => only.path.clone(),
            _ => self.disk_folder().to_path_buf(),
        };
        let mut stop = super::pane::Stop::folder(folder);
        stop.shows = crate::session::Shows::Duplicates;
        self.push_stop(stop, cx);
        self.focus_list(window, cx);
    }

    /// The page is on screen: looks through its folder unless that's what
    /// it shows already or is doing.
    pub(super) fn read_duplicates(&mut self, cx: &mut Context<Self>) {
        let floor = if self.duplicates.smaller { 1 } else { FLOOR };
        let wanted = (self.folder().to_path_buf(), floor);
        if self.duplicates.of.as_ref() != Some(&wanted) {
            self.scan_duplicates(wanted, cx);
        }
    }

    /// The page went off screen: a scan stops, and starts over when it's
    /// back. What a finished one found stays.
    pub(super) fn leave_duplicates(&mut self) {
        if let Some(scanning) = self.duplicates.scanning.take() {
            scanning.progress.cancel.store(true, Ordering::Relaxed);
            self.duplicates.of = None;
        }
    }

    fn scan_duplicates(&mut self, (folder, floor): (PathBuf, u64), cx: &mut Context<Self>) {
        let (tx, rx) = async_channel::bounded::<()>(1);
        let progress = Arc::new(Progress::notifying(move || {
            let _ = tx.try_send(());
        }));
        let reading = Arc::new(AtomicBool::new(false));
        let scanned = cx.background_executor().spawn({
            let (progress, reading, folder) = (progress.clone(), reading.clone(), folder.clone());
            async move {
                let report = storage::scan(&folder, &progress, None)?;
                reading.store(true, Ordering::Relaxed);
                let duplicates = report.duplicates(&|size| size >= floor, &progress, None);
                Ok::<_, String>(Found {
                    stopped: report.stopped.or(duplicates.stopped),
                    duplicates,
                    files: report.root().usage.files,
                    unreadable: report.unreadable,
                })
            }
        });
        // Its counts as they grow, while the scan is the page's.
        let redraw = cx.spawn(async move |this, cx| {
            while rx.recv().await.is_ok() {
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        let task = cx.spawn(async move |this, cx| {
            let found = scanned.await;
            let _ = this.update(cx, |this, cx| {
                let page = &mut this.duplicates;
                page.scanning = None;
                page.found = Some(found);
                page.trash.clear();
                page.open.clear();
                page.selected = None;
                page.relines();
                cx.notify();
            });
        });
        let page = &mut self.duplicates;
        page.of = Some((folder, floor));
        page.found = None;
        page.trash.clear();
        page.open.clear();
        page.selected = None;
        page.relines();
        page.scanning = Some(Scanning {
            progress,
            reading,
            _task: task,
            _redraw: redraw,
        });
        cx.notify();
    }

    /// Cancel or Esc: stops looking, keeping what was found so far.
    pub(super) fn stop_duplicates(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(scanning) = &self.duplicates.scanning else {
            return false;
        };
        scanning.progress.cancel.store(true, Ordering::Relaxed);
        cx.notify();
        true
    }

    fn scan_duplicates_again(&mut self, cx: &mut Context<Self>) {
        self.duplicates.of = None;
        self.read_duplicates(cx);
    }

    fn include_smaller(&mut self, smaller: bool, cx: &mut Context<Self>) {
        self.duplicates.smaller = smaller;
        self.read_duplicates(cx);
    }

    fn toggle_group(&mut self, g: usize, cx: &mut Context<Self>) {
        let page = &mut self.duplicates;
        let Some(first) = page
            .groups()
            .get(g)
            .map(|group| group.members[0].path.clone())
        else {
            return;
        };
        if !page.open.remove(&first) {
            page.open.insert(first);
        }
        page.relines();
        cx.notify();
    }

    fn toggle_copy(&mut self, g: usize, m: usize, cx: &mut Context<Self>) {
        let page = &mut self.duplicates;
        if !page.choosable(g, m) {
            return;
        }
        let path = page.groups()[g].members[m].path.clone();
        if !page.trash.remove(&path) {
            page.trash.insert(path);
        }
        cx.notify();
    }

    /// Show in Folder: the copy's folder, with it selected, so Back returns
    /// here.
    fn reveal_copy(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        self.select_on_listing(vec![path]);
        self.go(folder, window, cx);
    }

    /// ↑ and ↓ on the page, over groups and their copies.
    pub(super) fn step_duplicates(&mut self, down: bool, cx: &mut Context<Self>) {
        let items: Vec<(usize, Line)> = self
            .duplicates
            .items()
            .into_iter()
            .enumerate()
            .filter(|(_, line)| *line != Line::Unverified)
            .collect();
        let at = items
            .iter()
            .position(|(_, line)| Some(*line) == self.duplicates.selected);
        let next = match (at, down) {
            (None, true) => items.first(),
            (None, false) => items.last(),
            (Some(at), true) => items.get(at + 1).or(items.get(at)),
            (Some(at), false) => items.get(at.saturating_sub(1)),
        };
        if let Some(&(ix, line)) = next {
            self.duplicates.selected = Some(line);
            self.duplicates.lines.scroll_to_reveal_item(ix);
            cx.notify();
        }
    }

    /// ⌘↓ or →: opens or closes the selected group, or shows the selected
    /// copy in its folder.
    pub(super) fn open_duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.duplicates.selected {
            Some(Line::Group(g)) => self.toggle_group(g, cx),
            Some(Line::Copy(g, m)) => {
                if let Some(member) = self
                    .duplicates
                    .groups()
                    .get(g)
                    .and_then(|group| group.members.get(m))
                {
                    let path = member.path.clone();
                    self.reveal_copy(path, window, cx);
                }
            }
            _ => {}
        }
    }

    /// →: opens the selected group.
    pub(super) fn open_duplicate_group(&mut self, cx: &mut Context<Self>) {
        let page = &self.duplicates;
        if let Some(Line::Group(g)) = page.selected
            && !page.open.contains(&page.groups()[g].members[0].path)
        {
            self.toggle_group(g, cx);
        }
    }

    /// Space: the selected copy between Keep and Trash.
    pub(super) fn choose_duplicate(&mut self, cx: &mut Context<Self>) {
        if let Some(Line::Copy(g, m)) = self.duplicates.selected {
            self.toggle_copy(g, m, cx);
        }
    }

    /// ⌘⌫ or Review…: lists what would move in the window's card.
    pub(super) fn review_duplicates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = &self.duplicates;
        let Some(Ok(found)) = &page.found else {
            return;
        };
        let trash: Vec<PathBuf> = page.trash.iter().cloned().collect();
        if trash.is_empty() {
            return;
        }
        match found.duplicates.cleanup(&trash) {
            Ok(cleanup) => {
                self.duplicates.asking = Some(Review { cleanup });
                window.dispatch_action(Box::new(ReviewDuplicates), cx);
            }
            Err(why) => self.toast(why, true, None, cx),
        }
    }

    /// The card's Move to Trash: one operation for every chosen copy. A
    /// copy that changed since it was read stays, and says so.
    pub(in crate::browser) fn trash_duplicates(
        &mut self,
        request: Request,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        super::with_owner(
            window,
            cx,
            move |owner| owner.submit(Origin::Window, request),
            |this, done, _, cx| this.trashed_duplicates(done, cx),
        );
    }

    fn trashed_duplicates(&mut self, done: Result<Outcome, String>, cx: &mut Context<Self>) {
        let outcome = match done {
            Ok(outcome) => outcome,
            Err(why) => return self.toast(why, true, None, cx),
        };
        let moved: HashSet<&Path> = outcome
            .items
            .iter()
            .filter(|item| item.result.is_ok())
            .map(|item| item.path.as_path())
            .collect();
        let copies = |n: usize| {
            if n == 1 {
                "1 copy".to_string()
            } else {
                format!("{n} copies")
            }
        };
        let undo = (!moved.is_empty()).then_some(super::Offer::Undo(outcome.op));
        match outcome.failures().next() {
            None => self.toast(
                format!("Moved {} to the Trash", copies(moved.len())),
                false,
                undo,
                cx,
            ),
            Some(why) if moved.is_empty() => self.toast(why.to_string(), true, None, cx),
            Some(why) => self.toast(
                format!(
                    "{why} Moved the other {} to the Trash.",
                    copies(moved.len())
                ),
                true,
                undo,
                cx,
            ),
        }
        // What moved leaves the page; a group left with one copy goes too.
        let page = &mut self.duplicates;
        if let Some(Ok(found)) = &mut page.found {
            for group in &mut found.duplicates.groups {
                group
                    .members
                    .retain(|member| !moved.contains(member.path.as_path()));
            }
            found
                .duplicates
                .groups
                .retain(|group| group.members.len() > 1);
        }
        page.trash.retain(|path| !moved.contains(path.as_path()));
        page.selected = None;
        page.relines();
        cx.notify();
    }

    /// The page, when it's what's on screen.
    pub(super) fn duplicates_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.showing_duplicates() {
            return None;
        }
        let page = &self.duplicates;
        let name = level(self.folder());
        let summary = match (&page.scanning, &page.found) {
            (Some(scanning), _) => scanning_said(scanning),
            (None, Some(Ok(found))) => found_said(found),
            _ => String::new(),
        };
        let scanning = page.scanning.is_some();
        let bar = theme::pane_bar(theme::TITLE_BAR_HEIGHT)
            .gap_2()
            .border_b_1()
            .border_color(content(0.06))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::MEDIUM)
                    .child(name.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(summary),
            )
            .map(|d| {
                if scanning {
                    d.child(
                        text_button("duplicates-cancel", "Cancel", ButtonLook::Secondary).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.stop_duplicates(cx);
                            }),
                        ),
                    )
                } else if let Some(Ok(found)) = &page.found {
                    let smaller = page.smaller;
                    // With nothing found, the page's own button offers it.
                    let toggle = smaller || !found.duplicates.groups.is_empty();
                    d.when(toggle, |d| {
                        d.child(
                            text_button(
                                "duplicates-smaller",
                                if smaller {
                                    "1 MB and Up Only"
                                } else {
                                    "Include Smaller Files"
                                },
                                ButtonLook::Ghost,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.include_smaller(!smaller, cx)
                                }),
                            ),
                        )
                    })
                    .child(
                        text_button("duplicates-again", "Scan Again", ButtonLook::Secondary)
                            .on_click(cx.listener(|this, _, _, cx| this.scan_duplicates_again(cx))),
                    )
                } else {
                    d
                }
            });

        let body = match &page.found {
            None => centered(
                "Looking for duplicates…",
                "Comparing starts once every file in the folder has been met.".into(),
            )
            .into_any_element(),
            Some(Err(why)) => centered(
                &format!("Can't look for duplicates in \"{name}\""),
                super::short_reason(why, self.folder().parent()),
            )
            .child(
                div().pt_2().child(
                    text_button("duplicates-back", "Back", ButtonLook::Secondary)
                        .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
                ),
            )
            .into_any_element(),
            Some(Ok(found)) if found.duplicates.groups.is_empty() => {
                let stopped = found.stopped.is_some();
                let title = match (stopped, page.smaller) {
                    (true, _) => "No duplicates found before it stopped",
                    (false, false) => "No duplicates of 1 MB and up",
                    (false, true) => "No duplicates",
                };
                let mut detail = format!(
                    "Looked through {} in \"{name}\".",
                    counted(found.files, "file")
                );
                if !page.smaller {
                    detail.push_str(" Files under 1 MB weren't compared.");
                }
                // What it couldn't read or didn't download, so "none" isn't
                // taken for a whole answer.
                for line in notices(found) {
                    detail.push(' ');
                    detail.push_str(&line);
                }
                centered(title, detail)
                    .when(!page.smaller, |d| {
                        d.child(
                            div().pt_2().child(
                                text_button(
                                    "duplicates-include",
                                    "Include Smaller Files",
                                    ButtonLook::Secondary,
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.include_smaller(true, cx)),
                                ),
                            ),
                        )
                    })
                    .into_any_element()
            }
            Some(Ok(found)) => {
                let notices = notices(found).into_iter().map(|line| {
                    div()
                        .px(px(theme::PANE_INSET))
                        .pt_2()
                        .truncate()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(line)
                });
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .children(notices)
                    .child(
                        list(page.lines.clone(), cx.processor(Self::duplicate_line))
                            .flex_1()
                            .pt_1()
                            .pb_2(),
                    )
                    .into_any_element()
            }
        };

        let (chosen, frees) = page.chosen();
        let dock = (chosen > 0).then(|| {
            let copies = if chosen == 1 {
                "1 copy".to_string()
            } else {
                format!("{chosen} copies")
            };
            theme::pane_bar(theme::TITLE_BAR_HEIGHT)
                .gap_2()
                .border_t_1()
                .border_color(content(0.06))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(format!(
                            "{copies} to the Trash · frees at least {}",
                            format_bytes(frees)
                        )),
                )
                .child(
                    text_button("duplicates-clear", "Clear", ButtonLook::Ghost).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.duplicates.trash.clear();
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    text_button("duplicates-review", "Review…", ButtonLook::Primary)
                        .debug_selector(|| "duplicates-review".into())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.review_duplicates(window, cx)),
                        ),
                )
        });

        Some(
            div()
                .debug_selector(|| "duplicates-page".into())
                .id("duplicates")
                .role(Role::Group)
                .aria_label("Duplicates")
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
                .children(dock)
                .into_any_element(),
        )
    }

    /// The list's line `ix`.
    fn duplicate_line(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let keys_here = self.list_focus.contains_focused(window, cx);
        let line = self.duplicates.items().get(ix).copied();
        let shown = match line {
            Some(Line::Unverified) => Some(
                theme::section_label("Not read whole, so these stay")
                    .pt_3()
                    .into_any_element(),
            ),
            Some(Line::Group(g)) => Some(
                self.duplicate_group_row(g, keys_here, cx)
                    .into_any_element(),
            ),
            Some(Line::Copy(g, m)) => Some(
                self.duplicate_copy_row(g, m, keys_here, cx)
                    .into_any_element(),
            ),
            None => None,
        };
        div()
            .px(px(theme::CARD_INSET))
            .children(shown)
            .into_any_element()
    }

    /// A group: its first copy's name, how many there are, whether they
    /// were read whole, and what all but one take up.
    fn duplicate_group_row(
        &self,
        g: usize,
        keys_here: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let page = &self.duplicates;
        let group = &page.groups()[g];
        let open = page.open.contains(&group.members[0].path);
        let selected = page.selected == Some(Line::Group(g));
        let accented = selected && keys_here;
        let (ink, muted) = ink(accented);
        let name = file_name(&group.members[0].path);
        let extra = extra(group);
        let mut detail = format!(
            "{} of {}",
            counted(group.members.len() as u64, "copy"),
            format_bytes(group.size)
        );
        if group.matched == Match::Content && extra == 0 {
            detail.push_str(" · they share their space, so trashing one frees nothing");
        }
        let chip = match group.matched {
            Match::Content => ("Verified", true),
            Match::Sample => ("Same start, middle and end", false),
            Match::Size => ("Same size", false),
        };
        let selector = format!("duplicate-group-{name}");
        row_frame(gpui::ElementId::Name(format!("group-{g}").into()), selected)
            .debug_selector(move || selector.clone())
            .aria_label(name.clone())
            .aria_expanded(open)
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.focus_list(window, cx);
                this.duplicates.selected = Some(Line::Group(g));
                this.toggle_group(g, cx);
            }))
            .child(
                row_inside(selected, accented)
                    .child(icon("copy", 16.).text_color(muted))
                    .child(two_lines(name, detail, ink, muted))
                    .child(chip_of(chip.0, chip.1, accented))
                    .child(
                        div()
                            .w(px(72.))
                            .flex_shrink_0()
                            .text_right()
                            .text_size(px(theme::TEXT_BODY))
                            .text_color(ink)
                            .child(format_bytes(extra)),
                    )
                    .child(
                        icon(
                            if open {
                                "chevron-down-hi"
                            } else {
                                "chevron-right"
                            },
                            12.,
                        )
                        .text_color(muted),
                    ),
            )
    }

    /// A copy: its name and folder, Show in Folder, and Keep or Trash once
    /// it was read whole.
    fn duplicate_copy_row(
        &self,
        g: usize,
        m: usize,
        keys_here: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let page = &self.duplicates;
        let group = &page.groups()[g];
        let path = group.members[m].path.clone();
        let selected = page.selected == Some(Line::Copy(g, m));
        let accented = selected && keys_here;
        let (ink, muted) = ink(accented);
        let trashed = page.trash.contains(&path);
        let verified = group.matched == Match::Content;
        let choosable = page.choosable(g, m);
        let name = file_name(&path);
        let detail = path.parent().map_or_else(String::new, |folder| {
            format!("in {}", within(folder, self.folder()))
        });
        let id = |what: &str| -> gpui::ElementId {
            gpui::ElementId::Name(format!("{what}-{}", path.display()).into())
        };
        let selector = format!("duplicate-copy-{}", path.display());
        let reveal = path.clone();
        row_frame(id("copy"), selected)
            .debug_selector(move || selector.clone())
            .aria_label(name.clone())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.focus_list(window, cx);
                this.duplicates.selected = Some(Line::Copy(g, m));
                cx.notify();
            }))
            .child(
                row_inside(selected, accented)
                    .pl(px(COPY_INDENT))
                    .child(icon("file", 16.).text_color(muted))
                    .child(two_lines(
                        name,
                        detail,
                        if trashed && !accented {
                            content(0.55)
                        } else {
                            ink
                        },
                        muted,
                    ))
                    .child(tip(
                        row_icon_button(id("reveal"), "Show in Folder", "folder-01", muted)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.reveal_copy(reveal.clone(), window, cx);
                            })),
                        "Show in Folder",
                    ))
                    .when(verified, |d| {
                        d.child(keep_or_trash(
                            id("choice"),
                            trashed,
                            choosable,
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_copy(g, m, cx);
                            }),
                        ))
                    }),
            )
    }
}

/// Where a line's row lives, its handlers and role on it.
fn row_frame(id: gpui::ElementId, selected: bool) -> gpui::Stateful<gpui::Div> {
    theme::row_slot(id)
        .role(Role::ListBoxOption)
        .aria_selected(selected)
}

/// A row's highlight and layout.
fn row_inside(selected: bool, accented: bool) -> gpui::Div {
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
}

/// A row's title and detail ink.
fn ink(accented: bool) -> (Hsla, Hsla) {
    if accented {
        (on_accent(), on_accent())
    } else {
        (content(1.), content(0.65))
    }
}

fn two_lines(title: String, detail: String, ink: Hsla, muted: Hsla) -> gpui::Div {
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
                .child(title),
        )
        .child(
            div()
                .truncate()
                .h(px(LINE))
                .line_height(px(LINE))
                .text_size(px(theme::TEXT_SMALL))
                .text_color(muted)
                .child(detail),
        )
}

/// Whether a group was read whole, in `success` when it was.
fn chip_of(label: &'static str, verified: bool, accented: bool) -> impl IntoElement {
    let ink = match (accented, verified) {
        (true, _) => on_accent(),
        (false, true) => theme::success(),
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
        } else if verified {
            theme::success().opacity(0.14)
        } else {
            content(0.08)
        })
        .text_size(px(theme::TEXT_CAPTION))
        .text_color(ink)
        .child(label)
}

/// A copy's Keep and Trash, a segmented control: one click switches it.
/// The last kept copy of a group can't be switched, which its name says.
fn keep_or_trash(
    id: gpui::ElementId,
    trashed: bool,
    choosable: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let segment = |label: &'static str, on: bool, danger: bool| {
        div()
            .h(px(theme::BUTTON_HEIGHT - 6.))
            .px_2()
            .rounded_full()
            .flex()
            .items_center()
            .text_size(px(theme::TEXT_CAPTION))
            .map(|d| match (on, danger) {
                (true, true) => d
                    .bg(theme::danger().opacity(0.2))
                    .text_color(theme::danger())
                    .font_weight(FontWeight::MEDIUM),
                (true, false) => d
                    .bg(theme::raised())
                    .shadow(theme::raised_shadow())
                    .font_weight(FontWeight::MEDIUM),
                (false, _) if choosable => d.text_color(content(0.65)),
                (false, _) => d.text_color(content(0.3)),
            })
            .child(label)
    };
    div()
        .id(id)
        .role(Role::Switch)
        .aria_label(if choosable {
            "Move to Trash"
        } else {
            "Keep at least one copy"
        })
        .aria_toggled(if trashed {
            Toggled::True
        } else {
            Toggled::False
        })
        .flex()
        .flex_shrink_0()
        .p(px(2.))
        .gap(px(2.))
        .rounded_full()
        .bg(content(0.08))
        .when(choosable, |d| d.cursor_pointer().on_click(on_click))
        .child(segment("Keep", !trashed, false))
        .child(segment("Trash", trashed, true))
}

/// A message centered in the page, with room for a button under it.
fn centered(title: &str, detail: String) -> gpui::Div {
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
                .child(title.to_string()),
        )
        .child(
            div()
                .max_w(px(420.))
                .text_center()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child(detail),
        )
}

fn scanning_said(scanning: &Scanning) -> String {
    let progress = &scanning.progress;
    if progress.cancel.load(Ordering::Relaxed) {
        "Stopping…".to_string()
    } else if scanning.reading.load(Ordering::Relaxed) {
        let read = progress.read.load(Ordering::Relaxed);
        format!("Comparing copies… {} read", format_bytes(read))
    } else {
        let met = progress.items.load(Ordering::Relaxed);
        format!("Looking through {}…", counted(met, "item"))
    }
}

fn found_said(found: &Found) -> String {
    let groups = &found.duplicates.groups;
    let verified: Vec<&storage::Group> = groups
        .iter()
        .filter(|g| g.matched == Match::Content)
        .collect();
    if groups.is_empty() {
        return format!("{} looked through", counted(found.files, "file"));
    }
    let extra: u64 = verified.iter().map(|g| extra(g)).sum();
    format!(
        "{} · {} in extra copies",
        counted(verified.len() as u64, "group"),
        format_bytes(extra)
    )
}

/// What a scan stopped short of or left out, a line each.
fn notices(found: &Found) -> Vec<String> {
    let mut lines = Vec::new();
    match found.stopped {
        Some(Stop::Cancelled) => lines.push(
            "Stopped before every file was compared. Copies not read whole can't go to the Trash."
                .to_string(),
        ),
        Some(Stop::Budget) => lines.push(format!(
            "Stopped after {} items. Look through a smaller folder to compare all of it.",
            grouped(storage::MAX_ITEMS as u64)
        )),
        Some(Stop::Deadline) | None => {}
    }
    let unreadable = found.unreadable.len() + found.duplicates.unreadable.len();
    if unreadable > 0 {
        lines.push(format!(
            "{} couldn't be read, so {} compared.",
            counted(unreadable as u64, "item"),
            if unreadable == 1 {
                "it wasn't"
            } else {
                "they weren't"
            }
        ));
    }
    let cloud = found.duplicates.not_downloaded.len();
    if cloud > 0 {
        lines.push(format!(
            "{} in the cloud {} downloaded, so {} compared.",
            counted(cloud as u64, "file"),
            if cloud == 1 { "isn't" } else { "aren't" },
            if cloud == 1 {
                "it wasn't"
            } else {
                "they weren't"
            }
        ));
    }
    lines
}

/// What trashing all but the copy that holds the most of its own space
/// frees, at least: clones of each other free nothing.
fn extra(group: &storage::Group) -> u64 {
    let unshared = group.members.iter().map(|m| m.unshared);
    let most = unshared.clone().max().unwrap_or(0);
    unshared.sum::<u64>() - most
}

/// "1 copy", "3 copies", "1,904 items".
fn counted(n: u64, what: &str) -> String {
    let number = grouped(n);
    match (n, what) {
        (1, _) => format!("1 {what}"),
        (_, "copy") => format!("{number} copies"),
        _ => format!("{number} {what}s"),
    }
}

/// `n` with thousands separated, as Finder writes counts.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A folder's name as the page's bar says it.
fn level(folder: &Path) -> String {
    if folder == Path::new("/") {
        return "/".to_string();
    }
    file_name(folder)
}

/// Where `folder` is from the one looked through, as the page's bar names
/// it: "Downloads › Old installers". Anywhere else, the whole path.
fn within(folder: &Path, looked: &Path) -> String {
    match folder.strip_prefix(looked) {
        Ok(rest) => std::iter::once(level(looked))
            .chain(rest.iter().map(|part| part.to_string_lossy().into_owned()))
            .collect::<Vec<_>>()
            .join(" › "),
        Err(_) => tilde(folder),
    }
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
