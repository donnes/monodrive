//! Video and audio through `AVPlayer`. A player layer composites without any GPUI
//! repaint, keeps HDR and costs nothing while paused, and it takes no presses,
//! so GPUI keeps the keyboard. GPUI draws the controls in a strip under it.

use crate::native_view::Host;
use gpui::{Bounds, Pixels, Window};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_av_foundation::{AVLayerVideoGravityResizeAspect, AVPlayer, AVPlayerLayer};
use objc2_core_media::CMTime;
use objc2_foundation::{NSRect, NSString, NSURL};
use std::path::Path;

pub struct Player {
    player: Retained<AVPlayer>,
    /// `None` for audio, which has nothing to draw.
    host: Option<(Host<NSView>, Retained<AVPlayerLayer>)>,
}

impl Player {
    /// Paused at the start. `None` off the main thread, as in tests.
    pub fn new(path: &Path, video: bool, window: &Window) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        // SAFETY: on the main thread, with a file URL.
        let player = unsafe { AVPlayer::playerWithURL(&url, mtm) };
        let host = if video {
            // SAFETY: a new layer for this player, on a new view that hosts it.
            let (view, layer) = unsafe {
                let layer = AVPlayerLayer::playerLayerWithPlayer(Some(&player));
                if let Some(gravity) = AVLayerVideoGravityResizeAspect {
                    layer.setVideoGravity(gravity);
                }
                let view = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
                view.setLayer(Some(&layer));
                view.setWantsLayer(true);
                (view, layer)
            };
            Some((Host::new(view, false, window)?, layer))
        } else {
            None
        };
        Some(Self { player, host })
    }

    pub fn place(&self, bounds: Bounds<Pixels>, window: &Window) {
        if let Some((host, layer)) = &self.host {
            host.place(bounds, window);
            // A view doesn't size a layer it hosts.
            layer.setFrame(host.view.bounds());
        }
    }

    pub fn hide(&self) {
        if let Some((host, _)) = &self.host {
            host.hide();
        }
    }

    /// Whether a video's first frame is ready, when the poster can go. Audio
    /// has nothing to replace its artwork with.
    pub fn frame_ready(&self) -> bool {
        // SAFETY: a plain property, on the main thread that made it.
        self.host
            .as_ref()
            .is_some_and(|(_, layer)| unsafe { layer.isReadyForDisplay() })
    }

    pub fn is_video(&self) -> bool {
        self.host.is_some()
    }

    pub fn playing(&self) -> bool {
        // SAFETY: a plain property, on the main thread that made it.
        unsafe { self.player.rate() > 0. }
    }

    /// Play or pause. At the end, playing starts over.
    pub fn toggle(&self) {
        // SAFETY: plain calls on the main thread that made it.
        unsafe {
            if self.playing() {
                self.player.pause();
            } else {
                if let Some(total) = self.duration()
                    && self.position() >= total
                {
                    self.seek(0.);
                }
                self.player.play();
            }
        }
    }

    /// Seconds in.
    pub fn position(&self) -> f64 {
        // SAFETY: a plain property, on the main thread that made it.
        let seconds = unsafe { self.player.currentTime().seconds() };
        if seconds.is_finite() { seconds } else { 0. }
    }

    /// Seconds long, once the file has loaded.
    pub fn duration(&self) -> Option<f64> {
        // SAFETY: plain properties, on the main thread that made it.
        let seconds = unsafe { self.player.currentItem()?.duration().seconds() };
        (seconds.is_finite() && seconds > 0.).then_some(seconds)
    }

    pub fn seek(&self, seconds: f64) {
        // SAFETY: a plain call on the main thread that made it.
        unsafe { self.player.seekToTime(CMTime::with_seconds(seconds, 600)) };
    }

    pub fn muted(&self) -> bool {
        // SAFETY: a plain property, on the main thread that made it.
        unsafe { self.player.isMuted() }
    }

    pub fn toggle_muted(&self) {
        // SAFETY: a plain property, on the main thread that made it.
        unsafe { self.player.setMuted(!self.player.isMuted()) };
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: a plain call on the main thread that made it. Sound would
        // otherwise go on until the last reference goes.
        unsafe { self.player.pause() };
    }
}

/// `1:05`, or `1:02:05` past an hour.
pub fn clock(seconds: f64) -> String {
    let whole = seconds.max(0.) as u64;
    let (hours, minutes, seconds) = (whole / 3600, whole / 60 % 60, whole % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_reads_as_a_player_shows_it() {
        assert_eq!(clock(0.), "0:00");
        assert_eq!(clock(65.4), "1:05");
        assert_eq!(clock(3725.), "1:02:05");
        assert_eq!(clock(-1.), "0:00");
    }
}
