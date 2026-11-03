//! Small edits as typed recipes, so the window and agents make the same file.
//! This only writes a prepared file under a hidden name beside where it's going.
//! `ops` then checks the original didn't change, and moves the file into place
//! as one operation that Undo reverses, so a long export never holds the owner.

mod video;

pub use video::{
    Clip, VideoRecipe, export, frame, frame_bitmap, size as video_size, video_extension,
};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSAttributedStringNSStringDrawing as _, NSColor, NSFont, NSGraphicsContext};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage,
    CGImageAlphaInfo, CGImageByteOrderInfo, CGLineCap, CGLineJoin,
};
use objc2_foundation::{
    NSAttributedString, NSDictionary, NSMutableDictionary, NSNumber, NSPoint, NSString, NSURL,
};
use objc2_image_io::{
    CGImageDestination, CGImageSource, kCGImagePropertyOrientation,
    kCGImageSourceCreateThumbnailFromImageAlways, kCGImageSourceCreateThumbnailWithTransform,
    kCGImageSourceThumbnailMaxPixelSize,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Red, an arrow's default, in 0 to 1 channels with alpha.
fn red() -> [f64; 4] {
    [0.9, 0.2, 0.15, 1.]
}

fn yellow() -> [f64; 4] {
    [1., 0.85, 0.1, 0.4]
}

fn line() -> f64 {
    6.
}

/// In pixels of the picture as earlier steps left it, from its top left.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One edit, applied to what the ones before it left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    Crop {
        rect: Rect,
    },
    /// Clockwise.
    Rotate {
        quarter_turns: i32,
    },
    Flip {
        #[serde(default)]
        vertical: bool,
    },
    Pen {
        points: Vec<[f64; 2]>,
        #[serde(default = "red")]
        color: [f64; 4],
        #[serde(default = "line")]
        width: f64,
    },
    Highlighter {
        points: Vec<[f64; 2]>,
        #[serde(default = "yellow")]
        color: [f64; 4],
        #[serde(default = "highlight_width")]
        width: f64,
    },
    Arrow {
        from: [f64; 2],
        to: [f64; 2],
        #[serde(default = "red")]
        color: [f64; 4],
        #[serde(default = "line")]
        width: f64,
    },
    Rectangle {
        rect: Rect,
        #[serde(default = "red")]
        color: [f64; 4],
        #[serde(default = "line")]
        width: f64,
    },
    Ellipse {
        rect: Rect,
        #[serde(default = "red")]
        color: [f64; 4],
        #[serde(default = "line")]
        width: f64,
    },
    Text {
        at: [f64; 2],
        text: String,
        #[serde(default = "text_size")]
        size: f64,
        #[serde(default = "red")]
        color: [f64; 4],
    },
    /// Solid black, since a blur can be reversed. Saving then strips the
    /// metadata, as an embedded thumbnail can show what's hidden.
    Redact {
        rect: Rect,
    },
}

fn highlight_width() -> f64 {
    24.
}

fn text_size() -> f64 {
    36.
}

/// The extension an edited picture gets: its own when macOS can write that, or
/// else PNG, as a one-frame GIF or a WebP can't be written back as itself.
pub fn picture_extension(source: &Path) -> String {
    match source.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if picture_type(ext).is_some() => ext.to_string(),
        _ => "png".to_string(),
    }
}

/// What ImageIO calls a format it can write.
fn picture_type(extension: &str) -> Option<&'static str> {
    Some(match extension.to_lowercase().as_str() {
        "jpg" | "jpeg" => "public.jpeg",
        "heic" | "heif" => "public.heic",
        "tif" | "tiff" => "public.tiff",
        "bmp" => "com.microsoft.bmp",
        "png" => "public.png",
        _ => return None,
    })
}

/// A hidden name in `folder` for a file on its way to `extension`.
pub fn prepared_path(folder: &Path, extension: &str) -> PathBuf {
    use std::hash::{BuildHasher, RandomState};
    let unique = RandomState::new().hash_one(std::process::id());
    folder.join(format!("{PREPARED_PREFIX}{unique:016x}.{extension}"))
}

/// What a prepared file's name starts with. The owner moves only these.
pub const PREPARED_PREFIX: &str = ".monodrive-edit-";

/// Removes a prepared file that won't be used.
#[expect(
    clippy::disallowed_methods,
    reason = "only ever a hidden file this module just wrote, never one of the user's"
)]
pub fn discard(prepared: &Path) {
    if prepared
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(PREPARED_PREFIX))
    {
        let _ = std::fs::remove_file(prepared);
    }
}

fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

/// Applies `steps` to the picture at `source`, writing `into`, whose extension
/// picks the format. Returns the result's size in pixels.
pub fn flatten(source: &Path, steps: &[Step], into: &Path) -> Result<(usize, usize), String> {
    crate::cloud::refuse_placeholder(source)?;
    let (mut image, properties) = decode(source)?;
    for step in steps {
        image = apply(&image, step)?;
    }
    let redacted = steps.iter().any(|step| matches!(step, Step::Redact { .. }));
    encode(&image, into, properties.filter(|_| !redacted))?;
    Ok((CGImage::width(Some(&image)), CGImage::height(Some(&image))))
}

/// What `flatten` would write, in memory: for the window to show as it edits.
pub fn render(source: &Path, steps: &[Step]) -> Result<crate::thumbnails::Bitmap, String> {
    crate::cloud::refuse_placeholder(source)?;
    let (mut image, _) = decode(source)?;
    for step in steps {
        image = apply(&image, step)?;
    }
    crate::thumbnails::bitmap(&image)
}

/// The source's first picture the right way up, `side` pixels on its longer
/// edge at most, and never enlarged.
pub(crate) fn upright(source: &CGImageSource, side: i32) -> Option<CFRetained<CGImage>> {
    let options: Retained<NSMutableDictionary<NSString, AnyObject>> = NSMutableDictionary::new();
    let yes = NSNumber::numberWithBool(true);
    // SAFETY: the option keys are the framework's constants, and NSString and
    // CFString, like NSDictionary and CFDictionary, are the same objects.
    unsafe {
        let insert = |name: &CFString, value: &AnyObject| {
            let name: *const NSString = std::ptr::from_ref(name).cast();
            options.insert(&*name, value);
        };
        insert(kCGImageSourceCreateThumbnailFromImageAlways, yes.as_ref());
        insert(kCGImageSourceCreateThumbnailWithTransform, yes.as_ref());
        insert(
            kCGImageSourceThumbnailMaxPixelSize,
            NSNumber::numberWithInt(side).as_ref(),
        );
        let dictionary: &CFDictionary = &*Retained::as_ptr(&options).cast();
        source.thumbnail_at_index(0, Some(dictionary))
    }
}

/// The picture the right way up, at full size, and its metadata saying so.
fn decode(path: &Path) -> Result<(CFRetained<CGImage>, Option<CFRetained<CFDictionary>>), String> {
    let cannot = || format!("{} can't be read as a picture.", path.display());
    let url = url(path);
    let cf_url: &objc2_core_foundation::CFURL = unsafe { &*Retained::as_ptr(&url).cast() };
    // SAFETY: a valid URL and no options. The option keys are the framework's
    // constants, and NSDictionary and CFDictionary are the same object.
    unsafe {
        let source = CGImageSource::with_url(cf_url, None).ok_or_else(cannot)?;
        let image = upright(&source, 1 << 16).ok_or_else(cannot)?;
        let properties = source.properties_at_index(0, None).map(|properties| {
            // The pixels are upright now, so nothing may turn them again.
            let found: &NSDictionary<NSString, AnyObject> =
                &*CFRetained::as_ptr(&properties).as_ptr().cast();
            let kept = NSMutableDictionary::dictionaryWithDictionary(found);
            let orientation: *const NSString =
                std::ptr::from_ref(kCGImagePropertyOrientation).cast();
            kept.removeObjectForKey(&*orientation);
            let raw = Retained::into_raw(kept.into_super().into_super());
            CFRetained::from_raw(std::ptr::NonNull::new_unchecked(raw.cast::<CFDictionary>()))
        });
        Ok((image, properties))
    }
}

fn encode(
    image: &CGImage,
    into: &Path,
    properties: Option<CFRetained<CFDictionary>>,
) -> Result<(), String> {
    let kind = into
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(picture_type)
        .ok_or_else(|| format!("Pictures can't be saved as {}.", into.display()))?;
    let cannot = || format!("{} can't be written.", into.display());
    let url = url(into);
    let kind = CFString::from_str(kind);
    // SAFETY: valid objects, and one image for the one the destination expects.
    unsafe {
        let cf_url: &objc2_core_foundation::CFURL = &*Retained::as_ptr(&url).cast();
        let destination =
            CGImageDestination::with_url(cf_url, &kind, 1, None).ok_or_else(cannot)?;
        destination.add_image(image, properties.as_deref());
        if destination.finalize() {
            Ok(())
        } else {
            Err(cannot())
        }
    }
}

/// A drawing context on a blank picture, in top-left coordinates.
fn canvas(width: usize, height: usize) -> Result<CFRetained<CGContext>, String> {
    let space = CGColorSpace::new_device_rgb().ok_or("no color space")?;
    let info = CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0;
    // SAFETY: a null buffer asks Core Graphics to own the pixels.
    let context = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width,
            height,
            8,
            0,
            Some(&space),
            info,
        )
    }
    .ok_or("couldn't make a picture that size")?;
    Ok(context)
}

fn whole(width: f64, height: f64) -> CGRect {
    CGRect::new(CGPoint::ZERO, CGSize::new(width, height))
}

fn finished(context: &CGContext) -> Result<CFRetained<CGImage>, String> {
    CGBitmapContextCreateImage(Some(context)).ok_or("couldn't finish the picture".to_string())
}

/// `image` drawn again, with the context turned upside down so the marks use
/// the picture's top-left coordinates.
fn drawn_on(
    image: &CGImage,
    marks: impl FnOnce(&CGContext, f64),
) -> Result<CFRetained<CGImage>, String> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let context = canvas(width, height)?;
    let (w, h) = (width as f64, height as f64);
    CGContext::draw_image(Some(&context), whole(w, h), Some(image));
    CGContext::translate_ctm(Some(&context), 0., h);
    CGContext::scale_ctm(Some(&context), 1., -1.);
    marks(&context, h);
    finished(&context)
}

fn ink(context: &CGContext, color: [f64; 4], width: f64) {
    let [r, g, b, a] = color;
    CGContext::set_rgb_stroke_color(
        Some(context),
        r as CGFloat,
        g as CGFloat,
        b as CGFloat,
        a as CGFloat,
    );
    CGContext::set_line_width(Some(context), width as CGFloat);
    CGContext::set_line_cap(Some(context), CGLineCap::Round);
    CGContext::set_line_join(Some(context), CGLineJoin::Round);
}

fn stroke(context: &CGContext, points: &[[f64; 2]]) {
    let Some(first) = points.first() else {
        return;
    };
    CGContext::begin_path(Some(context));
    CGContext::move_to_point(Some(context), first[0], first[1]);
    // A single press still leaves a dot.
    if points.len() == 1 {
        CGContext::add_line_to_point(Some(context), first[0], first[1]);
    }
    for point in &points[1..] {
        CGContext::add_line_to_point(Some(context), point[0], point[1]);
    }
    CGContext::stroke_path(Some(context));
}

fn cg_rect(rect: Rect) -> CGRect {
    CGRect::new(
        CGPoint::new(rect.x, rect.y),
        CGSize::new(rect.width, rect.height),
    )
}

fn apply(image: &CGImage, step: &Step) -> Result<CFRetained<CGImage>, String> {
    let (width, height) = (
        CGImage::width(Some(image)) as f64,
        CGImage::height(Some(image)) as f64,
    );
    match step {
        Step::Crop { rect } => {
            let area = cg_rect(*rect);
            if rect.width < 1. || rect.height < 1. {
                return Err("A crop needs some width and height.".into());
            }
            CGImage::with_image_in_rect(Some(image), area)
                .ok_or("That crop is outside the picture.".to_string())
        }
        Step::Rotate { quarter_turns } => {
            let turns = quarter_turns.rem_euclid(4);
            let (new_w, new_h) = if turns % 2 == 1 {
                (height, width)
            } else {
                (width, height)
            };
            let context = canvas(new_w as usize, new_h as usize)?;
            let c = Some(&*context);
            CGContext::translate_ctm(c, new_w / 2., new_h / 2.);
            // Core Graphics' positive angle is counterclockwise here.
            CGContext::rotate_ctm(c, -f64::from(turns) * std::f64::consts::FRAC_PI_2);
            CGContext::draw_image(
                c,
                CGRect::new(
                    CGPoint::new(-width / 2., -height / 2.),
                    CGSize::new(width, height),
                ),
                Some(image),
            );
            finished(&context)
        }
        Step::Flip { vertical } => {
            let context = canvas(width as usize, height as usize)?;
            let c = Some(&*context);
            if *vertical {
                CGContext::translate_ctm(c, 0., height);
                CGContext::scale_ctm(c, 1., -1.);
            } else {
                CGContext::translate_ctm(c, width, 0.);
                CGContext::scale_ctm(c, -1., 1.);
            }
            CGContext::draw_image(c, whole(width, height), Some(image));
            finished(&context)
        }
        Step::Pen {
            points,
            color,
            width,
        }
        | Step::Highlighter {
            points,
            color,
            width,
        } => drawn_on(image, |c, _| {
            ink(c, *color, *width);
            stroke(c, points);
        }),
        Step::Arrow {
            from,
            to,
            color,
            width,
        } => drawn_on(image, |c, _| {
            ink(c, *color, *width);
            stroke(c, &[*from, *to]);
            let angle = (to[1] - from[1]).atan2(to[0] - from[0]);
            let barb = width * 4.;
            for side in [-0.5, 0.5_f64] {
                let wing = [
                    to[0] - barb * (angle + side).cos(),
                    to[1] - barb * (angle + side).sin(),
                ];
                stroke(c, &[wing, *to]);
            }
        }),
        Step::Rectangle { rect, color, width } => drawn_on(image, |c, _| {
            ink(c, *color, *width);
            CGContext::stroke_rect(Some(c), cg_rect(*rect));
        }),
        Step::Ellipse { rect, color, width } => drawn_on(image, |c, _| {
            ink(c, *color, *width);
            CGContext::stroke_ellipse_in_rect(Some(c), cg_rect(*rect));
        }),
        Step::Redact { rect } => drawn_on(image, |c, _| {
            CGContext::set_rgb_fill_color(Some(c), 0., 0., 0., 1.);
            CGContext::fill_rect(Some(c), cg_rect(*rect));
        }),
        Step::Text {
            at,
            text,
            size,
            color,
        } => drawn_on(image, |c, _| draw_text(c, *at, text, *size, *color)),
    }
}

fn draw_text(context: &CGContext, at: [f64; 2], text: &str, size: f64, color: [f64; 4]) {
    let [r, g, b, a] = color;
    // SAFETY: the context outlives this, is set current only on this thread for
    // the draw, and the keys are the framework's own.
    unsafe {
        let graphics = NSGraphicsContext::graphicsContextWithCGContext_flipped(context, true);
        NSGraphicsContext::saveGraphicsState_class();
        NSGraphicsContext::setCurrentContext(Some(&graphics));
        let font = NSFont::systemFontOfSize(size);
        let color = NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a);
        let keys: [&NSString; 2] = [
            objc2_app_kit::NSFontAttributeName,
            objc2_app_kit::NSForegroundColorAttributeName,
        ];
        let values: [&AnyObject; 2] = [font.as_ref(), color.as_ref()];
        let attributes = NSDictionary::from_slices(&keys, &values);
        let string = NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&attributes),
        );
        string.drawAtPoint(NSPoint::new(at[0], at[1]));
        NSGraphicsContext::restoreGraphicsState_class();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(dir: &Path, name: &str, width: u32, height: u32, rgb: [u8; 3]) -> PathBuf {
        let path = dir.join(name);
        image::RgbImage::from_pixel(width, height, image::Rgb(rgb))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        path
    }

    fn pixels(path: &Path) -> image::RgbaImage {
        image::ImageReader::open(path)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap()
            .into_rgba8()
    }

    #[test]
    fn crop_rotate_and_flip_give_the_expected_shape_and_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let source = picture(dir.path(), "a.png", 40, 20, [0, 0, 255]);
        let out = dir.path().join("out.png");
        let size = flatten(
            &source,
            &[
                Step::Redact {
                    rect: Rect {
                        x: 0.,
                        y: 0.,
                        width: 10.,
                        height: 20.,
                    },
                },
                Step::Rotate { quarter_turns: 1 },
            ],
            &out,
        )
        .unwrap();
        assert_eq!(size, (20, 40));
        // Black was on the left; a clockwise turn puts it on top.
        let drawn = pixels(&out);
        assert_eq!(drawn.dimensions(), (20, 40));
        assert_eq!(drawn.get_pixel(10, 2).0[..3], [0, 0, 0]);
        let bottom = drawn.get_pixel(10, 38).0;
        assert!(bottom[2] > 200 && bottom[0] < 40, "{bottom:?}");

        flatten(
            &source,
            &[Step::Crop {
                rect: Rect {
                    x: 5.,
                    y: 5.,
                    width: 10.,
                    height: 8.,
                },
            }],
            &out,
        )
        .unwrap();
        assert_eq!(pixels(&out).dimensions(), (10, 8));

        flatten(
            &source,
            &[
                Step::Redact {
                    rect: Rect {
                        x: 0.,
                        y: 0.,
                        width: 10.,
                        height: 20.,
                    },
                },
                Step::Flip { vertical: false },
            ],
            &out,
        )
        .unwrap();
        assert_eq!(pixels(&out).get_pixel(38, 10).0[..3], [0, 0, 0]);
    }

    #[test]
    fn marks_land_where_the_recipe_says_from_the_top_left() {
        let dir = tempfile::tempdir().unwrap();
        let source = picture(dir.path(), "a.png", 100, 100, [255, 255, 255]);
        let out = dir.path().join("out.png");
        flatten(
            &source,
            &[
                Step::Pen {
                    points: vec![[10., 10.], [30., 10.]],
                    color: [1., 0., 0., 1.],
                    width: 4.,
                },
                Step::Rectangle {
                    rect: Rect {
                        x: 50.,
                        y: 70.,
                        width: 40.,
                        height: 20.,
                    },
                    color: [0., 0., 1., 1.],
                    width: 2.,
                },
                Step::Text {
                    at: [10., 40.],
                    text: "Hi".into(),
                    size: 20.,
                    color: [0., 0., 0., 1.],
                },
            ],
            &out,
        )
        .unwrap();
        let drawn = pixels(&out);
        let at = |x, y| drawn.get_pixel(x, y).0;
        assert!(
            at(20, 10)[0] > 200 && at(20, 10)[1] < 80,
            "{:?}",
            at(20, 10)
        );
        assert!(at(20, 90)[1] > 200, "the pen didn't land at the bottom");
        assert!(
            at(70, 70)[2] > 200 && at(70, 70)[0] < 80,
            "{:?}",
            at(70, 70)
        );
        let inked = (10..40).any(|x| (40..65).any(|y| at(x, y)[0] < 100));
        assert!(inked, "the text drew nothing near where it was asked");
    }

    /// A JPEG whose EXIF description says `SECRET-PLACE`.
    fn jpeg_with_exif(path: &Path) {
        let mut body = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(30, 30, image::Rgb([10, 200, 10]))
            .write_to(&mut body, image::ImageFormat::Jpeg)
            .unwrap();
        let body = body.into_inner();
        // A big-endian TIFF with one entry: ImageDescription, 13 ASCII bytes,
        // stored right after the directory.
        let mut tiff: Vec<u8> =
            b"MM\0\x2a\0\0\0\x08\0\x01\x01\x0e\0\x02\0\0\0\x0d\0\0\0\x1a\0\0\0\0".to_vec();
        tiff.extend_from_slice(b"SECRET-PLACE\0");
        let mut segment = b"Exif\0\0".to_vec();
        segment.extend_from_slice(&tiff);
        let mut out = body[..2].to_vec();
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&((segment.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&segment);
        out.extend_from_slice(&body[2..]);
        std::fs::write(path, out).unwrap();
    }

    fn says_secret(path: &Path) -> bool {
        let bytes = std::fs::read(path).unwrap();
        bytes.windows(12).any(|window| window == b"SECRET-PLACE")
    }

    #[test]
    fn metadata_stays_unless_a_redact_means_it_could_leak() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("a.jpg");
        jpeg_with_exif(&source);
        assert!(says_secret(&source));
        let kept = dir.path().join("kept.jpg");
        flatten(&source, &[Step::Flip { vertical: false }], &kept).unwrap();
        assert!(says_secret(&kept), "an ordinary edit keeps metadata");

        let rect = Rect {
            x: 0.,
            y: 0.,
            width: 30.,
            height: 30.,
        };
        let redacted = dir.path().join("redacted.jpg");
        flatten(&source, &[Step::Redact { rect }], &redacted).unwrap();
        assert!(!says_secret(&redacted));
        let drawn = pixels(&redacted);
        assert!(
            drawn
                .pixels()
                .all(|pixel| pixel.0[..3].iter().all(|c| *c < 16))
        );
    }

    #[test]
    fn a_recipe_reads_from_json_with_defaults() {
        let steps: Vec<Step> = serde_json::from_str(
            r#"[{"type":"rotate","quarter_turns":1},{"type":"arrow","from":[0,0],"to":[1,1]}]"#,
        )
        .unwrap();
        assert_eq!(steps[0], Step::Rotate { quarter_turns: 1 });
        assert!(matches!(&steps[1], Step::Arrow { width, .. } if *width == 6.));
    }

    #[test]
    fn a_picture_keeps_a_format_macos_can_write_and_never_lies_about_one() {
        assert_eq!(picture_extension(Path::new("a.JPG")), "JPG");
        assert_eq!(picture_extension(Path::new("a.heic")), "heic");
        assert_eq!(picture_extension(Path::new("a.webp")), "png");
        assert_eq!(picture_extension(Path::new("a.gif")), "png");
        let dir = tempfile::tempdir().unwrap();
        let source = picture(dir.path(), "a.png", 4, 4, [0, 0, 0]);
        assert!(flatten(&source, &[], &dir.path().join("out.webp")).is_err());
    }

    #[test]
    fn only_a_prepared_file_can_be_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let mine = prepared_path(dir.path(), "png");
        std::fs::write(&mine, "x").unwrap();
        let theirs = dir.path().join("photo.png");
        std::fs::write(&theirs, "x").unwrap();
        discard(&mine);
        discard(&theirs);
        assert!(!mine.exists());
        assert!(theirs.exists());
    }
}
