//! Open With: the apps LaunchServices says can open a file, its default
//! one, opening files with a chosen app, and changing the default for a
//! file's type. Only the window offers these; an agent opens files itself.

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSBundle, NSError, NSFileManager, NSString, NSURL};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// As Finder shows it, without `.app`.
    pub name: String,
    pub bundle_id: Option<String>,
    pub path: PathBuf,
}

fn url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn app(url: &NSURL) -> Option<App> {
    let path = PathBuf::from(url.path()?.to_string());
    // Localized, and with `.app` only when Finder shows every extension.
    let shown = NSFileManager::defaultManager()
        .displayNameAtPath(&NSString::from_str(&path.to_string_lossy()))
        .to_string();
    let name = shown.strip_suffix(".app").unwrap_or(&shown).to_string();
    let bundle_id = NSBundle::bundleWithURL(url)
        .and_then(|bundle| bundle.bundleIdentifier())
        .map(|id| id.to_string());
    Some(App {
        name,
        bundle_id,
        path,
    })
}

/// Every app that can open `file`, as LaunchServices ranks them.
pub fn can_open(file: &Path) -> Vec<App> {
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .URLsForApplicationsToOpenURL(&url(file))
            .to_vec()
            .into_iter()
            .filter_map(|url| app(&url))
            .collect()
    })
}

/// The app that opens `file` when it's double-clicked.
pub fn default_for(file: &Path) -> Option<App> {
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .URLForApplicationToOpenURL(&url(file))
            .and_then(|url| app(&url))
    })
}

/// Opens `files` in the app at `app`, launching it if needed. Waits for
/// macOS to say it did, so call it off the UI thread.
pub fn open_with(files: &[PathBuf], app: &Path) -> Result<(), String> {
    let (sent, done) = std::sync::mpsc::channel();
    let finished = RcBlock::new(move |_: *mut NSRunningApplication, error: *mut NSError| {
        // SAFETY: AppKit passes a valid error or null.
        let _ = sent.send(unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()));
    });
    objc2::rc::autoreleasepool(|_| {
        let urls: Vec<Retained<NSURL>> = files.iter().map(|file| url(file)).collect();
        NSWorkspace::sharedWorkspace()
            .openURLs_withApplicationAtURL_configuration_completionHandler(
                &NSArray::from_retained_slice(&urls),
                &url(app),
                &NSWorkspaceOpenConfiguration::configuration(),
                Some(&finished),
            );
    });
    answer(done.recv(), app)
}

/// Makes the app at `app` the default for every file of `like`'s type, as
/// Finder's Change All does. Waits for macOS, which may ask the user to
/// confirm, so call it off the UI thread.
pub fn make_default(app: &Path, like: &Path) -> Result<(), String> {
    let (sent, done) = std::sync::mpsc::channel();
    let finished = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: AppKit passes a valid error or null.
        let _ = sent.send(unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string()));
    });
    objc2::rc::autoreleasepool(|_| {
        NSWorkspace::sharedWorkspace()
            .setDefaultApplicationAtURL_toOpenContentTypeOfFileAtURL_completionHandler(
                &url(app),
                &url(like),
                Some(&finished),
            );
    });
    answer(done.recv(), app)
}

fn answer(
    reply: Result<Option<String>, std::sync::mpsc::RecvError>,
    app: &Path,
) -> Result<(), String> {
    match reply {
        Ok(None) => Ok(()),
        Ok(Some(why)) => Err(format!("{}: {why}", app.display())),
        Err(_) => Err(format!("{}: macOS didn't answer.", app.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_file_has_apps_to_open_it_and_a_default_among_them() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "hello").unwrap();
        let apps = can_open(&file);
        let default = default_for(&file).expect("a default app for .txt");
        assert!(apps.contains(&default), "{default:?} not in {apps:?}");
        assert!(
            !default.name.ends_with(".app") && default.bundle_id.is_some(),
            "{default:?}"
        );
    }
}
