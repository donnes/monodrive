//! The preview's Edit mode: tools under the picture, with a recipe in memory.
//! Each committed step flattens off the UI thread and the result replaces what's
//! shown, so what you see is what saving writes. Only the mark being drawn is
//! GPUI's. Nothing touches the disk until Save, and undo here pops a step.

use super::{Body, PreviewView};
use crate::browser::Operations;
use crate::text_field::{self, TextField};
use crate::theme::{self, ButtonLook, content, danger, text_button};
use gpui::{
    AnyElement, Bounds, Context, Entity, EventEmitter, Focusable, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Pixels, Point, RenderImage,
    Stateful, Subscription, Task, Window, canvas, deferred, div, point, prelude::*, px, size,
};
use monodrive_core::edits::{self, Clip, Rect, Step};
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
const PICTURE_ROW: f32 = 720.;
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
        /// The picture as the steps leave it, in pixels.
        image: Option<Arc<RenderImage>>,
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
    /// Text waiting for a place: the field, and where it goes.
    text: Option<(Entity<TextField>, [f64; 2], Subscription)>,
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
        Tool::Redact if !tiny => Step::Redact { rect },
        Tool::Rectangle if !tiny => Step::Rectangle {
            rect,
            color: ink,
            width,
        },
        Tool::Ellipse if !tiny => Step::Ellipse {
            rect,
            color: ink,
            width,
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
        },
        Tool::Highlighter => Step::Highlighter {
            points: draft.to_vec(),
            color: [ink[0], ink[1], ink[2], 0.4],
            width: width * 4.,
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
                    image: None,
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
            text: None,
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
        if edit.menu.take().is_some() || edit.text.take().is_some() {
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
                    image, flattening, ..
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
                    }
                    Err(why) => edit.error = Some(why),
                }
                cx.notify();
            });
        }));
    }

    fn add_step(&mut self, step: Step, window: &Window, cx: &mut Context<Self>) {
        if let Some(Edit {
            subject: Subject::Picture { steps, .. },
            error,
            ..
        }) = &mut self.edit
        {
            steps.push(step);
            *error = None;
        }
        self.flatten(window, cx);
        cx.notify();
    }

    fn undo_step(&mut self, window: &Window, cx: &mut Context<Self>) {
        match &mut self.edit {
            Some(Edit {
                subject: Subject::Picture { steps, .. },
                ..
            }) => {
                steps.pop();
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
            Some(Tool::Text) => {
                edit.keys_before = window.focused(cx);
                let field = cx.new(|cx| TextField::new("Text", "Type, then press Return", cx));
                window.focus(&field.focus_handle(cx), cx);
                let edits = cx.subscribe(&field, |_, _, _: &text_field::Changed, _| {});
                edit.text = Some((field, place, edits));
            }
            Some(_) => edit.draft = vec![place],
            None => {}
        }
        cx.notify();
    }

    fn dragged(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !event.dragging() {
            return;
        }
        let Some(place) = self.pixel_at(event.position) else {
            return;
        };
        let Some(edit) = &mut self.edit else {
            return;
        };
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
                // A crop changes the picture, so it isn't a tool to stay in.
                if matches!(step, Step::Crop { .. }) {
                    edit.tool = None;
                }
                self.add_step(step, window, cx);
            }
            _ => cx.notify(),
        }
    }

    /// Return in the text field: places the words where the click was.
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
        let long = edit.image().map_or(1000., |i| {
            let (w, h) = image_size(i);
            f64::from(w.max(h))
        });
        let color = edit.ink;
        if !words.is_empty() {
            self.add_step(
                Step::Text {
                    at,
                    text: words,
                    size: (long / 30.).max(12.),
                    color,
                },
                window,
                cx,
            );
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
    pub(super) fn edit_surface(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
            .on_mouse_move(cx.listener(|this, event, _, cx| this.dragged(event, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::released))
            .child(super::fitted(image))
            .child(
                canvas(
                    move |bounds, _, _| measured.set(bounds),
                    move |bounds, _, window, _| {
                        let (origin, scale) = fit(bounds, source);
                        paint_mark(&draft, tool, ink, origin, scale, source, window);
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
            .into_any_element()
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
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(edit) = &mut this.edit {
                                edit.ink = color;
                                edit.menu = None;
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
                                edit.text = None;
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
        let text_field = edit.text.as_ref().map(|(field, _, _)| field.clone());
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
        } else if let Some(field) = text_field {
            row.child(
                theme::field(theme::accent())
                    .debug_selector(|| "edit-text-field".into())
                    .key_context("Rename")
                    .on_action(
                        cx.listener(|this, _: &crate::browser::CommitRename, window, cx| {
                            this.commit_text(window, cx)
                        }),
                    )
                    .on_action(
                        cx.listener(|this, _: &crate::browser::CancelRename, window, cx| {
                            if let Some(edit) = &mut this.edit {
                                edit.text = None;
                                if let Some(keys) = edit.keys_before.take() {
                                    window.focus(&keys, cx);
                                }
                            }
                            cx.notify();
                        }),
                    )
                    .h(px(theme::FIELD_HEIGHT))
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .child(field),
            )
        } else {
            let tools = self.tool_buttons(cx);
            let inks = self.inks(cx);
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
            Some(Step::Redact { rect })
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
            text: None,
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
