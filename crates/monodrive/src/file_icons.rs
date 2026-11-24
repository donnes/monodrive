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
    NSImageInterpolation, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_uniform_type_identifiers::{UTType, UTTypeData, UTTypeFolder};
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
    /// Any folder.
    Folder,
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
                    Art::Folder => {
                        NSWorkspace::sharedWorkspace().iconForContentType(unsafe { UTTypeFolder })
                    }
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
pub(crate) fn draw(icon: &NSImage, pixels: u32) -> Option<RenderImage> {
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
    context.setImageInterpolation(NSImageInterpolation::High);
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
/// list's size, the grid's and the inspector's, on a Retina and a plain
/// screen, fit in six;
/// dragging the grid size slider gives back the sizes it leaves behind.
#[derive(Default)]
struct Rasters {
    sizes: VecDeque<u32>,
    /// At each size, each icon and when it was last drawn.
    icons: HashMap<u32, HashMap<Art, u64>>,
    draws: u64,
}

impl Global for Rasters {}

const SIZES_KEPT: usize = 6;

/// Bytes of icons kept at each size, since every folder is its own icon:
/// about 21,000 list rows, or 160 of the grid's largest cells, more than a
/// screen shows.
const BYTES_KEPT: u64 = 64 * 1024 * 1024;

impl Rasters {
    /// The sizes `art` is kept at, most recent first.
    fn sizes_of(&self, art: &Art) -> Vec<u32> {
        self.sizes
            .iter()
            .copied()
            .filter(|pixels| {
                self.icons
                    .get(pixels)
                    .is_some_and(|icons| icons.contains_key(art))
            })
            .collect()
    }

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
fn raster(
    art: Art,
    generic: Option<Art>,
    side: f32,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match shown(art, generic, side, window, cx) {
        Some(image) => img(image).size(px(side)).flex_shrink_0().into_any_element(),
        None => div().size(px(side)).flex_shrink_0().into_any_element(),
    }
}

/// What draws for `art` now: it, a kept size of it, or `generic` while it's
/// on its way, so a folder never seen before doesn't flash blank.
fn shown(
    art: Art,
    generic: Option<Art>,
    side: f32,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let raster = keep(art, side, window, cx);
    window
        .use_asset::<Raster>(&raster, cx)
        .flatten()
        .or_else(|| stand_in(&raster, cx))
        .or_else(|| {
            let generic = keep(generic?, side, window, cx);
            window.use_asset::<Raster>(&generic, cx).flatten()
        })
}

/// Notes the icon at `side` points as drawn, letting go of what that pushes
/// out, and returns it.
fn keep(art: Art, side: f32, window: &mut Window, cx: &mut App) -> Raster {
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
    raster
}

/// Until a size lands, a kept size of the same icon stands in, so dragging
/// the size slider doesn't blank the icons.
fn stand_in(raster: &Raster, cx: &mut App) -> Option<Arc<RenderImage>> {
    let kept = cx.global::<Rasters>().sizes_of(&raster.art);
    kept.into_iter().find_map(|pixels| {
        let art = raster.art.clone();
        cx.fetch_asset::<Raster>(&Raster { art, pixels }).flatten()
    })
}

/// The icon `icon` draws, as it is now, for what can't wait for it, like a
/// drag's image: at `side` points, a kept size until that lands, or none.
/// Asking starts it rasterizing, so ask early.
pub fn image(
    entry: &Entry,
    side: f32,
    window: &mut Window,
    cx: &mut App,
) -> Option<Arc<RenderImage>> {
    let raster = keep(entry_art(entry), side, window, cx);
    cx.fetch_asset::<Raster>(&raster)
        .flatten()
        .or_else(|| stand_in(&raster, cx))
}

/// The icon Finder shows for an entry.
pub fn icon(entry: &Entry, side: f32, window: &mut Window, cx: &mut App) -> AnyElement {
    raster(entry_art(entry), generic_art(entry), side, window, cx)
}

/// What a folder or package looks like before its own icon is drawn: most
/// folders look like any folder, and an app like any app.
fn generic_art(entry: &Entry) -> Option<Art> {
    (entry.kind == Kind::Dir).then(|| {
        if crate::workspace::is_package(&entry.path) {
            by_extension(&entry.path)
        } else {
            Art::Folder
        }
    })
}

fn entry_art(entry: &Entry) -> Art {
    match entry.kind {
        Kind::Dir | Kind::Link => Art::Path(entry.path.clone()),
        Kind::File | Kind::Other => by_extension(&entry.path),
    }
}

/// The icon for what an item is, without looking on the disk: for an
/// archive's entries or a server's, which aren't there.
pub fn kind_icon(
    kind: Kind,
    path: &Path,
    side: f32,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let art = match kind {
        Kind::Dir => Art::Folder,
        Kind::File | Kind::Link | Kind::Other => by_extension(path),
    };
    raster(art, None, side, window, cx)
}

fn by_extension(path: &Path) -> Art {
    Art::Type(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_lowercase(),
    )
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
        for pixels in [100, 102, 104, 106, 108] {
            assert!(rasters.draw(at("rs", pixels)).is_empty());
        }
        // Drawing the list's size again keeps it from being the oldest.
        assert!(rasters.draw(at("rs", 32)).is_empty());
        assert_eq!(rasters.draw(at("rs", 110)), vec![at("rs", 100)]);
        let mut gone = rasters.draw(at("rs", 112));
        gone.extend(rasters.draw(at("rs", 114)));
        assert_eq!(gone, vec![at("rs", 102), at("rs", 104)]);
        assert_eq!(rasters.sizes, [114, 112, 110, 32, 108, 106]);
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

    /// Draws `art` and notes whether a picture showed.
    struct Probe {
        art: Art,
        showed: Option<bool>,
    }

    impl Render for Probe {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let image = shown(self.art.clone(), Some(Art::Folder), 32., window, cx);
            self.showed = Some(image.is_some());
            div()
        }
    }

    #[gpui::test]
    fn a_folder_draws_as_any_folder_until_its_own_icon_lands(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (seen, new) = (dir.path().join("seen"), dir.path().join("new"));
        std::fs::create_dir(&seen).unwrap();
        std::fs::create_dir(&new).unwrap();
        let (probe, cx) = cx.add_window_view(|_, _| Probe {
            art: Art::Path(seen),
            showed: None,
        });
        cx.run_until_parked();
        // The next frame draws a folder whose own icon hasn't been drawn yet.
        probe.update(cx, |probe, cx| {
            probe.art = Art::Path(new);
            cx.notify();
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert_eq!(probe.read_with(cx, |probe, _| probe.showed), Some(true));
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
