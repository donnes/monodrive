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

/// Whether an agent runs this dev build beside the user's work, as
/// `scripts/agent-app` starts it. The app then never activates and keeps its
/// windows out of the way. A release build ignores this.
pub fn unattended() -> bool {
    monodrive_core::DEV && std::env::var_os("MONODRIVE_UNATTENDED").is_some()
}

/// Activates the app, unless an agent is running it.
pub fn bring_forward(cx: &App) {
    if !unattended() {
        cx.activate(true);
    }
}

/// The binary that starts the operation owner. An agent's run names one outside
/// the bundle, or the owner shares the app's name and agent-desktop can't tell
/// the two apart. A release build ignores this.
pub fn owner_exe() -> Option<std::path::PathBuf> {
    monodrive_core::DEV
        .then(|| std::env::var_os("MONODRIVE_OWNER_EXE"))
        .flatten()
        .map(Into::into)
        .or_else(|| std::env::current_exe().ok())
}

/// `bounds` moved past the bottom right of the `screens`, with a sliver left on
/// the last one: GPUI stops drawing a window that's entirely out of sight, and
/// then an agent would read a stale accessibility tree.
pub fn out_of_the_way(bounds: Bounds<Pixels>, screens: &[Bounds<Pixels>]) -> Bounds<Pixels> {
    let Some(last) = screens.iter().max_by_key(|screen| screen.right()) else {
        return bounds;
    };
    Bounds::new(
        last.bottom_right() - point(px(SLIVER), px(SLIVER)),
        bounds.size,
    )
}

const SLIVER: f32 = 2.;

/// How far each open window pushes a new one, so it doesn't land exactly on top.
const CASCADE: f32 = 24.;

/// `bounds` moved along for a window that opens with `open` others showing.
pub fn cascaded(bounds: Bounds<Pixels>, open: usize) -> Bounds<Pixels> {
    let step = px(CASCADE * open as f32);
    Bounds::new(bounds.origin + point(step, step), bounds.size)
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
    fn an_unattended_window_stays_off_every_screen_but_a_sliver() {
        let left = Bounds::new(point(px(-1800.), px(271.)), size(px(1800.), px(1169.)));
        let main = Bounds::new(point(px(0.), px(0.)), size(px(2560.), px(1440.)));
        let window = Bounds::new(point(px(100.), px(100.)), size(px(1025.), px(600.)));
        let moved = out_of_the_way(window, &[left, main]);
        assert_eq!(moved.origin, point(px(2558.), px(1438.)));
        assert_eq!(moved.size, window.size);
        assert_eq!(out_of_the_way(window, &[main, left]), moved);
    }

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
