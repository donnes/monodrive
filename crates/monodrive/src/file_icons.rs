//! The icons Finder shows, from `NSWorkspace`, each rasterized at the pixels
//! it covers, off the UI thread.
//!
//! Files share one icon per extension. Folders and links are drawn one by
//! one, since special folders, apps, packages and custom icons depend on the
//! path.

use gpui::{AnyElement, App, Asset, Global, RenderImage, Window, div, img, prelude::*, px};
use monodrive_core::entries::{Entry, Kind};
use objc2::AnyThread;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{
    NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace, NSGraphicsContext, NSImage,
    NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_uniform_type_identifiers::{UTType, UTTypeData};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What an icon is drawn from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Art {
    /// Every file with this lowercase extension; empty for none.
    Type(String),
    /// This folder or link.
    Path(PathBuf),
}

/// One icon at one size in device pixels.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Raster {
    art: Art,
    pixels: u32,
}

impl Asset for Raster {
    type Source = Raster;
    type Output = Option<Arc<RenderImage>>;

    // An `async fn` would hold the `&mut App` borrow; the asset cache needs
    // a future that outlives it.
    #[allow(clippy::manual_async_fn)]
    fn load(
        raster: Raster,
        _: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        async move {
            autoreleasepool(|_| {
                let icon = match &raster.art {
                    Art::Type(extension) => type_icon(extension),
                    Art::Path(path) => path_icon(path)?,
                };
                draw(&icon, raster.pixels)
            })
            .map(Arc::new)
        }
    }
}

fn type_icon(extension: &str) -> Retained<NSImage> {
    let kind = UTType::typeWithFilenameExtension(&NSString::from_str(extension));
    let kind = kind.as_deref().unwrap_or(unsafe { UTTypeData });
    NSWorkspace::sharedWorkspace().iconForContentType(kind)
}

/// Apps resolve links first: Safari in /Applications links into a system
/// cryptex, and Finder draws it without the alias arrow AppKit adds.
fn path_icon(path: &Path) -> Option<Retained<NSImage>> {
    let resolved;
    let path = if path.extension().is_some_and(|extension| extension == "app") {
        resolved = std::fs::canonicalize(path).ok()?;
        &resolved
    } else {
        path
    };
    Some(NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path.to_str()?)))
}

/// Draws `icon` `pixels` square. AppKit picks the representation that fits
/// and draws into a bitmap context of this thread's own, so it's safe off
/// the main thread.
fn draw(icon: &NSImage, pixels: u32) -> Option<RenderImage> {
    let side = isize::try_from(pixels).ok()?;
    // Premultiplied RGBA, rows padded to `bytesPerRow`.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            side,
            side,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    let rect = NSRect::new(NSPoint::ZERO, NSSize::new(side as f64, side as f64));
    icon.drawInRect_fromRect_operation_fraction(
        rect,
        NSRect::ZERO,
        NSCompositingOperation::SourceOver,
        1.,
    );
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();

    let stride = usize::try_from(bitmap.bytesPerRow()).ok()?;
    let data = bitmap.bitmapData();
    if data.is_null() {
        return None;
    }
    let rows = pixels as usize;
    // The bitmap owns `stride * rows` bytes and outlives this borrow.
    let bytes = unsafe { std::slice::from_raw_parts(data, stride * rows) };
    let mut buffer = image::RgbaImage::new(pixels, pixels);
    for (out, row) in buffer
        .chunks_exact_mut(rows * 4)
        .zip(bytes.chunks_exact(stride))
    {
        out.copy_from_slice(&row[..rows * 4]);
    }
    for pixel in buffer.pixels_mut() {
        gpui::swap_rgba_pa_to_bgra(&mut pixel.0);
    }
    Some(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// The sizes drawn lately, newest first, and the icons drawn at each. The
/// list's size and the grid's, on a Retina and a plain screen, fit in four;
/// dragging the grid size slider gives back the sizes it leaves behind.
#[derive(Default)]
struct Rasters {
    sizes: VecDeque<u32>,
    /// At each size, each icon and when it was last drawn.
    icons: HashMap<u32, HashMap<Art, u64>>,
    draws: u64,
}

impl Global for Rasters {}

const SIZES_KEPT: usize = 4;

/// Bytes of icons kept at each size, since every folder is its own icon:
/// about 21,000 list rows, or 160 of the grid's largest cells, more than a
/// screen shows.
const BYTES_KEPT: u64 = 64 * 1024 * 1024;

impl Rasters {
    /// Notes a raster as drawn and returns the ones it pushed out: the
    /// least recently drawn past this size's budget, and every icon at the
    /// sizes no longer kept.
    fn draw(&mut self, raster: Raster) -> Vec<Raster> {
        let pixels = raster.pixels;
        if self.sizes.front() != Some(&pixels) {
            self.sizes.retain(|&kept| kept != pixels);
            self.sizes.push_front(pixels);
        }
        self.draws += 1;
        let icons = self.icons.entry(pixels).or_default();
        icons.insert(raster.art, self.draws);
        let fits = (BYTES_KEPT / (u64::from(pixels).pow(2) * 4).max(1)).max(1);
        let mut gone = Vec::new();
        while icons.len() as u64 > fits {
            let Some(oldest) = icons
                .iter()
                .min_by_key(|(_, drawn)| **drawn)
                .map(|(art, _)| art.clone())
            else {
                break;
            };
            icons.remove(&oldest);
            gone.push(Raster {
                art: oldest,
                pixels,
            });
        }
        let kept = SIZES_KEPT.min(self.sizes.len());
        for pixels in self.sizes.split_off(kept) {
            let icons = self.icons.remove(&pixels).unwrap_or_default();
            gone.extend(icons.into_keys().map(|art| Raster { art, pixels }));
        }
        gone
    }
}

/// The icon at `side` points, or an empty space of that size until it's
/// rasterized; the view draws again when it lands.
fn raster(art: Art, side: f32, window: &mut Window, cx: &mut App) -> AnyElement {
    let raster = Raster {
        art,
        pixels: (side * window.scale_factor()).ceil() as u32,
    };
    let gone = cx.default_global::<Rasters>().draw(raster.clone());
    for old in gone {
        let image = cx.fetch_asset::<Raster>(&old).flatten();
        cx.remove_asset::<Raster>(&old);
        if let Some(image) = image {
            cx.drop_image(image, Some(window));
        }
    }
    // Until this size lands, a kept size stands in, so dragging the size
    // slider doesn't blank the icons.
    let image = window
        .use_asset::<Raster>(&raster, cx)
        .flatten()
        .or_else(|| {
            let rasters = cx.global::<Rasters>();
            let kept: Vec<u32> = rasters
                .sizes
                .iter()
                .copied()
                .filter(|pixels| {
                    rasters
                        .icons
                        .get(pixels)
                        .is_some_and(|icons| icons.contains_key(&raster.art))
                })
                .collect();
            kept.into_iter().find_map(|pixels| {
                let art = raster.art.clone();
                cx.fetch_asset::<Raster>(&Raster { art, pixels }).flatten()
            })
        });
    match image {
        Some(image) => img(image).size(px(side)).flex_shrink_0().into_any_element(),
        None => div().size(px(side)).flex_shrink_0().into_any_element(),
    }
}

/// The icon Finder shows for an entry.
pub fn icon(entry: &Entry, side: f32, window: &mut Window, cx: &mut App) -> AnyElement {
    let art = match entry.kind {
        Kind::Dir | Kind::Link => Art::Path(entry.path.clone()),
        Kind::File | Kind::Other => Art::Type(
            entry
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default()
                .to_lowercase(),
        ),
    };
    raster(art, side, window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(extension: &str, pixels: u32) -> Raster {
        Raster {
            art: Art::Type(extension.into()),
            pixels,
        }
    }

    #[test]
    fn sliding_the_size_gives_back_the_sizes_left_behind() {
        let mut rasters = Rasters::default();
        assert!(rasters.draw(at("rs", 32)).is_empty());
        assert!(rasters.draw(at("", 32)).is_empty());
        for pixels in [100, 102, 104] {
            assert!(rasters.draw(at("rs", pixels)).is_empty());
        }
        // Drawing the list's size again keeps it from being the oldest.
        assert!(rasters.draw(at("rs", 32)).is_empty());
        assert_eq!(rasters.draw(at("rs", 106)), vec![at("rs", 100)]);
        let mut gone = rasters.draw(at("rs", 108));
        gone.extend(rasters.draw(at("rs", 110)));
        assert_eq!(gone, vec![at("rs", 102), at("rs", 104)]);
        assert_eq!(rasters.sizes, [110, 108, 106, 32]);
        assert_eq!(rasters.icons[&32].len(), 2);
    }

    #[test]
    fn a_size_past_its_budget_gives_back_the_least_recently_drawn() {
        let mut rasters = Rasters::default();
        // Four icons of 16 MB fill the budget.
        for extension in ["a", "b", "c", "d"] {
            assert!(rasters.draw(at(extension, 2048)).is_empty());
        }
        assert!(rasters.draw(at("a", 2048)).is_empty());
        assert_eq!(rasters.draw(at("e", 2048)), vec![at("b", 2048)]);
        assert_eq!(rasters.icons[&2048].len(), 4);
    }

    #[test]
    fn finder_icons_draw_off_the_main_thread() {
        let app = path_icon(Path::new("/System/Applications/Calculator.app")).unwrap();
        for icon in [app, type_icon("pdf"), type_icon("")] {
            let image = draw(&icon, 64).unwrap();
            assert_eq!(image.size(0).width.0, 64);
            assert_eq!(image.size(0).height.0, 64);
            let middle = (32 * 64 + 32) * 4;
            assert_eq!(image.as_bytes(0).unwrap()[middle + 3], 255, "opaque");
        }
    }
}
