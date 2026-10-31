//! The inspector: a panel to the right of the panes about what's selected in
//! the pane commands act on, or its folder. When the selection moves to one
//! item, the panel keeps showing the last until that item's read and picture
//! are ready, then swaps them in one frame. Past `PATIENCE` it shows what's
//! known and fills in the rest. A read or folder total for what's no longer
//! selected is dropped. While it's closed, nothing here reads or runs.

use super::pane::{Pane, format_date, format_size, level_name, real};
use super::tabs::{CARD_GAP, NARROWEST_PANE};
use super::{Browser, GetInfo, HideInspector, OpenWith, format_bytes, with_owner};
use crate::file_icons;
use crate::native::native;
use crate::preferences::{self, Preferences};
use crate::theme::{self, ButtonLook, content, danger, icon, icon_button, text_button};
use crate::thumbnails::{self, Thumb};
use chrono::Local;
use gpui::{
    AnyElement, App, Context, CursorStyle, Div, Entity, EntityId, FontWeight, MouseButton,
    RenderImage, Role, ScrollHandle, Task, Window, div, prelude::*, px,
};
use monodrive_core::cloud::{self, Availability};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::info::Info;
use monodrive_core::ops::Origin;
use monodrive_core::paths;
use monodrive_core::sizes::FolderSize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How wide it is.
pub(super) const WIDTH: f32 = 260.;
/// How long the selection must rest before the disk is read, so arrowing
/// through a list asks nothing about the rows it passes.
pub(super) const READ_DELAY: Duration = Duration::from_millis(100);
/// How long the disk must settle before what's shown is read again.
pub(super) const REFRESH_DELAY: Duration = Duration::from_millis(250);
/// How long a selection waits for its read and picture before showing anyway,
/// so a slow volume or a picture that won't decode can't leave the old item up.
pub(super) const PATIENCE: Duration = Duration::from_millis(250);
/// The picture at its top.
const ICON_SIDE: f32 = 96.;
/// The file list keeps at least this much of the room, or it stays closed.
const LEAST_LIST: f32 = 200.;
/// How much of a value's row its label takes.
const LABEL_WIDTH: f32 = 84.;
/// How many kinds the summary of several items names.
const KINDS_NAMED: usize = 3;

/// What's selected, as last drawn.
struct Key {
    pane: EntityId,
    folder: PathBuf,
    showing_folder: bool,
    only: Option<PathBuf>,
    count: usize,
    bytes: u64,
    cursor: Option<PathBuf>,
}

impl Key {
    fn of(id: EntityId, pane: &Pane) -> Self {
        Self {
            pane: id,
            folder: pane.folder().to_path_buf(),
            showing_folder: pane.shows_a_real_folder(),
            only: pane.selection.only().map(Path::to_path_buf),
            count: pane.selection.len(),
            bytes: pane.selection.size(),
            cursor: pane.selection.cursor().map(Path::to_path_buf),
        }
    }

    fn matches(&self, id: EntityId, pane: &Pane) -> bool {
        self.pane == id
            && self.folder == pane.folder()
            && self.showing_folder == pane.shows_a_real_folder()
            && self.only.as_deref() == pane.selection.only()
            && self.count == pane.selection.len()
            && self.bytes == pane.selection.size()
            && self.cursor.as_deref() == pane.selection.cursor()
    }
}

/// What it's about.
#[derive(Default)]
pub(super) enum Subject {
    /// Search results or Recents with nothing selected, or a location that
    /// can't open: there's no one thing to read.
    #[default]
    Nothing,
    /// One item. `selected` says it's a row and not the folder shown, so its
    /// name can be edited.
    One { entry: Entry, selected: bool },
    Several {
        count: usize,
        bytes: u64,
        first: Entry,
        kinds: String,
        folders: usize,
        /// The folder they share, if they do.
        parent: Option<PathBuf>,
    },
}

/// What the disk said about the one item.
#[derive(Default)]
pub(super) enum Read {
    #[default]
    Waiting,
    /// Reading it could show a privacy prompt, so only the listing's facts show.
    Gated,
    Failed(String),
    Known {
        info: Box<Info>,
        /// As FSEvents and folder totals name it.
        real: PathBuf,
    },
}

/// A folder's total.
#[derive(Clone, Default, PartialEq)]
pub(super) enum Total {
    #[default]
    Unknown,
    Done(FolderSize),
    Failed(String),
}

/// A folder's total on its way. Dropping it cancels the walk.
struct Measuring {
    folder: PathBuf,
    /// The folder changed since this began, so another follows.
    again: bool,
    _walk: Box<dyn Send>,
    _land: Task<()>,
}

/// The item selected next, until it swaps in for what's shown.
struct Pending {
    key: Key,
    subject: Subject,
    /// Its read and any kept folder total, once they've landed.
    read: Option<(Read, Option<FolderSize>)>,
    /// Its picture, held and decoding once the read says it may be read.
    thumb: Option<(Thumb, Option<Arc<RenderImage>>)>,
    /// Out of patience: it swaps in as it is.
    late: bool,
    _patience: Task<()>,
}

#[derive(Default)]
pub struct Inspector {
    key: Option<Key>,
    pending: Option<Pending>,
    /// While the next item waits, what's shown has changed or had its work
    /// stopped, so selecting back to it reads it again.
    stale: bool,
    pub(super) subject: Subject,
    pub(super) read: Read,
    pub(super) total: Total,
    measuring: Option<Measuring>,
    reading: Option<Task<()>>,
    refresh: Option<Task<()>>,
    /// The picture at the top, once its file has been read as allowed.
    pub(super) thumb: Option<(Thumb, Option<Arc<RenderImage>>)>,
    scroll: ScrollHandle,
}

impl Inspector {
    /// Forgets what's shown and gives back its picture.
    fn clear(&mut self, window: Option<&mut Window>, cx: &mut App) {
        self.stale = false;
        self.read = Read::Waiting;
        self.total = Total::Unknown;
        self.measuring = None;
        self.reading = None;
        self.refresh = None;
        self.release(window, cx);
    }

    /// Gives back both pictures, and stops waiting for the next item.
    pub(super) fn release(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        self.drop_pending(window.as_deref_mut(), cx);
        give_back(self.thumb.take(), window, cx);
    }

    /// Stops waiting for the next item, and gives back its picture.
    fn drop_pending(&mut self, window: Option<&mut Window>, cx: &mut App) {
        if let Some(pending) = self.pending.take() {
            give_back(pending.thumb, window, cx);
        }
    }
}

fn give_back(
    thumb: Option<(Thumb, Option<Arc<RenderImage>>)>,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    if let Some((thumb, image)) = thumb {
        thumbnails::release(&thumb, image, window, cx);
    }
}

/// How much of `room`, the panes' width without it, the inspector takes: none
/// if that would leave the list under its least, or two panes under theirs
/// with the divider between them.
pub(super) fn room(room: f32, split: bool, cx: &App) -> f32 {
    let prefs = cx.global::<Preferences>();
    if !prefs.show_inspector {
        return 0.;
    }
    let gap = match prefs.workspace_style {
        preferences::WorkspaceStyle::Rounded => CARD_GAP,
        preferences::WorkspaceStyle::Boxy => 0.,
    };
    let taken = WIDTH + gap;
    let least = if split {
        2. * NARROWEST_PANE + gap
    } else {
        LEAST_LIST
    };
    if room - taken >= least { taken } else { 0. }
}

/// `2 folders, 3 PNG files, 1 more`: the commonest kinds first.
pub(super) fn kinds_summary(entries: &[&Entry]) -> String {
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries {
        *kinds.entry(entry.kind_label()).or_default() += 1;
    }
    let mut kinds: Vec<(String, usize)> = kinds.into_iter().collect();
    kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let others: usize = kinds.iter().skip(KINDS_NAMED).map(|(_, count)| count).sum();
    let mut named: Vec<String> = kinds
        .into_iter()
        .take(KINDS_NAMED)
        .map(|(kind, count)| {
            let word = match kind.as_str() {
                "Folder" => "folder",
                "Alias" => "alias",
                "--" => "item",
                other => other,
            };
            let plural = match (count, word) {
                (1, _) => "",
                (_, "alias") => "es",
                _ => "s",
            };
            format!("{count} {word}{plural}")
        })
        .collect();
    if others > 0 {
        named.push(format!("{others} more"));
    }
    named.join(", ")
}

/// Whether a change FSEvents reports at `changed`, and below it if `below`,
/// touches what's at `real`: inside it, in the folder holding it, or at it.
pub(super) fn touches(real: &Path, changed: &Path, below: bool) -> bool {
    changed.starts_with(real)
        || real.parent() == Some(changed)
        || (below && real.starts_with(changed))
}

fn subject_of(pane: &Pane) -> Subject {
    if !pane.shows_a_real_folder() && pane.selection.is_empty() {
        return Subject::Nothing;
    }
    let chosen = pane.selection.in_order(&pane.entries);
    match chosen.as_slice() {
        [] => {
            let folder = pane.folder().to_path_buf();
            Subject::One {
                entry: Entry {
                    name: level_name(&folder),
                    path: folder,
                    kind: Kind::Dir,
                    size: 0,
                    modified: 0,
                    availability: Availability::Local,
                },
                selected: false,
            }
        }
        [only] => Subject::One {
            entry: (*only).clone(),
            selected: true,
        },
        many => {
            let parent = many[0].path.parent().map(Path::to_path_buf);
            let shared = many.iter().all(|e| e.path.parent() == parent.as_deref());
            Subject::Several {
                count: many.len(),
                bytes: pane.selection.size(),
                first: many[0].clone(),
                kinds: kinds_summary(many),
                folders: many.iter().filter(|e| e.kind == Kind::Dir).count(),
                parent: parent.filter(|_| shared),
            }
        }
    }
}

impl Browser {
    /// Each frame, after the keys have settled on a pane: whether what it
    /// should show changed. Closed, it holds nothing.
    pub(super) fn follow_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.inspector_open(window, cx) {
            if self.inspector.key.take().is_some() {
                self.inspector.clear(Some(window), cx);
                self.inspector.subject = Subject::Nothing;
            }
            return;
        }
        let pane = self.pane().clone();
        let id = pane.entity_id();
        let pane = pane.read(cx);
        if let Some(key) = &self.inspector.key
            && key.matches(id, pane)
        {
            if self.inspector.pending.is_some() {
                self.back_to_whats_shown(window, cx);
            }
            return;
        }
        if self
            .inspector
            .pending
            .as_ref()
            .is_some_and(|pending| pending.key.matches(id, pane))
        {
            return self.swap_when_ready(window, cx);
        }
        // What a rename or a job selected isn't listed yet; its listing is on
        // its way, and this asks again until it lands.
        if !pane.selection.is_empty() && pane.selection.in_order(&pane.entries).is_empty() {
            return;
        }
        let opening = self.inspector.key.is_none();
        let key = Key::of(id, pane);
        let subject = subject_of(pane);
        if opening {
            native(cx).forget_sizes();
        }
        // What's shown stays only while it's still there to show. A rename or
        // a move in this folder has taken it away.
        let same_folder = self
            .inspector
            .key
            .as_ref()
            .is_some_and(|shown| shown.pane == key.pane && shown.folder == key.folder);
        let gone = same_folder
            && matches!(
                &self.inspector.subject,
                Subject::One { entry, selected: true }
                    if !pane.entries.iter().any(|e| e.path == entry.path)
            );
        let was_waiting = self.inspector.pending.is_some();
        self.inspector.drop_pending(Some(window), cx);
        let holds = !opening && !gone && matches!(subject, Subject::One { selected: true, .. });
        if !holds {
            self.inspector.key = Some(key);
            self.inspector.clear(Some(window), cx);
            self.inspector.subject = subject;
            self.inspector
                .scroll
                .set_offset(gpui::point(px(0.), px(0.)));
            return self.schedule_read(READ_DELAY, cx);
        }
        // Nothing more is asked about what's shown, and its rows stay. While
        // another waited, what's running was that one's.
        if !was_waiting {
            self.inspector.stale = self.inspector.reading.is_some()
                || self.inspector.refresh.is_some()
                || self.inspector.measuring.is_some();
        }
        self.inspector.measuring = None;
        self.inspector.reading = None;
        self.inspector.refresh = None;
        let patience = cx.background_executor().timer(PATIENCE);
        self.inspector.pending = Some(Pending {
            key,
            subject,
            read: None,
            thumb: None,
            late: false,
            _patience: cx.spawn(async move |this, cx| {
                patience.await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(pending) = &mut this.inspector.pending {
                        pending.late = true;
                    }
                    cx.notify();
                });
            }),
        });
        self.schedule_read(READ_DELAY, cx);
    }

    /// The selection came back to what's shown before the next item swapped
    /// in. Nothing new draws, but what went stale meanwhile is read again.
    fn back_to_whats_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inspector.drop_pending(Some(window), cx);
        self.inspector.reading = None;
        if std::mem::take(&mut self.inspector.stale) {
            self.schedule_read(READ_DELAY, cx);
        }
    }

    /// Swaps the next item in once its read has landed and any picture it
    /// wants is decoded, or once patience runs out. Its picture starts
    /// decoding only after the read, so nothing unread or gated gets opened.
    fn swap_when_ready(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thumbs_on = cx.global::<Preferences>().thumbnails;
        let Some(pending) = &mut self.inspector.pending else {
            return;
        };
        let ready = match (&pending.read, &pending.subject) {
            (None, _) => false,
            (Some((Read::Known { .. }, _)), Subject::One { entry, .. })
                if thumbs_on && thumbnails::wanted(entry) =>
            {
                let thumb = Thumb {
                    path: entry.path.clone(),
                    side: (ICON_SIDE * window.scale_factor()).ceil() as u32,
                    modified: entry.modified,
                };
                if pending.thumb.is_none() {
                    thumbnails::hold(&thumb, cx);
                    pending.thumb = Some((thumb.clone(), None));
                }
                let image = thumbnails::get(&thumb, window, cx);
                let ready = image.is_some();
                if let Some(held) = &mut pending.thumb {
                    held.1 = image;
                }
                ready
            }
            _ => true,
        };
        if !ready && !pending.late {
            return;
        }
        let Some(next) = self.inspector.pending.take() else {
            return;
        };
        self.inspector.stale = false;
        give_back(self.inspector.thumb.take(), Some(window), cx);
        self.inspector.key = Some(next.key);
        self.inspector.subject = next.subject;
        self.inspector.thumb = next.thumb;
        self.inspector.read = Read::Waiting;
        self.inspector.total = Total::Unknown;
        self.inspector
            .scroll
            .set_offset(gpui::point(px(0.), px(0.)));
        // Later, a read still on its way fills in, as `reading` runs on.
        if let Some((read, known)) = next.read {
            self.apply_read(read, known, cx);
        }
    }

    /// Whether it's on screen: asked for, and with room for the list beside it.
    fn inspector_open(&self, window: &Window, cx: &App) -> bool {
        let split = self.tabs[self.active].second.is_some();
        cx.global::<Preferences>().show_inspector && super::inspector_fits(window, split, cx)
    }

    /// Reads the next item, or else the one shown, after `delay`, unless the
    /// selection moves first.
    fn schedule_read(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let subject = match &self.inspector.pending {
            Some(pending) => &pending.subject,
            None => &self.inspector.subject,
        };
        let Subject::One { entry, .. } = subject else {
            return;
        };
        let path = entry.path.clone();
        let native = native(cx);
        let wait = cx.background_executor().timer(delay);
        self.inspector.reading = Some(cx.spawn(async move |this, cx| {
            wait.await;
            let read = cx.background_executor().spawn(async move {
                if native.needs_consent(&path) {
                    return (Read::Gated, None);
                }
                match native.info(&path) {
                    Ok(info) => {
                        let real = real(&path);
                        let known = native.known_size(&real);
                        let read = Read::Known {
                            info: Box::new(info),
                            real,
                        };
                        (read, known)
                    }
                    Err(why) => (Read::Failed(why), None),
                }
            });
            let (read, known) = read.await;
            let _ = this.update(cx, |this, cx| this.land_read(read, known, cx));
        }));
    }

    fn land_read(&mut self, read: Read, known: Option<FolderSize>, cx: &mut Context<Self>) {
        self.inspector.reading = None;
        match &mut self.inspector.pending {
            Some(pending) => pending.read = Some((read, known)),
            None => self.apply_read(read, known, cx),
        }
        cx.notify();
    }

    /// What's shown takes this read.
    fn apply_read(&mut self, read: Read, known: Option<FolderSize>, cx: &mut Context<Self>) {
        if let Read::Known { info, real } = &read
            && info.kind == Kind::Dir
        {
            match known {
                Some(total) => self.inspector.total = Total::Done(total),
                None => self.measure(real.clone(), cx),
            }
        }
        if matches!(read, Read::Gated) && self.subject_is_folder() {
            self.inspector.total = Total::Failed("Needs Full Disk Access".into());
        }
        self.inspector.read = read;
    }

    /// Whether `path` is what's shown, with nothing waiting to replace it.
    fn shows(&self, path: &Path) -> bool {
        self.inspector.pending.is_none()
            && matches!(&self.inspector.subject, Subject::One { entry, .. } if entry.path == path)
    }

    fn subject_is_folder(&self) -> bool {
        matches!(&self.inspector.subject, Subject::One { entry, .. } if entry.kind == Kind::Dir)
    }

    /// Starts a total of `folder`. One already on its way runs out, however
    /// often the folder changes, and another follows: a folder being copied
    /// into would otherwise never get a total.
    fn measure(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        if let Some(measuring) = &mut self.inspector.measuring
            && measuring.folder == folder
        {
            measuring.again = true;
            return;
        }
        let (send, landed) = async_channel::bounded(1);
        let walk = native(cx).measure(
            folder.clone(),
            Box::new(move |total| {
                let _ = send.try_send(total);
            }),
        );
        let land = cx.spawn(async move |this, cx| {
            if let Ok(total) = landed.recv().await {
                let _ = this.update(cx, |this, cx| this.land_total(total, cx));
            }
        });
        self.inspector.measuring = Some(Measuring {
            folder,
            again: false,
            _walk: walk,
            _land: land,
        });
    }

    fn land_total(&mut self, total: Result<FolderSize, String>, cx: &mut Context<Self>) {
        let measuring = self.inspector.measuring.take();
        self.inspector.total = match total {
            Ok(total) => Total::Done(total),
            Err(why) => Total::Failed(why),
        };
        if let Some(measuring) = measuring.filter(|m| m.again) {
            self.measure(measuring.folder, cx);
        }
        cx.notify();
    }

    /// The disk changed: totals it makes wrong go, and what's shown is read
    /// again once a burst has settled. Closed, nothing here listens, and
    /// opening starts from no totals at all.
    pub(super) fn inspector_disk_changed(
        &mut self,
        batch: &[(PathBuf, bool)],
        cx: &mut Context<Self>,
    ) {
        if self.inspector.key.is_none() {
            return;
        }
        let native = native(cx);
        let changed: Vec<PathBuf> = batch.iter().map(|(path, _)| path.clone()).collect();
        cx.background_executor()
            .spawn(async move {
                for path in changed {
                    native.invalidate(&path);
                }
            })
            .detach();
        // While the next item waits, what's shown only notes the change, in case
        // the selection comes back, and a landed read of the next one can go stale.
        if self.inspector.pending.is_some()
            && let Read::Known { real, .. } = &self.inspector.read
            && batch
                .iter()
                .any(|(changed, below)| touches(real, changed, *below))
        {
            self.inspector.stale = true;
        }
        let read = match &self.inspector.pending {
            Some(pending) => pending.read.as_ref().map(|(read, _)| read),
            None => Some(&self.inspector.read),
        };
        let Some(Read::Known { real, .. }) = read else {
            return;
        };
        if self.inspector.refresh.is_some()
            || !batch
                .iter()
                .any(|(changed, below)| touches(real, changed, *below))
        {
            return;
        }
        let wait = cx.background_executor().timer(REFRESH_DELAY);
        self.inspector.refresh = Some(cx.spawn(async move |this, cx| {
            wait.await;
            let _ = this.update(cx, |this, cx| {
                this.inspector.refresh = None;
                if this.inspector.pending.is_none() {
                    this.schedule_read(Duration::ZERO, cx);
                }
            });
        }));
    }

    /// ⌘I, Hide Inspector and its button. A name being edited in the inspector
    /// takes what's typed so far.
    fn set_inspector(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !on {
            self.pane().update(cx, |pane, cx| {
                if pane.renaming.as_ref().is_some_and(|r| r.in_inspector) {
                    pane.commit_rename(window, cx);
                }
            });
        }
        preferences::update(cx, |p| p.show_inspector = on);
    }

    pub(super) fn inspector_commands(
        &self,
        root: Div,
        sheet: bool,
        selected: bool,
        prefs: &Preferences,
        cx: &mut Context<Self>,
    ) -> Div {
        root.when(!sheet, |d| {
            d.on_action(cx.listener(|this, _: &GetInfo, window, cx| {
                let on = !cx.global::<Preferences>().show_inspector;
                this.set_inspector(on, window, cx)
            }))
            .when(prefs.show_inspector, |d| {
                d.on_action(cx.listener(|this, _: &HideInspector, window, cx| {
                    this.set_inspector(false, window, cx)
                }))
            })
            .when(selected, |d| {
                d.on_action(
                    cx.listener(|this, action: &OpenWith, _, cx| this.open_with_menu(action, cx)),
                )
            })
        })
    }

    /// The panel, if it's open.
    pub(super) fn inspector_view(
        &mut self,
        boxy: bool,
        split: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.inspector.key.as_ref()?;
        let pane = self.pane().clone();
        let picture = self.inspector_picture(window, cx);
        let today = Local::now().date_naive();
        let contents: Vec<AnyElement> = match &self.inspector.subject {
            Subject::Nothing => vec![
                div()
                    .pt(px(48.))
                    .w_full()
                    .text_center()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(content(0.65))
                    .child("Nothing selected.")
                    .into_any_element(),
            ],
            Subject::One { entry, selected } => {
                let entry = entry.clone();
                let selected = *selected;
                self.one_item(&entry, selected, picture, today, &pane, window, cx)
            }
            Subject::Several {
                count,
                bytes,
                first,
                kinds,
                folders,
                parent,
            } => {
                let (count, bytes, first) = (*count, *bytes, first.clone());
                let (kinds, folders, parent) = (kinds.clone(), *folders, parent.clone());
                let art = file_icons::icon(&first, ICON_SIDE, window, cx);
                // Folders' totals aren't walked for a selection; the Size says so.
                let size = if folders == count {
                    "--".to_string()
                } else if folders > 0 {
                    format!("{} in files", format_bytes(bytes))
                } else {
                    format_bytes(bytes)
                };
                let mut rows = vec![
                    header(
                        art,
                        div()
                            .debug_selector(|| "inspector-name".into())
                            .child(format!("{count} items"))
                            .into_any_element(),
                        kinds.clone(),
                    ),
                    section("file", "Details"),
                    fact("Size", size),
                    fact("Kind", kinds),
                ];
                if let Some(parent) = parent {
                    rows.push(section("hard-drive", "Storage"));
                    rows.push(self.where_row(parent, None, &pane, cx));
                }
                rows
            }
        };
        let hide = icon_button("inspector-hide", "Hide Inspector", "cancel-01", true)
            .debug_selector(|| "inspector-hide".into())
            .on_click(cx.listener(|this, _, window, cx| this.set_inspector(false, window, cx)));
        Some(
            div()
                .id("inspector")
                .debug_selector(|| "inspector".into())
                .relative()
                .w(px(WIDTH))
                .h_full()
                .flex_shrink_0()
                .overflow_hidden()
                .text_size(px(theme::TEXT_BODY))
                .border_color(content(0.06))
                .map(|d| {
                    if boxy {
                        d.border_l_1().when(split, |d| d.border_t_1())
                    } else {
                        d.ml(px(CARD_GAP))
                            .border_1()
                            .rounded(px(theme::RADIUS_CARD))
                            .bg(theme::pane_fill())
                    }
                })
                // A press here leaves the keys where they are.
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .child(
                    div()
                        .id("inspector-scroll")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.inspector.scroll)
                        .px(px(theme::PANE_INSET))
                        .pb(px(theme::PANE_INSET))
                        .flex()
                        .flex_col()
                        .children(contents),
                )
                .child(div().absolute().top(px(8.)).right(px(8.)).child(hide))
                .into_any_element(),
        )
    }

    /// A picture of the one item, once reading it is known to be allowed and
    /// the selection has rested: the grid's decoder, at this size.
    fn inspector_picture(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        let Subject::One { entry, .. } = &self.inspector.subject else {
            return None;
        };
        let wanted = matches!(self.inspector.read, Read::Known { .. })
            && cx.global::<Preferences>().thumbnails
            && thumbnails::wanted(entry);
        if !wanted {
            return None;
        }
        let thumb = Thumb {
            path: entry.path.clone(),
            side: (ICON_SIDE * window.scale_factor()).ceil() as u32,
            modified: entry.modified,
        };
        if self.inspector.thumb.as_ref().map(|(t, _)| t) != Some(&thumb) {
            give_back(self.inspector.thumb.take(), Some(window), cx);
            thumbnails::hold(&thumb, cx);
            self.inspector.thumb = Some((thumb.clone(), None));
        }
        let image = thumbnails::get(&thumb, window, cx);
        if let Some(held) = &mut self.inspector.thumb {
            held.1 = image.clone();
        }
        image
    }

    #[allow(clippy::too_many_arguments)]
    fn one_item(
        &mut self,
        entry: &Entry,
        selected: bool,
        picture: Option<Arc<RenderImage>>,
        today: chrono::NaiveDate,
        pane: &Entity<Pane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let known = match &self.inspector.read {
            Read::Known { info, .. } => Some(info.as_ref().clone()),
            _ => None,
        };
        let art = match picture {
            Some(image) => gpui::img(image)
                .size(px(ICON_SIDE))
                .object_fit(gpui::ObjectFit::Contain)
                .rounded(px(theme::RADIUS_THUMB))
                .into_any_element(),
            None => file_icons::icon(entry, ICON_SIDE, window, cx),
        };
        let name = self.name_view(entry, selected, known.as_ref(), pane, cx);
        let kind = known
            .as_ref()
            .map_or_else(|| entry.kind_label(), |info| info.kind_name.clone());
        let mut rows = vec![
            header(art, name, kind.clone()),
            section("file", "Details"),
            fact("Size", self.size_text(entry)),
            fact("Kind", kind),
        ];
        if let Total::Done(total) = &self.inspector.total
            && total.unreadable > 0
        {
            rows.push(fact_ink(
                "Unreadable",
                format!("{} folders", total.unreadable),
                danger(),
            ));
        }
        let date = |time: Option<SystemTime>| {
            let seconds = time?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some(format_date(seconds, today))
        };
        let modified = known.as_ref().map_or_else(
            || (entry.modified > 0).then(|| format_date(entry.modified, today)),
            |i| date(i.modified),
        );
        let dates: Vec<_> = [
            ("Created", known.as_ref().and_then(|i| date(i.created))),
            ("Modified", modified),
            ("Opened", known.as_ref().and_then(|i| date(i.opened))),
        ]
        .into_iter()
        .filter_map(|(label, value)| Some(fact(label, value?)))
        .collect();
        if !dates.is_empty() {
            rows.push(section("clock", "Dates"));
            rows.extend(dates);
        }
        rows.push(section("hard-drive", "Storage"));
        if let Some(parent) = entry.path.parent() {
            rows.push(self.where_row(parent.to_path_buf(), Some(entry.path.clone()), pane, cx));
        }
        if let Some(info) = &known {
            self.storage_rows(info, &mut rows, cx);
        }
        match &self.inspector.read {
            Read::Gated => rows.push(note("Needs Full Disk Access to read more.", content(0.65))),
            Read::Failed(why) => rows.push(note(why.clone(), danger())),
            _ => {}
        }
        rows
    }

    /// The name, which a click edits for a selected row, through the rename
    /// the list uses.
    fn name_view(
        &self,
        entry: &Entry,
        selected: bool,
        known: Option<&Info>,
        pane: &Entity<Pane>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let width = WIDTH - 2. * theme::PANE_INSET;
        let field = pane.update(cx, |pane, cx| {
            pane.inspector_rename_field(&entry.path, width, cx)
        });
        if let Some(field) = field {
            return field.into_any_element();
        }
        let name = known.map_or_else(|| entry.name.clone(), |info| info.name.clone());
        let label = div()
            .id("inspector-name")
            .debug_selector(|| "inspector-name".into())
            .max_w_full()
            .px(px(6.))
            .rounded(px(theme::RADIUS_ROW))
            .text_center()
            .child(name);
        if !selected {
            return label.into_any_element();
        }
        let pane = pane.clone();
        let path = entry.path.clone();
        label
            .role(Role::Button)
            .aria_label("Rename")
            .cursor(CursorStyle::IBeam)
            .hover(|d| d.bg(content(0.08)))
            .on_click(cx.listener(move |_, _, window, cx| {
                pane.update(cx, |pane, cx| pane.rename_in_inspector(&path, window, cx));
            }))
            .into_any_element()
    }

    /// What the Size row says.
    pub(super) fn size_text(&self, entry: &Entry) -> String {
        if let Read::Known { info, .. } = &self.inspector.read
            && let Some(size) = info.size
        {
            return format_bytes(size.bytes);
        }
        if entry.kind != Kind::Dir {
            return format_size(entry);
        }
        match &self.inspector.total {
            Total::Done(total) => format_bytes(total.bytes),
            Total::Failed(why) => why.clone(),
            Total::Unknown if matches!(self.inspector.read, Read::Failed(_)) => "--".into(),
            Total::Unknown => "Calculating…".into(),
        }
    }

    /// Where it is: a click shows it there.
    fn where_row(
        &self,
        parent: PathBuf,
        select: Option<PathBuf>,
        pane: &Entity<Pane>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = paths::abbreviate(&parent);
        let pane = pane.clone();
        fact_row("Where")
            .child(
                div()
                    .id("inspector-where")
                    .debug_selector(|| "inspector-where".into())
                    .role(Role::Link)
                    .aria_label(shown.clone())
                    .flex_1()
                    .min_w_0()
                    .px(px(theme::CRUMB_INSET))
                    .mr(px(-theme::CRUMB_INSET))
                    .rounded_full()
                    .text_right()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis_middle()
                    .text_color(content(0.65))
                    .cursor_pointer()
                    .hover(|d| d.bg(content(0.08)).text_color(content(1.)))
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let (parent, select) = (parent.clone(), select.clone());
                        pane.update(cx, |pane, cx| pane.go_to(parent, select, window, cx));
                    }))
                    .child(shown),
            )
            .into_any_element()
    }

    fn storage_rows(&self, info: &Info, rows: &mut Vec<AnyElement>, cx: &mut Context<Self>) {
        if let Some(volume) = &info.volume {
            rows.push(fact("Volume", volume.name.clone()));
        }
        let access = match (info.permissions.readable, info.permissions.writable) {
            (true, true) => "Read & Write",
            (true, false) => "Read only",
            (false, true) => "Write only",
            (false, false) => "No access",
        };
        rows.push(fact(
            "Permissions",
            format!("{access} ({:o})", info.permissions.mode),
        ));
        rows.push(fact("Owner", info.permissions.owner.clone()));
        if let Some(link) = &info.link {
            rows.push(fact("Link to", paths::abbreviate(link)));
        }
        if info.package {
            rows.push(fact("Package", "Yes".to_string()));
        }
        if info.in_cloud || !info.availability.is_local() {
            let state = match info.availability {
                Availability::Placeholder => "In the cloud",
                Availability::Downloading => "Downloading…",
                _ => "On this Mac",
            };
            rows.push(fact("Availability", state.to_string()));
        }
        if info.availability == Availability::Placeholder {
            let path = info.path.clone();
            let asked = path.clone();
            rows.push(
                div()
                    .flex()
                    .justify_end()
                    .py(px(4.))
                    .child(
                        text_button("inspector-download", "Download", ButtonLook::Secondary)
                            .debug_selector(|| "inspector-download".into())
                            .on_click(cx.listener(move |_, _, window, cx| {
                                let path = path.clone();
                                with_owner(
                                    window,
                                    cx,
                                    move |owner| cloud::download(&path, &Origin::Window, owner),
                                    {
                                        let asked = asked.clone();
                                        move |this: &mut Self, done, _, cx| match done {
                                            Ok(_) if this.shows(&asked) => {
                                                this.schedule_read(Duration::ZERO, cx)
                                            }
                                            Ok(_) => {}
                                            Err(why) => this.toast(why, true, None, cx),
                                        }
                                    },
                                );
                            })),
                    )
                    .into_any_element(),
            );
        }
        if let Some(why) = &info.download_error {
            rows.push(note(why.clone(), danger()));
        }
    }
}

/// The picture, the name and the kind, centered at the top.
fn header(art: AnyElement, name: AnyElement, kind: String) -> AnyElement {
    div()
        .w_full()
        .pt(px(24.))
        .pb(px(8.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .child(art)
        .child(
            div()
                .mt(px(8.))
                .max_w_full()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(FontWeight::MEDIUM)
                .child(name),
        )
        .child(
            div()
                .max_w_full()
                .text_center()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child(kind),
        )
        .into_any_element()
}

/// A section's small icon and label.
fn section(glyph: &'static str, label: &'static str) -> AnyElement {
    div()
        .mt(px(theme::PANE_INSET))
        .h(px(24.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_2()
        .text_size(px(theme::TEXT_SMALL))
        .font_weight(FontWeight::MEDIUM)
        .text_color(content(0.65))
        .child(icon(glyph, 14.).text_color(content(0.55)))
        .child(label)
        .into_any_element()
}

fn fact_row(label: &'static str) -> Div {
    div()
        .min_h(px(24.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_shrink_0()
                .truncate()
                .text_color(content(0.65))
                .child(label),
        )
}

fn fact(label: &'static str, value: String) -> AnyElement {
    fact_ink(label, value, content(1.))
}

fn fact_ink(label: &'static str, value: String, ink: gpui::Hsla) -> AnyElement {
    fact_row(label)
        .child(div().min_w_0().text_right().text_color(ink).child(value))
        .into_any_element()
}

/// A line of its own, such as why there's less to show.
fn note(text: impl Into<gpui::SharedString>, ink: gpui::Hsla) -> AnyElement {
    div()
        .pt(px(8.))
        .text_size(px(theme::TEXT_SMALL))
        .text_color(ink)
        .child(text.into())
        .into_any_element()
}
