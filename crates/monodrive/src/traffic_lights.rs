//! Monocode's traffic lights: 14 pt buttons, 6 pt apart, 12 pt from the left
//! edge. GPUI's `traffic_light_position` only moves the row and keeps
//! AppKit's spacing, so Auto Layout pins each button instead; GPUI's frame
//! updates on resize, focus and fullscreen then can't move them back.

use gpui::Window;
use objc2_app_kit::{NSLayoutAttribute, NSView, NSWindow, NSWindowButton};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

const BUTTON_SIZE: f64 = 14.;
const LEFT_MARGIN: f64 = 12.;
const BUTTON_SPACING: f64 = 6.;

/// Pins the buttons, or moves already pinned ones. AppKit can recreate them,
/// so call this again whenever the window may have rebuilt its titlebar.
pub fn pin(window: &Window, title_bar_height: f32) {
    let Some(ns_window) = ns_window(window) else {
        return;
    };
    let top = (f64::from(title_bar_height) - BUTTON_SIZE) / 2.;
    let kinds = [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        let Some(button) = ns_window.standardWindowButton(kind) else {
            continue;
        };
        let Some(titlebar) = (unsafe { button.superview() }) else {
            continue;
        };
        let x = LEFT_MARGIN + i as f64 * (BUTTON_SIZE + BUTTON_SPACING);
        // Already pinned: move the constraints, since a second set would conflict.
        if !button.translatesAutoresizingMaskIntoConstraints() {
            let target = objc2::rc::Retained::as_ptr(&button).cast::<objc2::runtime::AnyObject>();
            for constraint in titlebar.constraints().iter() {
                let first = unsafe { constraint.firstItem() };
                if !first.is_some_and(|item| std::ptr::eq(&*item, target)) {
                    continue;
                }
                match constraint.firstAttribute() {
                    NSLayoutAttribute::Leading => constraint.setConstant(x),
                    NSLayoutAttribute::Top => constraint.setConstant(top),
                    _ => {}
                }
            }
            continue;
        }
        button.setTranslatesAutoresizingMaskIntoConstraints(false);
        for constraint in [
            button.widthAnchor().constraintEqualToConstant(BUTTON_SIZE),
            button.heightAnchor().constraintEqualToConstant(BUTTON_SIZE),
            button
                .leadingAnchor()
                .constraintEqualToAnchor_constant(&titlebar.leadingAnchor(), x),
            button
                .topAnchor()
                .constraintEqualToAnchor_constant(&titlebar.topAnchor(), top),
        ] {
            constraint.setActive(true);
        }
    }
}

pub fn ns_window(window: &Window) -> Option<objc2::rc::Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI's AppKit handle points at its live content view.
    let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    view.window()
}
