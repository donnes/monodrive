//! Sharing files through public AppKit: macOS's share menu, AirDrop's own
//! window, and who this Mac sends as and who can find it. Listing the people
//! nearby and changing who can find this Mac take Apple's private AirDrop
//! entitlements, which macOS grants only to apps Apple signs.

use gpui::{Global, Pixels, Point, Window};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSSharingService, NSSharingServiceNameSendViaAirDrop, NSSharingServicePicker, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSFullUserName, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL, NSUserDefaults,
};
use std::cell::RefCell;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Discoverable {
    NoOne,
    ContactsOnly,
    Everyone,
}

impl Discoverable {
    pub fn label(self) -> &'static str {
        match self {
            Self::NoOne => "No One",
            Self::ContactsOnly => "Contacts Only",
            Self::Everyone => "Everyone",
        }
    }
}

/// What the AirDrop page shows. A global, so a headless snapshot can set it;
/// `live` false keeps the page from reading the system over it.
#[derive(Clone, Debug)]
pub struct AirDrop {
    pub me: String,
    pub discoverable: Option<Discoverable>,
    pub live: bool,
}

impl Global for AirDrop {}

impl AirDrop {
    pub fn read() -> Self {
        Self {
            me: NSFullUserName().to_string(),
            discoverable: discoverable(),
            live: true,
        }
    }
}

/// Who can find this Mac, as Control Center last set it.
fn discoverable() -> Option<Discoverable> {
    let suite = NSString::from_str("com.apple.sharingd");
    let defaults = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&suite))?;
    let mode = defaults.stringForKey(&NSString::from_str("DiscoverableMode"))?;
    match mode.to_string().as_str() {
        "Off" => Some(Discoverable::NoOne),
        "Contacts Only" => Some(Discoverable::ContactsOnly),
        "Everyone" => Some(Discoverable::Everyone),
        _ => None,
    }
}

/// System Settings' AirDrop & Handoff, where who can find this Mac is set.
pub fn open_settings() {
    let url = NSString::from_str(
        "x-apple.systempreferences:com.apple.AirDrop-Handoff-Settings.extension",
    );
    if let Some(url) = NSURL::URLWithString(&url) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

/// `paths` as file URLs, which every sharing service takes.
fn items(paths: &[PathBuf]) -> Retained<NSArray<AnyObject>> {
    let items: Vec<Retained<AnyObject>> = paths
        .iter()
        .map(|path| {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            url.into_super().into_super()
        })
        .collect();
    NSArray::from_retained_slice(&items)
}

thread_local! {
    /// The share menu on screen, kept until the next one replaces it.
    static PICKER: RefCell<Option<Retained<NSSharingServicePicker>>> = RefCell::default();
}

/// Share…: macOS's share menu on `paths`, AirDrop, Messages, Mail and the
/// rest, under `at` in `window`.
pub fn share(paths: &[PathBuf], window: &Window, at: Point<Pixels>) {
    let Some(view) = crate::traffic_lights::ns_view(window) else {
        return;
    };
    let flipped = view.isFlipped();
    let height = view.bounds().size.height;
    // GPUI lays out at the interface zoom; AppKit counts unzoomed points.
    let zoom = window.content_zoom();
    let (x, y) = (
        f64::from(f32::from(at.x) * zoom),
        f64::from(f32::from(at.y) * zoom),
    );
    let rect = NSRect::new(
        NSPoint::new(x, if flipped { y } else { height - y }),
        NSSize::new(1., 1.),
    );
    // Below the point, whichever way the view counts.
    let below = if flipped {
        NSRectEdge::MaxY
    } else {
        NSRectEdge::MinY
    };
    // SAFETY: file URLs conform to NSPasteboardWriting, as sharing requires.
    let picker = unsafe {
        NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items(paths))
    };
    picker.showRelativeToRect_ofView_preferredEdge(rect, &view, below);
    PICKER.set(Some(picker));
}

/// Sends `paths` with AirDrop, whose own window asks who to send them to.
pub fn send(paths: &[PathBuf]) -> Result<(), String> {
    // SAFETY: a constant of AppKit.
    let name = unsafe { NSSharingServiceNameSendViaAirDrop };
    let service = NSSharingService::sharingServiceNamed(name)
        .ok_or_else(|| "AirDrop isn't available on this Mac.".to_string())?;
    let items = items(paths);
    // SAFETY: file URLs conform to NSPasteboardWriting, as sharing requires.
    unsafe {
        if !service.canPerformWithItems(Some(&items)) {
            return Err("AirDrop can't send these items.".into());
        }
        service.performWithItems(&items);
    }
    Ok(())
}
