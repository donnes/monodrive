//! Files on a pasteboard as Finder puts them there: one file URL an item,
//! so Finder, Mail and Terminal paste what Monodrive copies, and the other
//! way round. GPUI's clipboard reads paths but can't write them.

use gpui::Global;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSString, NSURL};
use std::path::PathBuf;

/// The pasteboard Copy and Paste use. `run` sets the general one; without
/// it, as in benchmarks, they do nothing.
pub struct Pasteboard(pub Retained<NSPasteboard>);
impl Global for Pasteboard {}

impl Pasteboard {
    pub fn general() -> Self {
        Self(NSPasteboard::generalPasteboard())
    }

    /// Replaces what's on the pasteboard with these files.
    pub fn write_files(&self, paths: &[PathBuf]) {
        let urls: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = paths
            .iter()
            .map(|path| {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                ProtocolObject::from_retained(url)
            })
            .collect();
        self.0.clearContents();
        self.0.writeObjects(&NSArray::from_retained_slice(&urls));
    }

    /// The files on the pasteboard, whichever app put them there. Finder's
    /// file reference URLs resolve to paths.
    pub fn files(&self) -> Vec<PathBuf> {
        let Some(items) = self.0.pasteboardItems() else {
            return Vec::new();
        };
        // SAFETY: AppKit's constant, read once AppKit is loaded.
        let file_url = unsafe { NSPasteboardTypeFileURL };
        items
            .iter()
            .filter_map(|item| {
                let url = NSURL::URLWithString(&*item.stringForType(file_url)?)?;
                Some(PathBuf::from(url.filePathURL()?.path()?.to_string()))
            })
            .collect()
    }
}
