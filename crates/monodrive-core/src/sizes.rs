//! Folder sizes, as Get Info and a size column show them: every file under
//! a folder added up, links counted as themselves and never followed.
//! Measuring walks the whole tree, so `Sizes` runs it on its own thread,
//! stops when asked, and keeps each finished total until `invalidate` says
//! the folder changed. A cloud placeholder counts at its size without
//! being downloaded, and a placeholder folder isn't listed.

use crate::cloud::Availability;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::Metadata;
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
    /// Folders, itself included, that couldn't be listed or had items that
    /// couldn't be read, such as one the user may not read or search. What
    /// couldn't be read isn't counted.
    pub unreadable: u64,
    /// Files and folders among them that are cloud placeholders, counted
    /// without downloading: a file at its size, a folder without its items.
    pub placeholders: u64,
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
    let mut stopped =
        || cancelled.load(Ordering::Relaxed) || deadline.is_some_and(|end| Instant::now() >= end);
    let mut size = FolderSize::default();
    let mut counted = None;
    size.complete = walk(folder, &gated(), &mut stopped, &mut |met| match met {
        Met::Folder { placeholder, .. } => {
            size.folders += 1;
            size.placeholders += u64::from(placeholder);
        }
        Met::Item {
            meta, placeholder, ..
        } => {
            size.files += 1;
            size.placeholders += u64::from(placeholder);
            size.bytes += meta.len();
            size.on_disk += meta.blocks() * 512;
        }
        Met::Unreadable { at, .. } => {
            size.unreadable += u64::from(counted != Some(at));
            counted = Some(at);
        }
    })?;
    Ok(size)
}

/// What `walk` meets, a folder's items all together.
pub(crate) enum Met<'a> {
    /// A folder inside folder `parent`. Folders are numbered from 1 as
    /// they're met; the one walked is 0. A placeholder folder isn't walked.
    Folder {
        parent: usize,
        path: &'a Path,
        placeholder: bool,
    },
    /// An item in folder `at` that isn't a folder. A hard-linked file comes
    /// only the first time.
    Item {
        at: usize,
        name: OsString,
        meta: &'a Metadata,
        /// A cloud placeholder: reading it would download it.
        placeholder: bool,
    },
    /// Folder `at`, or an item in it at `path`, couldn't be read. What
    /// couldn't be read isn't met.
    Unreadable {
        at: usize,
        path: &'a Path,
        why: std::io::Error,
    },
}

/// The folders a walk mustn't open while this process lacks Full Disk
/// Access: fsearch's consent-gated folders in the home folder, and
/// /Volumes. None with it.
pub(crate) fn gated() -> Vec<PathBuf> {
    if fsearch::has_full_disk_access() {
        return Vec::new();
    }
    let Ok(home) = std::env::var("HOME") else {
        return Vec::new();
    };
    // Real paths, as the walk guards them.
    let real = std::fs::canonicalize(&home).ok();
    let home = real.as_deref().and_then(Path::to_str).unwrap_or(&home);
    paths(fsearch::gated(home))
}

fn paths(gated: Vec<Vec<u8>>) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    gated
        .into_iter()
        .map(|path| OsString::from_vec(path).into())
        .collect()
}

/// Whether opening `path` could show a privacy prompt, which blocks until
/// someone answers it. Reads the disk.
pub fn needs_consent(path: &Path) -> bool {
    leads_into(path, &gated())
}

/// Whether `path` is or leads into one of `gated`. It follows links a component
/// at a time, reading each link's own target and stopping at the gate, so
/// nothing inside a gated folder is ever touched. Links that never end count,
/// since where they lead can't be told.
pub fn leads_into(path: &Path, gated: &[PathBuf]) -> bool {
    const MOST_LINKS: usize = 40;
    let mut left: Vec<OsString> = Vec::new();
    let push_front = |left: &mut Vec<OsString>, path: &Path| {
        let parts = path.components().map(|part| part.as_os_str().to_owned());
        left.splice(0..0, parts);
    };
    push_front(&mut left, path);
    let (mut here, mut links) = (PathBuf::from("/"), 0);
    while !left.is_empty() {
        let part = left.remove(0);
        match Path::new(&part).components().next() {
            Some(std::path::Component::RootDir) => here = "/".into(),
            Some(std::path::Component::ParentDir) => {
                here.pop();
            }
            Some(std::path::Component::Normal(name)) => {
                here.push(name);
                if guarded(&here, gated).is_some() {
                    return true;
                }
                let Ok(target) = std::fs::read_link(&here) else {
                    continue;
                };
                links += 1;
                if links > MOST_LINKS {
                    return true;
                }
                here.pop();
                push_front(&mut left, &target);
            }
            _ => {}
        }
    }
    false
}

/// Why a walk didn't open `path`, if it's at or under one of `gated`:
/// without Full Disk Access, opening a consent-gated folder shows a privacy
/// prompt and blocks until it's answered, which no deadline interrupts.
pub(crate) fn guarded(path: &Path, gated: &[PathBuf]) -> Option<std::io::Error> {
    gated.iter().any(|g| path.starts_with(g)).then(|| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "needs Full Disk Access",
        )
    })
}

/// Walks every item under `folder` without following links or opening
/// `gated` or placeholder folders, until `stopped` says to stop. A link given as `folder`
/// is resolved first, so it can't lead into one. True if it met
/// everything. Fails only when `folder` itself can't be listed, or is a
/// placeholder.
pub(crate) fn walk(
    folder: &Path,
    gated: &[PathBuf],
    stopped: &mut dyn FnMut() -> bool,
    met: &mut dyn FnMut(Met),
) -> Result<bool, String> {
    let root = folder
        .canonicalize()
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    if std::fs::symlink_metadata(&root).is_ok_and(|meta| !Availability::of(&meta).is_local()) {
        return Err(crate::cloud::not_downloaded(folder));
    }
    let mut linked = HashSet::new();
    let mut found = 0;
    let mut pending = vec![(root, 0)];
    while let Some((dir, at)) = pending.pop() {
        if stopped() {
            return Ok(false);
        }
        if let Some(why) = guarded(&dir, gated) {
            if at == 0 {
                return Err(format!("{}: {why}", folder.display()));
            }
            met(Met::Unreadable {
                at,
                path: &dir,
                why,
            });
            continue;
        }
        let listing = match std::fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(e) if at == 0 => return Err(format!("{}: {e}", folder.display())),
            Err(why) => {
                met(Met::Unreadable {
                    at,
                    path: &dir,
                    why,
                });
                continue;
            }
        };
        for item in listing {
            if stopped() {
                return Ok(false);
            }
            // DirEntry's metadata doesn't follow links. An item gone since
            // the listing began has nothing to count; any other failure,
            // such as a folder without search permission, hides some.
            let gone = |e: &std::io::Error| e.kind() == std::io::ErrorKind::NotFound;
            let item = match item {
                Ok(item) => item,
                Err(e) if gone(&e) => continue,
                Err(why) => {
                    met(Met::Unreadable {
                        at,
                        path: &dir,
                        why,
                    });
                    continue;
                }
            };
            let meta = match item.metadata() {
                Ok(meta) => meta,
                Err(e) if gone(&e) => continue,
                Err(why) => {
                    met(Met::Unreadable {
                        at,
                        path: &item.path(),
                        why,
                    });
                    continue;
                }
            };
            let placeholder = !Availability::of(&meta).is_local();
            if meta.is_dir() {
                found += 1;
                let path = item.path();
                met(Met::Folder {
                    parent: at,
                    path: &path,
                    placeholder,
                });
                if !placeholder {
                    pending.push((path, found));
                }
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
            met(Met::Item {
                at,
                name: item.file_name(),
                meta: &meta,
                placeholder,
            });
        }
    }
    Ok(true)
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
        // One it can't list, and one it can list but not look inside.
        let (locked, listed) = (root.join("locked"), root.join("listed"));
        write(&locked.join("secret.txt"), 5_000);
        write(&listed.join("hidden.txt"), 123);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(&listed, std::fs::Permissions::from_mode(0o400)).unwrap();

        let size = measure(&root, &AtomicBool::new(false), None);
        for folder in [&locked, &listed] {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let size = size.unwrap();
        let link = std::fs::symlink_metadata(root.join("outside"))
            .unwrap()
            .len();
        assert_eq!(size.bytes, 60 + link);
        assert_eq!((size.files, size.folders, size.unreadable), (4, 4, 2));
        assert!(size.on_disk > 0 && size.complete);

        let file = root.join("a.txt");
        let refused = measure(&file, &AtomicBool::new(false), None).unwrap_err();
        assert_eq!(
            refused,
            format!("{}: Not a directory (os error 20)", file.display())
        );
    }

    #[test]
    fn a_link_that_leads_into_a_gated_folder_counts_and_nothing_in_it_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let gated = paths(fsearch::gated(home.to_str().unwrap()));
        let documents = home.join("Documents");
        write(&documents.join("deep/a.txt"), 1);
        write(&home.join("Documentary/b.txt"), 1);
        let link = |name: &str, to: &Path| {
            let path = home.join(name);
            std::os::unix::fs::symlink(to, &path).unwrap();
            path
        };
        let direct = link("direct", &documents.join("deep"));
        let chained = link("chained", &direct);
        let relative = link("relative", Path::new("Documents/deep"));
        let up = link("up", Path::new("Documentary/../Documents"));
        let parent = link("parent", &home);
        let through = parent.join("Documents/deep");
        let fine = link("fine", &home.join("Documentary"));
        let looping = home.join("looping");
        std::os::unix::fs::symlink(&looping, &looping).unwrap();
        // Locked, so a read of anything inside would fail another way, and
        // what's past the gate isn't asked about: `missing` isn't there.
        let missing = link("missing", &documents.join("no/such"));
        std::fs::set_permissions(&documents, std::fs::Permissions::from_mode(0o000)).unwrap();
        let met: Vec<bool> = [
            &documents,
            &documents.join("deep"),
            &direct,
            &chained,
            &relative,
            &up,
            &through,
            &missing,
            &looping,
        ]
        .into_iter()
        .map(|path| leads_into(path, &gated))
        .collect();
        let clear = [&fine, &home.join("Documentary/b.txt"), &home.join("gone")]
            .map(|path| leads_into(path, &gated));
        std::fs::set_permissions(&documents, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(met, [true; 9]);
        assert_eq!(clear, [false; 3]);
    }

    #[test]
    fn consent_gated_folders_are_reported_without_being_opened() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        let gated = paths(fsearch::gated(home.to_str().unwrap()));
        let (desktop, containers) = (home.join("Desktop"), home.join("Library/Containers"));
        write(&desktop.join("a.txt"), 10);
        write(&containers.join("app/b.txt"), 20);
        write(&home.join("Desktops/c.txt"), 30);
        write(&home.join("Library/Caches/d.txt"), 40);
        let link = home.join("to-desktop");
        std::os::unix::fs::symlink(&desktop, &link).unwrap();
        // Locked too, so opening either would fail another way.
        for folder in [&desktop, &containers] {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o000)).unwrap();
        }
        let (mut bytes, mut unreadable) = (0, Vec::new());
        let complete = walk(&home, &gated, &mut || false, &mut |met| match met {
            Met::Item { meta, .. } => bytes += meta.len(),
            Met::Unreadable { path, why, .. } => {
                unreadable.push(format!("{}: {why}", path.display()));
            }
            Met::Folder { .. } => {}
        });
        let inside = walk(&desktop, &gated, &mut || false, &mut |_| {});
        let linked = walk(&link, &gated, &mut || false, &mut |_| {});
        for folder in [&desktop, &containers] {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let link_size = std::fs::symlink_metadata(&link).unwrap().len();
        assert_eq!((complete, bytes), (Ok(true), 70 + link_size));
        unreadable.sort();
        let needs = |path: &Path| format!("{}: needs Full Disk Access", path.display());
        assert_eq!(unreadable, [needs(&desktop), needs(&containers)]);
        assert_eq!(inside, Err(needs(&desktop)));
        assert_eq!(linked, Err(needs(&link)));
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
