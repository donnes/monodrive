//! The icon the Dock, the app switcher and Finder show for Monodrive, picked
//! in Settings from `assets/app-icons`.

use gpui::App;
use monodrive_core::updates::Channel;
use objc2::rc::Retained;
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage, NSWorkspace, NSWorkspaceIconCreationOptions};
use objc2_foundation::{NSBundle, NSData, NSString};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppIcon {
    Classic,
    Nightly,
    Daylight,
    Aurora,
    Liftoff,
}

impl AppIcon {
    pub const ALL: [AppIcon; 5] = [
        AppIcon::Classic,
        AppIcon::Nightly,
        AppIcon::Daylight,
        AppIcon::Aurora,
        AppIcon::Liftoff,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AppIcon::Classic => "Classic",
            AppIcon::Nightly => "Nightly",
            AppIcon::Daylight => "Daylight",
            AppIcon::Aurora => "Aurora",
            AppIcon::Liftoff => "Liftoff",
        }
    }

    /// Its picture, for `img`.
    pub fn asset(self) -> &'static str {
        match self {
            AppIcon::Classic => "app-icon/classic",
            AppIcon::Nightly => "app-icon/nightly",
            AppIcon::Daylight => "app-icon/daylight",
            AppIcon::Aurora => "app-icon/aurora",
            AppIcon::Liftoff => "app-icon/liftoff",
        }
    }

    pub fn png(self) -> &'static [u8] {
        match self {
            AppIcon::Classic => include_bytes!("../../../assets/app-icons/classic.png"),
            AppIcon::Nightly => include_bytes!("../../../assets/app-icons/nightly.png"),
            AppIcon::Daylight => include_bytes!("../../../assets/app-icons/daylight.png"),
            AppIcon::Aurora => include_bytes!("../../../assets/app-icons/aurora.png"),
            AppIcon::Liftoff => include_bytes!("../../../assets/app-icons/liftoff.png"),
        }
    }

    /// The icon `scripts/bundle` built this app with, or none for a dev
    /// build's blueprint.
    pub fn bundled() -> Option<Self> {
        if monodrive_core::DEV {
            None
        } else if Channel::default() == Channel::Nightly {
            Some(AppIcon::Nightly)
        } else {
            Some(AppIcon::Classic)
        }
    }

    fn image(self) -> Option<Retained<NSImage>> {
        let data = NSData::with_bytes(self.png());
        NSImage::initWithData(NSImage::alloc(), &data)
    }
}

/// The icon the bundle should carry. Writes run off the UI thread in no set
/// order, so each takes `WRITING` and then writes what's wanted at that moment.
static WANTED: Mutex<Option<AppIcon>> = Mutex::new(None);
static WRITING: Mutex<()> = Mutex::new(());

/// Shows `choice` in the Dock and the app switcher, and writes it onto the
/// app bundle, so the Dock and Finder keep it after Monodrive quits. `None`,
/// or the icon the bundle was built with, puts the bundle's own back. At
/// `launch`, the bundle is written only when an update has replaced it.
pub fn apply(choice: Option<AppIcon>, launch: bool, cx: &App) {
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let custom = choice.filter(|icon| Some(*icon) != AppIcon::bundled());
    let image = custom.and_then(AppIcon::image);
    // SAFETY: nil is how AppKit restores the bundle's icon.
    unsafe { NSApplication::sharedApplication(main).setApplicationIconImage(image.as_deref()) };
    let Some(bundle) = bundle() else {
        return;
    };
    *WANTED.lock().unwrap_or_else(PoisonError::into_inner) = custom;
    cx.background_executor()
        .spawn(async move {
            let _writing = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
            let wanted = *WANTED.lock().unwrap_or_else(PoisonError::into_inner);
            if !launch || wanted.is_some() != has_custom_icon(&bundle) {
                write(wanted, &bundle);
            }
        })
        .detach();
}

/// The `.app` this process runs from; none when run from `target/` bare.
fn bundle() -> Option<PathBuf> {
    let path = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
    (path.extension()? == "app").then_some(path)
}

/// Finder keeps a folder's custom icon in an `Icon\r` file inside it.
fn has_custom_icon(bundle: &Path) -> bool {
    bundle.join("Icon\r").exists()
}

fn write(icon: Option<AppIcon>, bundle: &Path) {
    let image = icon.and_then(AppIcon::image);
    NSWorkspace::sharedWorkspace().setIcon_forFile_options(
        image.as_deref(),
        &NSString::from_str(&bundle.to_string_lossy()),
        NSWorkspaceIconCreationOptions::empty(),
    );
}
