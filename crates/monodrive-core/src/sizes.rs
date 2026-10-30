//! Folder sizes, as Get Info and a size column show them: every file under
//! a folder added up, links counted as themselves and never followed.
//! Measuring walks the whole tree, so `Sizes` runs it on its own thread,
//! stops when asked, and keeps each finished total until `invalidate` says
//! the folder changed.

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FolderSize {
    /// The files' lengths added up, as Finder's Size.
    pub bytes: u64,
    /// The blocks they take up.
    pub on_disk: u64,
    /// Files and links, each hard-linked file once.
    pub files: u64,
    /// Folders inside it, itself not included.
    pub folders: u64,
    /// Folders inside it that couldn't be listed, such as one the user may
    /// not read. Their contents aren't counted.
    pub unreadable: u64,
    /// False when it stopped early: the totals are then what it had counted.
    pub complete: bool,
}

/// Past this many distinct hard-linked files, further ones may count twice.
/// It bounds memory on trees full of hard links, such as old Time Machine
/// backups.
const HARD_LINKS: usize = 100_000;

/// Adds up `folder`, stopping early once `cancelled` is set or `deadline`
/// passes. Fails only when `folder` itself can't be listed, with the path
/// and macOS's reason. A link given as `folder` is followed; links inside
/// it aren't.
pub fn measure(
    folder: &Path,
    cancelled: &AtomicBool,
    deadline: Option<Instant>,
) -> Result<FolderSize, String> {
    let stopped =
        || cancelled.load(Ordering::Relaxed) || deadline.is_some_and(|end| Instant::now() >= end);
    let mut size = FolderSize::default();
    let mut linked = HashSet::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if stopped() {
            return Ok(size);
        }
        let listing = match std::fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(e) if dir == folder => return Err(format!("{}: {e}", folder.display())),
            Err(_) => {
                size.unreadable += 1;
                continue;
            }
        };
        for item in listing {
            if stopped() {
                return Ok(size);
            }
            // Gone since the listing began: nothing to count. DirEntry's
            // metadata doesn't follow links.
            let Some((item, meta)) = item
                .ok()
                .and_then(|item| Some((item.path(), item.metadata().ok()?)))
            else {
                continue;
            };
            if meta.is_dir() {
                size.folders += 1;
                pending.push(item);
                continue;
            }
            if meta.nlink() > 1 {
                let id = (meta.dev(), meta.ino());
                if linked.contains(&id) {
                    continue;
                }
                if linked.len() < HARD_LINKS {
                    linked.insert(id);
                }
            }
            size.files += 1;
            size.bytes += meta.len();
            size.on_disk += meta.blocks() * 512;
        }
    }
    size.complete = true;
    Ok(size)
}

/// How many folders' totals are kept. The oldest go first.
const KEPT: usize = 4096;

/// Folder sizes measured in the background and kept until the folder
/// changes. Paths compare as given, so measure and invalidate with real
/// paths, the form FSEvents reports. Clones share the same totals.
#[derive(Clone, Default)]
pub struct Sizes {
    shared: Arc<Mutex<Shared>>,
}

#[derive(Default)]
struct Shared {
    /// Each total with when it was kept, to drop the oldest.
    known: HashMap<PathBuf, (FolderSize, u64)>,
    /// Measurements underway, and whether the folder changed since each
    /// began, which makes its total stale.
    running: HashMap<u64, (PathBuf, bool)>,
    next: u64,
}

/// A measurement underway. Dropping it cancels it.
pub struct Measuring {
    cancelled: Arc<AtomicBool>,
}

impl Measuring {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

impl Drop for Measuring {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Sizes {
    /// The total kept for `folder`, if it's been measured since it last
    /// changed.
    pub fn get(&self, folder: &Path) -> Option<FolderSize> {
        let shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.known.get(folder).map(|(size, _)| *size)
    }

    /// Measures `folder` on a thread of its own and calls `done` there with
    /// the total, partial if it was cancelled. Returns at once. A complete
    /// total is kept unless the folder changed while it was measured.
    pub fn measure(
        &self,
        folder: PathBuf,
        done: impl FnOnce(Result<FolderSize, String>) + Send + 'static,
    ) -> Measuring {
        let cancelled = Arc::new(AtomicBool::new(false));
        let id = {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            shared.next += 1;
            let id = shared.next;
            shared.running.insert(id, (folder.clone(), false));
            id
        };
        let (shared, stop) = (self.shared.clone(), cancelled.clone());
        std::thread::spawn(move || {
            let result = measure(&folder, &stop, None);
            {
                let mut shared = shared.lock().unwrap_or_else(|e| e.into_inner());
                let stale = shared.running.remove(&id).is_some_and(|(_, stale)| stale);
                if let Ok(size) = &result
                    && size.complete
                    && !stale
                {
                    shared.keep(folder, *size, id);
                }
            }
            done(result);
        });
        Measuring { cancelled }
    }

    /// Forgets the totals a change at `path` makes wrong: its own, every
    /// folder above it, and any below it, which may have moved or gone.
    /// Measurements underway there finish but aren't kept.
    pub fn invalidate(&self, path: &Path) {
        let related = |folder: &Path| path.starts_with(folder) || folder.starts_with(path);
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.known.retain(|folder, _| !related(folder));
        for (folder, stale) in shared.running.values_mut() {
            *stale |= related(folder);
        }
    }
}

impl Shared {
    fn keep(&mut self, folder: PathBuf, size: FolderSize, stamp: u64) {
        if self.known.len() >= KEPT && !self.known.contains_key(&folder) {
            let oldest = self
                .known
                .iter()
                .min_by_key(|(_, (_, kept))| *kept)
                .map(|(folder, _)| folder.clone());
            if let Some(oldest) = oldest {
                self.known.remove(&oldest);
            }
        }
        self.known.insert(folder, (size, stamp));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    #[test]
    fn nested_files_add_up_once_each_without_following_links_or_failing_on_unreadable_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        write(&root.join("a.txt"), 10);
        write(&root.join("sub/b.txt"), 20);
        write(&root.join("sub/deeper/c.txt"), 30);
        std::fs::hard_link(root.join("a.txt"), root.join("sub/a-again.txt")).unwrap();
        // A link to a big folder outside counts as one small link.
        write(&dir.path().join("outside/big.bin"), 10_000);
        std::os::unix::fs::symlink(dir.path().join("outside"), root.join("outside")).unwrap();
        let locked = root.join("locked");
        write(&locked.join("secret.txt"), 5_000);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let size = measure(&root, &AtomicBool::new(false), None);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let size = size.unwrap();
        let link = std::fs::symlink_metadata(root.join("outside"))
            .unwrap()
            .len();
        assert_eq!(size.bytes, 60 + link);
        assert_eq!((size.files, size.folders, size.unreadable), (4, 3, 1));
        assert!(size.on_disk > 0 && size.complete);

        let file = root.join("a.txt");
        let refused = measure(&file, &AtomicBool::new(false), None).unwrap_err();
        assert_eq!(
            refused,
            format!("{}: Not a directory (os error 20)", file.display())
        );
    }

    #[test]
    fn a_cancelled_or_late_measurement_stops_and_says_its_partial() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), 10);
        let cancelled = measure(dir.path(), &AtomicBool::new(true), None).unwrap();
        assert_eq!(cancelled, FolderSize::default());
        let late = measure(dir.path(), &AtomicBool::new(false), Some(Instant::now())).unwrap();
        assert!(!late.complete);
        let done = measure(dir.path(), &AtomicBool::new(false), None).unwrap();
        assert_eq!((done.bytes, done.files, done.complete), (10, 1, true));
    }

    #[test]
    fn a_change_forgets_the_folder_its_ancestors_and_whats_below() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write(&root.join("a/b/c/file.txt"), 10);
        write(&root.join("other/file.txt"), 10);
        let sizes = Sizes::default();
        let measured = |folder: PathBuf| {
            let (sent, got) = std::sync::mpsc::channel();
            let _measuring = sizes.measure(folder, move |size| sent.send(size).unwrap());
            got.recv().unwrap().unwrap()
        };
        for folder in ["", "a", "a/b", "a/b/c", "other"] {
            assert!(measured(root.join(folder)).complete);
        }
        assert_eq!(sizes.get(&root).map(|size| size.bytes), Some(20));

        sizes.invalidate(&root.join("a/b"));
        let kept: Vec<bool> = ["", "a", "a/b", "a/b/c", "other"]
            .iter()
            .map(|folder| sizes.get(&root.join(folder)).is_some())
            .collect();
        assert_eq!(kept, [false, false, false, false, true]);
    }
}
