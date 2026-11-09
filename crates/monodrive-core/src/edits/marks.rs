//! The marks in a recipe as things that can still move: where each one is, what
//! a press hits, and what a drag makes of it. The window edits marks through
//! these, and an agent writes the same fields into a recipe directly.

use super::{Rect, Step, canvas, draw_mark, finished, text_frame};
use crate::thumbnails::Bitmap;
use objc2_core_graphics::CGContext;

const SMALLEST: f64 = 2.;
const SMALLEST_TEXT: f64 = 6.;

/// Where a mark sits: `rect`, then turned clockwise about its center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub rect: Rect,
    pub rotation: f64,
}

/// What a mark can be dragged by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// From the top left, clockwise.
    Corner(usize),
    /// An arrow's ends.
    From,
    To,
    Turn,
}

/// `at` turned clockwise about `about`, in top-left coordinates.
fn turned(at: [f64; 2], about: [f64; 2], degrees: f64) -> [f64; 2] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (x, y) = (at[0] - about[0], at[1] - about[1]);
    [about[0] + x * cos - y * sin, about[1] + x * sin + y * cos]
}

fn around(points: &[[f64; 2]]) -> Rect {
    let (mut x0, mut y0) = (f64::MAX, f64::MAX);
    let (mut x1, mut y1) = (f64::MIN, f64::MIN);
    for point in points {
        x0 = x0.min(point[0]);
        y0 = y0.min(point[1]);
        x1 = x1.max(point[0]);
        y1 = y1.max(point[1]);
    }
    if points.is_empty() {
        return Rect {
            x: 0.,
            y: 0.,
            width: 0.,
            height: 0.,
        };
    }
    Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

/// How far `at` is from the segment `a` to `b`.
fn off_segment(at: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx * dx + dy * dy;
    let along = if length == 0. {
        0.
    } else {
        (((at[0] - a[0]) * dx + (at[1] - a[1]) * dy) / length).clamp(0., 1.)
    };
    (at[0] - a[0] - along * dx).hypot(at[1] - a[1] - along * dy)
}

impl Frame {
    pub fn center(&self) -> [f64; 2] {
        [
            self.rect.x + self.rect.width / 2.,
            self.rect.y + self.rect.height / 2.,
        ]
    }

    fn unturned_corner(&self, corner: usize) -> [f64; 2] {
        let Rect {
            x,
            y,
            width,
            height,
        } = self.rect;
        match corner % 4 {
            0 => [x, y],
            1 => [x + width, y],
            2 => [x + width, y + height],
            _ => [x, y + height],
        }
    }

    /// A corner where it shows, in the picture's pixels.
    pub fn corner(&self, corner: usize) -> [f64; 2] {
        turned(self.unturned_corner(corner), self.center(), self.rotation)
    }

    /// The point above the top edge's middle, `by` pixels out from it.
    pub fn above_top(&self, by: f64) -> [f64; 2] {
        let center = self.center();
        turned([center[0], self.rect.y - by], center, self.rotation)
    }

    /// Whether `at`, in the picture's pixels, is inside the frame.
    pub fn contains(&self, at: [f64; 2], slop: f64) -> bool {
        let local = turned(at, self.center(), -self.rotation);
        local[0] >= self.rect.x - slop
            && local[0] <= self.rect.x + self.rect.width + slop
            && local[1] >= self.rect.y - slop
            && local[1] <= self.rect.y + self.rect.height + slop
    }
}

impl Step {
    /// Drawn on the picture, as opposed to changing its shape.
    pub fn is_mark(&self) -> bool {
        !matches!(
            self,
            Step::Crop { .. } | Step::Rotate { .. } | Step::Flip { .. }
        )
    }

    /// Where this mark sits in a picture of this size. `None` isn't a mark.
    pub fn frame(&self, picture: (f64, f64)) -> Option<Frame> {
        let (rect, rotation) = match self {
            Step::Crop { .. } | Step::Rotate { .. } | Step::Flip { .. } => return None,
            Step::Pen {
                points, rotation, ..
            }
            | Step::Highlighter {
                points, rotation, ..
            } => (around(points), *rotation),
            Step::Arrow { from, to, .. } => (around(&[*from, *to]), 0.),
            Step::Rectangle { rect, rotation, .. }
            | Step::Ellipse { rect, rotation, .. }
            | Step::Redact { rect, rotation } => (*rect, *rotation),
            Step::Text {
                at,
                text,
                size,
                wrap,
                rotation,
                ..
            } => (text_frame(*at, text, *size, *wrap, picture), *rotation),
        };
        Some(Frame { rect, rotation })
    }

    /// What this mark can be dragged by, and where each handle is. `out` is
    /// how many pixels the turning handle sits off the mark.
    pub fn handles(&self, picture: (f64, f64), out: f64) -> Vec<(Handle, [f64; 2])> {
        if let Step::Arrow { from, to, .. } = self {
            return vec![(Handle::From, *from), (Handle::To, *to)];
        }
        let Some(frame) = self.frame(picture) else {
            return Vec::new();
        };
        let mut handles: Vec<_> = (0..4)
            .map(|corner| (Handle::Corner(corner), frame.corner(corner)))
            .collect();
        handles.push((Handle::Turn, frame.above_top(out)));
        handles
    }

    /// Whether a press at `at` lands on the mark's ink, or inside it for the
    /// ones that are solid. An empty box doesn't cover what's inside it.
    pub fn hit(&self, at: [f64; 2], picture: (f64, f64), slop: f64) -> bool {
        let Some(frame) = self.frame(picture) else {
            return false;
        };
        let local = turned(at, frame.center(), -frame.rotation);
        let along = |points: &[[f64; 2]], reach: f64| {
            if let [only] = points {
                return off_segment(local, *only, *only) <= reach;
            }
            points
                .windows(2)
                .any(|pair| off_segment(local, pair[0], pair[1]) <= reach)
        };
        match self {
            Step::Pen { points, width, .. } | Step::Highlighter { points, width, .. } => {
                along(points, width / 2. + slop)
            }
            Step::Arrow {
                from, to, width, ..
            } => along(&[*from, *to], width / 2. + slop),
            Step::Rectangle { rect, width, .. } => {
                let reach = width / 2. + slop;
                frame.contains(at, reach)
                    && (local[0] - rect.x < reach
                        || rect.x + rect.width - local[0] < reach
                        || local[1] - rect.y < reach
                        || rect.y + rect.height - local[1] < reach)
            }
            Step::Ellipse { rect, width, .. } => {
                let center = frame.center();
                let (rx, ry) = ((rect.width / 2.).max(1.), (rect.height / 2.).max(1.));
                let round = ((local[0] - center[0]) / rx).hypot((local[1] - center[1]) / ry);
                (round - 1.).abs() * rx.min(ry) <= width / 2. + slop
            }
            _ => frame.contains(at, slop),
        }
    }

    /// The mark `dx` and `dy` pixels away.
    pub fn moved(&self, dx: f64, dy: f64) -> Step {
        let mut step = self.clone();
        let shift = |point: &mut [f64; 2]| {
            point[0] += dx;
            point[1] += dy;
        };
        match &mut step {
            Step::Crop { .. } | Step::Rotate { .. } | Step::Flip { .. } => {}
            Step::Pen { points, .. } | Step::Highlighter { points, .. } => {
                points.iter_mut().for_each(shift);
            }
            Step::Arrow { from, to, .. } => {
                shift(from);
                shift(to);
            }
            Step::Rectangle { rect, .. }
            | Step::Ellipse { rect, .. }
            | Step::Redact { rect, .. } => {
                rect.x += dx;
                rect.y += dy;
            }
            Step::Text { at, .. } => shift(at),
        }
        step
    }

    /// The mark set to turn `degrees` clockwise. An arrow turns by its ends.
    pub fn turned_to(&self, degrees: f64) -> Step {
        let mut step = self.clone();
        match &mut step {
            Step::Pen { rotation, .. }
            | Step::Highlighter { rotation, .. }
            | Step::Rectangle { rotation, .. }
            | Step::Ellipse { rotation, .. }
            | Step::Redact { rotation, .. }
            | Step::Text { rotation, .. } => *rotation = degrees.rem_euclid(360.),
            Step::Crop { .. } | Step::Rotate { .. } | Step::Flip { .. } | Step::Arrow { .. } => {}
        }
        step
    }

    /// The mark in another ink. A redact stays black, and a highlighter keeps
    /// the alpha it's see-through by.
    pub fn in_color(&self, ink: [f64; 4]) -> Step {
        let mut step = self.clone();
        match &mut step {
            Step::Highlighter { color, .. } => {
                *color = [ink[0], ink[1], ink[2], color[3]];
            }
            Step::Pen { color, .. }
            | Step::Arrow { color, .. }
            | Step::Rectangle { color, .. }
            | Step::Ellipse { color, .. }
            | Step::Text { color, .. } => *color = ink,
            Step::Crop { .. } | Step::Rotate { .. } | Step::Flip { .. } | Step::Redact { .. } => {}
        }
        step
    }

    /// Text gets a fixed place and width, so moving it later can't reflow it.
    /// An agent's recipe may leave them to the picture's right edge.
    pub fn pinned(self, picture: (f64, f64)) -> Step {
        let Step::Text {
            at,
            text,
            size,
            color,
            wrap,
            rotation,
        } = self
        else {
            return self;
        };
        let frame = text_frame(at, &text, size, wrap, picture);
        Step::Text {
            at: [frame.x, frame.y],
            text,
            size,
            color,
            // A little over, as the measure is what it wrapped to.
            wrap: wrap.or(Some(frame.width.ceil() + 1.)),
            rotation,
        }
    }

    /// The mark dragged by `handle` to `to`, with the opposite corner holding
    /// still. Turning is `turned_to`, as it needs where the drag began.
    pub fn resized(&self, handle: Handle, to: [f64; 2], picture: (f64, f64)) -> Step {
        let mut step = self.clone();
        if let Step::Arrow { from, to: head, .. } = &mut step {
            match handle {
                Handle::From => *from = to,
                Handle::To => *head = to,
                Handle::Corner(_) | Handle::Turn => {}
            }
            return step;
        }
        let (Handle::Corner(corner), Some(frame)) = (handle, self.frame(picture)) else {
            return step;
        };
        let held = frame.corner(corner + 2);
        let dragged = frame.unturned_corner(corner);
        let fixed = frame.unturned_corner(corner + 2);
        // The drag in the mark's own axes, from the corner that holds.
        let along = turned(to, held, -frame.rotation);
        let reach = [along[0] - held[0], along[1] - held[1]];
        let was = [dragged[0] - fixed[0], dragged[1] - fixed[1]];
        let ratio = |reach: f64, was: f64| if was == 0. { 1. } else { reach / was };
        let (scale_x, scale_y) = (ratio(reach[0], was[0]), ratio(reach[1], was[1]));
        // Where the center goes for a new size. Dragged past the corner that
        // holds, a scale goes negative and the mark opens the other way.
        let center_at = |scale_x: f64, scale_y: f64, width: f64, height: f64| {
            let side = |scale: f64, was: f64| {
                if (scale < 0.) == (was < 0.) {
                    0.5
                } else {
                    -0.5
                }
            };
            turned(
                [
                    held[0] + side(scale_x, was[0]) * width,
                    held[1] + side(scale_y, was[1]) * height,
                ],
                held,
                frame.rotation,
            )
        };
        match &mut step {
            Step::Crop { .. } | Step::Rotate { .. } | Step::Flip { .. } | Step::Arrow { .. } => {}
            Step::Rectangle { rect, .. }
            | Step::Ellipse { rect, .. }
            | Step::Redact { rect, .. } => {
                let (width, height) = (
                    (rect.width * scale_x.abs()).max(SMALLEST),
                    (rect.height * scale_y.abs()).max(SMALLEST),
                );
                let center = center_at(scale_x, scale_y, width, height);
                *rect = Rect {
                    x: center[0] - width / 2.,
                    y: center[1] - height / 2.,
                    width,
                    height,
                };
            }
            Step::Pen { points, .. } | Step::Highlighter { points, .. } => {
                let old = frame.center();
                let width = frame.rect.width * scale_x.abs();
                let height = frame.rect.height * scale_y.abs();
                let center = center_at(scale_x, scale_y, width, height);
                for point in points.iter_mut() {
                    *point = [
                        center[0] + (point[0] - old[0]) * scale_x,
                        center[1] + (point[1] - old[1]) * scale_y,
                    ];
                }
            }
            Step::Text { at, size, wrap, .. } => {
                // Type keeps its shape, so it scales along the corner's diagonal,
                // which keeps the corner near the pointer on a wide, short frame.
                let least = SMALLEST_TEXT / *size;
                let along_diagonal = (reach[0] * was[0] + reach[1] * was[1])
                    / (was[0] * was[0] + was[1] * was[1]).max(1.);
                let scale = along_diagonal.max(least);
                let (width, height) = (frame.rect.width * scale, frame.rect.height * scale);
                let center = center_at(1., 1., width, height);
                *at = [center[0] - width / 2., center[1] - height / 2.];
                *size *= scale;
                *wrap = wrap.map(|wrap| wrap * scale);
            }
        }
        step
    }
}

/// A mark alone on a clear bitmap, `scale` times its pixels, and the part of the
/// picture that bitmap covers. It goes through the same drawing as saving, so a
/// mark being dragged looks like the mark that gets saved.
pub fn render_mark(step: &Step, picture: (f64, f64), scale: f64) -> Result<(Bitmap, Rect), String> {
    let frame = step.frame(picture).ok_or("That step isn't a mark.")?;
    let corners: Vec<_> = (0..4).map(|corner| frame.corner(corner)).collect();
    let reach = around(&corners);
    // Room for a stroke's width and an arrow's barbs.
    let pad = match step {
        Step::Pen { width, .. } | Step::Highlighter { width, .. } => *width,
        Step::Rectangle { width, .. } | Step::Ellipse { width, .. } => *width,
        Step::Arrow { width, .. } => width * 5.,
        _ => 2.,
    };
    let x0 = (reach.x - pad).max(0.).floor();
    let y0 = (reach.y - pad).max(0.).floor();
    let x1 = (reach.x + reach.width + pad).min(picture.0).ceil();
    let y1 = (reach.y + reach.height + pad).min(picture.1).ceil();
    // Off the picture altogether, one clear pixel is all there is to show.
    let area = Rect {
        x: x0,
        y: y0,
        width: (x1 - x0).max(1.),
        height: (y1 - y0).max(1.),
    };
    let (width, height) = (
        (area.width * scale).ceil().max(1.),
        (area.height * scale).ceil().max(1.),
    );
    let context = canvas(width as usize, height as usize)?;
    let c = Some(&*context);
    CGContext::translate_ctm(c, 0., height);
    CGContext::scale_ctm(c, scale, -scale);
    CGContext::translate_ctm(c, -area.x, -area.y);
    draw_mark(&context, step, picture);
    let image = finished(&context)?;
    let mut bitmap = crate::thumbnails::bitmap(&image)?;
    // Core Graphics premultiplies, and the window blends straight alpha. Left
    // alone, a highlighter would show far darker than it saves.
    for pixel in bitmap.bytes.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha > 0 && alpha < 255 {
            for channel in &mut pixel[..3] {
                *channel = (u32::from(*channel) * 255 / alpha).min(255) as u8;
            }
        }
    }
    Ok((bitmap, area))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PICTURE: (f64, f64) = (400., 300.);

    fn close(a: [f64; 2], b: [f64; 2]) {
        assert!(
            (a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01,
            "{a:?} and {b:?}"
        );
    }

    fn boxed(rotation: f64) -> Step {
        Step::Rectangle {
            rect: Rect {
                x: 100.,
                y: 100.,
                width: 80.,
                height: 40.,
            },
            color: [1., 0., 0., 1.],
            width: 4.,
            rotation,
        }
    }

    /// The corner across from the one dragged stays where it shows, turned or
    /// not, as otherwise a resize drifts across the picture.
    #[test]
    fn resizing_holds_the_opposite_corner_and_puts_the_dragged_one_at_the_pointer() {
        for rotation in [0., 30., 200.] {
            let step = boxed(rotation);
            let before = step.frame(PICTURE).unwrap();
            for corner in 0..4 {
                let to = [260., 40.];
                let after = step
                    .resized(Handle::Corner(corner), to, PICTURE)
                    .frame(PICTURE)
                    .unwrap();
                // Dragged past the held corner it opens the other way, which
                // renumbers the corners, so these look for the place.
                let has = |place: [f64; 2]| {
                    (0..4).any(|c| {
                        let at = after.corner(c);
                        (at[0] - place[0]).abs() < 0.01 && (at[1] - place[1]).abs() < 0.01
                    })
                };
                assert!(
                    has(before.corner(corner + 2)),
                    "{rotation} {corner} {after:?}"
                );
                assert!(has(to), "{rotation} {corner} {after:?}");
            }
        }
    }

    /// The handles and the hit test work from `Frame`, and the ink from Core
    /// Graphics. They must turn the same way, or handles sit off the mark.
    #[test]
    fn a_turned_marks_ink_is_where_its_frame_says() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("white.png");
        image::RgbImage::from_pixel(400, 300, image::Rgb([255, 255, 255]))
            .save(&path)
            .unwrap();
        let step = boxed(30.);
        let out = dir.path().join("out.png");
        crate::edits::flatten(&path, std::slice::from_ref(&step), &out).unwrap();
        let drawn = image::open(&out).unwrap().into_rgb8();
        let frame = step.frame(PICTURE).unwrap();
        for corner in 0..4 {
            let at = frame.corner(corner);
            let pixel = drawn
                .get_pixel(at[0].round() as u32, at[1].round() as u32)
                .0;
            assert!(pixel[1] < 128, "corner {corner} at {at:?} is {pixel:?}");
        }
        assert_eq!(
            drawn.get_pixel(100, 100).0,
            [255, 255, 255],
            "the unturned corner is clear"
        );
    }

    #[test]
    fn a_stroke_scales_with_its_frame() {
        let pen = Step::Pen {
            points: vec![[10., 10.], [30., 50.]],
            color: [0., 0., 0., 1.],
            width: 2.,
            rotation: 0.,
        };
        let Step::Pen { points, .. } = pen.resized(Handle::Corner(2), [50., 90.], PICTURE) else {
            unreachable!();
        };
        close(points[0], [10., 10.]);
        close(points[1], [50., 90.]);
    }

    #[test]
    fn text_scales_by_its_size_and_keeps_its_shape() {
        let text = Step::Text {
            at: [50., 50.],
            text: "Hover".into(),
            size: 20.,
            color: [0., 0., 0., 1.],
            wrap: Some(200.),
            rotation: 0.,
        };
        let frame = text.frame(PICTURE).unwrap();
        let at = [
            frame.rect.x + frame.rect.width * 2.,
            frame.rect.y + frame.rect.height * 2.,
        ];
        let Step::Text { at, size, wrap, .. } = text.resized(Handle::Corner(2), at, PICTURE) else {
            unreachable!();
        };
        close(at, [50., 50.]);
        assert!((size - 40.).abs() < 0.01);
        assert_eq!(wrap, Some(400.));
    }

    #[test]
    fn a_press_lands_on_ink_and_a_boxs_inside_stays_free() {
        let step = boxed(0.);
        assert!(step.hit([100., 120.], PICTURE, 2.), "on the left edge");
        assert!(!step.hit([140., 120.], PICTURE, 2.), "inside it");
        let turned_box = boxed(90.);
        // Turned a quarter about (140, 120), the box is 40 wide and 80 tall.
        assert!(turned_box.hit([140., 80.], PICTURE, 2.), "the new top edge");
        assert!(
            !turned_box.hit([100., 120.], PICTURE, 2.),
            "the old left edge"
        );
        let redact = Step::Redact {
            rect: Rect {
                x: 0.,
                y: 0.,
                width: 50.,
                height: 50.,
            },
            rotation: 0.,
        };
        assert!(
            redact.hit([25., 25.], PICTURE, 0.),
            "a solid mark is all ink"
        );
    }

    /// The layer shown during a drag must sit where saving will draw the mark.
    /// See-through ink keeps its own color on the layer, for the window to blend.
    #[test]
    fn a_layer_is_in_straight_alpha() {
        let step = Step::Highlighter {
            points: vec![[20., 20.], [80., 20.]],
            color: [1., 0., 0., 0.4],
            width: 10.,
            rotation: 0.,
        };
        let (bitmap, area) = render_mark(&step, PICTURE, 1.).unwrap();
        let column = (50. - area.x) as usize;
        let row = (20. - area.y) as usize;
        let start = (row * bitmap.width as usize + column) * 4;
        let [blue, _green, red, alpha] = bitmap.bytes[start..start + 4] else {
            unreachable!();
        };
        assert!((100..=104).contains(&alpha), "{alpha}");
        assert!(red > 245, "{red}");
        assert!(blue < 10, "{blue}");
    }

    #[test]
    fn a_marks_layer_covers_where_it_lands_at_the_scale_asked() {
        let step = boxed(0.);
        let (bitmap, area) = render_mark(&step, PICTURE, 0.5).unwrap();
        assert_eq!(bitmap.width as f64, (area.width * 0.5).ceil());
        let alpha = |x: f64, y: f64| {
            let column = ((x - area.x) * 0.5) as usize;
            let row = ((y - area.y) * 0.5) as usize;
            bitmap.bytes[(row * bitmap.width as usize + column) * 4 + 3]
        };
        assert!(alpha(100., 120.) > 128, "the left edge is drawn");
        assert_eq!(alpha(140., 120.), 0, "and inside is clear");
    }
}
