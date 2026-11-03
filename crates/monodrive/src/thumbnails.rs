//! Pictures of images and videos for the grid. Each is decoded and shrunk to
//! the size shown, off the UI thread, and kept by GPUI's asset cache. Once
//! nothing draws it, it stays in memory up to `KEPT_BYTES`, so coming back to a
//! folder or closing a preview draws it at once instead of decoding it again.

use gpui::{App, Asset, Global, RenderImage, Window};
use image::ImageReader;
use monodrive_core::cloud::Availability;
use monodrive_core::entries::{Entry, Kind};
use std::collections::{BTreeMap, HashMap};
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
        let held = cx.default_global::<Holders>().held.clone();
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
/// one folder draw the same pictures. `kept` has the ones nothing holds.
#[derive(Default)]
pub(crate) struct Holders {
    pub(crate) held: Arc<Mutex<HashMap<Thumb, usize>>>,
    kept: Kept,
}
impl Global for Holders {}

/// How much of what no pane draws stays in memory. That's about 800 thumbnails
/// at the grid's usual size.
const KEPT_BYTES: usize = 128 * 1024 * 1024;

/// Loaded thumbnails that nothing draws, dropped oldest first past a budget.
#[derive(Default)]
struct Kept {
    images: HashMap<Thumb, (u64, Arc<RenderImage>)>,
    /// `images`' keys by when each was kept.
    order: BTreeMap<u64, Thumb>,
    next: u64,
    bytes: usize,
}

fn bytes_of(image: &RenderImage) -> usize {
    image.as_bytes(0).map_or(0, <[u8]>::len)
}

impl Kept {
    /// Keeps `image`, and says which older ones no longer fit.
    fn keep(&mut self, thumb: Thumb, image: Arc<RenderImage>, budget: usize) -> Vec<Thumb> {
        self.take(&thumb);
        self.bytes += bytes_of(&image);
        self.order.insert(self.next, thumb.clone());
        self.images.insert(thumb, (self.next, image));
        self.next += 1;
        let mut dropped = Vec::new();
        while self.bytes > budget {
            let Some((_, oldest)) = self.order.first_key_value() else {
                break;
            };
            let oldest = oldest.clone();
            self.take(&oldest);
            dropped.push(oldest);
        }
        dropped
    }

    /// Someone draws `thumb` again, so it no longer counts against the budget.
    fn take(&mut self, thumb: &Thumb) {
        if let Some((at, image)) = self.images.remove(thumb) {
            self.order.remove(&at);
            self.bytes -= bytes_of(&image);
        }
    }
}

/// A pane holds `thumb` until it calls `release`.
pub fn hold(thumb: &Thumb, cx: &mut App) {
    let holders = cx.default_global::<Holders>();
    holders.kept.take(thumb);
    if let Ok(mut held) = holders.held.lock() {
        *held.entry(thumb.clone()).or_default() += 1;
    }
}

/// The thumbnail if it's ready; until then, `None`, and the view draws again
/// when it lands.
pub fn get(thumb: &Thumb, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
    window.use_asset::<Thumbnail>(thumb, cx).flatten()
}

/// A pane lets go of a thumbnail. Once none hold it, the GPU's copy is given
/// back, as drawing it again uploads it in that frame, and the decoded one is
/// kept while it fits. One still queued never decodes, and one decoding is
/// dropped when it finishes. `window` is the window being drawn, which `App`
/// can't reach from inside its update.
pub fn release(
    thumb: &Thumb,
    image: Option<Arc<RenderImage>>,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    let holders = cx.default_global::<Holders>();
    if let Ok(mut held) = holders.held.lock()
        && let Some(count) = held.get_mut(thumb)
    {
        *count -= 1;
        if *count > 0 {
            return;
        }
        held.remove(thumb);
    }
    let Some(image) = image else {
        cx.remove_asset::<Thumbnail>(thumb);
        return;
    };
    let dropped = holders.kept.keep(thumb.clone(), image.clone(), KEPT_BYTES);
    for old in &dropped {
        cx.remove_asset::<Thumbnail>(old);
    }
    cx.drop_image(image, window);
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
    fn kept_thumbnails_drop_oldest_first_past_the_budget() {
        let thumb = |name: &str| Thumb {
            path: name.into(),
            side: 2,
            modified: 0,
        };
        // Sixteen bytes each, so three fit in 48.
        let image = || {
            Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::new(2, 2),
            )]))
        };
        let mut kept = Kept::default();
        for name in ["a", "b", "c"] {
            assert!(kept.keep(thumb(name), image(), 48).is_empty());
        }
        // Drawing "a" again takes it out, so it can't be what's dropped.
        kept.take(&thumb("a"));
        assert_eq!(kept.bytes, 32);
        assert!(kept.keep(thumb("d"), image(), 48).is_empty());
        assert_eq!(kept.keep(thumb("e"), image(), 48), [thumb("b")]);
        assert_eq!(kept.bytes, 48);
        // One too big for the budget goes at once, along with the rest.
        assert_eq!(kept.keep(thumb("f"), image(), 0).len(), 4);
        assert_eq!(kept.bytes, 0);
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
