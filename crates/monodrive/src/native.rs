//! What macOS says about files and apps, for the inspector and Open With.
//! Views reach it through `native`, so their tests can make it up and never
//! walk a real folder or launch a real app.

use crate::workspace;
use gpui::{App, Global};
use monodrive_core::apps::{self, App as MacApp};
use monodrive_core::info::{self, Info};
use monodrive_core::sizes::{self, FolderSize, Sizes};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// Called once with a folder's total, on any thread.
pub type SizeDone = Box<dyn FnOnce(Result<FolderSize, String>) + Send>;

pub trait Native: Send + Sync {
    /// Reads the disk, so call it off the UI thread.
    fn info(&self, path: &Path) -> Result<Info, String>;
    /// Whether reading `path` could show a privacy prompt. Reads the disk.
    fn needs_consent(&self, path: &Path) -> bool;
    /// The total kept for this real path, if it hasn't changed since.
    fn known_size(&self, folder: &Path) -> Option<FolderSize>;
    /// Measures this real path in the background. Dropping what it returns
    /// cancels it.
    fn measure(&self, folder: PathBuf, done: SizeDone) -> Box<dyn Send>;
    /// Forgets the totals a change at this real path makes wrong.
    fn invalidate(&self, path: &Path);
    /// Forgets every total, since changes went unheard while nothing asked.
    fn forget_sizes(&self);
    /// Asks LaunchServices, so call it off the UI thread.
    fn can_open(&self, file: &Path) -> Vec<MacApp>;
    /// Asks LaunchServices, so call it off the UI thread.
    fn default_for(&self, file: &Path) -> Option<MacApp>;
    /// Waits for macOS, so call it off the UI thread.
    fn open_with(&self, files: &[PathBuf], app: &Path) -> Result<(), String>;
    /// Waits for macOS and any prompt, so call it off the UI thread.
    fn make_default(&self, app: &Path, like: &Path) -> Result<(), String>;
    /// Asks a person for an app, without blocking. Call it on the UI thread.
    fn choose_app(&self, done: Box<dyn FnOnce(Option<PathBuf>)>);
    /// Whether an app with this bundle identifier is installed. Call it off
    /// the UI thread.
    fn installed(&self, bundle_id: &str) -> bool;
}

/// The real thing.
#[derive(Default)]
pub struct System {
    sizes: Mutex<Sizes>,
}

impl System {
    fn sizes(&self) -> Sizes {
        self.sizes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Native for System {
    fn info(&self, path: &Path) -> Result<Info, String> {
        info::info(path)
    }

    fn needs_consent(&self, path: &Path) -> bool {
        sizes::needs_consent(path)
    }

    fn known_size(&self, folder: &Path) -> Option<FolderSize> {
        self.sizes().get(folder)
    }

    fn measure(&self, folder: PathBuf, done: SizeDone) -> Box<dyn Send> {
        Box::new(self.sizes().measure(folder, done))
    }

    fn invalidate(&self, path: &Path) {
        self.sizes().invalidate(path);
    }

    fn forget_sizes(&self) {
        *self.sizes.lock().unwrap_or_else(PoisonError::into_inner) = Sizes::default();
    }

    fn can_open(&self, file: &Path) -> Vec<MacApp> {
        apps::can_open(file)
    }

    fn default_for(&self, file: &Path) -> Option<MacApp> {
        apps::default_for(file)
    }

    fn open_with(&self, files: &[PathBuf], app: &Path) -> Result<(), String> {
        apps::open_with(files, app)
    }

    fn make_default(&self, app: &Path, like: &Path) -> Result<(), String> {
        apps::make_default(app, like)
    }

    fn choose_app(&self, done: Box<dyn FnOnce(Option<PathBuf>)>) {
        workspace::choose_app(done);
    }

    fn installed(&self, bundle_id: &str) -> bool {
        apps::with_bundle_id(bundle_id).is_some()
    }
}

/// Replaces the real thing, for tests and the snapshots.
pub struct MadeUp(pub Arc<dyn Native>);
impl Global for MadeUp {}

/// What views ask. One real `System` serves every window, so folder totals
/// are shared.
pub fn native(cx: &App) -> Arc<dyn Native> {
    static SYSTEM: OnceLock<Arc<System>> = OnceLock::new();
    match cx.try_global::<MadeUp>() {
        Some(made_up) => made_up.0.clone(),
        None => SYSTEM.get_or_init(Arc::default).clone(),
    }
}

/// The apps Open With offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offered {
    /// A to Z, the default first.
    pub apps: Vec<MacApp>,
    /// Whether the first app is the default for every file.
    pub has_default: bool,
}

/// The apps that can open all of `files`. LaunchServices answers by type and a
/// selection can be thousands of files, so regular files with an extension ask
/// once per extension. A link, a folder or a file with no extension may be any
/// type, so each of those asks for itself. Reads the disk.
pub fn offered(native: &dyn Native, files: &[PathBuf]) -> Offered {
    let mut seen = HashSet::new();
    let of_each: Vec<&PathBuf> = files
        .iter()
        .filter(|file| {
            let regular = std::fs::symlink_metadata(file).is_ok_and(|meta| meta.is_file());
            match file.extension().filter(|_| regular) {
                Some(extension) => seen.insert(extension.to_string_lossy().to_lowercase()),
                None => true,
            }
        })
        .collect();
    let Some((first, rest)) = of_each.split_first() else {
        return Offered::default();
    };
    let mut apps = native.can_open(first);
    let mut default = native.default_for(first);
    for file in rest {
        let also = native.can_open(file);
        apps.retain(|app| also.iter().any(|other| other.path == app.path));
        if native.default_for(file).map(|app| app.path) != default.as_ref().map(|d| d.path.clone())
        {
            default = None;
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps.dedup_by(|a, b| a.path == b.path);
    let default_at = default.and_then(|d| apps.iter().position(|app| app.path == d.path));
    if let Some(at) = default_at {
        apps[..=at].rotate_right(1);
    }
    Offered {
        apps,
        has_default: default_at.is_some(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Apps {
        asked: Mutex<Vec<PathBuf>>,
    }

    fn app(name: &str) -> MacApp {
        MacApp {
            name: name.into(),
            bundle_id: None,
            path: format!("/Applications/{name}.app").into(),
        }
    }

    impl Native for Apps {
        fn info(&self, _: &Path) -> Result<Info, String> {
            Err("unused".into())
        }
        fn needs_consent(&self, _: &Path) -> bool {
            false
        }
        fn known_size(&self, _: &Path) -> Option<FolderSize> {
            None
        }
        fn measure(&self, _: PathBuf, _: SizeDone) -> Box<dyn Send> {
            Box::new(())
        }
        fn invalidate(&self, _: &Path) {}
        fn forget_sizes(&self) {}
        fn can_open(&self, file: &Path) -> Vec<MacApp> {
            self.asked.lock().unwrap().push(file.to_path_buf());
            // Where a link leads, as LaunchServices follows it.
            let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
            match file.extension().and_then(|e| e.to_str()) {
                Some("txt") => vec![app("Zed"), app("TextEdit"), app("Pages")],
                _ => vec![app("Pages"), app("Preview"), app("Zed")],
            }
        }
        fn default_for(&self, file: &Path) -> Option<MacApp> {
            let file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
            match file.extension().and_then(|e| e.to_str()) {
                Some("txt") => Some(app("TextEdit")),
                _ => Some(app("Preview")),
            }
        }
        fn open_with(&self, _: &[PathBuf], _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn make_default(&self, _: &Path, _: &Path) -> Result<(), String> {
            Ok(())
        }
        fn choose_app(&self, _: Box<dyn FnOnce(Option<PathBuf>)>) {}
        fn installed(&self, _: &str) -> bool {
            true
        }
    }

    fn names(offered: &Offered) -> Vec<&str> {
        offered.apps.iter().map(|app| app.name.as_str()).collect()
    }

    #[test]
    fn one_file_offers_its_default_first_then_the_rest_a_to_z() {
        let got = offered(&Apps::default(), &["/x/a.txt".into()]);
        assert_eq!(names(&got), ["TextEdit", "Pages", "Zed"]);
        assert!(got.has_default);
    }

    #[test]
    fn several_files_offer_only_apps_that_open_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, "").unwrap();
            path
        };
        let (a, b, shouting) = (file("a.txt"), file("b.pdf"), file("B.TXT"));
        let got = offered(&Apps::default(), &[a.clone(), b.clone()]);
        assert_eq!(names(&got), ["Pages", "Zed"]);
        // Their defaults differ, so nothing is marked.
        assert!(!got.has_default);
        // Same type, so one question and the one default.
        let apps = Apps::default();
        let got = offered(&apps, &[a.clone(), shouting]);
        assert_eq!(names(&got), ["TextEdit", "Pages", "Zed"]);
        assert!(got.has_default);
        assert_eq!(*apps.asked.lock().unwrap(), std::slice::from_ref(&a));
        // Two links with no extension say nothing of what they lead to.
        let (to_text, to_pdf) = (dir.path().join("one"), dir.path().join("two"));
        std::os::unix::fs::symlink(&a, &to_text).unwrap();
        std::os::unix::fs::symlink(&b, &to_pdf).unwrap();
        let got = offered(&Apps::default(), &[to_text, to_pdf]);
        assert_eq!(names(&got), ["Pages", "Zed"]);
        assert!(!got.has_default);
    }
}
