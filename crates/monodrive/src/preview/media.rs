//! Video and audio through `AVPlayer`. A player layer composites without any GPUI
//! repaint, keeps HDR and costs nothing while paused, and it takes no presses,
//! so GPUI keeps the keyboard. GPUI draws the controls in a strip under it.

use crate::native_view::Host;
use gpui::{Bounds, Pixels, Window};
use monodrive_core::edits::{self, Clip};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSView;
use objc2_av_foundation::{AVLayerVideoGravityResizeAspect, AVPlayer, AVPlayerLayer};
use objc2_core_media::{CMTime, kCMTimeInvalid};
use objc2_foundation::{NSRect, NSString, NSURL};
use std::cell::{Cell, RefCell};
use std::path::Path;

pub struct Player {
    player: Retained<AVPlayer>,
    /// `None` for audio, which has nothing to draw.
    host: Option<(Host<NSView>, Retained<AVPlayerLayer>)>,
    /// The crop and turns it shows, as last given to `reframe`.
    framed: RefCell<Vec<Clip>>,
    /// Muted with the strip's button, which an edit's Remove Sound overrides.
    muted: Cell<bool>,
    /// Silent because the edit removes the sound.
    silenced: Cell<bool>,
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
        Some(Self {
            player,
            host,
            framed: RefCell::new(Vec::new()),
            muted: Cell::new(false),
            silenced: Cell::new(false),
        })
    }

    pub fn place(&self, bounds: Bounds<Pixels>, window: &Window) {
        if let Some((host, layer)) = &self.host {
            host.place(bounds, window);
            // A view doesn't size a layer it hosts.
            layer.setFrame(host.view.bounds());
        }
    }

    /// Shows the video cropped and turned as `clips` say, as Save will write
    /// it. Does nothing when they haven't changed.
    pub fn reframe(&self, clips: &[Clip]) {
        if self.host.is_none() || *self.framed.borrow() == clips {
            return;
        }
        *self.framed.borrow_mut() = clips.to_vec();
        // SAFETY: plain properties, on the main thread that made it. The item's
        // tracks are loaded by the time it plays, so these reads don't wait.
        unsafe {
            let Some(item) = self.player.currentItem() else {
                return;
            };
            // A crop that no longer fits shows the whole frame, and Save says
            // what's wrong with it.
            let framing = edits::framing(&item.asset(), clips).ok().flatten();
            item.setVideoComposition(framing.as_deref());
        }
        // A paused player shows the new framing once it seeks.
        self.seek(self.position());
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

    /// Play or pause, within `range` when there is one, stopping at its end.
    /// Playing from its end or from outside it starts at its start.
    pub fn toggle(&self, range: Option<(f64, f64)>) {
        if self.playing() {
            // SAFETY: a plain call on the main thread that made it.
            unsafe { self.player.pause() };
            return;
        }
        let Some((from, to)) = range.or_else(|| Some((0., self.duration()?))) else {
            return;
        };
        let at = self.position();
        // AVPlayer stops a hair short of an end it's given.
        if at < from || at >= to - 0.05 {
            self.seek(from);
        }
        self.set_end(range.map(|(_, to)| to));
        // SAFETY: a plain call on the main thread that made it.
        unsafe { self.player.play() };
    }

    /// Where playing stops: `None` for the end of the file.
    pub fn set_end(&self, end: Option<f64>) {
        // SAFETY: plain properties, on the main thread that made it.
        unsafe {
            if let Some(item) = self.player.currentItem() {
                item.setForwardPlaybackEndTime(
                    end.map_or(kCMTimeInvalid, |end| CMTime::with_seconds(end, 600)),
                );
            }
        }
    }

    pub fn pause(&self) {
        // SAFETY: a plain call on the main thread that made it.
        unsafe { self.player.pause() };
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
        // Exactly there: the default tolerance snaps to a keyframe, which can be
        // the start of the file for the whole of a short clip.
        unsafe {
            let exact = CMTime::with_seconds(0., 600);
            self.player.seekToTime_toleranceBefore_toleranceAfter(
                CMTime::with_seconds(seconds, 600),
                exact,
                exact,
            );
        }
    }

    /// Whether the strip's button muted it. The edit's silence is the
    /// caller's to add, as it reaches the player only when it's painted.
    pub fn muted(&self) -> bool {
        self.muted.get()
    }

    pub fn toggle_muted(&self) {
        self.muted.set(!self.muted.get());
        self.apply_mute();
    }

    /// Plays without sound while `silenced`, as Save will write it.
    pub fn silence(&self, silenced: bool) {
        if self.silenced.replace(silenced) != silenced {
            self.apply_mute();
        }
    }

    fn apply_mute(&self) {
        // SAFETY: a plain property, on the main thread that made it.
        unsafe {
            self.player
                .setMuted(self.muted.get() || self.silenced.get())
        };
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: a plain call on the main thread that made it. Sound would
        // otherwise go on until the last reference goes.
        unsafe { self.player.pause() };
    }
}

/// `1:05.4`, to a tenth of a second, for trimming.
pub fn clock_tenths(seconds: f64) -> String {
    let tenths = (seconds.max(0.) * 10.).round();
    format!("{}.{}", clock(tenths / 10.), tenths as u64 % 10)
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
        assert_eq!(clock_tenths(65.44), "1:05.4");
        // Rounding up carries into the seconds.
        assert_eq!(clock_tenths(59.96), "1:00.0");
    }
}
