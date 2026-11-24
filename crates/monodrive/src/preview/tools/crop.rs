//! Cropping: freezing the picture to crop, and applying or cancelling it.

use super::*;

impl PreviewView {
    /// Crop: a frame over the whole picture, or the video's crop so far, to
    /// drag in from its edges.
    pub(super) fn start_crop(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        // Until the last step is drawn, the picture's size may still change.
        if matches!(
            edit.subject,
            Subject::Picture {
                flattening: Some(_),
                ..
            }
        ) {
            return;
        }
        let Some((width, height)) = edit.image().map(|image| image_size(image)) else {
            return;
        };
        let rect = match &edit.subject {
            Subject::Video {
                crop: Some(crop), ..
            } => *crop,
            _ => Rect {
                x: 0.,
                y: 0.,
                width: f64::from(width),
                height: f64::from(height),
            },
        };
        edit.text = None;
        edit.selected = None;
        edit.tool = Some(Tool::Crop);
        edit.cropping = Some(Cropping { rect, grab: None });
        cx.notify();
    }

    /// Whether the crop's frame is up, for Return to apply it.
    pub fn cropping(&self) -> bool {
        self.edit
            .as_ref()
            .is_some_and(|edit| edit.cropping.is_some())
    }

    /// Return or the Crop button: what's inside the frame becomes the picture.
    pub fn apply_crop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &mut self.edit else {
            return;
        };
        let Some(cropping) = edit.cropping.take() else {
            return;
        };
        edit.tool = None;
        let rect = cropping.rect;
        let whole = edit.image().is_some_and(|image| {
            let (width, height) = image_size(image);
            rect.x < 0.5
                && rect.y < 0.5
                && rect.width > f64::from(width) - 1.
                && rect.height > f64::from(height) - 1.
        });
        // Widened back to the whole frame, a video's crop goes.
        if let Subject::Video { crop, .. } = &edit.subject {
            let next = (!whole).then_some(rect);
            if *crop != next {
                edit.remember();
                if let Subject::Video { crop, .. } = &mut edit.subject {
                    *crop = next;
                }
            }
            return cx.notify();
        }
        if whole {
            return cx.notify();
        }
        self.add_step(Step::Crop { rect }, window, cx);
    }

    pub(super) fn cancel_crop(&mut self, cx: &mut Context<Self>) {
        if let Some(edit) = &mut self.edit {
            edit.cropping = None;
            edit.tool = None;
        }
        cx.notify();
    }

    /// Crop on a video: pauses, and puts the current frame where handles can
    /// sit over it, as nothing GPUI draws can sit over the player.
    pub(super) fn freeze_for_crop(&mut self, cx: &mut Context<Self>) {
        let seconds = self.player.borrow().as_ref().map_or(0., |player| {
            player.pause();
            player.position()
        });
        let turns = match self.edit.as_ref().map(|edit| &edit.subject) {
            Some(Subject::Video { turns, .. }) => *turns,
            _ => 0,
        };
        let path = self.path.clone();
        let work = cx.background_executor().spawn(async move {
            let bitmap = edits::frame_bitmap(&path, seconds)?;
            let pixels = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bytes)
                .ok_or("couldn't draw the frame".to_string())?;
            let upright = (f64::from(pixels.width()), f64::from(pixels.height()));
            let pixels = match turns {
                1 => image::imageops::rotate90(&pixels),
                2 => image::imageops::rotate180(&pixels),
                3 => image::imageops::rotate270(&pixels),
                _ => pixels,
            };
            let frame = Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]));
            Ok::<_, String>((frame, upright))
        });
        cx.spawn(async move |this, cx| {
            let done = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(edit) = &mut this.edit else {
                    return;
                };
                let Subject::Video {
                    frozen, upright, ..
                } = &mut edit.subject
                else {
                    return;
                };
                match done {
                    Ok((frame, size)) => {
                        *upright = Some(size);
                        if let Some(old) = frozen.replace(frame) {
                            cx.drop_image(old, None);
                        }
                        this.start_crop(cx);
                    }
                    Err(why) => edit.error = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// What a press at `at` takes hold of on a crop drawn at `frame`: an edge or a
/// corner within `GRIP` of it, the middle, or else a new frame.
pub(super) fn grab_at(frame: Bounds<Pixels>, at: Point<Pixels>) -> CropGrab {
    let grip = px(GRIP);
    let near = |a: Pixels, b: Pixels| (a - b).abs() <= grip;
    let across = at.x >= frame.left() - grip && at.x <= frame.right() + grip;
    let down = at.y >= frame.top() - grip && at.y <= frame.bottom() + grip;
    let (mut left, mut right) = (
        near(at.x, frame.left()) && down,
        near(at.x, frame.right()) && down,
    );
    let (mut top, mut bottom) = (
        near(at.y, frame.top()) && across,
        near(at.y, frame.bottom()) && across,
    );
    // A frame narrower than two grips takes the nearer edge.
    if left && right {
        let to_left = (at.x - frame.left()).abs() <= (at.x - frame.right()).abs();
        (left, right) = (to_left, !to_left);
    }
    if top && bottom {
        let to_top = (at.y - frame.top()).abs() <= (at.y - frame.bottom()).abs();
        (top, bottom) = (to_top, !to_top);
    }
    if left || right || top || bottom {
        CropGrab::Edges {
            left,
            right,
            top,
            bottom,
        }
    } else if frame.contains(&at) {
        CropGrab::Move
    } else {
        CropGrab::New
    }
}

/// The crop after dragging what `grab` holds from `from` to `to`, in picture
/// pixels. It never leaves the picture or gets smaller than `SMALLEST_CROP`.
pub(super) fn reshaped(
    start: Rect,
    grab: CropGrab,
    from: [f64; 2],
    to: [f64; 2],
    image: (f32, f32),
) -> Rect {
    let (width, height) = (f64::from(image.0), f64::from(image.1));
    let least = (SMALLEST_CROP.min(width), SMALLEST_CROP.min(height));
    match grab {
        CropGrab::Move => Rect {
            x: (start.x + to[0] - from[0]).clamp(0., width - start.width),
            y: (start.y + to[1] - from[1]).clamp(0., height - start.height),
            ..start
        },
        // Never thinner than the smallest crop, so its edges can still drag.
        CropGrab::New => {
            let rect = corners(from, to, image);
            let (rect_width, rect_height) = (rect.width.max(least.0), rect.height.max(least.1));
            Rect {
                x: rect.x.min(width - rect_width),
                y: rect.y.min(height - rect_height),
                width: rect_width,
                height: rect_height,
            }
        }
        CropGrab::Edges {
            left,
            right,
            top,
            bottom,
        } => {
            // By how far the pointer moved, so a press beside an edge doesn't
            // make it jump.
            let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
            let (mut x0, mut x1) = (start.x, start.x + start.width);
            let (mut y0, mut y1) = (start.y, start.y + start.height);
            if left {
                x0 = (x0 + dx).clamp(0., x1 - least.0);
            }
            if right {
                x1 = (x1 + dx).clamp(x0 + least.0, width);
            }
            if top {
                y0 = (y0 + dy).clamp(0., y1 - least.1);
            }
            if bottom {
                y1 = (y1 + dy).clamp(y0 + least.1, height);
            }
            Rect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            }
        }
    }
}
