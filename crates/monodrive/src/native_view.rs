//! A native view above GPUI's Metal layer, in a clip that follows an element's
//! bounds as it paints. Nothing GPUI draws can sit over it, so frames keep their
//! controls beside it. A press inside can make it first responder, which would
//! take Space and the arrows from GPUI, so a key press hands them back; ⌘ keys
//! stay, so ⌘C still copies what's selected inside.

use block2::RcBlock;
use gpui::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSView, NSWindowOrderingMode};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::Cell;
use std::ptr::NonNull;

define_class!(
    // SAFETY: NSView has no subclassing requirements, and `Clip` doesn't
    // implement `Drop`.
    #[unsafe(super(NSView))]
    #[name = "MonodriveNativeViewClip"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Cell<bool>]
    struct Clip;

    unsafe impl NSObjectProtocol for Clip {}

    impl Clip {
        #[unsafe(method(isFlipped))]
        fn flipped(&self) -> bool {
            true
        }

        // A view that takes no presses lets them through to GPUI.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            if self.ivars().get() {
                unsafe { msg_send![super(self), hitTest: point] }
            } else {
                None
            }
        }
    }
);

/// A native view in its clip, removed when this drops.
pub struct Host<V: Message + AsRef<NSView>> {
    clip: Retained<Clip>,
    pub view: Retained<V>,
    monitor: Option<Retained<AnyObject>>,
}

impl<V: Message + AsRef<NSView>> Host<V> {
    /// `None` without an AppKit window or off the main thread, as in tests.
    pub fn new(view: Retained<V>, interactive: bool, window: &Window) -> Option<Self> {
        let parent = content_view(window)?;
        let mtm = MainThreadMarker::new()?;
        let zero = NSRect::new(NSPoint::new(0., 0.), NSSize::new(0., 0.));
        let clip = Clip::alloc(mtm).set_ivars(Cell::new(interactive));
        // SAFETY: NSView's designated initializer.
        let clip: Retained<Clip> = unsafe { msg_send![super(clip), initWithFrame: zero] };
        clip.setWantsLayer(true);
        clip.setClipsToBounds(true);
        clip.addSubview(view.as_ref());
        parent.addSubview_positioned_relativeTo(&clip, NSWindowOrderingMode::Above, None);
        let monitor = interactive.then(|| keys_back_to(&parent)).flatten();
        Some(Self {
            clip,
            view,
            monitor,
        })
    }

    /// Out of the way of GPUI's sheets and menus, which can't draw over it.
    pub fn hide(&self) {
        self.clip.setHidden(true);
    }

    /// Follows `bounds`, from the element's paint.
    pub fn place(&self, bounds: Bounds<Pixels>, window: &Window) {
        let Some(parent) = content_view(window) else {
            return;
        };
        // AppKit frames are in window points; GPUI's are divided by its zoom.
        let zoom = window.content_zoom();
        let clipped = bounds
            .intersect(&window.content_mask().bounds)
            .map(|v| v * zoom);
        if clipped.size.width <= gpui::px(0.) || clipped.size.height <= gpui::px(0.) {
            self.clip.setHidden(true);
            return;
        }
        let bounds = bounds.map(|v| v * zoom);
        let height = f64::from(clipped.size.height);
        let y = f64::from(clipped.origin.y);
        let y = if parent.isFlipped() {
            y
        } else {
            parent.bounds().size.height - y - height
        };
        self.clip.setFrame(NSRect::new(
            NSPoint::new(f64::from(clipped.origin.x), y),
            NSSize::new(f64::from(clipped.size.width), height),
        ));
        self.view.as_ref().setFrame(NSRect::new(
            NSPoint::new(
                f64::from(bounds.left() - clipped.left()),
                f64::from(bounds.top() - clipped.top()),
            ),
            NSSize::new(f64::from(bounds.size.width), f64::from(bounds.size.height)),
        ));
        self.clip.setHidden(false);
    }
}

impl<V: Message + AsRef<NSView>> Drop for Host<V> {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.take() {
            // SAFETY: this is the object `addLocalMonitor` returned.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        self.clip.removeFromSuperview();
    }
}

fn content_view(window: &Window) -> Option<Retained<NSView>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI's AppKit handle points at its live content view.
    let view = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    Some(view.retain())
}

/// Before each key press in `gpui_view`'s window, gives it the keys back.
fn keys_back_to(gpui_view: &Retained<NSView>) -> Option<Retained<AnyObject>> {
    let gpui_view = gpui_view.clone();
    let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event, and runs this on the main thread.
        let held = unsafe { event.as_ref() }.modifierFlags();
        if !held.contains(NSEventModifierFlags::Command)
            && let Some(mtm) = MainThreadMarker::new()
            && let Some(window) = gpui_view.window()
            && unsafe { event.as_ref() }
                .window(mtm)
                .is_some_and(|from| from == window)
        {
            window.makeFirstResponder(Some(&gpui_view));
        }
        event.as_ptr()
    });
    // SAFETY: the block copies itself, and `Drop` removes the monitor.
    unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) }
}
