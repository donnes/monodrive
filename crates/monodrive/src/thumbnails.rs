//! Pictures of images and videos for the grid. Each is decoded and shrunk to
//! the size shown, off the UI thread, and kept by GPUI's asset cache until the
//! browser leaves the folder and releases it.

use gpui::{App, Asset, Global, RenderImage, Window};
use image::ImageReader;
use monodrive_core::cloud::Availability;
use monodrive_core::entries::{Entry, Kind};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
/// What the system's generator opened when measured. `.avi`, `.mkv` and `.webm`
/// fail, and `.ts` is more often TypeScript than video.
const VIDEOS: [&str; 5] = ["mov", "mp4", "m4v", "mpg", "3gp"];
/// Bigger files aren't worth reading for a thumbnail.
const LARGEST: u64 = 64 * 1024 * 1024;

fn extension(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_lowercase())
}

fn is_video(path: &Path) -> bool {
    extension(path).is_some_and(|ext| VIDEOS.contains(&ext.as_str()))
}

/// Whether the grid draws `entry` as a thumbnail: a picture or video whose
/// contents are on this Mac. Reading a cloud placeholder would download it,
/// so it, and anything whose availability isn't known, keeps its icon.
pub fn wanted(entry: &Entry) -> bool {
    entry.kind == Kind::File
        && entry.availability == Availability::Local
        && extension(&entry.path)
            .is_some_and(|ext| PICTURES.contains(&ext.as_str()) || VIDEOS.contains(&ext.as_str()))
}

pub(crate) struct Thumbnail;

/// How many pictures decode at once.
const DECODERS: usize = 2;
/// How many videos generate at once, apart from pictures: a share stalled on a
/// seek mustn't hold up anything else.
const VIDEO_DECODERS: usize = 2;

/// A pool of permits, as a channel holding that many tokens.
struct Pool(async_channel::Sender<()>, async_channel::Receiver<()>);

impl Pool {
    fn new(permits: usize) -> Self {
        let (give, take) = async_channel::bounded(permits);
        for _ in 0..permits {
            let _ = give.try_send(());
        }
        Self(give, take)
    }

    /// Waits for a permit, then runs `work` unless it's no longer `wanted`.
    /// Scrolling past thousands of videos queues thousands of these, and the
    /// row on screen mustn't wait behind the ones already gone.
    async fn run<T>(
        &self,
        wanted: impl FnOnce() -> bool,
        work: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        let permit = self.1.recv().await;
        let out = if wanted() { work() } else { None };
        if let Ok(permit) = permit {
            let _ = self.0.send(permit).await;
        }
        out
    }
}

fn pool(video: bool) -> &'static Pool {
    static PICTURES: std::sync::OnceLock<Pool> = std::sync::OnceLock::new();
    static VIDEOS: std::sync::OnceLock<Pool> = std::sync::OnceLock::new();
    if video {
        VIDEOS.get_or_init(|| Pool::new(VIDEO_DECODERS))
    } else {
        PICTURES.get_or_init(|| Pool::new(DECODERS))
    }
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
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let held = cx.default_global::<Holders>().0.clone();
        async move {
            // A folder of big photos mustn't take every background thread
            // from listings and searches.
            let live = || held.lock().is_ok_and(|held| held.contains_key(&thumb));
            pool(is_video(&thumb.path))
                .run(live, || {
                    if is_video(&thumb.path) {
                        generated(&thumb.path, thumb.side, &|| !live())
                    } else {
                        decode(&thumb.path, thumb.side)
                    }
                })
                .await
                .map(Arc::new)
        }
    }
}

/// A frame from the system's generator, which is BGRA already.
fn generated(path: &Path, side: u32, cancelled: &dyn Fn() -> bool) -> Option<RenderImage> {
    let bitmap = monodrive_core::thumbnails::generate(path, side, cancelled).ok()?;
    let pixels = image::RgbaImage::from_raw(bitmap.width, bitmap.height, bitmap.bytes)?;
    Some(RenderImage::new(vec![image::Frame::new(pixels)]))
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

/// How many panes hold each thumbnail: GPUI's cache is shared, and two panes on
/// one folder draw the same pictures.
#[derive(Default)]
struct Holders(Arc<Mutex<HashMap<Thumb, usize>>>);
impl Global for Holders {}

/// A pane holds `thumb` until it calls `release`.
pub fn hold(thumb: &Thumb, cx: &mut App) {
    if let Ok(mut held) = cx.default_global::<Holders>().0.lock() {
        *held.entry(thumb.clone()).or_default() += 1;
    }
}

/// The thumbnail if it's ready; until then, `None`, and the view draws again
/// when it lands.
pub fn get(thumb: &Thumb, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
    window.use_asset::<Thumbnail>(thumb, cx).flatten()
}

/// A pane lets go of a thumbnail. Once none hold it, it's given back, in memory
/// and on the GPU; one still queued never decodes, and one decoding is dropped
/// when it finishes. `window` is the window being
/// drawn, which `App` can't reach from inside its update.
pub fn release(
    thumb: &Thumb,
    image: Option<Arc<RenderImage>>,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    if let Ok(mut held) = cx.default_global::<Holders>().0.lock()
        && let Some(count) = held.get_mut(thumb)
    {
        *count -= 1;
        if *count > 0 {
            return;
        }
        held.remove(thumb);
    }
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

    #[gpui::test]
    async fn queued_work_that_was_released_never_runs(cx: &mut gpui::TestAppContext) {
        // One permit, taken: three thumbnails wait, and only the last stays wanted.
        let pool = Arc::new(Pool::new(1));
        pool.1.recv().await.unwrap();
        let ran = Arc::new(Mutex::new(Vec::new()));
        let waiting: Vec<_> = (0..3)
            .map(|n| {
                let (pool, ran) = (pool.clone(), ran.clone());
                cx.executor().spawn(async move {
                    pool.run(
                        || n == 2,
                        || {
                            ran.lock().unwrap().push(n);
                            Some(())
                        },
                    )
                    .await
                })
            })
            .collect();
        cx.run_until_parked();
        pool.0.send(()).await.unwrap();
        for thumbnail in waiting {
            thumbnail.await;
        }
        assert_eq!(*ran.lock().unwrap(), [2]);
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
        for video in ["clip.MOV", "clip.mp4", "clip.m4v", "clip.mpg", "clip.3gp"] {
            assert!(wanted(&entry(video, Kind::File, Availability::Local)));
            assert!(!wanted(&entry(
                video,
                Kind::File,
                Availability::Placeholder
            )));
        }
        for unsupported in ["clip.mkv", "clip.webm", "clip.avi", "types.ts"] {
            assert!(!wanted(&entry(
                unsupported,
                Kind::File,
                Availability::Local
            )));
        }
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
