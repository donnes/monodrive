//! Pictures of images for the grid. Each is decoded and shrunk to the size
//! shown, off the UI thread, and kept by GPUI's asset cache until the
//! browser leaves the folder and releases it.

use gpui::{App, Asset, RenderImage, Window};
use image::ImageReader;
use monodrive_core::cloud::Availability;
use monodrive_core::entries::{Entry, Kind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One image at one size; a newer file is a new thumbnail.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Thumb {
    pub path: PathBuf,
    /// Pixels on the longer side.
    pub side: u32,
    pub modified: u64,
}

/// What `image` decodes in this build.
const PICTURES: [&str; 8] = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"];
/// Bigger files aren't worth reading for a thumbnail.
const LARGEST: u64 = 64 * 1024 * 1024;

/// Whether the grid draws `entry` as a thumbnail: a picture file whose
/// contents are on this Mac. Reading a cloud placeholder would download it,
/// so it, and anything whose availability isn't known, keeps its icon.
pub fn wanted(entry: &Entry) -> bool {
    entry.kind == Kind::File
        && entry.availability == Availability::Local
        && entry
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| PICTURES.contains(&ext.to_lowercase().as_str()))
}

pub(crate) struct Thumbnail;

/// How many thumbnails decode at once.
const DECODERS: usize = 2;

/// A pool of `DECODERS` permits, as a channel holding that many tokens.
fn decoders() -> &'static (async_channel::Sender<()>, async_channel::Receiver<()>) {
    static POOL: std::sync::OnceLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> =
        std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let (give, take) = async_channel::bounded(DECODERS);
        for _ in 0..DECODERS {
            let _ = give.try_send(());
        }
        (give, take)
    })
}

impl Asset for Thumbnail {
    type Source = Thumb;
    /// `None` for a file that isn't a picture after all, or too big.
    type Output = Option<Arc<RenderImage>>;

    // An `async fn` would hold the `&mut App` borrow; the asset cache needs
    // a future that outlives it.
    #[allow(clippy::manual_async_fn)]
    fn load(
        thumb: Thumb,
        _: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        async move {
            // A folder of big photos mustn't take every background thread
            // from listings and searches.
            let (give, take) = decoders();
            let permit = take.recv().await;
            let image = decode(&thumb.path, thumb.side).map(Arc::new);
            if let Ok(permit) = permit {
                let _ = give.send(permit).await;
            }
            image
        }
    }
}

fn decode(path: &Path, side: u32) -> Option<RenderImage> {
    if std::fs::metadata(path).ok()?.len() > LARGEST {
        return None;
    }
    let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    // `DynamicImage::thumbnail` keeps the picture's shape; `imageops`'s
    // stretches it to the box.
    let mut pixels = if image.width() > side || image.height() > side {
        image.thumbnail(side, side)
    } else {
        image
    }
    .into_rgba8();
    // GPUI's textures are BGRA.
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(RenderImage::new(vec![image::Frame::new(pixels)]))
}

/// The thumbnail if it's ready; until then, `None`, and the view draws again
/// when it lands.
pub fn get(thumb: &Thumb, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
    window.use_asset::<Thumbnail>(thumb, cx).flatten()
}

/// Gives back a thumbnail no longer drawn, in memory and on the GPU; one
/// still decoding is dropped when it finishes. `window` is the window being
/// drawn, which `App` can't reach from inside its update.
pub fn release(
    thumb: &Thumb,
    image: Option<Arc<RenderImage>>,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    cx.remove_asset::<Thumbnail>(thumb);
    if let Some(image) = image {
        cx.drop_image(image, window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_shrink_to_the_side_asked_and_keep_their_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.PNG");
        image::RgbaImage::from_pixel(400, 200, image::Rgba([10, 20, 30, 255]))
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let thumb = decode(&path, 100).unwrap();
        assert_eq!(thumb.size(0).width.0, 100);
        assert_eq!(thumb.size(0).height.0, 50);
        // Red and blue swap places for GPUI.
        let bytes = thumb.as_bytes(0).unwrap();
        assert_eq!(&bytes[..4], &[30, 20, 10, 255]);
        std::fs::write(dir.path().join("fake.png"), b"no").unwrap();
        assert!(decode(&dir.path().join("fake.png"), 100).is_none());
    }

    #[test]
    fn only_local_picture_files_get_thumbnails() {
        let entry = |name: &str, kind, availability| Entry {
            path: PathBuf::from("/x").join(name),
            name: name.into(),
            kind,
            size: 10,
            modified: 0,
            availability,
        };
        assert!(wanted(&entry("wide.PNG", Kind::File, Availability::Local)));
        for availability in [
            Availability::Placeholder,
            Availability::Downloading,
            Availability::Unknown,
        ] {
            assert!(!wanted(&entry("wide.png", Kind::File, availability)));
        }
        assert!(!wanted(&entry("notes.md", Kind::File, Availability::Local)));
        assert!(!wanted(&entry(
            "photos.png",
            Kind::Dir,
            Availability::Local
        )));
    }
}
