//! Hands files to Finder and terminals through `NSWorkspace`, which needs no
//! automation permission, unlike scripting either app with Apple events.

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel, NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_uniform_type_identifiers::{
    UTType, UTTypeApplicationBundle, UTTypeDirectory, UTTypePackage,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::preferences::Terminal;

fn urls(paths: &[PathBuf]) -> Retained<NSArray<NSURL>> {
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|path| NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy())))
        .collect();
    NSArray::from_retained_slice(&urls)
}

/// Shows `paths` selected in Finder.
pub fn reveal(paths: &[PathBuf]) {
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls(paths));
}

/// Asks for an app in an open panel on /Applications, without blocking, and
/// calls `done` with the choice. Call it on the main thread.
pub fn choose_app(done: Box<dyn FnOnce(Option<PathBuf>)>) {
    let Some(main) = MainThreadMarker::new() else {
        return done(None);
    };
    let panel = NSOpenPanel::openPanel(main);
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(
        "/Applications",
    ))));
    // SAFETY: a constant of UniformTypeIdentifiers, which links with AppKit.
    let apps = NSArray::from_slice(&[unsafe { UTTypeApplicationBundle }]);
    panel.setAllowedContentTypes(&apps);
    let chosen = panel.clone();
    let done = RefCell::new(Some(done));
    let finished = RcBlock::new(move |response| {
        let app = (response == NSModalResponseOK)
            .then(|| chosen.URL())
            .flatten()
            .and_then(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()));
        if let Some(done) = done.borrow_mut().take() {
            done(app);
        }
    });
    panel.beginWithCompletionHandler(&finished);
}

/// Opens a window of the terminal with this bundle identifier at each of
/// `folders`. Only folders: a terminal runs a file it's given as a script.
pub fn open_in_terminal(folders: &[PathBuf], terminal: &Terminal) -> Result<(), String> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace
        .URLForApplicationWithBundleIdentifier(&NSString::from_str(terminal.bundle_id()))
        .ok_or_else(|| format!("{} isn't installed.", terminal.name()))?;
    workspace.openURLs_withApplicationAtURL_configuration_completionHandler(
        &urls(folders),
        &app,
        &NSWorkspaceOpenConfiguration::configuration(),
        None,
    );
    Ok(())
}

/// Whether `path` names an app or another package, which Finder opens
/// rather than goes into. Decided by its extension, so it reads no disk.
pub fn is_package(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    thread_local! {
        /// The list asks for each folder row it draws.
        static KNOWN: RefCell<HashMap<String, bool>> = RefCell::default();
    }
    KNOWN.with_borrow_mut(|known| {
        if let Some(is) = known.get(extension) {
            return *is;
        }
        // SAFETY: constants of UniformTypeIdentifiers, which links with AppKit.
        let (folder, package) = unsafe { (UTTypeDirectory, UTTypePackage) };
        // As a folder: by default an extension names a file type, never a package.
        let is = UTType::typeWithFilenameExtension_conformingToType(
            &NSString::from_str(extension),
            folder,
        )
        .is_some_and(|kind| kind.conformsToType(package));
        known.insert(extension.to_owned(), is);
        is
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_and_document_packages_are_packages_and_folders_are_not() {
        assert!(is_package(Path::new("/Applications/Safari.app")));
        assert!(is_package(Path::new("/x/Plug.bundle")));
        assert!(!is_package(Path::new("/x/docs")));
        assert!(!is_package(Path::new("/x/release.v2")));
        assert!(!is_package(Path::new("/x/notes.md")));
    }
}
