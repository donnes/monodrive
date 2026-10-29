//! Window sizes: where a new window opens, and the resize when Default size
//! changes in Settings.

use crate::preferences::{Frame, WindowSize};
use crate::traffic_lights;
use gpui::{App, Bounds, Pixels, Size, Window, point, px};

pub const MIN_WIDTH: f32 = 720.;
pub const MIN_HEIGHT: f32 = 440.;

/// Where a new window opens on `screen`, the part clear of the menu bar and
/// the Dock: a share of it, centered, or the
/// last frame if it's still on one of the `screens`.
pub fn opening(
    size: WindowSize,
    last: Option<Frame>,
    screen: Bounds<Pixels>,
    screens: &[Bounds<Pixels>],
) -> Bounds<Pixels> {
    if let Some((part, most)) = part(size) {
        return share(screen, part, most);
    }
    let (part, most) = REASONABLE;
    last.map(|f| {
        Bounds::new(
            point(px(f.x), px(f.y)),
            gpui::size(px(f.width), px(f.height)),
        )
    })
    .filter(|frame| screens.iter().any(|screen| frame.intersects(screen)))
    .unwrap_or_else(|| share(screen, part, most))
}

/// Tinycast's Reasonable Size: 60% of the screen, at most 1025 × 900.
const REASONABLE: (f32, Size<Pixels>) = (0.6, gpui::size(px(1025.), px(900.)));

/// The share of the screen `size` takes and the most it grows to; Last Used
/// has none.
fn part(size: WindowSize) -> Option<(f32, Size<Pixels>)> {
    match size {
        WindowSize::Reasonable => Some(REASONABLE),
        WindowSize::AlmostFull => Some((0.9, gpui::size(Pixels::MAX, Pixels::MAX))),
        WindowSize::LastUsed => None,
    }
}

/// `part` of `screen` on each side up to `most`, centered, never smaller
/// than the smallest usable window or bigger than the screen.
fn share(screen: Bounds<Pixels>, part: f32, most: Size<Pixels>) -> Bounds<Pixels> {
    let fit =
        |side: Pixels, most: Pixels, least: f32| (side * part).min(most).max(px(least)).min(side);
    let wanted = gpui::size(
        fit(screen.size.width, most.width, MIN_WIDTH),
        fit(screen.size.height, most.height, MIN_HEIGHT),
    );
    Bounds::centered_at(screen.center(), wanted)
}

/// Resizes `window` to `size` on the screen it's on. Last Used and a full
/// screen window stay as they are.
pub fn apply(size: WindowSize, window: &Window, cx: &App) {
    let Some((part, most)) = part(size) else {
        return;
    };
    if window.is_fullscreen() {
        return;
    }
    let Some(ns_window) = traffic_lights::ns_window(window) else {
        return;
    };
    let Some(screen) = ns_window.screen() else {
        return;
    };
    // AppKit's y axis points up, but a centered frame is centered either way.
    let mut rect = screen.visibleFrame();
    let fit = share(
        Bounds::new(
            point(px(rect.origin.x as f32), px(rect.origin.y as f32)),
            gpui::size(px(rect.size.width as f32), px(rect.size.height as f32)),
        ),
        part,
        most,
    );
    rect.origin.x = fit.origin.x.as_f32().into();
    rect.origin.y = fit.origin.y.as_f32().into();
    rect.size.width = fit.size.width.as_f32().into();
    rect.size.height = fit.size.height.as_f32().into();
    // AppKit tells GPUI about the resize at once, which GPUI drops while
    // it's busy with this update, so the resize waits until it's done.
    cx.foreground_executor()
        .spawn(async move { ns_window.setFrame_display_animate(rect, true, false) })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn windows_open_at_a_share_of_the_screen_or_where_they_were() {
        let screen = Bounds::new(point(px(0.), px(0.)), size(px(2000.), px(1000.)));
        let reasonable = opening(WindowSize::Reasonable, None, screen, &[screen]);
        // Reasonable stops growing at 1025 × 900, as Tinycast's does.
        assert_eq!(reasonable.size, size(px(1025.), px(600.)));
        assert_eq!(reasonable.center(), screen.center());
        let big = Bounds::new(point(px(0.), px(0.)), size(px(3000.), px(2000.)));
        assert_eq!(
            opening(WindowSize::Reasonable, None, big, &[big]).size,
            size(px(1025.), px(900.))
        );
        let full = opening(WindowSize::AlmostFull, None, screen, &[screen]);
        assert_eq!(full.size, size(px(1800.), px(900.)));
        // A small screen still gets the smallest usable window, never more
        // than the screen.
        let small = Bounds::new(point(px(0.), px(0.)), size(px(800.), px(500.)));
        assert_eq!(
            opening(WindowSize::Reasonable, None, small, &[small]).size,
            size(px(720.), px(440.))
        );
        let last = Frame {
            x: 100.,
            y: 50.,
            width: 900.,
            height: 700.,
        };
        assert_eq!(
            opening(WindowSize::LastUsed, Some(last), screen, &[screen]).origin,
            point(px(100.), px(50.))
        );
        // A frame on a second display opens there.
        let second = Bounds::new(point(px(2000.), px(0.)), size(px(1500.), px(900.)));
        let there = Frame { x: 2200., ..last };
        assert_eq!(
            opening(WindowSize::LastUsed, Some(there), screen, &[screen, second]).origin,
            point(px(2200.), px(50.))
        );
        // A frame on a screen that's gone falls back to Reasonable.
        let gone = Frame { x: 5000., ..last };
        assert_eq!(
            opening(WindowSize::LastUsed, Some(gone), screen, &[screen]),
            reasonable
        );
    }
}
