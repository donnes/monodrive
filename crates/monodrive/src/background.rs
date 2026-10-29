//! The image behind the file list, ported from Monocode's
//! `project_appearance.rs` and `project_background.rs`. Choosing an image
//! imports a validated copy; each effect is baked into a texture once, off
//! the UI thread; the patched Metal renderer crops, fades and hazes it while
//! painting, so resizing never redoes the work.

use gpui::{
    App, Asset, Context, Corners, ObjectFit, RenderImage, Role, Subscription, Window, canvas, div,
    img, prelude::*, px,
};
use image::{ImageReader, RgbaImage, imageops};
use serde::{Deserialize, Serialize};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    #[default]
    None,
    Dither,
    Ascii,
    Halftone,
    Scanlines,
    Haze,
}

impl Effect {
    pub const ALL: [Effect; 6] = [
        Effect::None,
        Effect::Dither,
        Effect::Ascii,
        Effect::Halftone,
        Effect::Scanlines,
        Effect::Haze,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Effect::None => "None",
            Effect::Dither => "Dither",
            Effect::Ascii => "ASCII",
            Effect::Halftone => "Halftone",
            Effect::Scanlines => "Scanlines",
            Effect::Haze => "Haze",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Effect::None => "Shows the original artwork.",
            Effect::Dither => "Rebuilds the artwork with a dithered color palette.",
            Effect::Ascii => "Recreates the artwork with colored characters on black.",
            Effect::Halftone => "Recreates the artwork with colored print dots on black.",
            Effect::Scanlines => "Adds a pronounced horizontal display-line texture.",
            Effect::Haze => "Blurs and fades the artwork into the background below.",
        }
    }
}

/// Where the image shows: only in empty folders, or in every folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Empty,
    #[default]
    All,
}

/// Decodes with bounded allocations, then shrinks to at most 2048 px.
/// Callers run off the UI thread.
fn read_image(path: &Path) -> Result<RgbaImage, String> {
    let metadata = std::fs::metadata(path).map_err(|e| format!("Can't read the image: {e}"))?;
    if metadata.len() > 30 * 1024 * 1024 {
        return Err("Choose an image smaller than 30 MB.".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "Choose a PNG, JPEG, GIF, WebP or BMP image.".to_owned())?;
    Ok(if image.width() > 2048 || image.height() > 2048 {
        image.thumbnail(2048, 2048)
    } else {
        image
    }
    .into_rgba8())
}

/// Saves a validated copy into `directory`, named by its pixels, so moving or
/// deleting the original doesn't lose the background.
pub fn import_image(source: &Path, directory: &Path) -> Result<PathBuf, String> {
    let image = read_image(source)?;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut hash = DefaultHasher::new();
    image.dimensions().hash(&mut hash);
    image.as_raw().hash(&mut hash);
    let path = directory.join(format!("background-{:016x}.png", hash.finish()));
    if !path.exists() {
        image
            .save(&path)
            .map_err(|e| format!("Can't save the image: {e}"))?;
    }
    Ok(path)
}

/// The artwork with its effect, at the artwork's resolution. Haze is the
/// renderer's, so it leaves the pixels alone.
fn texture(source: RgbaImage, effect: Effect) -> RgbaImage {
    if matches!(effect, Effect::None | Effect::Haze) {
        return source;
    }
    const BAYER: [[f32; 4]; 4] = [
        [0., 8., 2., 10.],
        [12., 4., 14., 6.],
        [3., 11., 1., 9.],
        [15., 7., 13., 5.],
    ];
    // 5×7 bitmaps from blank to dense, picked by brightness.
    const GLYPHS: [[u8; 7]; 10] = [
        [0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0, 0, 0, 4, 0],
        [0, 4, 0, 0, 4, 0, 0],
        [0, 0, 0, 14, 0, 0, 0],
        [0, 0, 14, 0, 14, 0, 0],
        [0, 4, 4, 31, 4, 4, 0],
        [0, 21, 14, 31, 14, 21, 0],
        [10, 10, 31, 10, 31, 10, 10],
        [17, 2, 4, 4, 8, 16, 17],
        [14, 17, 23, 21, 23, 16, 14],
    ];
    let (width, height) = source.dimensions();
    let mut output = source.clone();
    let sample = |x: u32, y: u32| source.get_pixel(x.min(width - 1), y.min(height - 1));
    let luma = |p: &image::Rgba<u8>| {
        (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722).round()
    };
    for (x, y, pixel) in output.enumerate_pixels_mut() {
        match effect {
            Effect::None | Effect::Haze => {}
            Effect::Dither => {
                let p = sample(x / 2 * 2 + 1, y / 2 * 2 + 1);
                let peak = p[0].max(p[1]).max(p[2]) as f32;
                let gain = if peak / 255.
                    > (BAYER[(y / 2 % 4) as usize][(x / 2 % 4) as usize] + 0.5) / 16.
                {
                    255. / peak.max(1.)
                } else {
                    0.08
                };
                for c in 0..3 {
                    pixel[c] = (p[c] as f32 * gain).round() as u8;
                }
                pixel[3] = p[3];
            }
            Effect::Ascii => {
                let p = sample(x / 6 * 6 + 3, y / 8 * 8 + 4);
                let glyph = &GLYPHS[((luma(p) / 255.).sqrt() * 9.) as usize];
                let ink =
                    x % 6 < 5 && y % 8 < 7 && (glyph[(y % 8) as usize] & (1 << (4 - x % 6))) != 0;
                for c in 0..3 {
                    pixel[c] = (pixel[c] as f32 * 0.6 + if ink { p[c] as f32 * 0.4 } else { 0. })
                        .round() as u8;
                }
            }
            Effect::Halftone => {
                let radius = 2. * (0.3 + 0.7 * (luma(sample(x / 4 * 4, y / 4 * 4)) / 255.).sqrt());
                let p = sample(x / 4 * 4 + 2, y / 4 * 4 + 2);
                let distance = (x as f32 % 4. - 1.5).hypot(y as f32 % 4. - 1.5);
                let coverage = (radius + 0.5 - distance).clamp(0., 1.) * p[3] as f32 / 255.;
                for c in 0..3 {
                    pixel[c] = (pixel[c] as f32 * 0.6 + p[c] as f32 * coverage * 0.4).round() as u8;
                }
            }
            Effect::Scanlines => {
                if y % 3 == 0 {
                    for c in 0..3 {
                        pixel[c] = (pixel[c] as f32 * 0.52).round() as u8;
                    }
                }
            }
        }
    }
    output
}

/// The smallest size that still covers `screen`, when the artwork is larger.
/// The view cover-crops it, so pixels past the screen would only be discarded.
fn covering_size(artwork: (u32, u32), screen: (u32, u32)) -> Option<(u32, u32)> {
    let (width, height) = artwork;
    if screen.0 == 0 || screen.1 == 0 || width == 0 || height == 0 {
        return None;
    }
    let scale = (screen.0 as f64 / width as f64).max(screen.1 as f64 / height as f64);
    (scale < 1.).then(|| {
        (
            ((width as f64 * scale).ceil() as u32).clamp(1, width),
            ((height as f64 * scale).ceil() as u32).clamp(1, height),
        )
    })
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct TextureRequest {
    path: PathBuf,
    effect: Effect,
}

/// One finished texture per image and effect, shared by every view that
/// draws it: the file list and the Settings preview.
struct BackgroundTexture;

impl Asset for BackgroundTexture {
    type Source = TextureRequest;
    type Output = Result<Arc<RenderImage>, String>;

    fn load(
        request: TextureRequest,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        // The largest display at Retina scale.
        let screen = cx
            .displays()
            .iter()
            .fold((0, 0), |(width, height), display| {
                let size = display.bounds().size;
                (
                    width.max((size.width.as_f32() * 2.).ceil() as u32),
                    height.max((size.height.as_f32() * 2.).ceil() as u32),
                )
            });
        async move {
            let mut pixels = texture(read_image(&request.path)?, request.effect);
            if let Some((width, height)) = covering_size(pixels.dimensions(), screen) {
                pixels = imageops::resize(&pixels, width, height, imageops::FilterType::Triangle);
            }
            // GPUI's textures are BGRA.
            for pixel in pixels.pixels_mut() {
                pixel.0.swap(0, 2);
            }
            Ok(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
        }
    }
}

/// How a view shows the background.
#[derive(Clone, PartialEq)]
pub struct Look {
    pub path: PathBuf,
    pub effect: Effect,
    pub opacity: f32,
    /// Empty views show more of the artwork before it fades out.
    pub empty: bool,
    /// The Settings preview: a smaller, rounded Haze.
    pub preview: bool,
    pub radius: f32,
}

#[derive(Default)]
pub struct Backdrop {
    look: Option<Look>,
    request: Option<TextureRequest>,
    image: Option<Arc<RenderImage>>,
    loading: bool,
    _release: Option<Subscription>,
}

impl Backdrop {
    pub fn set(&mut self, look: Option<Look>, cx: &mut Context<Self>) {
        if self.look != look {
            self.look = look;
            cx.notify();
        }
    }

    /// Gives back a texture no other view draws. The asset cache would
    /// otherwise keep every image and effect, in memory and on the GPU.
    fn release(&mut self, window: Option<&mut Window>, cx: &mut App) {
        let (Some(request), Some(image)) = (self.request.take(), self.image.take()) else {
            return;
        };
        // Only this view and the asset cache hold it.
        if Arc::strong_count(&image) == 2 {
            cx.remove_asset::<BackgroundTexture>(&request);
            cx.drop_image(image, window);
        }
    }

    fn prepare(&mut self, request: TextureRequest, window: &mut Window, cx: &mut Context<Self>) {
        if self._release.is_none() {
            self._release = Some(cx.on_release(|this, cx| this.release(None, cx)));
        }
        if self.request.as_ref() != Some(&request) {
            self.release(Some(window), cx);
        }
        self.request = Some(request.clone());
        // A finished texture returns in this render; a pending one re-renders
        // this view when it lands.
        match window.use_asset::<BackgroundTexture>(&request, cx) {
            Some(Ok(image)) => {
                self.image = Some(image);
                self.loading = false;
            }
            // An image that can't be read anymore simply isn't drawn.
            Some(Err(_)) => {
                self.image = None;
                self.loading = false;
            }
            None => self.loading = true,
        }
    }

    /// Brackets the image so the renderer fades, rounds and hazes it.
    fn mask(begin: bool, look: &Look) -> impl IntoElement {
        let marker = gpui_apple::metal_renderer::background_mask_marker(
            begin,
            look.empty,
            look.effect == Effect::Haze,
            look.preview,
            look.opacity,
        );
        let radius = px(look.radius);
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                window.paint_quad(
                    gpui::fill(bounds, gpui::black())
                        .corner_radii(Corners::all(radius))
                        // The renderer reads the display scale from this border.
                        .border_widths(gpui::Edges {
                            left: px(1.),
                            ..Default::default()
                        })
                        .border_color(marker),
                );
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

impl Render for Backdrop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(look) = self.look.clone().filter(|look| look.opacity > 0.) else {
            return div().into_any_element();
        };
        let request = TextureRequest {
            path: look.path.clone(),
            effect: look.effect,
        };
        if self.request.as_ref() != Some(&request) || self.loading {
            self.prepare(request, window, cx);
        }
        div()
            .id("background")
            .role(Role::Image)
            .aria_label("Background")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .when_some(self.image.clone(), |d, image| {
                d.child(Self::mask(true, &look))
                    .child(img(image).size_full().object_fit(ObjectFit::Cover))
                    .child(Self::mask(false, &look))
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dither_uses_two_pixel_bayer_cells_and_keeps_alpha() {
        let source = RgbaImage::from_pixel(9, 9, image::Rgba([130, 180, 210, 127]));
        let result = texture(source, Effect::Dither);
        let bayer = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
        for (x, y, pixel) in result.enumerate_pixels() {
            let expected = if bayer[(y / 2 % 4) as usize][(x / 2 % 4) as usize] <= 12 {
                [158, 219, 255, 127]
            } else {
                [10, 14, 17, 127]
            };
            assert_eq!(pixel.0, expected, "at {x},{y}");
        }
    }

    #[test]
    fn baked_effects_change_pixels_and_haze_is_left_to_the_renderer() {
        let source = RgbaImage::from_fn(24, 24, |x, y| {
            image::Rgba([(x * 10) as u8, (y * 10) as u8, 90, 255])
        });
        for effect in [
            Effect::Dither,
            Effect::Ascii,
            Effect::Halftone,
            Effect::Scanlines,
        ] {
            let result = texture(source.clone(), effect);
            assert_eq!(result.dimensions(), source.dimensions());
            assert_ne!(result, source, "{effect:?}");
        }
        for effect in [Effect::None, Effect::Haze] {
            assert_eq!(texture(source.clone(), effect), source);
        }
    }

    #[test]
    fn textures_shrink_only_to_still_cover_the_largest_screen() {
        assert_eq!(
            covering_size((3360, 2038), (2880, 1800)),
            Some((2968, 1800))
        );
        assert_eq!(
            covering_size((3266, 2324), (2880, 1800)),
            Some((2880, 2050))
        );
        assert_eq!(covering_size((2048, 1242), (2880, 1800)), None);
        assert_eq!(covering_size((3360, 2038), (0, 0)), None);
    }

    #[test]
    fn importing_keeps_its_own_copy_and_rejects_non_images() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("original.png");
        RgbaImage::from_pixel(12, 8, image::Rgba([100, 180, 90, 255]))
            .save(&source)
            .unwrap();
        let imported = import_image(&source, &directory.path().join("backgrounds")).unwrap();
        std::fs::remove_file(source).unwrap();
        assert_eq!(read_image(&imported).unwrap().dimensions(), (12, 8));

        let bad = directory.path().join("bad.png");
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(import_image(&bad, &directory.path().join("backgrounds")).is_err());
    }
}
