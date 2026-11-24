//! Dragging files. A drag out of a pane is AppKit's from its first move, as
//! Finder's is: GPUI hands it a session of file URLs (`external_drag_payload`),
//! which survives ⌘-Tab, and gives the drag back while it's over the window.
//! AppKit keeps the keys meanwhile, and ends the drag for Esc. Drags in from
//! another app arrive as `ExternalPaths`, and a server folder's drag stays the
//! window's own. What's here decides what a drop would do, draws that, and runs
//! it through the operation owner like any copy or move.
//!
//! Outside the app, GPUI's session offers only Copy. That's the right mask for
//! us: a move Finder carried out wouldn't be in the journal, so it couldn't be
//! undone.

use super::{Pane, SpringReturn};
use crate::feedback::Haptic;
use crate::file_icons;
use crate::motion;
use crate::system::Drives;
use crate::theme::{self};
use gpui::{
    AnimationExt, AnyElement, App, Context, Div, Entity, EntityId, ExternalDragPayload,
    FileDragPaths, FontWeight, Modifiers, MouseButton, MouseUpEvent, Pixels, PlatformInput, Point,
    Render, RenderImage, Stateful, Task, Window, div, prelude::*, px,
};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::localsend::Device;
use monodrive_core::remote::RemotePath;
use objc2_app_kit::{NSEvent, NSEventModifierFlags};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// How long a drag rests on a folder before it opens, the pulse included.
pub(in crate::browser) const SPRING_DELAY: Duration = Duration::from_millis(1000);
/// How often a drag checks that the button is still down.
pub(in crate::browser) const BUTTON_CHECK: Duration = Duration::from_millis(50);
/// The drag image's icon.
pub(in crate::browser) const IMAGE_SIDE: f32 = 32.;
/// The pill's height, which its card radius makes fully round.
pub(in crate::browser) const PILL_HEIGHT: f32 = 28.;
pub(in crate::browser) const PILL_WIDEST: f32 = 360.;
/// How far the pill sits from the pointer, clear of the drag image.
pub(in crate::browser) const PILL_OFFSET: Point<f32> = Point { x: 24., y: 4. };

/// What a person is dragging out of a pane: the selection, once it's started.
pub(in crate::browser) struct FileDrag;

/// What a person is dragging out of a server folder. Nothing leaves the
/// window: another app would need a real local file, and none exists yet.
pub(in crate::browser) struct ServerDrag;

/// A place on screen that a drop can land on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::browser) enum Spot {
    Row(EntityId, usize),
    Pane(EntityId),
    Crumb(EntityId, usize),
    Tab(usize),
    /// A sidebar row, by the sidebar's numbering.
    Place(usize),
    /// A device on the LocalSend page, by its place there.
    Device(usize),
}

/// What a drop at the pointer would land on.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::browser) enum Target {
    /// Into a folder, named as the person sees it there.
    Folder {
        spot: Spot,
        path: PathBuf,
        name: String,
    },
    /// Into Favorites, before the one at this position.
    Pin(usize),
    /// Into a folder on a server, named with its server: "srv on box".
    Server {
        spot: Spot,
        at: RemotePath,
        name: String,
    },
    /// Sent with AirDrop, which asks who to.
    AirDrop(Spot),
    /// Sent to a device with LocalSend.
    LocalSend { spot: Spot, device: Box<Device> },
    /// Held by the LocalSend page, for the device clicked next.
    LocalSendPage(Spot),
}

/// What a drop would do.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::browser) enum Decision {
    Send {
        paths: Vec<PathBuf>,
        into: PathBuf,
        name: String,
        moving: bool,
    },
    Pin {
        paths: Vec<PathBuf>,
        at: usize,
    },
    /// Copies up to a server; nothing moves to one, which has no Trash.
    Upload {
        paths: Vec<PathBuf>,
        into: RemotePath,
        name: String,
    },
    /// Copies down from a server.
    Download {
        paths: Vec<RemotePath>,
        into: PathBuf,
        name: String,
    },
    AirDrop(Vec<PathBuf>),
    /// Sends a copy to a device; nothing here changes.
    LocalSend {
        paths: Vec<PathBuf>,
        device: Box<Device>,
    },
    /// Opens the LocalSend page holding these.
    ChooseDevice(Vec<PathBuf>),
    /// Nothing, for this reason.
    Nothing(String),
}

/// Finder's rule: within a volume a drag moves and across volumes it copies; ⌥
/// forces a copy and ⌘ a move. What's already in the folder stays out of it,
/// and a folder can't go inside itself. `same_volume` is false too for a volume
/// not known yet, since a copy can't surprise anyone.
pub(in crate::browser) fn decide(
    sources: &[PathBuf],
    target: &Target,
    keys: Modifiers,
    same_volume: bool,
) -> Decision {
    let (into, name) = match target {
        Target::Pin(at) => {
            return Decision::Pin {
                paths: sources.to_vec(),
                at: *at,
            };
        }
        Target::AirDrop(_) => return Decision::AirDrop(sources.to_vec()),
        Target::Folder { path, name, .. } => (path, name),
        Target::Server { at, name, .. } => {
            return Decision::Upload {
                paths: sources.to_vec(),
                into: at.clone(),
                name: name.clone(),
            };
        }
        Target::LocalSend { device, .. } => {
            return Decision::LocalSend {
                paths: sources.to_vec(),
                device: device.clone(),
            };
        }
        Target::LocalSendPage(_) => return Decision::ChooseDevice(sources.to_vec()),
    };
    if let Some(source) = sources.iter().find(|source| into.starts_with(source)) {
        let name = shown_name(source);
        return Decision::Nothing(format!("\"{name}\" can't go inside itself."));
    }
    let paths: Vec<PathBuf> = sources
        .iter()
        .filter(|source| source.parent() != Some(into))
        .cloned()
        .collect();
    if paths.is_empty() {
        return Decision::Nothing(format!("Already in {name}"));
    }
    let moving = !keys.alt && (keys.platform || same_volume);
    Decision::Send {
        paths,
        into: into.clone(),
        name: name.clone(),
        moving,
    }
}

impl Decision {
    /// What the pill says.
    pub(in crate::browser) fn says(&self) -> String {
        match self {
            Self::Send {
                paths,
                name,
                moving,
                ..
            } => {
                let verb = if *moving { "Move" } else { "Copy" };
                format!("{verb} {} to {name}", count_of(paths, "items"))
            }
            Self::Pin { paths, .. } => {
                format!("Pin {} to Favorites", count_of(paths, "folders"))
            }
            Self::Upload { paths, name, .. } => {
                format!("Copy {} to {name}", count_of(paths, "items"))
            }
            Self::AirDrop(paths) => format!("AirDrop {}", count_of(paths, "items")),
            Self::LocalSend { paths, device } => {
                format!("Send {} to {}", count_of(paths, "items"), device.alias)
            }
            Self::ChooseDevice(paths) => {
                format!("Send {} with LocalSend", count_of(paths, "items"))
            }
            Self::Download { paths, name, .. } => {
                let what = match paths.as_slice() {
                    [one] => one.name().unwrap_or(one.source()).to_owned(),
                    many => format!("{} items", many.len()),
                };
                format!("Copy {what} to {name}")
            }
            Self::Nothing(why) => why.clone(),
        }
    }
}

pub(in crate::browser) fn count_of(paths: &[PathBuf], noun: &str) -> String {
    match paths {
        [one] => shown_name(one),
        many => format!("{} {noun}", many.len()),
    }
}

pub(in crate::browser) fn shown_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What a drop of items from a server would do: a copy down into a folder
/// here, and nothing anywhere else.
pub(in crate::browser) fn decide_from_server(sources: &[RemotePath], target: &Target) -> Decision {
    match target {
        Target::Folder { path, name, .. } => Decision::Download {
            paths: sources.to_vec(),
            into: path.clone(),
            name: name.clone(),
        },
        Target::Server { at, name, .. }
            if sources.iter().all(|s| s.parent().as_ref() == Some(at)) =>
        {
            Decision::Nothing(format!("Already in {name}"))
        }
        Target::Server { .. } => Decision::Nothing("Copy between a server and this Mac.".into()),
        Target::Pin(_) => Decision::Nothing("Only folders on this Mac go in Favorites.".into()),
        Target::AirDrop(_) => Decision::Nothing("AirDrop sends files on this Mac.".into()),
        Target::LocalSend { .. } | Target::LocalSendPage(_) => {
            Decision::Nothing("LocalSend sends files on this Mac.".into())
        }
    }
}

/// `entries` without what's inside another of them, which goes along with it.
pub(in crate::browser) fn outermost(entries: Vec<&Entry>) -> Vec<&Entry> {
    let all: HashSet<&Path> = entries.iter().map(|entry| entry.path.as_path()).collect();
    let inside = |entry: &Entry| entry.path.ancestors().skip(1).any(|up| all.contains(up));
    entries.iter().copied().filter(|e| !inside(e)).collect()
}

/// The device holding where `path` leads, reading the disk: never a place that
/// could raise a privacy prompt, which then simply stays unknown.
pub(in crate::browser) fn device_of(drives: Option<&Arc<dyn Drives>>, path: &Path) -> Option<u64> {
    match drives {
        Some(drives) if drives.needs_consent(path) => None,
        Some(drives) => drives.device(path),
        None => std::fs::metadata(path).ok().map(|meta| meta.dev()),
    }
}

/// The devices holding `sources`, if all can be told. A link is where its
/// parent is, whatever it leads to, since only the link would move.
pub(in crate::browser) fn devices_of(
    drives: Option<&Arc<dyn Drives>>,
    sources: &[PathBuf],
) -> Option<HashSet<u64>> {
    sources
        .iter()
        .map(|source| {
            let holder = if source.is_symlink() {
                source.parent()?
            } else {
                source.as_path()
            };
            device_of(drives, holder)
        })
        .collect()
}

/// The modifiers held now. During a drag AppKit runs, from another app or back
/// from outside the window, the window hears no key changes; AppKit still
/// knows. A window with no AppKit behind it, as in tests, has what it was told.
pub(in crate::browser) fn held_keys(window: &Window) -> Modifiers {
    if crate::traffic_lights::ns_window(window).is_none() {
        return window.modifiers();
    }
    let flags = NSEvent::modifierFlags_class();
    Modifiers {
        alt: flags.contains(NSEventModifierFlags::Option),
        platform: flags.contains(NSEventModifierFlags::Command),
        ..Modifiers::default()
    }
}

impl Session {
    /// Whether all of it is known to be on `target`'s volume.
    fn same_volume(&self, target: &Target) -> bool {
        let Target::Folder { path, .. } = target else {
            return false;
        };
        match (&self.source_devices, self.target_devices.get(path)) {
            (Some(sources), Some(Some(device))) => sources.iter().all(|d| d == device),
            _ => false,
        }
    }
}

struct Session {
    sources: Vec<PathBuf>,
    /// What's dragged off a server, in place of `sources`.
    from_server: Vec<RemotePath>,
    /// All of it is folders, as pinning asks.
    folders: bool,
    /// What the pointer is over now, as this move finds it.
    target: Option<Target>,
    /// The devices holding the sources, once read. Asked for once.
    source_devices: Option<HashSet<u64>>,
    sources_asked: bool,
    /// The device of each target met in this drag, `None` until it's read or
    /// if it can't be. Being in here means it's been asked for.
    target_devices: HashMap<PathBuf, Option<u64>>,
    /// What the frame shows, worked out once per target, keys and answer about
    /// its volume, since a selection can run to thousands.
    decided: Option<Decided>,
    /// What the spring timer below is for, or was: a target that has opened
    /// doesn't open again while the pointer stays on it.
    spring_for: Option<Target>,
    spring_timer: Option<Task<()>>,
    /// The outline's pulse is on, just before opening.
    pulsing: bool,
    /// What Esc goes back to: the tab on screen and each pane opened in.
    spring_tab: Option<EntityId>,
    spring_returns: Vec<(EntityId, SpringReturn)>,
    /// Watches the button while the window owns the drag.
    button_watch: Option<Task<()>>,
    /// The files came from another app, whose drag AppKit ends.
    external: bool,
}

/// A decision, with the target, ⌥, ⌘ and same volume it was made for.
type Decided = (Target, (bool, bool, bool), Decision);

/// The tap a new decision earns over the last one: ⌥ or ⌘ turning a move into a
/// copy or back, or the insertion line in Favorites finding a slot. A volume's
/// answer arriving can turn a copy into a move too, but no hand did that.
pub(in crate::browser) fn tap(was: Option<&Decided>, now: &Decided) -> Option<Haptic> {
    let (target, (alt, cmd, _), decision) = now;
    match (was, decision) {
        (Some((_, _, Decision::Pin { at: before, .. })), Decision::Pin { at, .. })
            if before == at =>
        {
            None
        }
        (_, Decision::Pin { .. }) => Some(Haptic::Alignment),
        (
            Some((
                before,
                (was_alt, was_cmd, _),
                Decision::Send {
                    moving: was_moving, ..
                },
            )),
            Decision::Send { moving, .. },
        ) if before == target && (was_alt, was_cmd) != (alt, cmd) && was_moving != moving => {
            Some(Haptic::LevelChange)
        }
        _ => None,
    }
}

/// What a cancelled drag goes back to.
#[derive(Default)]
pub(in crate::browser) struct SpringBack {
    pub(in crate::browser) tab: Option<EntityId>,
    pub(in crate::browser) panes: Vec<(EntityId, SpringReturn)>,
}

#[derive(Default)]
struct DragState {
    /// What the drag out of a pane took when it started, each path with
    /// whether it's a folder, whatever the keys select meanwhile.
    carried: Vec<(PathBuf, bool)>,
    /// The icon of what the drag was grabbed by, for AppKit to draw.
    carried_image: Option<Arc<RenderImage>>,
    /// What the drag out of a server folder took, in place of `carried`.
    carried_from_server: Vec<RemotePath>,
    session: Option<Session>,
}

/// The window's drag of files, if there is one. The panes and the sidebar hold
/// it to offer what's under the pointer, and draw from it. GPUI redraws the
/// window after each move of a drag, so nothing here asks for a frame.
#[derive(Clone, Default)]
pub(in crate::browser) struct Drag(Rc<RefCell<DragState>>);

impl Drag {
    /// Files from another app start a drag, which AppKit ends.
    pub(in crate::browser) fn begin_external(&self, sources: Vec<PathBuf>) {
        self.begin(sources, false);
        if let Some(session) = self.0.borrow_mut().session.as_mut() {
            session.external = true;
        }
    }

    /// Whether `sources` are all folders, as the disk said, for the drag
    /// that still has them.
    pub(in crate::browser) fn folders_read(&self, sources: &[PathBuf], folders: bool) {
        if let Some(session) = self.0.borrow_mut().session.as_mut()
            && session.sources == sources
        {
            session.folders = folders;
        }
    }

    pub(in crate::browser) fn active(&self) -> bool {
        self.0.borrow().session.is_some()
    }

    pub(in crate::browser) fn begin(&self, sources: Vec<PathBuf>, folders: bool) {
        self.0.borrow_mut().session = Some(Session {
            sources,
            from_server: Vec::new(),
            folders,
            target: None,
            source_devices: None,
            sources_asked: false,
            target_devices: HashMap::new(),
            decided: None,
            spring_for: None,
            spring_timer: None,
            pulsing: false,
            spring_tab: None,
            spring_returns: Vec::new(),
            button_watch: None,
            external: false,
        });
    }

    /// A drag out of a pane starts, with `files`, under `image`.
    pub(in crate::browser) fn carry(
        &self,
        files: Vec<(PathBuf, bool)>,
        image: Option<Arc<RenderImage>>,
    ) {
        let mut state = self.0.borrow_mut();
        state.carried = files;
        state.carried_image = image;
        state.carried_from_server.clear();
        drop(state);
        self.resume();
    }

    /// A drag out of a server folder starts, with `items`.
    pub(in crate::browser) fn carry_from_server(&self, items: Vec<RemotePath>) {
        let mut state = self.0.borrow_mut();
        state.carried.clear();
        state.carried_image = None;
        state.carried_from_server = items;
        drop(state);
        self.resume();
    }

    /// The session of what's carried.
    pub(in crate::browser) fn resume(&self) {
        let (sources, folders, from_server) = {
            let state = self.0.borrow();
            (
                state.carried.iter().map(|(path, _)| path.clone()).collect(),
                !state.carried.is_empty() && state.carried.iter().all(|(_, folder)| *folder),
                state.carried_from_server.clone(),
            )
        };
        self.begin(sources, folders);
        if let Some(session) = self.0.borrow_mut().session.as_mut() {
            session.from_server = from_server;
        }
    }

    /// A move starts: what's under the pointer comes from the offers after it.
    /// Says whether there's a session to offer to.
    pub(in crate::browser) fn next_move(&self) -> bool {
        let mut state = self.0.borrow_mut();
        if let Some(session) = state.session.as_mut() {
            session.target = None;
        }
        state.session.is_some()
    }

    /// The innermost element's offer comes first, and stands.
    pub(in crate::browser) fn offer(&self, target: Target) {
        if let Some(session) = self.0.borrow_mut().session.as_mut()
            && session.target.is_none()
        {
            session.target = Some(target);
        }
    }

    /// Everything dragged is a folder, so it can be pinned.
    pub(in crate::browser) fn folders(&self) -> bool {
        self.0.borrow().session.as_ref().is_some_and(|s| s.folders)
    }

    /// Before a frame: works out what a drop would do, with the tap a change
    /// in that earns.
    pub(in crate::browser) fn settle(&self, keys: Modifiers) -> Option<Haptic> {
        let mut state = self.0.borrow_mut();
        let session = state.session.as_mut()?;
        let Some(target) = session.target.clone() else {
            session.decided = None;
            return None;
        };
        let same_volume = session.same_volume(&target);
        let pressed = (keys.alt, keys.platform, same_volume);
        if !session
            .decided
            .as_ref()
            .is_some_and(|(was, held, _)| *was == target && *held == pressed)
        {
            let decision = if session.from_server.is_empty() {
                decide(&session.sources, &target, keys, same_volume)
            } else {
                decide_from_server(&session.from_server, &target)
            };
            let decided = (target.clone(), pressed, decision);
            let tap = tap(session.decided.as_ref(), &decided);
            session.decided = Some(decided);
            return tap;
        }
        None
    }

    /// Reads what the frame shows a drop would do.
    pub(in crate::browser) fn decision<R>(
        &self,
        read: impl FnOnce(&Target, &Decision) -> R,
    ) -> Option<R> {
        let state = self.0.borrow();
        let (target, _, decision) = state.session.as_ref()?.decided.as_ref()?;
        Some(read(target, decision))
    }

    /// Whether `spot` is what a drop would go into.
    pub(in crate::browser) fn outlines(&self, spot: &Spot) -> bool {
        self.decision(|target, decision| {
            let lands = matches!(
                decision,
                Decision::Send { .. }
                    | Decision::Upload { .. }
                    | Decision::Download { .. }
                    | Decision::AirDrop(_)
                    | Decision::LocalSend { .. }
                    | Decision::ChooseDevice(_)
            );
            lands
                && matches!(
                    target,
                    Target::Folder { spot: at, .. }
                        | Target::Server { spot: at, .. }
                        | Target::AirDrop(at)
                        | Target::LocalSend { spot: at, .. }
                        | Target::LocalSendPage(at) if at == spot
                )
        })
        .unwrap_or(false)
    }

    /// The position in Favorites a drop would pin at.
    pub(in crate::browser) fn pins_at(&self) -> Option<usize> {
        self.decision(|_, decision| match decision {
            Decision::Pin { at, .. } => Some(*at),
            _ => None,
        })
        .flatten()
    }

    /// What's under the pointer, if it opens when rested on: a folder, a tab, a
    /// favorite or a drive that would take the files.
    pub(in crate::browser) fn springable(&self) -> Option<Target> {
        self.decision(|target, decision| {
            let opens = matches!(decision, Decision::Send { .. } | Decision::Download { .. })
                && matches!(
                    target,
                    Target::Folder {
                        spot: Spot::Row(..) | Spot::Tab(_) | Spot::Place(_),
                        ..
                    }
                );
            opens.then(|| target.clone())
        })
        .flatten()
    }

    /// Whether the pointer has come to a new place to time, which ends the
    /// timer for the last one.
    pub(in crate::browser) fn spring_moved_to(&self, target: &Option<Target>) -> bool {
        let mut state = self.0.borrow_mut();
        let Some(session) = state.session.as_mut() else {
            return false;
        };
        if session.spring_for == *target {
            return false;
        }
        session.spring_for = target.clone();
        session.spring_timer = None;
        session.pulsing = false;
        true
    }

    pub(in crate::browser) fn keep_spring_timer(&self, timer: Task<()>) {
        if let Some(session) = self.0.borrow_mut().session.as_mut() {
            session.spring_timer = Some(timer);
        }
    }

    pub(in crate::browser) fn set_pulsing(&self, on: bool) {
        if let Some(session) = self.0.borrow_mut().session.as_mut() {
            session.pulsing = on;
        }
    }

    /// `outline`, with its one pulse if it's about to open.
    pub(in crate::browser) fn pulsed(&self, outline: Div) -> AnyElement {
        let pulsing = self.0.borrow().session.as_ref().is_some_and(|s| s.pulsing);
        if !pulsing {
            return outline.into_any_element();
        }
        outline
            .with_animation("spring-pulse", motion::pulse(), |outline, t| {
                outline.opacity(0.3 + 0.7 * (2. * t - 1.).abs())
            })
            .into_any_element()
    }

    /// The drag is outside the window, where AppKit carries it on: nothing
    /// here is under the pointer, so nothing shows a drop or waits to open.
    pub(in crate::browser) fn away(&self) {
        if let Some(session) = self.0.borrow_mut().session.as_mut() {
            session.target = None;
        }
    }

    /// The drag is over without a drop here, which takes the session first:
    /// the window goes back to where it began, as for Esc, which AppKit keeps
    /// for itself. A drag from another app may only have left, which AppKit
    /// doesn't tell apart, so it just ends.
    pub(in crate::browser) fn end(&self) -> Option<SpringBack> {
        let external = self.0.borrow().session.as_ref()?.external;
        let back = self.cancel();
        let sprang = back.tab.is_some() || !back.panes.is_empty();
        (sprang && !external).then_some(back)
    }

    /// The drag ends for Esc: what to go back to.
    pub(in crate::browser) fn cancel(&self) -> SpringBack {
        let session = self.0.borrow_mut().session.take();
        session.map_or_else(SpringBack::default, |session| SpringBack {
            tab: session.spring_tab,
            panes: session.spring_returns,
        })
    }

    /// There's a drag of the window's own, not yet watched.
    pub(in crate::browser) fn needs_button_watch(&self) -> bool {
        let state = self.0.borrow();
        let watchable = |s: &Session| !s.external && s.button_watch.is_none();
        state.session.as_ref().is_some_and(watchable)
    }

    pub(in crate::browser) fn keep_button_watch(&self, watch: Task<()>) {
        if let Some(session) = self.0.borrow_mut().session.as_mut() {
            session.button_watch = Some(watch);
        }
    }

    /// The sources, the first time they're asked for.
    pub(in crate::browser) fn sources_to_read(&self) -> Option<Vec<PathBuf>> {
        let mut state = self.0.borrow_mut();
        let session = state.session.as_mut()?;
        (!std::mem::replace(&mut session.sources_asked, true)).then(|| session.sources.clone())
    }

    /// The target's folder, the first time this drag meets it.
    pub(in crate::browser) fn target_to_read(&self) -> Option<PathBuf> {
        let mut state = self.0.borrow_mut();
        let session = state.session.as_mut()?;
        let Some(Target::Folder { path, .. }) = &session.target else {
            return None;
        };
        let path = path.clone();
        let new = !session.target_devices.contains_key(&path);
        new.then(|| {
            session.target_devices.insert(path.clone(), None);
            path
        })
    }

    /// An answer that isn't for this drag's sources is an old one.
    pub(in crate::browser) fn sources_read(
        &self,
        sources: &[PathBuf],
        devices: Option<HashSet<u64>>,
    ) {
        if let Some(session) = self.0.borrow_mut().session.as_mut()
            && session.sources == sources
        {
            session.source_devices = devices;
        }
    }

    pub(in crate::browser) fn target_read(&self, folder: &Path, device: Option<u64>) {
        if let Some(session) = self.0.borrow_mut().session.as_mut()
            && let Some(known) = session.target_devices.get_mut(folder)
        {
            *known = device;
        }
    }

    /// Keeps where pane `id` is, `before` it moves, the first time this drag
    /// opens something there.
    pub(in crate::browser) fn remember_pane(
        &self,
        id: EntityId,
        before: impl FnOnce() -> SpringReturn,
    ) {
        if let Some(session) = self.0.borrow_mut().session.as_mut()
            && !session.spring_returns.iter().any(|(kept, _)| *kept == id)
        {
            session.spring_returns.push((id, before()));
        }
    }

    pub(in crate::browser) fn remember_tab(&self, shown: EntityId) {
        if let Some(session) = self.0.borrow_mut().session.as_mut()
            && session.spring_tab.is_none()
        {
            session.spring_tab = Some(shown);
        }
    }

    /// The button came up, and what's under it has just offered itself: what
    /// the pill showed, as long as that's still what's there. The drag ends
    /// either way.
    pub(in crate::browser) fn finish(&self) -> Option<Decision> {
        let session = self.0.borrow_mut().session.take()?;
        let (shown, _, decision) = session.decided?;
        (session.target? == shown).then_some(decision)
    }
}

/// The button is up and the drag still thinks it's down: the mouse-up that went
/// missing, at the same place, through the same path as a real one.
pub(in crate::browser) fn release_lost_drag(window: &mut Window, cx: &mut App) {
    if !cx.has_active_drag() {
        return;
    }
    let event = MouseUpEvent {
        button: MouseButton::Left,
        position: window.mouse_position(),
        modifiers: window.modifiers(),
        click_count: 1,
    };
    window.dispatch_event(PlatformInput::MouseUp(event), cx);
}

/// Makes `element` offer what `offer` does while the pointer's over it: on
/// each move, for the pill, and at release, to check the pill against what's
/// on screen.
pub(in crate::browser) fn offers<E: InteractiveElement>(
    element: E,
    offer: impl Fn(&mut App) + 'static,
) -> E {
    let offer = Rc::new(offer);
    let at_release = offer.clone();
    element
        .on_mouse_move(move |_, _, cx| offer(cx))
        .on_mouse_up(MouseButton::Left, move |_, _, cx| at_release(cx))
}

/// What the drag image draws: an item here, or one on a server by its name.
pub(in crate::browser) enum Picture {
    Entry(Entry),
    Named(Kind, String),
}

/// The icon under the pointer, with a count when there's more than one.
pub(in crate::browser) struct DragImage {
    picture: Option<Picture>,
    count: usize,
    /// Where the press was in the element GPUI draws this over.
    grabbed: Point<Pixels>,
}

impl DragImage {
    pub(in crate::browser) fn new(
        picture: Option<Picture>,
        count: usize,
        grabbed: Point<Pixels>,
    ) -> Self {
        Self {
            picture,
            count,
            grabbed,
        }
    }
}

impl Render for DragImage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let inset = |at: Pixels| at - px(IMAGE_SIDE / 2.);
        div()
            .pl(inset(self.grabbed.x).max(px(0.)))
            .pt(inset(self.grabbed.y).max(px(0.)))
            .child(
                div()
                    .relative()
                    .size(px(IMAGE_SIDE))
                    .children(self.picture.as_ref().map(|picture| match picture {
                        Picture::Entry(entry) => file_icons::icon(entry, IMAGE_SIDE, window, cx),
                        Picture::Named(kind, name) => {
                            file_icons::kind_icon(*kind, Path::new(name), IMAGE_SIDE, window, cx)
                        }
                    }))
                    .when(self.count > 1, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top(px(-4.))
                                .right(px(-8.))
                                .h(px(16.))
                                .min_w(px(16.))
                                .px_1()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .font_family(theme::UI_FONT)
                                .text_size(px(theme::TEXT_CAPTION))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::on_accent())
                                .bg(theme::accent())
                                .child(self.count.to_string()),
                        )
                    }),
            )
    }
}

impl Pane {
    /// Makes `element` start a drag of the selection, its press no rectangle.
    pub(in crate::browser) fn grab(
        element: Stateful<Div>,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let pane = cx.entity();
        element
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.grabbed = true;
                    // The drag's image, ready by the time the drag starts.
                    if let Some(entry) = this.entries.get(ix) {
                        file_icons::image(entry, IMAGE_SIDE, window, cx);
                    }
                }),
            )
            .on_drag(FileDrag, {
                let pane = pane.clone();
                move |_, grabbed, window, cx| {
                    pane.update(cx, |pane, cx| pane.begin_drag(ix, grabbed, window, cx))
                }
            })
            .external_drag_payload(move |_: &FileDrag, _, cx| pane.read(cx).drag_out())
    }

    /// The drag has started on row `ix`: a row outside the selection joins it
    /// as a click would, and all of it goes.
    fn begin_drag(
        &mut self,
        ix: usize,
        grabbed: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<DragImage> {
        let entry = self.entries.get(ix).cloned();
        if let Some(entry) = &entry
            && !self.selection.contains(&entry.path)
        {
            let keys = window.modifiers();
            if keys.platform {
                self.selection.toggle(entry);
            } else if keys.shift {
                self.selection.extend(&self.entries, ix);
            } else {
                self.selection.select(entry);
            }
        }
        let dragged = outermost(self.selection.in_order(&self.entries));
        let count = dragged.len();
        let files = dragged
            .into_iter()
            .map(|entry| (entry.path.clone(), entry.kind == Kind::Dir));
        let image = entry
            .as_ref()
            .and_then(|entry| file_icons::image(entry, IMAGE_SIDE, window, cx));
        self.drag.carry(files.collect(), image);
        cx.notify();
        cx.new(|_| DragImage::new(entry.map(Picture::Entry), count, grabbed))
    }

    /// What AppKit gets as the drag starts: file URLs, built without asking
    /// the disk, so nothing downloads a cloud placeholder, and our icon of
    /// what was grabbed.
    pub(in crate::browser) fn drag_out(&self) -> Option<ExternalDragPayload> {
        let state = self.drag.0.borrow();
        let files = FileDragPaths::new(state.carried.clone());
        let files = match &state.carried_image {
            Some(image) => files.with_image(image.clone()),
            None => files,
        };
        Some(ExternalDragPayload::Files(files))
    }

    /// Makes row or cell `ix` offer itself to a drag.
    pub(in crate::browser) fn takes_drops(
        element: Stateful<Div>,
        ix: usize,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let pane = cx.entity();
        offers(element, move |cx| {
            let id = pane.entity_id();
            pane.read(cx).offer_row(ix, id);
        })
    }

    /// The pointer's over row `ix`, on the way to a drop: a folder takes it,
    /// and a file in an open folder gives way to that folder. Anything else
    /// offers nothing, so it goes to what's behind the row.
    fn offer_row(&self, ix: usize, id: EntityId) {
        let Some(row) = self.folder_row(ix) else {
            return;
        };
        let entry = &self.entries[row];
        self.drag.offer(Target::Folder {
            spot: Spot::Row(id, row),
            path: entry.path.clone(),
            name: entry.name.clone(),
        });
    }

    /// The row a drop on row `ix` goes into.
    fn folder_row(&self, ix: usize) -> Option<usize> {
        let takes = |e: &Entry| e.kind == Kind::Dir && !crate::workspace::is_package(&e.path);
        if takes(self.entries.get(ix)?) {
            return Some(ix);
        }
        let depth = *self.depths.get(ix)?;
        if depth == 0 {
            return None;
        }
        (0..ix)
            .rev()
            .find(|row| self.depths.get(*row).is_some_and(|d| *d < depth))
            .filter(|row| takes(&self.entries[*row]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::tests::drag::{carry, draw, hover, says};
    use crate::browser::tests::{
        browse, folder as project_folder, in_pane, own, press, release, restart, row, shown,
    };
    use gpui::{ExternalDragPayload as Payload, TestAppContext};

    #[gpui::test]
    fn esc_back_to_a_tab_keeps_the_selection_over_its_search_results(cx: &mut TestAppContext) {
        use crate::onboarding::{IndexGate, tests::Fake};
        let (_dir, project) = project_folder();
        cx.update(|cx| cx.set_global(IndexGate::new(Arc::new(Fake::default()), true)));
        let (browser, cx) = browse(&project, cx);
        press(cx, "down down down");
        let source = browser.read_with(cx, |browser, _| browser.pane().clone());
        let chosen = source.read_with(cx, |pane, _| pane.chosen());
        source.update(cx, |pane, _| pane.query = "a".into());
        browser.update_in(cx, |browser, window, cx| {
            browser.new_tab(
                crate::browser::pane::Stop::folder(project.clone()),
                window,
                cx,
            );
            let back = SpringBack {
                tab: Some(source.entity_id()),
                panes: Vec::new(),
            };
            browser.spring_back(back, window, cx);
        });
        source.update(cx, |pane, _| {
            pane.pick_selection(None);
            assert_eq!(pane.chosen(), chosen);
        });
        cx.run_until_parked();
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn folder(path: &str, name: &str) -> Target {
        Target::Folder {
            spot: Spot::Tab(0),
            path: path.into(),
            name: name.into(),
        }
    }

    fn keys(alt: bool, platform: bool) -> Modifiers {
        Modifiers {
            alt,
            platform,
            ..Modifiers::default()
        }
    }

    #[test]
    fn a_drag_moves_within_a_volume_copies_across_and_the_keys_force_either() {
        let moving = |same_volume: bool, keys: Modifiers| {
            let target = folder("/b", "x");
            match decide(&paths(&["/a/f"]), &target, keys, same_volume) {
                Decision::Send { moving, .. } => moving,
                other => panic!("{other:?}"),
            }
        };
        let plain = Modifiers::default();
        assert!(moving(true, plain));
        assert!(!moving(false, plain));
        // ⌥ forces a copy, ⌘ a move, and ⌥ wins with both, since it's the safe one.
        assert!(!moving(true, keys(true, false)));
        assert!(moving(false, keys(false, true)));
        assert!(!moving(false, keys(true, true)));
    }

    #[test]
    fn a_target_not_read_yet_is_a_copy_until_its_volume_comes() {
        let drag = Drag::default();
        drag.begin(paths(&["/a/f"]), false);
        let target = folder("/b", "b");
        let settle_and_say = |drag: &Drag| {
            drag.offer(target.clone());
            drag.settle(Modifiers::default());
            drag.decision(|_, d| d.says()).unwrap()
        };
        assert_eq!(settle_and_say(&drag), "Copy f to b");
        assert_eq!(drag.sources_to_read(), Some(paths(&["/a/f"])));
        assert_eq!(drag.sources_to_read(), None);
        assert_eq!(drag.target_to_read(), Some("/b".into()));
        assert_eq!(drag.target_to_read(), None);
        // The target alone says nothing, and neither do the sources alone.
        drag.target_read(Path::new("/b"), Some(1));
        assert_eq!(settle_and_say(&drag), "Copy f to b");
        drag.sources_read(&paths(&["/a/f"]), Some(HashSet::from([1])));
        assert_eq!(settle_and_say(&drag), "Move f to b");
        // Another target starts from a copy again, and its own volume decides.
        drag.next_move();
        let other = folder("/ext", "ext");
        drag.offer(other);
        drag.settle(Modifiers::default());
        assert_eq!(drag.decision(|_, d| d.says()).unwrap(), "Copy f to ext");
        assert_eq!(drag.target_to_read(), Some("/ext".into()));
        drag.target_read(Path::new("/ext"), Some(2));
        drag.settle(Modifiers::default());
        assert_eq!(drag.decision(|_, d| d.says()).unwrap(), "Copy f to ext");
    }

    #[test]
    fn sources_on_two_volumes_are_a_copy() {
        let drag = Drag::default();
        drag.begin(paths(&["/a/f", "/ext/g"]), false);
        drag.offer(folder("/b", "b"));
        drag.settle(Modifiers::default());
        drag.target_to_read();
        drag.target_read(Path::new("/b"), Some(1));
        drag.sources_read(&paths(&["/a/f", "/ext/g"]), Some(HashSet::from([1, 2])));
        drag.offer(folder("/b", "b"));
        drag.settle(Modifiers::default());
        assert_eq!(drag.decision(|_, d| d.says()).unwrap(), "Copy 2 items to b");
    }

    #[test]
    fn a_folder_cant_go_inside_itself_and_what_is_there_stays_out() {
        let nothing = Decision::Nothing("\"docs\" can't go inside itself.".into());
        for into in ["/a/docs", "/a/docs/deep/er"] {
            let target = folder(into, "x");
            let decided = decide(&paths(&["/a/docs"]), &target, Modifiers::default(), true);
            assert_eq!(decided, nothing, "{into}");
        }
        // A name that only starts the same isn't inside it.
        let target = folder("/a/docs2", "docs2");
        let decided = decide(&paths(&["/a/docs"]), &target, Modifiers::default(), true);
        assert!(matches!(decided, Decision::Send { .. }));
        // ⌥ doesn't turn a drop on its own folder into a duplicate.
        let target = folder("/a", "a");
        let decided = decide(&paths(&["/a/f"]), &target, keys(true, false), true);
        assert_eq!(decided, Decision::Nothing("Already in a".into()));
        // Search rows, where only one is in the folder already: the other goes.
        let decided = decide(
            &paths(&["/a/f", "/c/g"]),
            &target,
            Modifiers::default(),
            true,
        );
        assert_eq!(decided.says(), "Move g to a");
    }

    #[test]
    fn airdrop_sends_what_is_dropped_whatever_the_keys_and_moves_nothing() {
        let airdrop = Target::AirDrop(Spot::Place(4));
        let sources = paths(&["/a/f", "/a/g"]);
        let decided = decide(&sources, &airdrop, keys(false, true), true);
        assert_eq!(decided, Decision::AirDrop(sources));
        assert_eq!(decided.says(), "AirDrop 2 items");
        let remote = RemotePath::new("box", "/srv/f").unwrap();
        assert!(matches!(
            decide_from_server(&[remote], &airdrop),
            Decision::Nothing(_)
        ));
    }

    #[test]
    fn a_drop_on_a_localsend_device_sends_whatever_the_keys_and_moves_nothing() {
        let device = monodrive_core::localsend::Device {
            alias: "Pixel 8".into(),
            fingerprint: "AB".into(),
            kind: monodrive_core::localsend::Kind::Phone,
            model: None,
            host: "192.168.1.20".into(),
            port: 53317,
            https: true,
        };
        let target = Target::LocalSend {
            spot: Spot::Device(0),
            device: Box::new(device.clone()),
        };
        let sources = paths(&["/a/f", "/a/g"]);
        for keys in [keys(false, false), keys(true, false), keys(false, true)] {
            let decided = decide(&sources, &target, keys, true);
            assert_eq!(
                decided,
                Decision::LocalSend {
                    paths: sources.clone(),
                    device: Box::new(device.clone())
                }
            );
            assert_eq!(decided.says(), "Send 2 items to Pixel 8");
        }
        let page = Target::LocalSendPage(Spot::Place(9));
        let decided = decide(&sources, &page, keys(false, true), true);
        assert_eq!(decided, Decision::ChooseDevice(sources.clone()));
        assert_eq!(decided.says(), "Send 2 items with LocalSend");
        let remote = RemotePath::new("box", "/srv/f").unwrap();
        assert!(matches!(
            decide_from_server(&[remote], &target),
            Decision::Nothing(_)
        ));
    }

    #[test]
    fn a_tap_comes_from_the_keys_flipping_a_drop_or_a_new_slot_not_from_the_disk() {
        let sources = paths(&["/a/f"]);
        let at = |target: Target, alt: bool, same_volume: bool| -> Decided {
            let decision = decide(&sources, &target, keys(alt, false), same_volume);
            (target, (alt, false, same_volume), decision)
        };
        let docs = || folder("/a/docs", "docs");
        let moving = at(docs(), false, true);
        // ⌥ turns the move into a copy, and letting go turns it back.
        let copying = at(docs(), true, true);
        assert_eq!(tap(Some(&moving), &copying), Some(Haptic::LevelChange));
        assert_eq!(tap(Some(&copying), &moving), Some(Haptic::LevelChange));
        // The volume's answer turns a copy into a move with no key pressed.
        let unknown = at(docs(), false, false);
        assert_eq!(tap(Some(&unknown), &moving), None);
        // A new folder under the pointer is no flip, whatever it does.
        let other = at(folder("/b", "b"), true, true);
        assert_eq!(tap(Some(&moving), &other), None);
        // The insertion line taps where it shows, once.
        let pin = |at: usize| {
            (
                Target::Pin(at),
                (false, false, false),
                decide(&sources, &Target::Pin(at), keys(false, false), false),
            )
        };
        assert_eq!(tap(None, &pin(1)), Some(Haptic::Alignment));
        assert_eq!(tap(Some(&moving), &pin(1)), Some(Haptic::Alignment));
        assert_eq!(tap(Some(&pin(1)), &pin(2)), Some(Haptic::Alignment));
        assert_eq!(tap(Some(&pin(2)), &pin(2)), None);
    }

    #[gpui::test]
    fn the_drag_keeps_what_it_started_with_whatever_the_keys_select_after(cx: &mut TestAppContext) {
        let (_dir, project) = project_folder();
        let (browser, cx) = browse(&project, cx);
        let (docs, file) = (row(&browser, 0, cx), row(&browser, 2, cx));
        carry(file, docs, "", cx);
        press(cx, "up");
        assert_eq!(shown(&browser, cx).1, ["zeta"]);
        assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
        // What leaves the window is what the image shows.
        let Payload::Files(files) = in_pane(&browser, cx, |pane, _| pane.drag_out()).unwrap();
        assert_eq!(files.entries(), [(project.join("a.txt"), false)]);
        // Out of the window, AppKit carries it, and it comes back the same drag.
        browser.read_with(cx, |b, _| b.drag.away());
        hover(docs, "", cx);
        hover(docs, "", cx);
        draw(cx);
        assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
        press(cx, "escape");
    }

    #[gpui::test]
    fn a_button_up_with_no_mouse_up_still_ends_the_drag_and_drops(cx: &mut TestAppContext) {
        let (_dir, project) = project_folder();
        let mut owned = own(cx);
        let (browser, cx) = browse(&project, cx);
        let (docs, file) = (row(&browser, 0, cx), row(&browser, 2, cx));
        carry(file, docs, "", cx);
        assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
        cx.update(release_lost_drag);
        draw(cx);
        assert!(!cx.update(|_, cx| cx.has_active_drag()));
        assert!(!browser.read_with(cx, |b, _| b.drag.active()));
        restart(&mut owned, cx);
        cx.executor().advance_clock(crate::jobs::CHECK);
        cx.run_until_parked();
        assert!(project.join("docs/a.txt").exists());
        release(owned, cx);
    }

    #[test]
    fn what_is_inside_another_dragged_folder_goes_along_with_it() {
        let entries: Vec<Entry> = ["/a/docs", "/a/docs/in.md", "/a/docs/deep/er", "/a/docs2"]
            .iter()
            .map(|path| Entry {
                path: path.into(),
                name: String::new(),
                kind: Kind::Dir,
                size: 0,
                modified: 0,
                availability: Default::default(),
            })
            .collect();
        let left: Vec<&Path> = outermost(entries.iter().collect())
            .iter()
            .map(|entry| entry.path.as_path())
            .collect();
        assert_eq!(left, [Path::new("/a/docs"), Path::new("/a/docs2")]);
    }
}
