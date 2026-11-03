//! Where a picture sits in its box, and how ⌘-scroll, a pinch and dragging move
//! it. Zoom multiplies the fitted size, so a smaller decode and the full one
//! land on the same frame and can stand in for each other.

use block2::RcBlock;
use gpui::{Bounds, Pixels, Point, ScrollWheelEvent, Window, point, px};
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask};
use std::ptr::NonNull;

/// Far enough in to see single pixels of a big photo.
const MAX_ZOOM: f32 = 16.;
/// How far one point of ⌘-scroll zooms.
const SCROLL_ZOOM: f32 = 0.01;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Zoom {
    /// 1 fits the picture, never enlarged; 2 shows it twice that size.
    pub level: f32,
    /// How far the picture's center sits from the box's.
    pub pan: Point<Pixels>,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            level: 1.,
            pan: Point::default(),
        }
    }
}

/// The picture's top left in `bounds` and its points per pixel.
pub(super) fn place(bounds: Bounds<Pixels>, image: (f32, f32), zoom: Zoom) -> (Point<Pixels>, f32) {
    let (width, height) = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
    let fit = (width / image.0).min(height / image.1).min(1.);
    let scale = fit * zoom.level;
    let center = bounds.center();
    let origin = point(
        center.x + zoom.pan.x - px(image.0 * scale / 2.),
        center.y + zoom.pan.y - px(image.1 * scale / 2.),
    );
    (origin, scale)
}

impl Zoom {
    /// Anything but fit, including a small picture at its actual size, which
    /// is smaller than fit on a Retina screen.
    pub fn zoomed(self) -> bool {
        self.level != 1.
    }

    /// Bigger than fit, so there's somewhere to move it.
    pub fn movable(self) -> bool {
        self.level > 1.
    }

    /// `factor` times as close, keeping the pixel under `at` where it is.
    pub fn around(
        self,
        factor: f32,
        at: Point<Pixels>,
        bounds: Bounds<Pixels>,
        image: (f32, f32),
    ) -> Self {
        // Actual Size can be below fit, and zooming from there goes no lower.
        let level = (self.level * factor).clamp(self.level.min(1.), MAX_ZOOM);
        let (origin, scale) = place(bounds, image, self);
        let next = scale * level / self.level;
        let under = (
            (at.x - origin.x).as_f32() / scale,
            (at.y - origin.y).as_f32() / scale,
        );
        let center = bounds.center();
        let pan = point(
            at.x - px(under.0 * next) + px(image.0 * next / 2.) - center.x,
            at.y - px(under.1 * next) + px(image.1 * next / 2.) - center.y,
        );
        Self { level, pan }.kept_in(bounds, image)
    }

    pub fn panned(self, by: Point<Pixels>, bounds: Bounds<Pixels>, image: (f32, f32)) -> Self {
        Self {
            pan: self.pan + by,
            ..self
        }
        .kept_in(bounds, image)
    }

    /// A picture bigger than its box can't leave a gap at an edge, and one
    /// smaller stays centered.
    fn kept_in(self, bounds: Bounds<Pixels>, image: (f32, f32)) -> Self {
        let (_, scale) = place(bounds, image, self);
        let slack = |shown: f32, room: Pixels| px(((shown - room.as_f32()) / 2.).max(0.));
        let (x, y) = (
            slack(image.0 * scale, bounds.size.width),
            slack(image.1 * scale, bounds.size.height),
        );
        Self {
            level: self.level,
            pan: point(self.pan.x.clamp(-x, x), self.pan.y.clamp(-y, y)),
        }
    }

    /// ⌘-scroll zooms about the pointer; scrolling alone moves a zoomed picture.
    /// `None` leaves the scroll to whatever's under it.
    pub fn scrolled(
        self,
        event: &ScrollWheelEvent,
        bounds: Bounds<Pixels>,
        image: (f32, f32),
    ) -> Option<Self> {
        let delta = event.delta.pixel_delta(px(20.));
        if event.modifiers.platform {
            let factor = (delta.y.as_f32() * SCROLL_ZOOM).exp();
            Some(self.around(factor, event.position, bounds, image))
        } else if self.movable() {
            Some(self.panned(delta, bounds, image))
        } else {
            None
        }
    }
}

/// A trackpad pinch over the window, which GPUI doesn't report on macOS: each
/// one's magnification and where it happened. Dropping it stops listening.
pub(super) struct Pinch {
    monitor: Retained<AnyObject>,
    pub events: async_channel::Receiver<(f32, Point<Pixels>)>,
}

impl Drop for Pinch {
    fn drop(&mut self) {
        // SAFETY: this is the object `addLocalMonitor` returned.
        unsafe { NSEvent::removeMonitor(&self.monitor) };
    }
}

/// `None` without a window on the main thread, as in tests.
pub(super) fn pinch(window: &Window) -> Option<Pinch> {
    MainThreadMarker::new()?;
    let view = crate::native_view::content_view(window)?;
    let (sender, events) = async_channel::unbounded();
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event, and runs this on the main thread.
        let found = unsafe { event.as_ref() };
        if let Some(mtm) = MainThreadMarker::new()
            && found
                .window(mtm)
                .is_some_and(|from| Some(from) == view.window())
        {
            let inside = view.convertPoint_fromView(found.locationInWindow(), None);
            let height = view.bounds().size.height;
            let y = if view.isFlipped() {
                inside.y
            } else {
                height - inside.y
            };
            let at = point(px(inside.x as f32), px(y as f32));
            let _ = sender.try_send((found.magnification() as f32, at));
        }
        event.as_ptr()
    });
    // SAFETY: the block copies itself, and `Drop` removes the monitor.
    let monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::Magnify, &handler)
    }?;
    Some(Pinch { monitor, events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    fn bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(400.), px(400.)))
    }

    #[test]
    fn the_picture_is_fit_centered_and_never_enlarged() {
        let (origin, scale) = place(bounds(), (800., 400.), Zoom::default());
        assert_eq!(scale, 0.5);
        assert_eq!((origin.x, origin.y), (px(10.), px(120.)));
        let (origin, scale) = place(bounds(), (100., 100.), Zoom::default());
        assert_eq!(scale, 1.);
        assert_eq!((origin.x, origin.y), (px(160.), px(170.)));
    }

    #[test]
    fn zooming_keeps_the_pixel_under_the_pointer_and_the_picture_in_its_box() {
        let image = (800., 800.);
        let at = point(px(110.), px(120.));
        let pixel = |zoom: Zoom| {
            let (origin, scale) = place(bounds(), image, zoom);
            (
                (at.x - origin.x).as_f32() / scale,
                (at.y - origin.y).as_f32() / scale,
            )
        };
        let before = pixel(Zoom::default());
        let zoomed = Zoom::default().around(2., at, bounds(), image);
        assert_eq!(zoomed.level, 2.);
        assert_eq!(pixel(zoomed), before);
        // Twice the box's size leaves half of it to either side to move into.
        let far = zoomed.panned(point(px(1000.), px(-1000.)), bounds(), image);
        assert_eq!(far.pan, point(px(200.), px(-200.)));
        // Zooming back out to fit centers it again, and no further.
        let out = far.around(0.1, at, bounds(), image);
        assert_eq!(out, Zoom::default());
    }
}
