//! Video edits through `AVAssetExportSession`. Trimming and muting alone use the
//! passthrough preset, with no re-encode, so cuts snap to keyframes. Cropping and
//! rotating re-encode through a video composition. These block until done or
//! cancelled, so call them off the UI thread.

// AVFoundation's newer loading calls are asynchronous, and all of this already
// runs off the UI thread.
#![allow(deprecated)]

use super::{Rect, encode, url};
use block2::RcBlock;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_av_foundation::{
    AVAsset, AVAssetExportPresetHighestQuality, AVAssetExportPresetPassthrough,
    AVAssetExportSession, AVAssetExportSessionStatus, AVAssetImageGenerator, AVAssetTrack,
    AVFileTypeMPEG4, AVFileTypeQuickTimeMovie, AVMediaTypeAudio, AVMediaTypeVideo,
    AVMutableComposition, AVMutableVideoComposition, AVMutableVideoCompositionInstruction,
    AVMutableVideoCompositionLayerInstruction, AVURLAsset, AVVideoCompositionInstructionProtocol,
};
use objc2_core_foundation::{CGAffineTransform, CGSize};
use objc2_core_media::{CMTime, CMTimeRange};
use objc2_foundation::{NSArray, NSError};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

/// One edit. Crop and rotate apply in order, in the video's picture as it plays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Clip {
    /// Seconds from the start.
    Trim {
        start: f64,
        end: f64,
    },
    Crop {
        rect: Rect,
    },
    /// Clockwise.
    Rotate {
        quarter_turns: i32,
    },
    Mute,
}

pub type VideoRecipe = Vec<Clip>;

const IDENTITY: CGAffineTransform = CGAffineTransform {
    a: 1.,
    b: 0.,
    c: 0.,
    d: 1.,
    tx: 0.,
    ty: 0.,
};

/// `first`, then `then`.
fn concat(first: CGAffineTransform, then: CGAffineTransform) -> CGAffineTransform {
    CGAffineTransform {
        a: first.a * then.a + first.b * then.c,
        b: first.a * then.b + first.b * then.d,
        c: first.c * then.a + first.d * then.c,
        d: first.c * then.b + first.d * then.d,
        tx: first.tx * then.a + first.ty * then.c + then.tx,
        ty: first.tx * then.b + first.ty * then.d + then.ty,
    }
}

fn translated(x: f64, y: f64) -> CGAffineTransform {
    CGAffineTransform {
        tx: x,
        ty: y,
        ..IDENTITY
    }
}

/// A track's picture as it plays: the transform that stands it upright with its
/// corner at the origin, and the size that comes to.
fn upright(track: &AVAssetTrack) -> (CGAffineTransform, CGSize) {
    // SAFETY: plain properties of a loaded track.
    let (size, transform) = unsafe { (track.naturalSize(), track.preferredTransform()) };
    let corners = [
        (0., 0.),
        (size.width, 0.),
        (0., size.height),
        (size.width, size.height),
    ]
    .map(|(x, y)| {
        (
            x * transform.a + y * transform.c + transform.tx,
            x * transform.b + y * transform.d + transform.ty,
        )
    });
    let least = |pick: fn(&(f64, f64)) -> f64| corners.iter().map(pick).fold(f64::MAX, f64::min);
    let most = |pick: fn(&(f64, f64)) -> f64| corners.iter().map(pick).fold(f64::MIN, f64::max);
    let (left, top) = (least(|c| c.0), least(|c| c.1));
    let shown = CGSize::new(most(|c| c.0) - left, most(|c| c.1) - top);
    (concat(transform, translated(-left, -top)), shown)
}

/// The size of each turn and crop, ending at the video's final transform.
fn framed(
    start: (CGAffineTransform, CGSize),
    steps: &[Clip],
) -> Result<(CGAffineTransform, CGSize), String> {
    let (mut transform, mut size) = start;
    for step in steps {
        match step {
            Clip::Rotate { quarter_turns } => {
                let turn = match quarter_turns.rem_euclid(4) {
                    1 => Some(CGAffineTransform {
                        a: 0.,
                        b: 1.,
                        c: -1.,
                        d: 0.,
                        tx: size.height,
                        ty: 0.,
                    }),
                    2 => Some(CGAffineTransform {
                        a: -1.,
                        b: 0.,
                        c: 0.,
                        d: -1.,
                        tx: size.width,
                        ty: size.height,
                    }),
                    3 => Some(CGAffineTransform {
                        a: 0.,
                        b: -1.,
                        c: 1.,
                        d: 0.,
                        tx: 0.,
                        ty: size.width,
                    }),
                    _ => None,
                };
                if let Some(turn) = turn {
                    transform = concat(transform, turn);
                    if quarter_turns.rem_euclid(2) == 1 {
                        size = CGSize::new(size.height, size.width);
                    }
                }
            }
            Clip::Crop { rect } => {
                if rect.width < 2. || rect.height < 2. {
                    return Err("A crop needs some width and height.".into());
                }
                if rect.x < 0.
                    || rect.y < 0.
                    || rect.x + rect.width > size.width
                    || rect.y + rect.height > size.height
                {
                    return Err("That crop is outside the video.".into());
                }
                transform = concat(transform, translated(-rect.x, -rect.y));
                // Encoders want even sizes.
                size = CGSize::new(
                    (rect.width / 2.).floor() * 2.,
                    (rect.height / 2.).floor() * 2.,
                );
            }
            Clip::Trim { .. } | Clip::Mute => {}
        }
    }
    Ok((transform, size))
}

fn asset(path: &Path) -> Retained<AVURLAsset> {
    // SAFETY: a file URL.
    unsafe { AVURLAsset::assetWithURL(&url(path)) }
}

fn time(seconds: f64) -> CMTime {
    // SAFETY: plain arithmetic.
    unsafe { CMTime::with_seconds(seconds, 600) }
}

fn video_track(asset: &AVAsset) -> Option<Retained<AVAssetTrack>> {
    // SAFETY: a plain, synchronous read of a local asset.
    unsafe { asset.tracksWithMediaType(AVMediaTypeVideo?).firstObject() }
}

/// The extension an edited video gets: its own when an export can write it.
pub fn video_extension(source: &Path) -> String {
    match source.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ["mov", "mp4", "m4v"].contains(&ext.to_lowercase().as_str()) => {
            ext.to_string()
        }
        _ => "mp4".to_string(),
    }
}

/// A video's size as it plays, in pixels.
pub fn size(path: &Path) -> Result<(f64, f64), String> {
    let asset = asset(path);
    let track = video_track(&asset)
        .ok_or_else(|| format!("{} has no video macOS can read.", path.display()))?;
    let (_, shown) = upright(&track);
    Ok((shown.width, shown.height))
}

/// Applies `recipe` to the video at `source`, writing `into`, whose extension
/// picks `.mp4` or `.mov`. A crop or turn re-encodes.
pub fn export(
    source: &Path,
    recipe: &[Clip],
    into: &Path,
    cancelled: &(impl Fn() -> bool + ?Sized),
) -> Result<(), String> {
    crate::cloud::refuse_placeholder(source)?;
    let asset = asset(source);
    let track = video_track(&asset)
        .ok_or_else(|| format!("{} has no video macOS can read.", source.display()))?;
    let muted = recipe.iter().any(|step| matches!(step, Clip::Mute));
    let reframed = recipe
        .iter()
        .any(|step| matches!(step, Clip::Crop { .. } | Clip::Rotate { .. }));
    // SAFETY: plain property of a loaded asset.
    let length = unsafe { asset.duration() };
    let trims: Vec<(f64, f64)> = recipe
        .iter()
        .filter_map(|step| match step {
            Clip::Trim { start, end } => Some((*start, *end)),
            _ => None,
        })
        .collect();
    let trim = match trims.as_slice() {
        [] => None,
        [(start, end)] if end > start && *start >= 0. => Some((*start, *end)),
        [_] => return Err("A trim's end must come after its start.".into()),
        _ => return Err("A video can only be trimmed once at a time.".into()),
    };
    let (transform, render) = framed(upright(&track), recipe)?;

    // SAFETY: valid objects throughout, on this thread until the export takes them.
    unsafe {
        let composition = AVMutableComposition::composition();
        let whole = CMTimeRange {
            start: time(0.),
            duration: length,
        };
        let copy = |media, from: &AVAssetTrack| -> Result<(), String> {
            let added = composition
                .addMutableTrackWithMediaType_preferredTrackID(media, 0)
                .ok_or("couldn't build the video")?;
            added
                .insertTimeRange_ofTrack_atTime_error(whole, from, time(0.))
                .map_err(|e: Retained<NSError>| e.localizedDescription().to_string())?;
            added.setPreferredTransform(from.preferredTransform());
            Ok(())
        };
        let video = AVMediaTypeVideo.ok_or("no video type")?;
        copy(video, &track)?;
        if !muted && let Some(audio) = AVMediaTypeAudio {
            for track in asset.tracksWithMediaType(audio).iter() {
                copy(audio, &track)?;
            }
        }
        let preset = if reframed {
            AVAssetExportPresetHighestQuality
        } else {
            AVAssetExportPresetPassthrough
        };
        let session = AVAssetExportSession::initWithAsset_presetName(
            AVAssetExportSession::alloc(),
            &composition,
            preset,
        )
        .ok_or("macOS can't export this video that way.")?;
        let kind = match into
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("mov") => AVFileTypeQuickTimeMovie,
            _ => AVFileTypeMPEG4,
        };
        session.setOutputFileType(kind);
        session.setOutputURL(Some(&url(into)));
        if let Some((start, end)) = trim {
            session.setTimeRange(CMTimeRange {
                start: time(start),
                duration: time(end - start),
            });
        }
        if reframed {
            let tracks = composition.tracksWithMediaType(video);
            let placed = tracks.firstObject().ok_or("couldn't build the video")?;
            let layer = AVMutableVideoCompositionLayerInstruction::videoCompositionLayerInstructionWithAssetTrack(&placed);
            layer.setTransform_atTime(transform, time(0.));
            let instruction = AVMutableVideoCompositionInstruction::videoCompositionInstruction();
            instruction.setTimeRange(whole);
            instruction.setLayerInstructions(&NSArray::from_retained_slice(&[
                Retained::into_super(layer),
            ]));
            let framing = AVMutableVideoComposition::videoComposition();
            framing.setRenderSize(render);
            framing.setFrameDuration(CMTime::new(1, 30));
            let steps: Retained<ProtocolObject<dyn AVVideoCompositionInstructionProtocol>> =
                ProtocolObject::from_retained(instruction);
            framing.setInstructions(&NSArray::from_retained_slice(&[steps]));
            session.setVideoComposition(Some(&framing));
        }
        let (send, receive) = mpsc::channel();
        let finished = RcBlock::new(move || {
            let _ = send.send(());
        });
        session.exportAsynchronouslyWithCompletionHandler(&finished);
        loop {
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if cancelled() {
                        session.cancelExport();
                    }
                }
            }
        }
        match session.status() {
            AVAssetExportSessionStatus::Completed => Ok(()),
            AVAssetExportSessionStatus::Cancelled => Err("cancelled".into()),
            _ => Err(session
                .error()
                .map_or("The export failed.".to_string(), |e| {
                    e.localizedDescription().to_string()
                })),
        }
    }
}

/// The frame at `seconds`, as a picture at `into`.
pub fn frame(source: &Path, seconds: f64, into: &Path) -> Result<(), String> {
    encode(&*grab(source, seconds)?, into, None)
}

/// The frame at `seconds`, in memory, for cropping on.
pub fn frame_bitmap(source: &Path, seconds: f64) -> Result<crate::thumbnails::Bitmap, String> {
    crate::thumbnails::bitmap(&*grab(source, seconds)?)
}

/// `count` frames spread evenly through the video, each at most `height` pixels
/// tall, for the trim bar's filmstrip. Each is the keyframe nearest its place,
/// which is fast and close enough at that size.
pub fn filmstrip(
    source: &Path,
    count: usize,
    height: f64,
) -> Result<Vec<crate::thumbnails::Bitmap>, String> {
    crate::cloud::refuse_placeholder(source)?;
    let asset = asset(source);
    // SAFETY: valid objects, on this thread only.
    unsafe {
        let length = asset.duration().seconds();
        if !(length.is_finite() && length > 0.) {
            return Err(format!(
                "{} has no length macOS can read.",
                source.display()
            ));
        }
        let slot = length / count as f64;
        let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset);
        generator.setAppliesPreferredTrackTransform(true);
        // Wide enough for any video's shape, so the height decides.
        generator.setMaximumSize(CGSize::new(height * 4., height));
        generator.setRequestedTimeToleranceBefore(time(slot / 2.));
        generator.setRequestedTimeToleranceAfter(time(slot / 2.));
        (0..count)
            .map(|at| {
                let image = generator
                    .copyCGImageAtTime_actualTime_error(
                        time(slot * (at as f64 + 0.5)),
                        std::ptr::null_mut(),
                    )
                    .map_err(|e| e.localizedDescription().to_string())?;
                crate::thumbnails::bitmap(&image)
            })
            .collect()
    }
}

fn grab(source: &Path, seconds: f64) -> Result<Retained<objc2_core_graphics::CGImage>, String> {
    crate::cloud::refuse_placeholder(source)?;
    let asset = asset(source);
    // SAFETY: valid objects, on this thread only.
    let image = unsafe {
        let generator = AVAssetImageGenerator::assetImageGeneratorWithAsset(&asset);
        generator.setAppliesPreferredTrackTransform(true);
        generator.setRequestedTimeToleranceBefore(time(0.));
        generator.setRequestedTimeToleranceAfter(time(0.));
        generator
            .copyCGImageAtTime_actualTime_error(time(seconds), std::ptr::null_mut())
            .map_err(|e| e.localizedDescription().to_string())?
    };
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn a_filmstrip_has_the_frames_asked_for_standing_upright() {
        // Stored 64 by 36, flagged to play turned a quarter.
        let frames = filmstrip(&fixture("rotated.mp4"), 3, 32.).unwrap();
        assert_eq!(frames.len(), 3);
        for frame in frames {
            assert!(frame.height <= 32);
            assert!(
                frame.height > frame.width,
                "{}×{}",
                frame.width,
                frame.height
            );
        }
    }

    #[test]
    fn a_rotated_video_crops_the_region_as_it_plays() {
        let dir = tempfile::tempdir().unwrap();
        // Stored 64 by 36, flagged to play turned a quarter, so 36 by 64.
        let source = fixture("rotated.mp4");
        assert_eq!(size(&source).unwrap(), (36., 64.));
        let out = dir.path().join("out.mp4");
        export(
            &source,
            &[Clip::Crop {
                rect: Rect {
                    x: 0.,
                    y: 0.,
                    width: 30.,
                    height: 20.,
                },
            }],
            &out,
            &|| false,
        )
        .unwrap();
        assert_eq!(size(&out).unwrap(), (30., 20.));
        let outside = export(
            &source,
            &[Clip::Crop {
                rect: Rect {
                    x: 30.,
                    y: 0.,
                    width: 30.,
                    height: 20.,
                },
            }],
            &dir.path().join("no.mp4"),
            &|| false,
        );
        assert!(outside.unwrap_err().contains("outside"));
    }

    #[test]
    fn a_quarter_turn_swaps_the_sides_and_a_trim_shortens_without_reencoding() {
        let dir = tempfile::tempdir().unwrap();
        let source = fixture("rotated.mp4");
        let turned = dir.path().join("turned.mov");
        export(
            &source,
            &[Clip::Rotate { quarter_turns: 1 }],
            &turned,
            &|| false,
        )
        .unwrap();
        assert_eq!(size(&turned).unwrap(), (64., 36.));
        let trimmed = dir.path().join("trimmed.mp4");
        export(
            &source,
            &[
                Clip::Trim {
                    start: 0.,
                    end: 0.4,
                },
                Clip::Mute,
            ],
            &trimmed,
            &|| false,
        )
        .unwrap();
        assert_eq!(size(&trimmed).unwrap(), (36., 64.));
        // SAFETY: a plain property of a local asset.
        let length = unsafe { asset(&trimmed).duration().seconds() };
        assert!(length > 0. && length < 0.9, "{length}");
        assert!(
            export(
                &source,
                &[Clip::Trim {
                    start: 1.,
                    end: 0.5
                }],
                &dir.path().join("bad.mp4"),
                &|| false
            )
            .is_err()
        );
        // Two trims would otherwise drop one without saying so.
        let twice = [
            Clip::Trim {
                start: 0.,
                end: 0.4,
            },
            Clip::Trim {
                start: 0.1,
                end: 0.2,
            },
        ];
        assert!(export(&source, &twice, &dir.path().join("twice.mp4"), &|| false).is_err());
    }

    #[test]
    fn a_frame_is_a_picture_the_right_way_up() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("frame.png");
        frame(&fixture("rotated.mp4"), 0.2, &out).unwrap();
        let picture = image::ImageReader::open(&out)
            .unwrap()
            .with_guessed_format()
            .unwrap();
        let picture = picture.decode().map(|p| (p.width(), p.height())).unwrap();
        assert_eq!(picture, (36, 64));
    }

    #[test]
    fn a_format_macos_cannot_open_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mp4");
        assert!(export(&fixture("clip.mkv"), &[Clip::Mute], &out, &|| false).is_err());
    }
}
