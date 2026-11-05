//! Taps on the trackpad and Finder's sounds. A Force Touch trackpad taps only
//! under a finger, so haptics go with what the pointer does: a drag that opens
//! a folder, flips between move and copy, finds a slot, or lands. Sounds play
//! as macOS's own interface sounds, at the alert volume and only while "Play
//! user interface sound effects" is on.
//!
//! Only the app installs `Feedback`, so tests, snapshots and benchmarks, which
//! draw the same views without it, stay still and silent.

use gpui::{App, Global};
use objc2_app_kit::{
    NSHapticFeedbackManager, NSHapticFeedbackPattern, NSHapticFeedbackPerformanceTime,
    NSHapticFeedbackPerformer,
};
use objc2_core_foundation::CFURL;
use std::cell::OnceCell;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Haptic {
    /// Something happened: a drop landed.
    Generic,
    /// Something snapped into place: an insertion line found a new slot.
    Alignment,
    /// Something went a step deeper or changed kind: a folder opened under a
    /// drag, or ⌥ turned a move into a copy.
    LevelChange,
}

#[derive(Clone, Copy)]
pub enum Sound {
    MoveToTrash,
    EmptyTrash,
}

impl Sound {
    /// Finder's own files.
    fn path(self) -> &'static str {
        match self {
            Self::MoveToTrash => {
                "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/finder/move to trash.aif"
            }
            Self::EmptyTrash => {
                "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/finder/empty trash.aif"
            }
        }
    }
}

/// The system sound ids, each registered on its first play. Registering reads
/// nothing; the sound server loads the file when it plays.
#[derive(Default)]
pub struct Feedback {
    move_to_trash: OnceCell<Option<u32>>,
    empty_trash: OnceCell<Option<u32>>,
}

impl Global for Feedback {}

pub fn haptic(haptic: Haptic, cx: &App) {
    if !cx.has_global::<Feedback>() {
        return;
    }
    let pattern = match haptic {
        Haptic::Generic => NSHapticFeedbackPattern::Generic,
        Haptic::Alignment => NSHapticFeedbackPattern::Alignment,
        Haptic::LevelChange => NSHapticFeedbackPattern::LevelChange,
    };
    NSHapticFeedbackManager::defaultPerformer()
        .performFeedbackPattern_performanceTime(pattern, NSHapticFeedbackPerformanceTime::Now);
}

pub fn sound(sound: Sound, cx: &App) {
    let Some(feedback) = cx.try_global::<Feedback>() else {
        return;
    };
    let cell = match sound {
        Sound::MoveToTrash => &feedback.move_to_trash,
        Sound::EmptyTrash => &feedback.empty_trash,
    };
    let id = cell.get_or_init(|| {
        let url = CFURL::from_file_path(sound.path())?;
        let mut id = 0;
        // SAFETY: a valid URL and somewhere to write the id.
        let status = unsafe { AudioServicesCreateSystemSoundID(&url, &mut id) };
        (status == 0).then_some(id)
    });
    if let Some(id) = *id {
        // SAFETY: an id registered above. It plays asynchronously, and as an
        // interface sound it stays quiet while the setting is off.
        unsafe { AudioServicesPlaySystemSound(id) };
    }
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioServicesCreateSystemSoundID(url: &CFURL, id: *mut u32) -> i32;
    fn AudioServicesPlaySystemSound(id: u32);
}
