//! The preview's Edit mode: tools under the picture, with a recipe in memory.
//! Each committed step flattens off the UI thread and the result replaces what's
//! shown, so what you see is what saving writes. Only the mark being drawn is
//! GPUI's. Nothing touches the disk until Save, and undo here pops a step.

use super::{Body, PreviewView};
use crate::browser::Operations;
use crate::text_field::{self, TextField};
use crate::theme::{self, ButtonLook, content, danger, text_button};
use gpui::{
    AnyElement, Bounds, Context, CursorStyle, Entity, EventEmitter, Focusable, FontWeight,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point,
    RenderImage, Stateful, Subscription, Task, Window, canvas, deferred, div, point, prelude::*,
    px, size,
};
use monodrive_core::edits::{self, Clip, Handle, Rect, Step};
use monodrive_core::ops::{Opened, Origin, Outcome, Request, SaveTo};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// How a saved edit came out, for the window's toast and its Undo.
pub struct Saved(pub Result<(Request, Outcome), String>);

impl EventEmitter<Saved> for PreviewView {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Crop,
    Pen,
    Highlighter,
    Arrow,
    Rectangle,
    Ellipse,
    Text,
    Redact,
}

/// Each tool, its button's id, its name when hovered, and its icon.
const PICTURE_TOOLS: [(Tool, &str, &str, &str); 8] = [
    (Tool::Crop, "tool-crop", "Crop", "tool-crop"),
    (Tool::Pen, "tool-pen", "Pen", "tool-pen"),
    (
        Tool::Highlighter,
        "tool-highlighter",
        "Highlight",
        "tool-highlight",
    ),
    (Tool::Arrow, "tool-arrow", "Arrow", "tool-arrow"),
    (Tool::Rectangle, "tool-rectangle", "Box", "tool-box"),
    (Tool::Ellipse, "tool-ellipse", "Circle", "tool-circle"),
    (Tool::Text, "tool-text", "Text", "tool-text"),
    (Tool::Redact, "tool-redact", "Redact", "tool-redact"),
];

/// The row's width below which the tools move behind a menu, and below which
/// the name goes too: what each row needs, measured from its buttons.
const PICTURE_ROW: f32 = 730.;
const VIDEO_ROW: f32 = 480.;
const NAMELESS_ROW: f32 = 330.;

/// `(narrow, very_narrow)`. Before the first measure the row counts as wide.
fn row_fits(width: f32, picture: bool) -> (bool, bool) {
    if width <= 0. {
        return (false, false);
    }
    let needs = if picture { PICTURE_ROW } else { VIDEO_ROW };
    (width < needs, width < NAMELESS_ROW)
}

/// Which menu is open under the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Save,
    Tools,
}

/// A tool's name beside the pointer.
struct Tip(&'static str);

impl gpui::Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        theme::floating(theme::Elevation::Popover)
            .px_2()
            .py_1()
            .text_size(px(theme::TEXT_SMALL))
            .child(div().relative().child(self.0))
    }
}

pub(super) fn tip(button: Stateful<gpui::Div>, label: &'static str) -> Stateful<gpui::Div> {
    button.tooltip(move |_, cx| cx.new(|_| Tip(label)).into())
}

/// A menu under its button, at its right edge or its left.
fn menu_card(width: f32, from_right: bool) -> gpui::Div {
    theme::floating(theme::Elevation::Popover)
        .absolute()
        .top(px(theme::BUTTON_HEIGHT + 4.))
        .when(from_right, |d| d.right_0())
        .when(!from_right, |d| d.left_0())
        .w(px(width))
        .p_1()
        .occlude()
}

/// The card the Text tool writes in. Its height is only for keeping it inside
/// the picture's pane.
const NOTE_WIDTH: f32 = 320.;
const NOTE_HEIGHT: f32 = 204.;
const NOTE_ROWS: usize = 5;

/// How far, on screen, the turning handle sits off a mark, and how close a press
/// must be to a handle or to a mark's ink.
const TURN_HANDLE_OUT: f32 = 22.;
const HANDLE_REACH: f32 = 9.;
const INK_REACH: f32 = 5.;
/// Within this many degrees of a right angle, a turn lands on it.
const TURN_SNAP: f64 = 3.;

/// A drag on a mark, measured from where it began so it can't drift.
struct Gesture {
    grab: Grab,
    start: [f64; 2],
    original: Step,
    moved: bool,
}

#[derive(Clone, Copy)]
enum Grab {
    Move,
    Handle(Handle),
}

/// What the picture shows while a mark moves: everything else, flattened once,
/// and the mark alone on a small layer drawn the way saving draws it.
#[derive(Default)]
struct Dragged {
    under: Option<Under>,
    /// The under being drawn, and the mark and step count it's for.
    under_task: Option<((usize, usize), Task<()>)>,
    /// The mark's layer, the shape it was drawn at, and which mark it is.
    layer: Option<(Layer, Step, usize)>,
    layer_task: Option<Task<()>>,
    /// The mark moved while its layer was being drawn.
    layer_stale: bool,
}

/// A landing mark is still in the steps and not yet in the picture.
fn waiting(steps: &[Step], shown: &[Step], at: usize, step: &Step) -> bool {
    steps.get(at) == Some(step) && shown.get(at) != Some(step)
}

/// The picture without the mark at `index`, flattened from the first `count`
/// steps, so marks added after it aren't in it.
struct Under {
    index: usize,
    count: usize,
    image: Arc<RenderImage>,
}

/// A mark drawn alone, as saving draws it, and the part of the picture it covers.
type Layer = (Arc<RenderImage>, Rect);

fn layer_of(step: &Step, picture: (f64, f64), scale: f64) -> Result<Layer, String> {
    let (bitmap, area) = edits::render_mark(step, picture, scale)?;
    let pixels = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bytes)
        .ok_or("couldn't draw the mark")?;
    Ok((
        Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])),
        area,
    ))
}

/// Marks' colors, in the file and on screen.
const INKS: [(&str, [f64; 4]); 4] = [
    ("ink-red", [0.9, 0.2, 0.15, 1.]),
    ("ink-yellow", [1., 0.85, 0.1, 1.]),
    ("ink-blue", [0.15, 0.45, 0.95, 1.]),
    ("ink-black", [0., 0., 0., 1.]),
];

// design: a mark's own color, part of the file and not the app.
fn hsla(color: [f64; 4]) -> gpui::Hsla {
    // design: a mark's own color, which is part of the file and not the app.
    gpui::Rgba {
        r: color[0] as f32,
        g: color[1] as f32,
        b: color[2] as f32,
        a: color[3] as f32,
    }
    .into()
}

/// What's being edited. A video keeps its own strip, so its edits only say what
/// the export will do.
enum Subject {
    Picture {
        steps: Vec<Step>,
        /// What each change replaced, for Undo.
        past: Vec<Vec<Step>>,
        /// The picture as the steps leave it, in pixels.
        image: Option<Arc<RenderImage>>,
        /// The steps `image` was flattened from, which lag `steps` while it flattens.
        shown: Vec<Step>,
        flattening: Option<Task<()>>,
    },
    Video {
        start: Option<f64>,
        end: Option<f64>,
        muted: bool,
        turns: i32,
        /// A frame to crop on, as nothing GPUI draws can sit over the player.
        frozen: Option<Arc<RenderImage>>,
        crop: Option<Rect>,
        /// What each change replaced, for Undo.
        past: Vec<VideoState>,
    },
}

/// What a video's edits come to at one moment.
type VideoState = (Option<f64>, Option<f64>, bool, i32, Option<Rect>);

/// The edit in progress.
pub(super) struct Edit {
    subject: Subject,
    tool: Option<Tool>,
    ink: [f64; 4],
    /// The mark being dragged, in image pixels.
    draft: Vec<[f64; 2]>,
    /// Where the picture was last drawn, to turn the pointer into pixels.
    drawn: Rc<Cell<Bounds<Pixels>>>,
    /// The mark with handles on it, as an index into the steps.
    selected: Option<usize>,
    gesture: Option<Gesture>,
    dragged: Dragged,
    /// Marks just added, over the picture until it's flattened with them, so
    /// a mark doesn't blink out when it's let go.
    landing: Vec<(usize, Step, Layer)>,
    /// What the pointer would do if pressed now.
    cursor: CursorStyle,
    /// Text waiting for a place: the field, and where it goes.
    text: Option<(Entity<TextField>, [f64; 2], Subscription)>,
    /// The Text step the card is changing, when it isn't a new one.
    rewriting: Option<usize>,
    /// What had the keys before the text field, to give them back.
    keys_before: Option<gpui::FocusHandle>,
    /// Esc asked, with changes that would be lost.
    confirming: bool,
    menu: Option<Menu>,
    saving: Option<Arc<AtomicBool>>,
    error: Option<String>,
    opened: Option<Opened>,
}

impl Edit {
    /// Before a change to a video, so Undo can come back to this.
    fn remember(&mut self) {
        if let Subject::Video {
            start,
            end,
            muted,
            turns,
            crop,
            past,
            ..
        } = &mut self.subject
        {
            past.push((*start, *end, *muted, *turns, *crop));
        }
    }

    /// Takes a video back one change.
    fn undo_video(&mut self) {
        if let Subject::Video {
            start,
            end,
            muted,
            turns,
            crop,
            past,
            ..
        } = &mut self.subject
            && let Some(state) = past.pop()
        {
            (*start, *end, *muted, *turns, *crop) = state;
        }
    }

    fn changed(&self) -> bool {
        match &self.subject {
            Subject::Picture { steps, .. } => !steps.is_empty(),
            Subject::Video {
                start,
                end,
                muted,
                turns,
                crop,
                ..
            } => start.is_some() || end.is_some() || *muted || *turns != 0 || crop.is_some(),
        }
    }

    fn steps(&self) -> &[Step] {
        match &self.subject {
            Subject::Picture { steps, .. } => steps,
            Subject::Video { .. } => &[],
        }
    }

    /// Marks from before a crop, turn or flip are part of the picture by now:
    /// their pixels no longer mean the same place.
    fn first_movable(&self) -> usize {
        self.steps()
            .iter()
            .rposition(|step| !step.is_mark())
            .map_or(0, |last| last + 1)
    }

    fn picture(&self) -> Option<(f64, f64)> {
        let (width, height) = image_size(self.image()?);
        Some((f64::from(width), f64::from(height)))
    }

    /// The topmost movable mark a press at `at` lands on. The selected mark
    /// counts all over its frame, so a box can be dragged from its middle.
    fn mark_at(&self, at: [f64; 2], reach: f64) -> Option<usize> {
        let picture = self.picture()?;
        let steps = self.steps();
        if let Some(selected) = self.selected
            && let Some(frame) = steps.get(selected).and_then(|step| step.frame(picture))
            && frame.contains(at, reach)
        {
            return Some(selected);
        }
        (self.first_movable()..steps.len())
            .rev()
            .find(|index| steps[*index].hit(at, picture, reach))
    }

    /// The selected mark's handle under `at`, given the picture's scale on screen.
    fn handle_at(&self, at: [f64; 2], scale: f32) -> Option<Handle> {
        let picture = self.picture()?;
        let step = self.steps().get(self.selected?)?;
        let scale = f64::from(scale);
        let reach = f64::from(HANDLE_REACH) / scale;
        step.handles(picture, f64::from(TURN_HANDLE_OUT) / scale)
            .into_iter()
            .find(|(_, place)| (place[0] - at[0]).hypot(place[1] - at[1]) <= reach)
            .map(|(handle, _)| handle)
    }

    /// Before a change to a picture, so Undo can come back to this.
    fn remember_steps(&mut self) {
        if let Subject::Picture { steps, past, .. } = &mut self.subject {
            past.push(steps.clone());
        }
    }

    /// Landing marks the picture doesn't show yet.
    fn landing(&self) -> impl Iterator<Item = &(usize, Step, Layer)> {
        let (steps, shown) = match &self.subject {
            Subject::Picture { steps, shown, .. } => (steps.as_slice(), shown.as_slice()),
            Subject::Video { .. } => (&[][..], &[][..]),
        };
        self.landing
            .iter()
            .filter(move |(at, step, _)| waiting(steps, shown, *at, step))
    }

    /// What stands in for the picture while a moved mark waits for the picture
    /// to have it, when both halves are ready and are of that mark.
    fn stand_ins(&self) -> Option<(&Under, &Layer, &Step)> {
        let under = self.dragged.under.as_ref()?;
        let (layer, step, index) = self.dragged.layer.as_ref()?;
        (*index == under.index).then_some((under, layer, step))
    }

    /// The mark at `index` as the screen shows it right now, so its handles sit
    /// on its ink while the picture catches up with the steps.
    fn on_screen(&self, index: usize) -> Option<&Step> {
        if let Some((under, _, step)) = self.stand_ins()
            && under.index == index
        {
            return Some(step);
        }
        if let Some((_, step, _)) = self.landing().find(|(at, ..)| *at == index) {
            return Some(step);
        }
        match &self.subject {
            Subject::Picture { steps, shown, .. } if shown.len() == steps.len() => shown.get(index),
            _ => self.steps().get(index),
        }
    }

    /// Lets go of landing marks nothing needs any more: the picture has them,
    /// and no stand-in from before them is still up.
    fn prune_landing(&mut self, cx: &mut gpui::App) {
        let landing = std::mem::take(&mut self.landing);
        let Subject::Picture { steps, shown, .. } = &self.subject else {
            return;
        };
        let under = self.dragged.under.as_ref();
        let (keep, gone): (Vec<_>, Vec<_>) = landing.into_iter().partition(|(at, step, _)| {
            waiting(steps, shown, *at, step)
                || under.is_some_and(|under| *at >= under.count && steps.get(*at) == Some(step))
        });
        for (_, _, (layer, _)) in gone {
            cx.drop_image(layer, None);
        }
        self.landing = keep;
    }

    /// Draws the layer of the mark at `index` here and now, in place of any
    /// still being drawn. Says whether it could.
    fn layer_now(&mut self, index: usize, window: &Window, cx: &mut gpui::App) -> bool {
        let (Some(picture), Some(scale)) = (self.picture(), self.layer_scale(window)) else {
            return false;
        };
        let Some(step) = self.steps().get(index).cloned() else {
            return false;
        };
        let Ok(layer) = layer_of(&step, picture, scale) else {
            return false;
        };
        self.dragged.layer_task = None;
        self.dragged.layer_stale = false;
        if let Some(((old, _), ..)) = self.dragged.layer.replace((layer, step, index)) {
            cx.drop_image(old, None);
        }
        true
    }

    /// Stand-ins of another mark, which a moved mark needs until the picture
    /// has it.
    fn holding_for_other(&self, index: usize) -> bool {
        self.dragged
            .under
            .as_ref()
            .is_some_and(|under| under.index != index)
    }

    /// How finely a mark's layer is drawn, to look sharp at the size it shows.
    fn layer_scale(&self, window: &Window) -> Option<f64> {
        let picture = self.picture()?;
        let (_, fitted) = fit(self.drawn.get(), (picture.0 as f32, picture.1 as f32));
        Some(f64::from((fitted * window.scale_factor()).min(1.)))
    }

    fn image(&self) -> Option<&Arc<RenderImage>> {
        match &self.subject {
            Subject::Picture { image, .. } => image.as_ref(),
            Subject::Video { frozen, .. } => frozen.as_ref(),
        }
    }

    /// The video's edits, in export's order: the crop is chosen on the frame as
    /// it plays, so it comes before any turn.
    fn clips(&self) -> Vec<Clip> {
        let Subject::Video {
            start,
            end,
            muted,
            turns,
            crop,
            ..
        } = &self.subject
        else {
            return Vec::new();
        };
        let mut clips = Vec::new();
        if start.is_some() || end.is_some() {
            clips.push(Clip::Trim {
                start: start.unwrap_or(0.),
                end: end.unwrap_or(f64::MAX),
            });
        }
        if let Some(rect) = crop {
            clips.push(Clip::Crop { rect: *rect });
        }
        if *turns != 0 {
            clips.push(Clip::Rotate {
                quarter_turns: *turns,
            });
        }
        if *muted {
            clips.push(Clip::Mute);
        }
        clips
    }
}

/// The picture's frame in `bounds`, centered and fit, and its scale.
pub(super) fn fit(bounds: Bounds<Pixels>, image: (f32, f32)) -> (Point<Pixels>, f32) {
    let (width, height) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
    let scale = (width / image.0).min(height / image.1).min(1.);
    let origin = point(
        bounds.origin.x + px((width - image.0 * scale) / 2.),
        bounds.origin.y + px((height - image.1 * scale) / 2.),
    );
    (origin, scale)
}

fn image_size(image: &RenderImage) -> (f32, f32) {
    let size = image.size(0);
    (size.width.0 as f32, size.height.0 as f32)
}

/// A rectangle from its two corners, kept inside the picture.
fn corners(a: [f64; 2], b: [f64; 2], image: (f32, f32)) -> Rect {
    let clamp = |value: f64, most: f32| value.clamp(0., f64::from(most));
    let (x0, x1) = (clamp(a[0], image.0), clamp(b[0], image.0));
    let (y0, y1) = (clamp(a[1], image.1), clamp(b[1], image.1));
    Rect {
        x: x0.min(x1),
        y: y0.min(y1),
        width: (x1 - x0).abs(),
        height: (y1 - y0).abs(),
    }
}

/// What a finished drag adds. Sizes follow the picture, so marks read the same
/// on a phone photo and on a small screenshot.
fn step_of(tool: Tool, draft: &[[f64; 2]], ink: [f64; 4], image: (f32, f32)) -> Option<Step> {
    let (first, last) = (*draft.first()?, *draft.last()?);
    let long = f64::from(image.0.max(image.1));
    let width = (long / 300.).max(2.);
    let rect = corners(first, last, image);
    let tiny = rect.width < 2. || rect.height < 2.;
    Some(match tool {
        Tool::Crop if !tiny => Step::Crop { rect },
        Tool::Redact if !tiny => Step::Redact { rect, rotation: 0. },
        Tool::Rectangle if !tiny => Step::Rectangle {
            rect,
            color: ink,
            width,
            rotation: 0.,
        },
        Tool::Ellipse if !tiny => Step::Ellipse {
            rect,
            color: ink,
            width,
            rotation: 0.,
        },
        Tool::Arrow if first != last => Step::Arrow {
            from: first,
            to: last,
            color: ink,
            width,
        },
        Tool::Pen => Step::Pen {
            points: draft.to_vec(),
            color: ink,
            width,
            rotation: 0.,
        },
        Tool::Highlighter => Step::Highlighter {
            points: draft.to_vec(),
            color: [ink[0], ink[1], ink[2], 0.4],
            width: width * 4.,
            rotation: 0.,
        },
        _ => return None,
    })
}

impl PreviewView {
    fn can_edit(&self) -> bool {
        matches!(
            self.body,
            Body::Picture { .. } | Body::Media { video: true, .. }
        )
    }

    /// E: the tools, or leaving them if nothing would be lost.
    pub fn toggle_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edit.is_some() {
            self.ask_to_leave(cx);
            return;
        }
        if !self.can_edit() {
            return;
        }
        let picture = matches!(self.body, Body::Picture { .. });
        let opened = Opened::read(&self.path).ok();
        self.edit = Some(Edit {
            subject: if picture {
                Subject::Picture {
                    steps: Vec::new(),
                    past: Vec::new(),
                    image: None,
                    shown: Vec::new(),
                    flattening: None,
                }
            } else {
                Subject::Video {
                    start: None,
                    end: None,
                    muted: false,
                    turns: 0,
                    frozen: None,
                    crop: None,
                    past: Vec::new(),
                }
            },
            tool: None,
            ink: INKS[0].1,
            draft: Vec::new(),
            drawn: Rc::new(Cell::new(Bounds::default())),
            selected: None,
            gesture: None,
            dragged: Dragged::default(),
            landing: Vec::new(),
            cursor: CursorStyle::Arrow,
            text: None,
            rewriting: None,
            keys_before: None,
            confirming: false,
            menu: None,
            saving: None,
            error: opened
                .is_none()
                .then(|| "This file can't be read now.".to_string()),
            opened,
        });
        if picture {
            self.flatten(window, cx);
        }
        cx.notify();
    }

    /// Esc: says whether it used the key. With changes it asks first.
    pub fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.edit.is_none() {
            return false;
        }
        self.ask_to_leave(cx);
        true
    }

    /// Whether the text tool's field has the keys.
    pub fn typing(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| edit.text.is_some())
    }

    /// Whether the tools are up.
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    /// Whether what's on screen must stay, as a change would be lost.
    pub fn holding(&self) -> bool {
        self.edit.as_ref().is_some_and(Edit::changed)
    }

    fn ask_to_leave(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        if edit.saving.is_some() {
            return;
        }
        // Esc closes what's on top first.
        if edit.menu.take().is_some()
            || edit.text.take().is_some()
            || edit.selected.take().is_some()
        {
            return cx.notify();
        }
        if edit.changed() && !edit.confirming {
            edit.confirming = true;
            return cx.notify();
        }
        self.leave_edit(cx);
    }

    fn leave_edit(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = self.edit.take()
            && let Some(image) = edit.image()
        {
            cx.drop_image(image.clone(), None);
        }
        if let Some((path, folder, position)) = self.wanted.take() {
            self.show(&path, folder, position, cx);
        }
        cx.notify();
    }

    /// Drops what stood in for the picture while a mark moved.
    fn settle(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let dragged = std::mem::take(&mut edit.dragged);
        if let Some(under) = dragged.under {
            cx.drop_image(under.image, None);
        }
        if let Some(((layer, _), ..)) = dragged.layer {
            cx.drop_image(layer, None);
        }
        edit.prune_landing(cx);
    }

    /// Flattens the steps off the UI thread; only the newest result is kept.
    fn flatten(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let Subject::Picture {
            steps, flattening, ..
        } = &mut edit.subject
        else {
            return;
        };
        let (path, steps) = (self.path.clone(), steps.clone());
        let flattened = steps.clone();
        let work = cx.background_executor().spawn(async move {
            let bitmap = edits::render(&path, &steps)?;
            let pixels = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bytes)
                .ok_or("couldn't draw the picture")?;
            Ok::<_, String>(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
        });
        *flattening = Some(cx.spawn_in(window, async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(edit) = &mut this.edit else {
                    return;
                };
                let Subject::Picture {
                    image,
                    shown,
                    flattening,
                    ..
                } = &mut edit.subject
                else {
                    return;
                };
                *flattening = None;
                match done {
                    Ok(new) => {
                        if let Some(old) = image.replace(new) {
                            cx.drop_image(old, None);
                        }
                        *shown = flattened;
                    }
                    Err(why) => edit.error = Some(why),
                }
                // The truth is in, so the stand-ins can go unless a mark is
                // moving: only a drag changes the steps without flattening.
                if edit.gesture.as_ref().is_none_or(|gesture| !gesture.moved) {
                    this.settle(cx);
                } else {
                    edit.prune_landing(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Everything but the mark being dragged, once, for it to move over.
    fn flatten_under(&mut self, index: usize, window: &Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        // One for another mark, or from before a mark was added, is no use.
        let wanted = (index, edit.steps().len());
        if edit
            .dragged
            .under_task
            .as_ref()
            .is_some_and(|(of, _)| *of == wanted)
            || edit
                .dragged
                .under
                .as_ref()
                .is_some_and(|under| (under.index, under.count) == wanted)
        {
            return;
        }
        edit.dragged.under_task = None;
        let mut steps = edit.steps().to_vec();
        if index >= steps.len() {
            return;
        }
        let count = steps.len();
        steps.remove(index);
        // A mark drawn moments ago isn't in the picture yet, which is then
        // already everything but it.
        if let Subject::Picture {
            image: Some(image),
            shown,
            ..
        } = &edit.subject
            && *shown == steps
        {
            let image = image.clone();
            if let Some(old) = edit.dragged.under.replace(Under {
                index,
                count,
                image,
            }) {
                cx.drop_image(old.image, None);
            }
            if edit
                .dragged
                .layer
                .as_ref()
                .is_none_or(|(.., of)| *of != index)
            {
                edit.layer_now(index, window, cx);
            }
            return;
        }
        let path = self.path.clone();
        let work = cx.background_executor().spawn(async move {
            let bitmap = edits::render(&path, &steps)?;
            let pixels = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bytes)
                .ok_or("couldn't draw the picture")?;
            Ok::<_, String>(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
        });
        edit.dragged.under_task = Some((
            wanted,
            cx.spawn_in(window, async move |this, cx| {
                let done = work.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    let Some(edit) = &mut this.edit else {
                        return;
                    };
                    edit.dragged.under_task = None;
                    match done {
                        Ok(image) => {
                            if let Some(old) = edit.dragged.under.replace(Under {
                                index,
                                count,
                                image,
                            }) {
                                cx.drop_image(old.image, None);
                            }
                            // With its own layer, so the two swap in together.
                            edit.layer_now(index, window, cx);
                        }
                        Err(why) => edit.error = Some(why),
                    }
                    cx.notify();
                });
            }),
        ));
    }

    /// Draws the moving mark's layer off the UI thread at the size it shows.
    /// One at a time: a mark that moved meanwhile draws again when it's done.
    fn draw_layer(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        if edit.dragged.layer_task.is_some() {
            edit.dragged.layer_stale = true;
            return;
        }
        let (Some(picture), Some(scale)) = (edit.picture(), edit.layer_scale(window)) else {
            return;
        };
        let Some(index) = edit.selected else {
            return;
        };
        if edit.holding_for_other(index) {
            return;
        }
        let Some(step) = edit.steps().get(index).cloned() else {
            return;
        };
        let work = cx.background_executor().spawn(async move {
            layer_of(&step, picture, scale).map(|layer| (layer, step, index))
        });
        edit.dragged.layer_stale = false;
        edit.dragged.layer_task = Some(cx.spawn_in(window, async move |this, cx| {
            let done = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(edit) = &mut this.edit else {
                    return;
                };
                edit.dragged.layer_task = None;
                match done {
                    Ok(new) => {
                        if let Some(((old, _), ..)) = edit.dragged.layer.replace(new) {
                            cx.drop_image(old, None);
                        }
                    }
                    Err(why) => edit.error = Some(why),
                }
                if std::mem::take(&mut edit.dragged.layer_stale) {
                    this.draw_layer(window, cx);
                }
                cx.notify();
            });
        }));
    }

    fn add_step(&mut self, step: Step, window: &Window, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.remember_steps();
            edit.selected = None;
            edit.error = None;
            // Drawn here at screen size, so the mark stays on screen while the
            // whole picture flattens.
            if let (Some(picture), Some(scale)) = (edit.picture(), edit.layer_scale(window))
                && step.is_mark()
                && let Ok(layer) = layer_of(&step, picture, scale)
            {
                edit.landing.push((edit.steps().len(), step.clone(), layer));
            }
            if let Subject::Picture { steps, .. } = &mut edit.subject {
                steps.push(step);
            }
        }
        self.flatten(window, cx);
        cx.notify();
    }

    fn undo_step(&mut self, window: &Window, cx: &mut Context<Self>) {
        match &mut self.edit {
            Some(Edit {
                subject: Subject::Picture { steps, past, .. },
                selected,
                ..
            }) => {
                if let Some(before) = past.pop() {
                    *steps = before;
                }
                *selected = None;
                // Marks may have changed places, so the stand-ins no longer fit.
                self.settle(cx);
                self.flatten(window, cx);
            }
            Some(edit) => {
                edit.undo_video();
                edit.tool = None;
            }
            None => {}
        }
        // With nothing left to lose, it follows a selection that moved on.
        if !self.holding()
            && let Some((path, folder, position)) = self.wanted.take()
        {
            self.show(&path, folder, position, cx);
        }
        cx.notify();
    }

    /// The pointer's place in the picture's pixels, if it's on the picture.
    fn pixel_at(&self, at: Point<Pixels>) -> Option<[f64; 2]> {
        let edit = self.edit.as_ref()?;
        let image = image_size(edit.image()?);
        let (origin, scale) = fit(edit.drawn.get(), image);
        Some([
            f64::from((at.x - origin.x).as_f32() / scale),
            f64::from((at.y - origin.y).as_f32() / scale),
        ])
    }

    fn pressed(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // What's being saved was captured, so nothing may change under it.
        if self.edit.as_ref().is_some_and(|edit| edit.saving.is_some()) {
            return;
        }
        let Some(place) = self.pixel_at(event.position) else {
            return;
        };
        let Some(edit) = &mut self.edit else {
            return;
        };
        match edit.tool {
            Some(Tool::Text) => self.open_card(place, None, window, cx),
            Some(_) => edit.draft = vec![place],
            None => self.press_on_marks(place, event.click_count, window, cx),
        }
        cx.notify();
    }

    /// The card, for new words at `place` or for changing a Text step's.
    fn open_card(
        &mut self,
        place: [f64; 2],
        rewriting: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let words = rewriting.and_then(|at| match edit.steps().get(at) {
            Some(Step::Text { text, .. }) => Some(text.clone()),
            _ => None,
        });
        edit.keys_before = window.focused(cx);
        let field = cx.new(|cx| {
            let mut field = TextField::new("Text", "What should it say?", cx).with_rows(NOTE_ROWS);
            if let Some(words) = &words {
                field.set_text(words, words.len()..words.len(), cx);
            }
            field
        });
        window.focus(&field.focus_handle(cx), cx);
        // Add is only live with something to add.
        let edits = cx.subscribe(&field, |_, _, _: &text_field::Changed, cx| cx.notify());
        edit.text = Some((field, place, edits));
        edit.rewriting = rewriting;
    }

    /// With no tool up, a press picks up what's under it.
    fn press_on_marks(
        &mut self,
        place: [f64; 2],
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let Some(picture) = edit.picture() else {
            return;
        };
        let (_, scale) = fit(edit.drawn.get(), (picture.0 as f32, picture.1 as f32));
        let grab = edit.handle_at(place, scale).map(Grab::Handle);
        let hit = edit.mark_at(place, f64::from(INK_REACH / scale));
        let Some(index) = grab.and(edit.selected).or(hit) else {
            edit.selected = None;
            return;
        };
        let original = edit.steps()[index].clone();
        // Two clicks on words change them.
        if clicks == 2
            && let Step::Text { at, .. } = &original
        {
            let at = *at;
            edit.selected = Some(index);
            return self.open_card(at, Some(index), window, cx);
        }
        edit.selected = Some(index);
        edit.gesture = Some(Gesture {
            grab: grab.unwrap_or(Grab::Move),
            start: place,
            original,
            moved: false,
        });
    }

    /// The selected mark's new shape, for the pointer at `place`.
    fn gesture_step(edit: &Edit, gesture: &Gesture, place: [f64; 2]) -> Option<Step> {
        let picture = edit.picture()?;
        // Held inside the picture, so a mark can't be dropped out of reach.
        let place = [place[0].clamp(0., picture.0), place[1].clamp(0., picture.1)];
        Some(match gesture.grab {
            Grab::Move => gesture
                .original
                .moved(place[0] - gesture.start[0], place[1] - gesture.start[1]),
            Grab::Handle(Handle::Turn) => {
                let frame = gesture.original.frame(picture)?;
                let center = frame.center();
                let angle = |at: [f64; 2]| (at[1] - center[1]).atan2(at[0] - center[0]);
                let turned = frame.rotation + (angle(place) - angle(gesture.start)).to_degrees();
                let right = (turned / 90.).round() * 90.;
                gesture
                    .original
                    .turned_to(if (turned - right).abs() < TURN_SNAP {
                        right
                    } else {
                        turned
                    })
            }
            Grab::Handle(handle) => gesture.original.resized(handle, place, picture),
        })
    }

    /// A drag on the selected mark: the steps follow at once, and the picture
    /// follows through its layer.
    fn drag_mark(&mut self, place: [f64; 2], window: &Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let (Some(gesture), Some(index)) = (&edit.gesture, edit.selected) else {
            return;
        };
        let Some(step) = Self::gesture_step(edit, gesture, place) else {
            return;
        };
        if gesture.moved && edit.steps().get(index) == Some(&step) {
            return;
        }
        let first = !gesture.moved;
        if first {
            edit.remember_steps();
        }
        if let Some(gesture) = &mut edit.gesture {
            gesture.moved = true;
        }
        if let Subject::Picture { steps, .. } = &mut edit.subject
            && let Some(slot) = steps.get_mut(index)
        {
            *slot = step;
        }
        // The first move draws the layer here, so the mark doesn't leave the
        // screen while it's drawn off the UI thread. While another mark's
        // stand-ins are up, this one waits for its own under instead.
        let drawn = first && !edit.holding_for_other(index) && edit.layer_now(index, window, cx);
        self.flatten_under(index, window, cx);
        if !drawn {
            self.draw_layer(window, cx);
        }
        cx.notify();
    }

    /// The pointer's cursor says what a press would do.
    fn hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let place = self.pixel_at(position);
        let Some(edit) = &mut self.edit else {
            return;
        };
        let cursor = match (edit.tool, place, edit.picture()) {
            (None, Some(place), Some(picture)) => {
                let (_, scale) = fit(edit.drawn.get(), (picture.0 as f32, picture.1 as f32));
                if edit.handle_at(place, scale).is_some() {
                    CursorStyle::PointingHand
                } else if edit.mark_at(place, f64::from(INK_REACH / scale)).is_some() {
                    CursorStyle::OpenHand
                } else {
                    CursorStyle::Arrow
                }
            }
            (Some(_), ..) => CursorStyle::Crosshair,
            _ => CursorStyle::Arrow,
        };
        if edit.cursor != cursor {
            edit.cursor = cursor;
            cx.notify();
        }
    }

    /// The selected mark off the picture. Says whether there was one.
    pub fn delete_mark(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let Some(edit) = &mut self.edit else {
            return false;
        };
        if edit.text.is_some() || edit.saving.is_some() {
            return false;
        }
        let Some(index) = edit.selected.take() else {
            return false;
        };
        edit.remember_steps();
        if let Subject::Picture { steps, .. } = &mut edit.subject
            && index < steps.len()
        {
            steps.remove(index);
        }
        self.settle(cx);
        self.flatten(window, cx);
        cx.notify();
        true
    }

    fn dragged(&mut self, event: &MouseMoveEvent, window: &Window, cx: &mut Context<Self>) {
        if !event.dragging() {
            return self.hover(event.position, cx);
        }
        let Some(place) = self.pixel_at(event.position) else {
            return;
        };
        let Some(edit) = &mut self.edit else {
            return;
        };
        if edit.gesture.is_some() {
            return self.drag_mark(place, window, cx);
        }
        if edit.draft.is_empty() {
            return;
        }
        match edit.tool {
            Some(Tool::Pen | Tool::Highlighter) => edit.draft.push(place),
            _ => {
                edit.draft.truncate(1);
                edit.draft.push(place);
            }
        }
        cx.notify();
    }

    fn released(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        if let Some(gesture) = edit.gesture.take() {
            if gesture.moved {
                self.flatten(window, cx);
            }
            return cx.notify();
        }
        let draft = std::mem::take(&mut edit.draft);
        let (Some(tool), Some(image)) = (edit.tool, edit.image().map(|i| image_size(i))) else {
            return;
        };
        let ink = edit.ink;
        let Some(step) = step_of(tool, &draft, ink, image) else {
            return cx.notify();
        };
        if matches!(edit.subject, Subject::Video { .. }) {
            edit.remember();
        }
        match (&mut edit.subject, step) {
            (Subject::Video { crop, .. }, Step::Crop { rect }) => {
                *crop = Some(rect);
                edit.tool = None;
                cx.notify();
            }
            (Subject::Picture { .. }, step) => {
                // A crop changes the picture, so it isn't a tool to stay in. A
                // shape is drawn once and is ready to move. The pen stays down,
                // as a stroke is rarely the last.
                let stays = matches!(tool, Tool::Pen | Tool::Highlighter);
                if !stays {
                    edit.tool = None;
                }
                let is_mark = step.is_mark();
                let index = edit.steps().len();
                self.add_step(step, window, cx);
                if !stays
                    && is_mark
                    && let Some(edit) = &mut self.edit
                {
                    edit.selected = Some(index);
                }
            }
            _ => cx.notify(),
        }
    }

    /// Closes the card without adding its words.
    fn cancel_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.text = None;
            edit.rewriting = None;
            if let Some(keys) = edit.keys_before.take() {
                window.focus(&keys, cx);
            }
        }
        cx.notify();
    }

    /// Add in the card: places the words where the click was, or changes the
    /// words of the Text it opened on.
    pub(super) fn commit_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let Some((field, at, _)) = edit.text.take() else {
            return;
        };
        if let Some(keys) = edit.keys_before.take() {
            window.focus(&keys, cx);
        }
        let words = field.read(cx).text().trim().to_string();
        let rewriting = edit.rewriting.take();
        let Some(picture) = edit.picture() else {
            return;
        };
        if words.is_empty() {
            return cx.notify();
        }
        if let Some(index) = rewriting {
            edit.remember_steps();
            if let Subject::Picture { steps, .. } = &mut edit.subject
                && let Some(slot) = steps.get_mut(index)
                && let Step::Text { text, wrap, .. } = slot
            {
                // New words get room again, instead of the width the old ones fit.
                *text = words;
                *wrap = None;
                *slot = slot.clone().pinned(picture);
            }
            self.flatten(window, cx);
            return cx.notify();
        }
        let step = Step::Text {
            at,
            text: words,
            size: (picture.0.max(picture.1) / 30.).max(12.),
            color: edit.ink,
            wrap: None,
            rotation: 0.,
        }
        .pinned(picture);
        edit.tool = None;
        let index = edit.steps().len();
        self.add_step(step, window, cx);
        if let Some(edit) = &mut self.edit {
            edit.selected = Some(index);
        }
        cx.notify();
    }

    /// Crop on a video: pauses, and puts the current frame where handles can
    /// sit over it, as nothing GPUI draws can sit over the player.
    fn freeze_for_crop(&mut self, cx: &mut Context<Self>) {
        let seconds = self.player.borrow().as_ref().map_or(0., |player| {
            if player.playing() {
                player.toggle();
            }
            player.position()
        });
        let path = self.path.clone();
        let work = cx.background_executor().spawn(async move {
            let bitmap = edits::frame_bitmap(&path, seconds)?;
            image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bytes)
                .map(|pixels| Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
                .ok_or("couldn't draw the frame".to_string())
        });
        cx.spawn(async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(edit) = &mut this.edit else {
                    return;
                };
                let Subject::Video { frozen, .. } = &mut edit.subject else {
                    return;
                };
                match done {
                    Ok(frame) => {
                        if let Some(old) = frozen.replace(frame) {
                            cx.drop_image(old, None);
                        }
                        edit.tool = Some(Tool::Crop);
                    }
                    Err(why) => edit.error = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether the picture area shows the edit's image, in place of the viewer.
    pub(super) fn editing_image(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| match &edit.subject {
            Subject::Picture { .. } => true,
            Subject::Video { frozen, .. } => frozen.is_some() && edit.tool == Some(Tool::Crop),
        })
    }

    /// Writes the edit off the UI thread, then has the owner put it in place.
    fn save(&mut self, save: SaveTo, window: &Window, cx: &mut Context<Self>) {
        let Some(edit) = self.edit.as_mut().filter(|e| e.saving.is_none()) else {
            return;
        };
        let Some(source) = edit.opened.clone() else {
            return;
        };
        let Some(ops) = cx.try_global::<Operations>().map(|ops| ops.0.clone()) else {
            edit.error = Some("Monodrive's file operations aren't running.".into());
            return cx.notify();
        };
        let path = self.path.clone();
        let target_folder = match &save {
            SaveTo::As { path } => path.parent().map(Path::to_path_buf),
            _ => path.parent().map(Path::to_path_buf),
        };
        let Some(folder) = target_folder else {
            return;
        };
        let extension = match (&save, &edit.subject) {
            (SaveTo::As { path }, _) => path
                .extension()
                .map_or("png".to_string(), |ext| ext.to_string_lossy().into_owned()),
            (_, Subject::Picture { .. }) => edits::picture_extension(&path),
            (_, Subject::Video { .. }) => edits::video_extension(&path),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        edit.saving = Some(cancel.clone());
        edit.error = None;
        edit.menu = None;
        edit.confirming = false;
        let steps = match &edit.subject {
            Subject::Picture { steps, .. } => Some(steps.clone()),
            Subject::Video { .. } => None,
        };
        let clips = edit.clips();
        let prepared = edits::prepared_path(&folder, &extension);
        let cancelled = cancel.clone();
        let work = cx.background_executor().spawn(async move {
            let written = match &steps {
                Some(steps) => edits::flatten(&path, steps, &prepared).map(|_| ()),
                None => edits::export(&path, &clips, &prepared, &|| cancel.load(Ordering::Relaxed)),
            };
            let done = written.and_then(|()| {
                // The last moment Cancel can still mean nothing happened.
                if cancelled.load(Ordering::Relaxed) {
                    return Err("cancelled".to_string());
                }
                let request = Request::SaveEdit {
                    source,
                    prepared: prepared.clone(),
                    save,
                };
                let outcome = crate::browser::lock(&ops).submit(Origin::Window, request.clone())?;
                // A refusal comes back as an outcome, with nothing saved.
                if let Some(why) = outcome.failures().next() {
                    return Err(why.to_string());
                }
                Ok((request, outcome))
            });
            // Moved already when it worked, so this only clears a failure.
            edits::discard(&prepared);
            done
        });
        cx.spawn_in(window, async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |this, cx| match done {
                Ok(done) => {
                    cx.emit(Saved(Ok(done)));
                    this.edit = None;
                    // The file on screen may have just been replaced, so it
                    // reads again, unless the selection moved on meanwhile.
                    let (path, folder, position) = this
                        .wanted
                        .take()
                        .unwrap_or_else(|| (this.path.clone(), this.folder, this.position));
                    this.path = PathBuf::new();
                    this.show(&path, folder, position, cx);
                }
                Err(why) => {
                    if let Some(edit) = &mut this.edit {
                        edit.saving = None;
                        if why != "cancelled" {
                            edit.error = Some(why);
                        }
                    }
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Save As: the system's panel, then the same save.
    fn save_as(&mut self, window: &Window, cx: &mut Context<Self>) {
        // Before the panel, as cancelling it must leave nothing open.
        self.close_menu(cx);
        let Some(folder) = self.path.parent().map(Path::to_path_buf) else {
            return;
        };
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let chosen = cx.prompt_for_new_path(&folder, name.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.save(SaveTo::As { path }, window, cx);
            });
        })
        .detach();
    }

    /// The image under the tools, with the mark being drawn over it.
    pub(super) fn edit_surface(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(edit) = &self.edit else {
            return div().size_full().into_any_element();
        };
        let Some(image) = edit.image().cloned() else {
            return div().size_full().into_any_element();
        };
        let measured = edit.drawn.clone();
        let source = image_size(&image);
        let (draft, tool, ink) = (edit.draft.clone(), edit.tool, edit.ink);
        let menu_open = edit.menu.is_some();
        let crop = match &edit.subject {
            Subject::Video { crop, .. } => *crop,
            Subject::Picture { .. } => None,
        };
        let note = self.note_card(source, window, cx);
        // While a mark moves: the rest of the picture, with the mark's own layer
        // over it. Otherwise the picture, with marks it doesn't have yet.
        let (shown, layers): (_, Vec<Layer>) = match edit.stand_ins() {
            Some((under, layer, _)) => (
                under.image.clone(),
                std::iter::once(layer.clone())
                    .chain(
                        edit.landing
                            .iter()
                            .filter(|(at, step, _)| {
                                *at >= under.count && edit.steps().get(*at) == Some(step)
                            })
                            .map(|(_, _, layer)| layer.clone()),
                    )
                    .collect(),
            ),
            None => (
                image,
                edit.landing().map(|(_, _, layer)| layer.clone()).collect(),
            ),
        };
        let cursor = edit
            .gesture
            .as_ref()
            .map_or(edit.cursor, |_| CursorStyle::ClosedHand);
        let picture = (f64::from(source.0), f64::from(source.1));
        let chrome = edit
            .selected
            .and_then(|at| edit.on_screen(at))
            .filter(|_| edit.text.is_none())
            .cloned();
        div()
            .id("preview-edit-surface")
            .debug_selector(|| "preview-edit-surface".into())
            .relative()
            .size_full()
            .overflow_hidden()
            // A press that closes a menu is only that, and leaves no mark.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, window, cx| {
                    if !menu_open {
                        this.pressed(event, window, cx);
                    }
                }),
            )
            .cursor(cursor)
            .on_mouse_move(cx.listener(|this, event, window, cx| this.dragged(event, window, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::released))
            // A mark let go of past the pane's edge stays where it got to.
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, window, cx| {
                    if this
                        .edit
                        .as_ref()
                        .is_some_and(|edit| edit.gesture.is_some())
                    {
                        this.released(event, window, cx);
                    }
                }),
            )
            .child(super::fitted(shown))
            .child(
                canvas(
                    move |bounds, _, _| measured.set(bounds),
                    move |bounds, _, window, _| {
                        let (origin, scale) = fit(bounds, source);
                        for (layer, area) in &layers {
                            let frame = Bounds::new(
                                origin
                                    + point(px(area.x as f32 * scale), px(area.y as f32 * scale)),
                                size(
                                    px(area.width as f32 * scale),
                                    px(area.height as f32 * scale),
                                ),
                            );
                            let _ = window.paint_image(
                                frame,
                                frame,
                                gpui::Corners::default(),
                                layer.clone(),
                                0,
                                false,
                            );
                        }
                        paint_mark(&draft, tool, ink, origin, scale, source, window);
                        if let Some(step) = &chrome {
                            paint_handles(step, picture, origin, scale, window);
                        }
                        if let Some(rect) = crop {
                            let shown = [
                                [rect.x, rect.y],
                                [rect.x + rect.width, rect.y + rect.height],
                            ];
                            paint_mark(
                                &shown,
                                Some(Tool::Crop),
                                ink,
                                origin,
                                scale,
                                source,
                                window,
                            );
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .children(note)
            .into_any_element()
    }

    /// The Text tool's card, beside a dot where the words will land. It's
    /// modal: a press elsewhere closes an empty card and can't lose written
    /// words, and nothing behind it reacts.
    fn note_card(
        &self,
        source: (f32, f32),
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let edit = self.edit.as_ref()?;
        let (field, at, _) = edit.text.as_ref()?;
        let field = field.clone();
        let surface = edit.drawn.get();
        let (origin, scale) = fit(surface, source);
        let dot = origin + point(px(at[0] as f32 * scale), px(at[1] as f32 * scale));
        let width = px(NOTE_WIDTH).min(surface.size.width - px(16.));
        let left = (dot.x + px(12.))
            .min(surface.right() - width - px(8.))
            .max(surface.left() + px(8.));
        let top = (dot.y + px(12.))
            .min(surface.bottom() - px(NOTE_HEIGHT) - px(8.))
            .max(surface.top() + px(8.));
        let empty = field.read(cx).text().trim().is_empty();
        let rewriting = edit.rewriting.is_some();
        let label = if rewriting { "Done" } else { "Add" };
        let add = if empty {
            text_button("note-add", label, ButtonLook::Disabled)
        } else {
            text_button("note-add", label, ButtonLook::Primary)
                .on_click(cx.listener(|this, _, window, cx| this.commit_text(window, cx)))
        };
        let card = theme::floating(theme::Elevation::Popover)
            .debug_selector(|| "note-card".into())
            .absolute()
            .left(left)
            .top(top)
            .w(width)
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .occlude()
            .key_context("Rename")
            .on_action(
                cx.listener(|this, _: &crate::browser::CommitRename, window, cx| {
                    this.commit_text(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::browser::CancelRename, window, cx| {
                    this.cancel_text(window, cx)
                }),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_BODY))
                    .font_weight(FontWeight::MEDIUM)
                    .child(if rewriting { "Change text" } else { "Add text" }),
            )
            .child(
                theme::field(theme::accent())
                    .debug_selector(|| "note-field".into())
                    .rounded(px(theme::RADIUS_ROW))
                    .p_2()
                    .child(field.clone()),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_1()
                    .child(
                        text_button("note-cancel", "Cancel", ButtonLook::Ghost)
                            .debug_selector(|| "note-cancel".into())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel_text(window, cx)),
                            ),
                    )
                    .child(add.debug_selector(|| "note-add".into())),
            );
        let viewport = window.viewport_size();
        Some(deferred(
            gpui::anchored().position(point(px(0.), px(0.))).child(
                div()
                    .id("note-scrim")
                    .debug_selector(|| "note-scrim".into())
                    .relative()
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if field.read(cx).text().trim().is_empty() {
                                this.cancel_text(window, cx);
                            }
                        }),
                    )
                    .when(!rewriting, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(dot.x - px(5.))
                                .top(dot.y - px(5.))
                                .size(px(10.))
                                .rounded_full()
                                // design: a mark's own color, part of the file and not the app.
                                .bg(hsla(edit.ink))
                                .border_1()
                                .border_color(content(1.)),
                        )
                    })
                    .child(card),
            ),
        ))
    }

    /// A tool: a 24 pt icon, named when hovered.
    fn tool_button(
        &self,
        id: &'static str,
        label: &'static str,
        path: &'static str,
        on: bool,
        cx: &mut Context<Self>,
        click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Stateful<gpui::Div> {
        tip(theme::icon_button(id, label, path, true), label)
            .debug_selector(move || id.into())
            .when(on, |d| d.bg(content(0.14)))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(edit) = &mut this.edit {
                    edit.menu = None;
                }
                click(this, window, cx)
            }))
    }

    fn inks(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let edit = self.edit.as_ref()?;
        let ink = edit.ink;
        matches!(edit.subject, Subject::Picture { .. }).then(|| {
            div()
                .flex()
                .gap_1()
                .items_center()
                .children(INKS.map(|(id, color)| {
                    div()
                        .id(id)
                        .debug_selector(move || id.into())
                        .size(px(18.))
                        .rounded_full()
                        .cursor_pointer()
                        // design: a mark's own color, part of the file and not the app.
                        .bg(hsla(color))
                        // An outline, so black shows on a dark pane.
                        .border_1()
                        .border_color(content(0.3))
                        .when(ink == color, |d| d.border_2().border_color(content(1.)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let mut recolored = false;
                            if let Some(edit) = &mut this.edit {
                                edit.ink = color;
                                edit.menu = None;
                                if let Some(index) = edit.selected {
                                    let new = edit.steps().get(index).map(|m| m.in_color(color));
                                    if new.as_ref() != edit.steps().get(index) {
                                        edit.remember_steps();
                                        if let Subject::Picture { steps, .. } = &mut edit.subject
                                            && let (Some(slot), Some(new)) =
                                                (steps.get_mut(index), new)
                                        {
                                            *slot = new;
                                            recolored = true;
                                        }
                                    }
                                }
                            }
                            if recolored {
                                this.flatten(window, cx);
                            }
                            cx.notify();
                        }))
                }))
        })
    }

    /// What this file can have done to it, as icons.
    fn tool_buttons(&mut self, cx: &mut Context<Self>) -> Vec<Stateful<gpui::Div>> {
        let Some(edit) = self.edit.as_ref() else {
            return Vec::new();
        };
        let tool = edit.tool;
        let mut tools = Vec::new();
        match &edit.subject {
            Subject::Picture { .. } => {
                tools.push(self.tool_button(
                    "tool-select",
                    "Move",
                    "tool-select",
                    tool.is_none(),
                    cx,
                    |this, _, cx| {
                        if let Some(edit) = &mut this.edit {
                            edit.tool = None;
                        }
                        cx.notify();
                    },
                ));
                for (kind, id, label, path) in PICTURE_TOOLS {
                    tools.push(self.tool_button(
                        id,
                        label,
                        path,
                        tool == Some(kind),
                        cx,
                        move |this, _, cx| {
                            if let Some(edit) = &mut this.edit {
                                edit.tool = (edit.tool != Some(kind)).then_some(kind);
                                edit.selected = None;
                            }
                            cx.notify();
                        },
                    ));
                }
                tools.push(self.tool_button(
                    "tool-rotate",
                    "Rotate",
                    "tool-rotate",
                    false,
                    cx,
                    |this, window, cx| {
                        this.add_step(Step::Rotate { quarter_turns: 1 }, window, cx);
                    },
                ));
                tools.push(self.tool_button(
                    "tool-flip",
                    "Flip",
                    "tool-flip",
                    false,
                    cx,
                    |this, window, cx| {
                        this.add_step(Step::Flip { vertical: false }, window, cx);
                    },
                ));
            }
            Subject::Video {
                start,
                end,
                muted,
                turns,
                crop,
                ..
            } => {
                let (start, end, muted, turns, cropped) =
                    (*start, *end, *muted, *turns, crop.is_some());
                tools.push(self.tool_button(
                    "tool-start",
                    "Set Start",
                    "tool-start",
                    start.is_some(),
                    cx,
                    |this, _, cx| {
                        let at = this
                            .player
                            .borrow()
                            .as_ref()
                            .map(super::media::Player::position);
                        if let Some(edit) = &mut this.edit {
                            edit.remember();
                            if let Subject::Video { start, .. } = &mut edit.subject {
                                *start = at;
                            }
                        }
                        cx.notify();
                    },
                ));
                tools.push(self.tool_button(
                    "tool-end",
                    "Set End",
                    "tool-end",
                    end.is_some(),
                    cx,
                    |this, _, cx| {
                        let at = this
                            .player
                            .borrow()
                            .as_ref()
                            .map(super::media::Player::position);
                        if let Some(edit) = &mut this.edit {
                            edit.remember();
                            if let Subject::Video { end, .. } = &mut edit.subject {
                                *end = at;
                            }
                        }
                        cx.notify();
                    },
                ));
                tools.push(self.tool_button(
                    "tool-video-crop",
                    "Crop",
                    "tool-crop",
                    tool == Some(Tool::Crop) || cropped,
                    cx,
                    |this, _, cx| {
                        let cropping = this
                            .edit
                            .as_ref()
                            .is_some_and(|e| e.tool == Some(Tool::Crop));
                        if cropping {
                            if let Some(edit) = &mut this.edit {
                                edit.tool = None;
                            }
                            cx.notify();
                        } else {
                            this.freeze_for_crop(cx);
                        }
                    },
                ));
                tools.push(self.tool_button(
                    "tool-video-rotate",
                    "Rotate",
                    "tool-rotate",
                    turns != 0,
                    cx,
                    |this, _, cx| {
                        if let Some(edit) = &mut this.edit {
                            edit.remember();
                            if let Subject::Video { turns, .. } = &mut edit.subject {
                                *turns = (*turns + 1) % 4;
                            }
                        }
                        cx.notify();
                    },
                ));
                tools.push(self.tool_button(
                    "tool-video-mute",
                    "Remove Sound",
                    "tool-mute",
                    muted,
                    cx,
                    |this, _, cx| {
                        if let Some(edit) = &mut this.edit {
                            edit.remember();
                            if let Subject::Video { muted, .. } = &mut edit.subject {
                                *muted = !*muted;
                            }
                        }
                        cx.notify();
                    },
                ));
                tools.push(self.tool_button(
                    "tool-frame",
                    "Save Frame",
                    "tool-frame",
                    false,
                    cx,
                    |this, window, cx| {
                        this.save_frame(window, cx);
                    },
                ));
            }
        }
        tools
    }

    /// Save, which makes a copy and can't touch the original, and an arrow for
    /// the two other ways to save.
    fn save_control(&self, changed: bool, open: bool, cx: &mut Context<Self>) -> gpui::Div {
        let save = if changed {
            text_button("edit-save", "Save", ButtonLook::Primary)
                .debug_selector(|| "edit-save".into())
                .on_click(cx.listener(|this, _, window, cx| this.save(SaveTo::Copy, window, cx)))
        } else {
            text_button("edit-save", "Save", ButtonLook::Disabled)
        };
        // One button split in two: only the outer ends are round.
        // design: the inner corners of a split button meet square.
        let save = save.rounded_r_none().pr(px(8.));
        let more = theme::button(
            "edit-more",
            if changed {
                ButtonLook::Primary
            } else {
                ButtonLook::Disabled
            },
        )
        // design: the inner corners of a split button meet square.
        .rounded_l_none()
        .px(px(5.))
        .child(theme::icon("chevron-down-hi", 14.).text_color(if changed {
            theme::on_accent()
        } else {
            content(0.3)
        }));
        let more = if changed {
            more.debug_selector(|| "edit-more".into())
                .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Save, cx)))
        } else {
            more
        };
        div()
            .relative()
            .flex_shrink_0()
            .flex()
            .gap(px(1.))
            .child(save)
            .child(more)
            .when(open, |d| {
                let row = |id: &'static str, label: &'static str| {
                    theme::row(false)
                        .id(id)
                        .debug_selector(move || id.into())
                        .px_2()
                        .h(px(theme::ROW_HEIGHT))
                        .flex()
                        .items_center()
                        .text_size(px(theme::TEXT_BODY))
                        .child(label)
                };
                d.child(deferred(
                    menu_card(200., true)
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)))
                        .child(
                            row("edit-replace", "Replace Original").on_click(cx.listener(
                                |this, _, window, cx| this.save(SaveTo::Replace, window, cx),
                            )),
                        )
                        .child(
                            row("edit-save-as", "Save As…").on_click(
                                cx.listener(|this, _, window, cx| this.save_as(window, cx)),
                            ),
                        ),
                ))
            })
    }

    /// The title row while editing: tools at the left, Undo, Save and Done at the
    /// right. A narrow pane moves the tools behind a menu, so nothing wraps.
    pub(super) fn edit_row(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let width = self.row_width.get();
        if let Some(edit) = &mut self.edit {
            // A pane that grew has no Tools button left to hold this menu open.
            let picture = matches!(edit.subject, Subject::Picture { .. });
            if edit.menu == Some(Menu::Tools) && !row_fits(width, picture).0 {
                edit.menu = None;
            }
        }
        let edit = self.edit.as_ref()?;
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let saving = edit.saving.clone();
        let changed = edit.changed();
        let confirming = edit.confirming;
        let menu = edit.menu;
        let picture = matches!(edit.subject, Subject::Picture { .. });
        let edit_selected = edit.selected.is_some();
        let row_width = self.row_width.clone();
        let (narrow, very_narrow) = row_fits(row_width.get(), picture);
        let name_cell = div()
            .debug_selector(|| "preview-name".into())
            .min_w_0()
            .max_w(px(if narrow { 120. } else { 90. }))
            .flex_shrink(1.)
            .truncate()
            .text_size(px(theme::TEXT_BODY))
            .font_weight(FontWeight::MEDIUM)
            .child(name);
        let measure = canvas(
            move |bounds, window, _| {
                let width = bounds.size.width.as_f32();
                // A width the last frame guessed wrong lays out again next frame.
                if row_fits(width, picture) != row_fits(row_width.get(), picture) {
                    window.on_next_frame(|window, _| window.refresh());
                }
                row_width.set(width);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let row = theme::pane_bar(theme::TITLE_BAR_HEIGHT)
            .debug_selector(|| "preview-tools".into())
            .relative()
            .gap_1()
            .flex_shrink_0()
            .border_b_1()
            .border_color(content(0.06))
            .child(measure);
        let status = |text: &'static str| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child(text)
        };
        let row = if let Some(cancel) = saving {
            row.child(name_cell).child(status("Saving…")).child(
                text_button("edit-cancel", "Cancel", ButtonLook::Secondary)
                    .debug_selector(|| "edit-cancel".into())
                    .on_click(move |_, _, _| cancel.store(true, Ordering::Relaxed)),
            )
        } else if confirming {
            row.child(name_cell)
                .child(status("Discard these changes?"))
                .child(
                    text_button("edit-keep", "Keep Editing", ButtonLook::Ghost)
                        .debug_selector(|| "edit-keep".into())
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(edit) = &mut this.edit {
                                edit.confirming = false;
                            }
                            cx.notify();
                        })),
                )
                .child(
                    text_button("edit-discard", "Discard", ButtonLook::Primary)
                        .debug_selector(|| "edit-discard".into())
                        .on_click(cx.listener(|this, _, _, cx| this.leave_edit(cx))),
                )
        } else {
            let tools = self.tool_buttons(cx);
            let inks = self.inks(cx);
            let delete = edit_selected.then(|| {
                self.tool_button(
                    "edit-delete",
                    "Delete",
                    "trash",
                    false,
                    cx,
                    |this, window, cx| {
                        this.delete_mark(window, cx);
                    },
                )
            });
            let undo = self.tool_button(
                "edit-undo",
                "Undo",
                "tool-undo",
                false,
                cx,
                |this, window, cx| this.undo_step(window, cx),
            );
            let save = self.save_control(changed, menu == Some(Menu::Save), cx);
            let done = text_button("edit-done", "Done", ButtonLook::Ghost)
                .debug_selector(|| "edit-done".into())
                .on_click(cx.listener(|this, _, window, cx| this.toggle_edit(window, cx)));
            let separator = || {
                div()
                    .flex_shrink_0()
                    .w(px(1.))
                    .h(px(16.))
                    .mx_1()
                    .bg(content(0.12))
            };
            let row = if very_narrow {
                row
            } else {
                row.child(name_cell).child(separator())
            };
            let row = if narrow {
                let open = menu == Some(Menu::Tools);
                let more = theme::button("edit-tools", ButtonLook::Ghost)
                    .debug_selector(|| "edit-tools".into())
                    .when(open, |d| d.bg(content(0.14)))
                    .child("Tools")
                    .child(theme::icon("chevron-down-hi", 12.))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Tools, cx)));
                row.child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(more)
                        .when(open, |d| {
                            d.child(deferred(
                                menu_card(232., false)
                                    .on_mouse_down_out(
                                        cx.listener(|this, _, _, cx| this.close_menu(cx)),
                                    )
                                    .child(div().flex().flex_wrap().gap_1().p_2().children(tools))
                                    .children(inks.map(|inks| div().px_2().pb_2().child(inks))),
                            ))
                        }),
                )
            } else {
                row.children(tools)
                    .when(inks.is_some(), |d| d.child(separator()))
                    .children(inks)
            };
            row.child(div().flex_1().min_w_0())
                .children(delete)
                .child(undo)
                .child(save)
                .child(done)
        };
        Some(row.into_any_element())
    }

    /// A failed save's reason, under the row, until the next change.
    pub(super) fn edit_error(&self) -> Option<impl IntoElement + use<>> {
        let why = self.edit.as_ref()?.error.clone()?;
        Some(
            theme::pane_bar(theme::PATH_BAR_HEIGHT)
                .debug_selector(|| "edit-status".into())
                .flex_shrink_0()
                .border_b_1()
                .border_color(content(0.06))
                .text_size(px(theme::TEXT_SMALL))
                .text_color(danger())
                .child(div().min_w_0().truncate().child(why)),
        )
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.menu = None;
        }
        cx.notify();
    }

    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.menu = (edit.menu != Some(menu)).then_some(menu);
        }
        cx.notify();
    }

    /// A menu is open, which a player would otherwise draw over.
    pub(super) fn menu_open(&self) -> bool {
        self.edit.as_ref().is_some_and(|edit| edit.menu.is_some())
    }

    fn save_frame(&mut self, window: &Window, cx: &mut Context<Self>) {
        let seconds = self
            .player
            .borrow()
            .as_ref()
            .map_or(0., |player| player.position());
        let Some(source) = self.edit.as_ref().and_then(|edit| edit.opened.clone()) else {
            return;
        };
        let Some(ops) = cx.try_global::<Operations>().map(|ops| ops.0.clone()) else {
            return;
        };
        let path: PathBuf = self.path.clone();
        let Some(folder) = path.parent().map(Path::to_path_buf) else {
            return;
        };
        let prepared = edits::prepared_path(&folder, "png");
        let work = cx.background_executor().spawn(async move {
            let done = edits::frame(&path, seconds, &prepared).and_then(|()| {
                let request = Request::SaveEdit {
                    source,
                    prepared: prepared.clone(),
                    save: SaveTo::Copy,
                };
                let outcome = crate::browser::lock(&ops).submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            });
            edits::discard(&prepared);
            done
        });
        cx.spawn_in(window, async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |_, cx| cx.emit(Saved(done)));
        })
        .detach();
    }
}

/// The mark being drawn, in GPUI, over the picture.
fn paint_mark(
    draft: &[[f64; 2]],
    tool: Option<Tool>,
    ink: [f64; 4],
    origin: Point<Pixels>,
    scale: f32,
    image: (f32, f32),
    window: &mut Window,
) {
    let (Some(tool), Some(first), Some(last)) = (tool, draft.first(), draft.last()) else {
        return;
    };
    let on_screen = |p: &[f64; 2]| {
        point(
            origin.x + px(p[0] as f32 * scale),
            origin.y + px(p[1] as f32 * scale),
        )
    };
    let long = image.0.max(image.1);
    let width = px((long / 300.).max(2.) * scale);
    let area = corners(*first, *last, image);
    let frame = Bounds::new(
        point(
            origin.x + px(area.x as f32 * scale),
            origin.y + px(area.y as f32 * scale),
        ),
        size(
            px(area.width as f32 * scale),
            px(area.height as f32 * scale),
        ),
    );
    // design: a mark's own color, part of the file and not the app.
    let color = hsla(ink);
    let stroke =
        |points: &[Point<Pixels>], width: Pixels, color: gpui::Hsla, window: &mut Window| {
            let mut path = PathBuilder::stroke(width);
            path.add_polygon(points, false);
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        };
    match tool {
        Tool::Pen | Tool::Highlighter => {
            let points: Vec<Point<Pixels>> = draft.iter().map(on_screen).collect();
            if points.len() > 1 {
                let (width, color) = if tool == Tool::Pen {
                    (width, color)
                } else {
                    // design: a mark's own color, part of the file and not the app.
                    (width * 4., hsla([ink[0], ink[1], ink[2], 0.4]))
                };
                stroke(&points, width, color, window);
            }
        }
        Tool::Arrow => {
            stroke(&[on_screen(first), on_screen(last)], width, color, window);
        }
        Tool::Ellipse => {
            let (cx_, cy_) = (area.x + area.width / 2., area.y + area.height / 2.);
            let points: Vec<Point<Pixels>> = (0..=48)
                .map(|n| {
                    let angle = f64::from(n) / 48. * std::f64::consts::TAU;
                    on_screen(&[
                        cx_ + area.width / 2. * angle.cos(),
                        cy_ + area.height / 2. * angle.sin(),
                    ])
                })
                .collect();
            stroke(&points, width, color, window);
        }
        Tool::Rectangle | Tool::Crop | Tool::Text => {
            let color = if tool == Tool::Crop {
                theme::accent()
            } else {
                color
            };
            window.paint_quad(gpui::outline(frame, color, gpui::BorderStyle::Solid));
        }
        Tool::Redact => {
            // design: a mark's own color, part of the file and not the app.
            window.paint_quad(gpui::fill(frame, hsla([0., 0., 0., 1.])));
        }
    }
}

/// The selected mark's frame and handles.
fn paint_handles(
    step: &Step,
    picture: (f64, f64),
    origin: Point<Pixels>,
    scale: f32,
    window: &mut Window,
) {
    let on_screen = |at: [f64; 2]| {
        point(
            origin.x + px(at[0] as f32 * scale),
            origin.y + px(at[1] as f32 * scale),
        )
    };
    let handles = step.handles(picture, f64::from(TURN_HANDLE_OUT / scale));
    let line = |points: &[Point<Pixels>], window: &mut Window| {
        let mut path = PathBuilder::stroke(px(1.));
        path.add_polygon(points, false);
        if let Ok(path) = path.build() {
            window.paint_path(path, theme::accent());
        }
    };
    if let Some(frame) = step.frame(picture).filter(|_| handles.len() > 2) {
        let mut outline: Vec<_> = (0..4).map(|at| on_screen(frame.corner(at))).collect();
        outline.push(outline[0]);
        line(&outline, window);
        line(
            &[
                on_screen(frame.above_top(0.)),
                on_screen(frame.above_top(f64::from(TURN_HANDLE_OUT / scale))),
            ],
            window,
        );
    }
    for (_, at) in handles {
        let side = px(10.);
        let center = on_screen(at);
        window.paint_quad(
            gpui::fill(
                Bounds::new(center - point(side / 2., side / 2.), size(side, side)),
                theme::accent(),
            )
            .corner_radii(side / 2.)
            .border_widths(px(1.))
            .border_color(content(1.)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_becomes_the_step_its_tool_says_in_picture_pixels() {
        let image = (300., 300.);
        let ink = INKS[0].1;
        let drag = [[250., 200.], [100., 50.]];
        // Dragged either way, a rectangle's corner is its least.
        let rect = Rect {
            x: 100.,
            y: 50.,
            width: 150.,
            height: 150.,
        };
        assert_eq!(
            step_of(Tool::Crop, &drag, ink, image),
            Some(Step::Crop { rect })
        );
        assert_eq!(
            step_of(Tool::Redact, &drag, ink, image),
            Some(Step::Redact { rect, rotation: 0. })
        );
        // A press without a drag makes no shape, but it's a dot for the pen.
        let press = [[10., 10.]];
        assert_eq!(step_of(Tool::Crop, &press, ink, image), None);
        assert_eq!(step_of(Tool::Arrow, &press, ink, image), None);
        assert!(matches!(
            step_of(Tool::Pen, &press, ink, image),
            Some(Step::Pen { .. })
        ));
        // Past the edge is kept to the picture.
        let past = corners([-20., 280.], [40., 500.], image);
        assert_eq!(
            (past.x, past.y, past.width, past.height),
            (0., 280., 40., 20.)
        );
    }

    #[test]
    fn undo_takes_a_video_back_one_change_at_a_time() {
        let mut edit = Edit {
            subject: Subject::Video {
                start: None,
                end: None,
                muted: false,
                turns: 0,
                frozen: None,
                crop: None,
                past: Vec::new(),
            },
            tool: None,
            ink: INKS[0].1,
            draft: Vec::new(),
            drawn: Rc::new(Cell::new(Bounds::default())),
            selected: None,
            gesture: None,
            dragged: Dragged::default(),
            landing: Vec::new(),
            cursor: CursorStyle::Arrow,
            text: None,
            rewriting: None,
            keys_before: None,
            confirming: false,
            menu: None,
            saving: None,
            error: None,
            opened: None,
        };
        let set = |edit: &mut Edit, change: fn(&mut Option<f64>, &mut i32)| {
            edit.remember();
            if let Subject::Video { start, turns, .. } = &mut edit.subject {
                change(start, turns);
            }
        };
        set(&mut edit, |start, _| *start = Some(1.));
        set(&mut edit, |_, turns| *turns = 1);
        edit.undo_video();
        // The turn went, and the trim stayed.
        assert!(edit.changed());
        assert_eq!(
            edit.clips(),
            [Clip::Trim {
                start: 1.,
                end: f64::MAX
            }]
        );
        edit.undo_video();
        assert!(!edit.changed());
        edit.undo_video();
        assert!(!edit.changed());
    }

    #[test]
    fn a_narrow_row_moves_the_tools_and_then_the_name_out_of_the_way() {
        // Not measured yet, and wide.
        assert_eq!(row_fits(0., true), (false, false));
        assert_eq!(row_fits(800., true), (false, false));
        // A video has fewer tools, so it stays wide for longer.
        assert_eq!(row_fits(600., true), (true, false));
        assert_eq!(row_fits(600., false), (false, false));
        assert_eq!(row_fits(300., false), (true, true));
    }

    #[test]
    fn the_picture_is_fit_centered_and_never_enlarged() {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(400.), px(400.)));
        let (origin, scale) = fit(bounds, (800., 400.));
        assert_eq!(scale, 0.5);
        assert_eq!((origin.x, origin.y), (px(10.), px(120.)));
        let (origin, scale) = fit(bounds, (100., 100.));
        assert_eq!(scale, 1.);
        assert_eq!((origin.x, origin.y), (px(160.), px(170.)));
    }
}
