//! Files on a pasteboard as Finder puts them there: one file URL an item,
//! so Finder, Mail and Terminal paste what Monodrive copies, and the other
//! way round. GPUI's clipboard reads paths but can't write them.

use gpui::{Global, Image, ImageFormat};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSPasteboard,
    NSPasteboardTypeFileURL, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, NSURL};
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

    /// The files on the pasteboard, whichever app put them there.
    pub fn files(&self) -> Vec<PathBuf> {
        files_on(&self.0)
    }
}

/// The files on `pasteboard`, as Copy or a service request carries them.
/// Finder's file reference URLs resolve to paths.
pub fn files_on(pasteboard: &NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pasteboard.pasteboardItems() else {
        return Vec::new();
    };
    // SAFETY: AppKit's constant, read once AppKit is loaded.
    let file_url = unsafe { NSPasteboardTypeFileURL };
    items
        .iter()
        .filter_map(|item| path_of(&*item.stringForType(file_url)?))
        .collect()
}

/// The path a `file://` URL's text names, resolving a file reference URL.
pub fn path_of(url: &NSString) -> Option<PathBuf> {
    let url = NSURL::URLWithString(url)?;
    Some(PathBuf::from(url.filePathURL()?.path()?.to_string()))
}

/// A copied picture as a phone's gallery takes it, with its extension:
/// TIFF and BMP, which macOS's apps copy, become PNG.
pub fn shareable(image: Image) -> (&'static str, Vec<u8>) {
    match image.format {
        ImageFormat::Tiff | ImageFormat::Bmp => match png(&image.bytes) {
            Some(png) => ("png", png),
            None => (image.format.extension(), image.bytes),
        },
        format => (format.extension(), image.bytes),
    }
}

fn png(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(bytes))?;
    let options = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
    // SAFETY: the properties are the dictionary type AppKit declares.
    let png =
        unsafe { image.representationUsingType_properties(NSBitmapImageFileType::PNG, &options) }?;
    Some(png.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copied_tiff_goes_as_png() {
        let tiff = vec![
            73, 73, 42, 0, 8, 0, 0, 0, 9, 0, 0, 1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 1, 3, 0, 1, 0,
            0, 0, 1, 0, 0, 0, 2, 1, 3, 0, 1, 0, 0, 0, 8, 0, 0, 0, 3, 1, 3, 0, 1, 0, 0, 0, 1, 0, 0,
            0, 6, 1, 3, 0, 1, 0, 0, 0, 2, 0, 0, 0, 17, 1, 4, 0, 1, 0, 0, 0, 122, 0, 0, 0, 21, 1, 3,
            0, 1, 0, 0, 0, 3, 0, 0, 0, 22, 1, 3, 0, 1, 0, 0, 0, 1, 0, 0, 0, 23, 1, 4, 0, 1, 0, 0,
            0, 3, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0,
        ];
        let image = |format, bytes| Image {
            format,
            bytes,
            id: 0,
        };

        let (extension, bytes) = shareable(image(ImageFormat::Tiff, tiff));
        assert_eq!(extension, "png");
        assert!(bytes.starts_with(b"\x89PNG"));
        assert_eq!(
            shareable(image(ImageFormat::Jpeg, b"jpeg".to_vec())),
            ("jpg", b"jpeg".to_vec())
        );
    }
}
