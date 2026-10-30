//! Hands files to Finder and Terminal through `NSWorkspace`, which needs no
//! automation permission, unlike scripting either app with Apple events.

use objc2::rc::Retained;
use objc2_app_kit::{NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_uniform_type_identifiers::{UTType, UTTypeDirectory, UTTypePackage};
use std::path::{Path, PathBuf};

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

/// Opens a Terminal window at each of `folders`. Only folders: Terminal runs
/// a file it's given as a script.
pub fn open_in_terminal(folders: &[PathBuf]) -> Result<(), String> {
    let workspace = NSWorkspace::sharedWorkspace();
    let terminal = workspace
        .URLForApplicationWithBundleIdentifier(&NSString::from_str("com.apple.Terminal"))
        .ok_or("Terminal isn't installed.")?;
    workspace.openURLs_withApplicationAtURL_configuration_completionHandler(
        &urls(folders),
        &terminal,
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
    // SAFETY: constants of UniformTypeIdentifiers, which links with AppKit.
    let (folder, package) = unsafe { (UTTypeDirectory, UTTypePackage) };
    // As a folder: by default an extension names a file type, never a package.
    UTType::typeWithFilenameExtension_conformingToType(&NSString::from_str(extension), folder)
        .is_some_and(|kind| kind.conformsToType(package))
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
