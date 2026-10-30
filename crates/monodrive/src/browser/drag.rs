//! Dragging files. GPUI's drag covers all of it: within the window; out of it,
//! where it hands AppKit a session of file URLs once the pointer leaves
//! (`external_drag_payload`); and in from another app, as a drag of
//! `ExternalPaths`. What's here decides what a drop would do, draws that, and
//! runs it through the operation owner like any copy or move.
//!
//! Outside the app, GPUI's session offers only Copy. That's the right mask for
//! us: a move Finder carried out wouldn't be in the journal, so it couldn't be
//! undone.

use super::Browser;
use super::pane::{Pane, text_width};
use crate::file_icons;
use crate::theme::{self, Elevation, content};
use gpui::{
    App, Context, Div, DragMoveEvent, Entity, EntityId, ExternalDragPayload, ExternalPaths,
    FileDragPaths, FontWeight, Modifiers, MouseButton, MouseMoveEvent, Pixels, PlatformInput,
    Point, Render, Stateful, Window, div, prelude::*, px,
};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::volumes::{self, Volume};
use objc2_app_kit::{NSEvent, NSEventModifierFlags};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The drag image's icon.
const IMAGE_SIDE: f32 = 32.;
/// The pill's height, which its card radius makes fully round.
const PILL_HEIGHT: f32 = 28.;
const PILL_WIDEST: f32 = 360.;
/// How far the pill sits from the pointer, clear of the drag image.
const PILL_OFFSET: Point<f32> = Point { x: 24., y: 4. };

/// What a person is dragging out of a pane: the selection, once it's started.
pub(super) struct FileDrag {
    pane: Entity<Pane>,
    ix: usize,
}

/// A place on screen that a drop can land on.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Spot {
    Row(EntityId, usize),
    Pane(EntityId),
    Crumb(EntityId, usize),
    Tab(usize),
    /// A sidebar row, by the sidebar's numbering.
    Place(usize),
}

/// What a drop at the pointer would land on.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Target {
    /// Into a folder, named as the person sees it there.
    Folder {
        spot: Spot,
        path: PathBuf,
        name: String,
    },
    /// Into Favorites, before the one at this position.
    Pin(usize),
}

/// What a drop would do.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Decision {
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
    /// Nothing, for this reason.
    Nothing(String),
}

/// Finder's rule: within a volume a drag moves and across volumes it copies; ⌥
/// forces a copy and ⌘ a move. What's already in the folder stays out of it,
/// and a folder can't go inside itself.
pub(super) fn decide(
    sources: &[PathBuf],
    target: &Target,
    keys: Modifiers,
    same_volume: impl Fn(&Path, &Path) -> bool,
) -> Decision {
    let (into, name) = match target {
        Target::Pin(at) => {
            return Decision::Pin {
                paths: sources.to_vec(),
                at: *at,
            };
        }
        Target::Folder { path, name, .. } => (path, name),
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
    let moving = !keys.alt
        && (keys.platform || {
            // Rows of one folder come together, and share a volume.
            let mut last = None;
            paths.iter().all(|path| {
                let parent = path.parent();
                std::mem::replace(&mut last, parent) == parent || same_volume(path, into)
            })
        });
    Decision::Send {
        paths,
        into: into.clone(),
        name: name.clone(),
        moving,
    }
}

impl Decision {
    /// What the pill says.
    pub(super) fn says(&self) -> String {
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
            Self::Nothing(why) => why.clone(),
        }
    }
}

fn count_of(paths: &[PathBuf], noun: &str) -> String {
    match paths {
        [one] => shown_name(one),
        many => format!("{} {noun}", many.len()),
    }
}

fn shown_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `entries` without what's inside another of them, which goes along with it.
fn outermost(entries: Vec<&Entry>) -> Vec<&Entry> {
    let all: HashSet<&Path> = entries.iter().map(|entry| entry.path.as_path()).collect();
    let inside = |entry: &Entry| entry.path.ancestors().skip(1).any(|up| all.contains(up));
    entries.iter().copied().filter(|e| !inside(e)).collect()
}

/// Whether two paths are on one volume, as `decide` asks, from the table the
/// sidebar keeps, so a hover never touches the disk. A folder reached through
/// a link counts by its path.
fn same_volume(volumes: &[Volume], a: &Path, b: &Path) -> bool {
    let mount = |path| volumes::holding(volumes, path).map(|volume| &volume.mount);
    mount(a) == mount(b)
}

/// The modifiers held now. During a drag AppKit runs, from another app or back
/// from outside the window, the window hears no key changes; AppKit still
/// knows. A window with no AppKit behind it, as in tests, has what it was told.
fn held_keys(window: &Window) -> Modifiers {
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

struct Session {
    sources: Vec<PathBuf>,
    /// All of it is folders, as pinning asks.
    folders: bool,
    /// What the pointer is over now, as this move finds it.
    target: Option<Target>,
    /// What the frame shows, worked out once per target and keys, since
    /// a selection can run to thousands.
    decided: Option<(Target, (bool, bool), Decision)>,
    /// The next frame comes from something that doesn't move what's under the
    /// pointer: a move has just looked, or only the keys changed.
    settled: bool,
}

#[derive(Default)]
struct DragState {
    /// What the drag out of a pane took when it started, each path with
    /// whether it's a folder, whatever the keys select meanwhile. It outlasts
    /// the session, for a drag that comes back from outside the window.
    carried: Vec<(PathBuf, bool)>,
    session: Option<Session>,
}

/// The window's drag of files, if there is one. The panes and the sidebar hold
/// it to offer what's under the pointer, and draw from it. GPUI redraws the
/// window after each move of a drag, so nothing here asks for a frame.
#[derive(Clone, Default)]
pub(super) struct Drag(Rc<RefCell<DragState>>);

impl Drag {
    pub(super) fn active(&self) -> bool {
        self.0.borrow().session.is_some()
    }

    fn begin(&self, sources: Vec<PathBuf>, folders: bool) {
        self.0.borrow_mut().session = Some(Session {
            sources,
            folders,
            target: None,
            decided: None,
            settled: false,
        });
    }

    /// A drag out of a pane starts, with `files`.
    fn carry(&self, files: Vec<(PathBuf, bool)>) {
        self.0.borrow_mut().carried = files;
        self.resume();
    }

    /// The session of what's carried.
    fn resume(&self) {
        let (sources, folders) = {
            let state = self.0.borrow();
            (
                state.carried.iter().map(|(path, _)| path.clone()).collect(),
                state.carried.iter().all(|(_, folder)| *folder),
            )
        };
        self.begin(sources, folders);
    }

    /// A move starts: what's under the pointer comes from the offers after it.
    /// Says whether there's a session to offer to.
    fn next_move(&self) -> bool {
        let mut state = self.0.borrow_mut();
        if let Some(session) = state.session.as_mut() {
            session.target = None;
            session.settled = true;
        }
        state.session.is_some()
    }

    /// The innermost element's offer comes first, and stands.
    pub(super) fn offer(&self, target: Target) {
        if let Some(session) = self.0.borrow_mut().session.as_mut()
            && session.target.is_none()
        {
            session.target = Some(target);
        }
    }

    /// Everything dragged is a folder, so it can be pinned.
    pub(super) fn folders(&self) -> bool {
        self.0.borrow().session.as_ref().is_some_and(|s| s.folders)
    }

    /// What changed needs a frame, and nothing moved under the pointer.
    fn keep_settled(&self) {
        if let Some(session) = self.0.borrow_mut().session.as_mut() {
            session.settled = true;
        }
    }

    /// Before a frame: ends with GPUI's drag, and works out what it would do.
    /// Says whether what's under the pointer may have changed without a move,
    /// as a scroll or a closed tab changes it.
    fn settle(&self, dragging: bool, keys: Modifiers, volumes: &[Volume]) -> bool {
        let mut state = self.0.borrow_mut();
        if !dragging {
            state.session = None;
        }
        let Some(session) = state.session.as_mut() else {
            return false;
        };
        let moved_under = !std::mem::take(&mut session.settled);
        let Some(target) = session.target.clone() else {
            session.decided = None;
            return moved_under;
        };
        let pressed = (keys.alt, keys.platform);
        if !session
            .decided
            .as_ref()
            .is_some_and(|(was, held, _)| *was == target && *held == pressed)
        {
            let decision = decide(&session.sources, &target, keys, |a, b| {
                same_volume(volumes, a, b)
            });
            session.decided = Some((target, pressed, decision));
        }
        moved_under
    }

    /// Reads what the frame shows a drop would do.
    pub(super) fn decision<R>(&self, read: impl FnOnce(&Target, &Decision) -> R) -> Option<R> {
        let state = self.0.borrow();
        let (target, _, decision) = state.session.as_ref()?.decided.as_ref()?;
        Some(read(target, decision))
    }

    /// Whether `spot` is what a drop would go into.
    pub(super) fn outlines(&self, spot: &Spot) -> bool {
        self.decision(|target, decision| {
            matches!(decision, Decision::Send { .. })
                && matches!(target, Target::Folder { spot: at, .. } if at == spot)
        })
        .unwrap_or(false)
    }

    /// The position in Favorites a drop would pin at.
    pub(super) fn pins_at(&self) -> Option<usize> {
        self.decision(|_, decision| match decision {
            Decision::Pin { at, .. } => Some(*at),
            _ => None,
        })
        .flatten()
    }

    /// The button came up, and what's under it has just offered itself: what
    /// the pill showed, as long as that's still what's there. The drag ends
    /// either way.
    fn finish(&self) -> Option<Decision> {
        let session = self.0.borrow_mut().session.take()?;
        let (shown, _, decision) = session.decided?;
        (session.target? == shown).then_some(decision)
    }
}

/// Makes `element` offer what `offer` does while the pointer's over it: on
/// each move, for the pill, and at release, to check the pill against what's
/// on screen.
pub(super) fn offers<E: InteractiveElement>(element: E, offer: impl Fn(&mut App) + 'static) -> E {
    let offer = Rc::new(offer);
    let at_release = offer.clone();
    element
        .on_mouse_move(move |_, _, cx| offer(cx))
        .on_mouse_up(MouseButton::Left, move |_, _, cx| at_release(cx))
}

/// The icon under the pointer, with a count when there's more than one.
struct DragImage {
    entry: Option<Entry>,
    count: usize,
    /// Where the press was in the element GPUI draws this over.
    grabbed: Point<Pixels>,
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
                    .children(
                        self.entry
                            .as_ref()
                            .map(|entry| file_icons::icon(entry, IMAGE_SIDE, window, cx)),
                    )
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
    pub(super) fn grab(element: Stateful<Div>, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        element
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.grabbed = true),
            )
            .on_drag(
                FileDrag {
                    pane: cx.entity(),
                    ix,
                },
                |dragged, grabbed, window, cx| {
                    let ix = dragged.ix;
                    dragged
                        .pane
                        .update(cx, |pane, cx| pane.begin_drag(ix, grabbed, window, cx))
                },
            )
            .external_drag_payload(|dragged: &FileDrag, _, cx| dragged.pane.read(cx).drag_out())
    }

    /// The drag has started on row `ix`: a row outside the selection joins it
    /// as a click would, and all of it goes.
    fn begin_drag(
        &mut self,
        ix: usize,
        grabbed: Point<Pixels>,
        window: &Window,
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
        self.drag.carry(files.collect());
        cx.notify();
        cx.new(|_| DragImage {
            entry,
            count,
            grabbed,
        })
    }

    /// What AppKit gets once a drag leaves the window: file URLs, built without
    /// asking the disk, so nothing downloads a cloud placeholder.
    pub(super) fn drag_out(&self) -> Option<ExternalDragPayload> {
        let files = self.drag.0.borrow().carried.clone();
        Some(ExternalDragPayload::Files(FileDragPaths::new(files)))
    }

    /// Makes row or cell `ix` offer itself to a drag.
    pub(super) fn takes_drops(
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

impl Browser {
    /// The window's end of a drag: what starts each move and what a drop does.
    pub(super) fn drags(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.on_drag_move::<FileDrag>(cx.listener(|this, _: &DragMoveEvent<FileDrag>, _, _| {
            if !this.drag.next_move() {
                // Back from outside the window, where GPUI gave it to AppKit.
                this.drag.resume();
            }
        }))
        .on_drag_move::<ExternalPaths>(cx.listener(
            |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                if !this.drag.next_move() {
                    let paths = event.drag(cx).paths().to_vec();
                    this.begin_external(paths, cx);
                }
            },
        ))
        // What's under the pointer offers itself again at release.
        .capture_any_mouse_up(cx.listener(|this, _, _, _| {
            this.drag.next_move();
        }))
        .on_drop::<FileDrag>(cx.listener(|this, _, window, cx| this.drop_files(window, cx)))
        .on_drop::<ExternalPaths>(cx.listener(|this, _, window, cx| this.drop_files(window, cx)))
        .on_modifiers_changed(cx.listener(|this, _, _, cx| {
            if this.drag.active() {
                this.drag.keep_settled();
                cx.notify();
            }
        }))
    }

    /// Files from another app. Whether they're all folders takes the disk.
    fn begin_external(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.drag.begin(paths.clone(), false);
        let drag = self.drag.clone();
        let checked = cx.background_executor().spawn(async move {
            let folders = paths.iter().all(|path| path.is_dir());
            (paths, folders)
        });
        cx.spawn(async move |_, _| {
            let (paths, folders) = checked.await;
            if let Some(session) = drag.0.borrow_mut().session.as_mut()
                && session.sources == paths
            {
                session.folders = folders;
            }
        })
        .detach();
    }

    /// Before a frame: the drag's state for what's held and mounted now. A
    /// frame that may have moved things under a resting pointer looks again
    /// once it's drawn, with a move at the same place, so that frame's listeners
    /// answer. That move's own frame doesn't look again, so nothing repeats.
    pub(super) fn settle_drag(&self, window: &Window, cx: &App) {
        let keys = held_keys(window);
        let moved_under = self
            .drag
            .settle(cx.has_active_drag(), keys, &self.sidebar.volumes);
        if moved_under {
            window.on_next_frame(|window, cx| {
                if cx.has_active_drag() {
                    let event = MouseMoveEvent {
                        position: window.mouse_position(),
                        pressed_button: Some(MouseButton::Left),
                        modifiers: window.modifiers(),
                    };
                    window.dispatch_event(PlatformInput::MouseMove(event), cx);
                }
            });
        }
    }

    /// The button came up over the window. Nothing has touched the disk until
    /// now; the job checks each item again as it starts.
    fn drop_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.drag.finish() {
            Some(Decision::Send {
                paths,
                into,
                moving,
                ..
            }) => self
                .pane()
                .update(cx, |pane, cx| pane.send(paths, into, moving, window, cx)),
            Some(Decision::Pin { paths, at }) => self.pin_folders(paths, at, cx),
            Some(Decision::Nothing(_)) | None => {}
        }
    }

    /// The pill by the pointer: what a drop here would do.
    pub(super) fn drag_pill(&self, window: &Window) -> Option<impl IntoElement + use<>> {
        let (text, acts) = self
            .drag
            .decision(|_, decision| (decision.says(), !matches!(decision, Decision::Nothing(_))))?;
        let at = window.mouse_position();
        let room = window.viewport_size();
        // Its text, its side padding and its border.
        let width = (text_width(&text, theme::TEXT_BODY, FontWeight::NORMAL, window) + 2. * 13.)
            .min(PILL_WIDEST);
        // Past the window's edge, it flips to the pointer's other side.
        let right = at.x.as_f32() + PILL_OFFSET.x;
        let x = if right + width > room.width.as_f32() {
            at.x.as_f32() - PILL_OFFSET.x - width
        } else {
            right
        };
        let below = at.y.as_f32() + PILL_OFFSET.y;
        let y = if below + PILL_HEIGHT > room.height.as_f32() {
            at.y.as_f32() - PILL_OFFSET.y - PILL_HEIGHT
        } else {
            below
        };
        Some(
            theme::floating(Elevation::Popover)
                .absolute()
                .left(px(x))
                .top(px(y))
                .max_w(px(PILL_WIDEST))
                .h(px(PILL_HEIGHT))
                .px_3()
                .flex()
                .items_center()
                .child(
                    div()
                        .relative()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::TEXT_BODY))
                        .text_color(content(if acts { 1. } else { 0.65 }))
                        .child(text),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::drag_tests::{carry, draw, hover, says};
    use super::super::tests::{browse, folder as project_folder, in_pane, press, row, shown};
    use super::*;
    use gpui::{ExternalDragPayload as Payload, TestAppContext};

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

    /// `/ext` is a volume of its own, and everything else is on another.
    fn same(a: &Path, b: &Path) -> bool {
        a.starts_with("/ext") == b.starts_with("/ext")
    }

    #[test]
    fn a_drag_moves_within_a_volume_copies_across_and_the_keys_force_either() {
        let moving = |sources: &[&str], into: &str, keys: Modifiers| {
            let target = folder(into, "x");
            match decide(&paths(sources), &target, keys, same) {
                Decision::Send { moving, .. } => moving,
                other => panic!("{other:?}"),
            }
        };
        let plain = Modifiers::default();
        assert!(moving(&["/a/f"], "/b", plain));
        assert!(!moving(&["/a/f"], "/ext/b", plain));
        // ⌥ forces a copy, ⌘ a move, and ⌥ wins with both, since it's the safe one.
        assert!(!moving(&["/a/f"], "/b", keys(true, false)));
        assert!(moving(&["/a/f"], "/ext/b", keys(false, true)));
        assert!(!moving(&["/a/f"], "/ext/b", keys(true, true)));
        // Rows from several volumes, as search shows: one off it makes a copy.
        assert!(!moving(&["/a/f", "/ext/g"], "/b", plain));
        assert!(moving(&["/a/f", "/a/g", "/c/h"], "/b", plain));
    }

    #[test]
    fn the_pill_says_what_a_drop_would_do() {
        let says = |sources: &[&str], target: &Target, keys: Modifiers| {
            decide(&paths(sources), target, keys, same).says()
        };
        let inbox = folder("/b/Inbox", "Inbox");
        let plain = Modifiers::default();
        assert_eq!(
            says(&["/a/report.pdf"], &inbox, plain),
            "Move report.pdf to Inbox"
        );
        assert_eq!(
            says(&["/a/1", "/a/2", "/a/3"], &inbox, keys(true, false)),
            "Copy 3 items to Inbox"
        );
        assert_eq!(says(&["/b/Inbox/f"], &inbox, plain), "Already in Inbox");
        assert_eq!(
            says(&["/a/docs"], &Target::Pin(1), plain),
            "Pin docs to Favorites"
        );
        assert_eq!(
            says(&["/a/docs", "/a/src"], &Target::Pin(1), plain),
            "Pin 2 folders to Favorites"
        );
    }

    #[test]
    fn a_folder_cant_go_inside_itself_and_what_is_there_stays_out() {
        let nothing = Decision::Nothing("\"docs\" can't go inside itself.".into());
        for into in ["/a/docs", "/a/docs/deep/er"] {
            let target = folder(into, "x");
            let decided = decide(&paths(&["/a/docs"]), &target, Modifiers::default(), same);
            assert_eq!(decided, nothing, "{into}");
        }
        // A name that only starts the same isn't inside it.
        let target = folder("/a/docs2", "docs2");
        let decided = decide(&paths(&["/a/docs"]), &target, Modifiers::default(), same);
        assert!(matches!(decided, Decision::Send { .. }));
        // ⌥ doesn't turn a drop on its own folder into a duplicate.
        let target = folder("/a", "a");
        let decided = decide(&paths(&["/a/f"]), &target, keys(true, false), same);
        assert_eq!(decided, Decision::Nothing("Already in a".into()));
        // Search rows, where only one is in the folder already: the other goes.
        let decided = decide(
            &paths(&["/a/f", "/c/g"]),
            &target,
            Modifiers::default(),
            same,
        );
        assert_eq!(decided.says(), "Move g to a");
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
        // GPUI handing it to AppKit ends what the window knows of it, and it
        // comes back the same drag.
        browser.read_with(cx, |b, _| {
            b.drag.settle(false, Modifiers::default(), &[]);
        });
        hover(docs, "", cx);
        hover(docs, "", cx);
        draw(cx);
        assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to docs");
        press(cx, "escape");
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

    #[test]
    fn volumes_are_told_apart_by_the_mount_holding_each_path() {
        let volume = |mount: &str| Volume {
            name: mount.into(),
            mount: mount.into(),
            disk: None,
            id: None,
            total: 0,
            free: 0,
            read_only: false,
            network: false,
            ejectable: false,
            removable: false,
            startup: mount == "/",
        };
        let table = [volume("/"), volume("/Volumes/Ext")];
        assert!(same_volume(
            &table,
            "/Users/me/a".as_ref(),
            "/tmp/b".as_ref()
        ));
        assert!(!same_volume(
            &table,
            "/Users/me/a".as_ref(),
            "/Volumes/Ext/b".as_ref()
        ));
        // Before the sidebar has read them, all are one: a move, which is
        // what the owner sorts out per item anyway.
        assert!(same_volume(&[], "/a".as_ref(), "/Volumes/Ext/b".as_ref()));
    }
}
