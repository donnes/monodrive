//! Pictures of files from the system's generator, which Finder also uses. It
//! keeps its own cache and picks the frame, so Monodrive owns neither.

use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_image_io::CGImageSource;
use objc2_quick_look_thumbnailing::{
    QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes,
    QLThumbnailGenerator, QLThumbnailRepresentation,
};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A share can stall without ever failing, and a stuck caller holds a thread.
const PATIENCE: Duration = Duration::from_secs(20);
/// How often a wait looks at `cancelled`.
const CHECK: Duration = Duration::from_millis(100);

/// BGRA, top row first, with the alpha already applied to the colors.
#[derive(Debug)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// `path` decoded by ImageIO, `side` pixels on its longer edge at most, the
/// right way up. The generator below errors on a file over about 2300 pixels
/// when asked for more than that, so it can't draw a big picture large. This
/// reads pictures alone, and blocks, so call it off the UI thread. Refuses a
/// cloud placeholder, since reading one would download it.
pub fn picture(path: &Path, side: u32) -> Result<Bitmap, String> {
    crate::cloud::refuse_placeholder(path)?;
    let cannot = || format!("{} can't be read as a picture.", path.display());
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    // SAFETY: NSURL and CFURL are the same object, and `url` outlives the call.
    let source = unsafe { CGImageSource::with_url(&*Retained::as_ptr(&url).cast(), None) }
        .ok_or_else(cannot)?;
    let image = crate::edits::upright(&source, side as i32).ok_or_else(cannot)?;
    bitmap(&image)
}

/// A picture of `path`, `side` pixels on its longer edge at most, the right
/// way up. Blocks until it's ready, `cancelled` returns true, or `PATIENCE`
/// runs out, so call it off the UI thread. Refuses a cloud placeholder, since
/// generating from one would download it.
pub fn generate(
    path: &Path,
    side: u32,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> Result<Bitmap, String> {
    crate::cloud::refuse_placeholder(path)?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let size = CGSize::new(CGFloat::from(side), CGFloat::from(side));
    // SAFETY: the arguments are valid for the initializer, and a generator's
    // methods may be called from any thread.
    let (generator, request) = unsafe {
        (
            QLThumbnailGenerator::sharedGenerator(),
            QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
                QLThumbnailGenerationRequest::alloc(),
                &url,
                size,
                1.0,
                QLThumbnailGenerationRequestRepresentationTypes::Thumbnail,
            ),
        )
    };
    let (send, receive) = mpsc::channel();
    let finished = RcBlock::new(
        move |picture: *mut QLThumbnailRepresentation, error: *mut NSError| {
            // SAFETY: the generator passes a valid or null object of each.
            let reply = match unsafe { (picture.as_ref(), error.as_ref()) } {
                (Some(picture), _) => bitmap(&*unsafe { picture.CGImage() }),
                (None, Some(error)) => Err(error.localizedDescription().to_string()),
                (None, None) => Err("the system returned no thumbnail".to_string()),
            };
            let _ = send.send(reply);
        },
    );
    // SAFETY: the block copies itself, and is called once on the generator's
    // own queue.
    unsafe {
        generator.generateBestRepresentationForRequest_completionHandler(&request, &finished)
    };
    let began = Instant::now();
    loop {
        match receive.recv_timeout(CHECK) {
            Ok(reply) => return reply,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("the system returned no thumbnail".into());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let stopped = cancelled();
        if stopped || began.elapsed() > PATIENCE {
            // SAFETY: cancelling is allowed at any point in a request's life.
            unsafe { generator.cancelRequest(&request) };
            return Err(if stopped {
                "cancelled".into()
            } else {
                format!("no thumbnail of {} after 20 seconds", path.display())
            });
        }
    }
}

/// Draws into memory, since a `CGImage` may be in any pixel format.
pub(crate) fn bitmap(image: &CGImage) -> Result<Bitmap, String> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let mut bytes = vec![0u8; width * height * 4];
    let space = CGColorSpace::new_device_rgb().ok_or("no color space")?;
    // BGRA in memory: a little-endian word holding alpha first.
    let info = CGImageByteOrderInfo::Order32Little.0 | CGImageAlphaInfo::PremultipliedFirst.0;
    // SAFETY: `bytes` holds `width * 4` bytes for each of `height` rows and
    // outlives the context.
    let context = unsafe {
        CGBitmapContextCreate(
            bytes.as_mut_ptr().cast(),
            width,
            height,
            8,
            width * 4,
            Some(&space),
            info,
        )
    }
    .ok_or("couldn't draw the thumbnail")?;
    let whole = CGRect::new(
        CGPoint::ZERO,
        CGSize::new(width as CGFloat, height as CGFloat),
    );
    CGContext::draw_image(Some(&context), whole, Some(image));
    drop(context);
    Ok(Bitmap {
        width: width as u32,
        height: height as u32,
        bytes,
    })
}

/// A bitmap as JPEG, which keeps an agent's reply small.
pub fn jpeg(bitmap: &Bitmap) -> Result<Vec<u8>, String> {
    let rgb: Vec<u8> = bitmap
        .bytes
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[blue, green, red, _]| [red, green, blue])
        .collect();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80)
        .encode(
            &rgb,
            bitmap.width,
            bitmap.height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn a_rotated_video_comes_back_the_right_way_up() {
        // Stored 64 by 36, flagged to play turned a quarter.
        let bitmap = generate(&fixture("rotated.mp4"), 128, &|| false).unwrap();
        assert!(bitmap.height > bitmap.width, "{bitmap:?}");
        assert_eq!(
            bitmap.bytes.len(),
            (bitmap.width * bitmap.height * 4) as usize
        );
        assert!(jpeg(&bitmap).unwrap().starts_with(&[0xff, 0xd8]));
    }

    #[test]
    fn a_big_picture_comes_at_any_size_asked_and_never_larger_than_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        image::RgbImage::new(3000, 2000).save(&path).unwrap();
        for (side, width, height) in [(1500, 1500, 1000), (2800, 2800, 1867), (8192, 3000, 2000)] {
            let bitmap = picture(&path, side).unwrap();
            assert_eq!((bitmap.width, bitmap.height), (width, height), "at {side}");
        }
    }

    #[test]
    fn a_format_apple_cannot_open_is_an_error() {
        assert!(generate(&fixture("clip.mkv"), 128, &|| false).is_err());
    }
}
