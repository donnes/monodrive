//! The system calls behind each operation: macOS's own Trash, renames that
//! never replace, and copies that clone where APFS can.

use crate::cloud::Availability;
use crate::entries::{Kind, each_item};
use objc2_foundation::{NSFileManager, NSString, NSURL};
use serde::{Deserialize, Serialize};
use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Which item a path holds. Renames, including into the Trash, keep it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileId {
    dev: u64,
    ino: u64,
}

impl FileId {
    /// The item `meta` describes.
    pub(crate) fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
        }
    }
}

/// What an item was like, to tell whether it changed since. A folder's
/// covers everything inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    entries: u64,
    size: u64,
    /// Every item's inode, size and modification time, mixed so that one
    /// edit changes it whatever the other times are.
    digest: u64,
}

/// The item at `path` itself, not what a link there points to.
pub fn id(path: &Path) -> Option<FileId> {
    lookup(path).ok()
}

/// Like `id`, keeping the OS's reason when there's no item.
pub fn lookup(path: &Path) -> io::Result<FileId> {
    std::fs::symlink_metadata(path).map(|meta| FileId::of(&meta))
}

/// The item at `path` and its stamp. A folder is walked whole, without
/// following links. A subfolder that can't be read counts as empty, and so
/// does a cloud placeholder folder, which listing would download.
pub fn stamp(path: &Path) -> io::Result<(FileId, Stamp)> {
    let meta = std::fs::symlink_metadata(path)?;
    let id = FileId::of(&meta);
    let mut stamp = Stamp {
        entries: 0,
        size: meta.size(),
        digest: mix(&meta),
    };
    let mut folders = Vec::new();
    if listable(&meta) {
        folders.push(path.to_path_buf());
    }
    while let Some(folder) = folders.pop() {
        let _unreadable_counts_as_empty = each_item(&folder, |item| {
            let Ok(item) = item else { return Ok(()) };
            let inside = folder.join(OsStr::from_bytes(item.name));
            // Folders report no size in bulk, and the journal keeps stamps
            // made with lstat's, so a folder's comes from lstat. Folders are
            // about one item in ten.
            let size = if item.kind == Kind::Dir {
                match std::fs::symlink_metadata(&inside) {
                    Ok(meta) => meta.size(),
                    Err(_) => return Ok(()),
                }
            } else {
                item.size
            };
            stamp.entries += 1;
            stamp.size += size;
            let (seconds, nanoseconds) = item.modified;
            stamp.digest = stamp.digest.wrapping_add(mix_parts(
                item.inode,
                size,
                seconds as u64,
                nanoseconds as u64,
            ));
            if item.kind == Kind::Dir && Availability::from_flags(item.flags).is_local() {
                folders.push(inside);
            }
            Ok(())
        });
    }
    Ok((id, stamp))
}

/// A file's identity and stamp, read from what `file` has open, as
/// `stamp` reads them from a path.
pub fn stamp_open(file: &std::fs::File) -> io::Result<(FileId, Stamp)> {
    let meta = file.metadata()?;
    if meta.is_dir() {
        return Err(io::Error::other("a folder, not a file"));
    }
    let stamp = Stamp {
        entries: 0,
        size: meta.size(),
        digest: mix(&meta),
    };
    Ok((FileId::of(&meta), stamp))
}

/// One item's share of a stamp's digest. Summed, so the order a folder lists
/// in doesn't matter; std's hasher isn't stable across releases, and the
/// journal keeps stamps.
fn mix(meta: &std::fs::Metadata) -> u64 {
    mix_parts(
        meta.ino(),
        meta.size(),
        meta.mtime() as u64,
        meta.mtime_nsec() as u64,
    )
}

fn mix_parts(inode: u64, size: u64, seconds: u64, nanoseconds: u64) -> u64 {
    [inode, size, seconds, nanoseconds]
        .into_iter()
        .fold(0xcbf2_9ce4_8422_2325, |hash, part| {
            (hash ^ part).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

/// Moves `path` to its volume's Trash and returns where it landed. Fails
/// rather than deleting when the volume has no Trash.
pub fn trash(path: &Path) -> Result<PathBuf, String> {
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let mut landed = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut landed))
            .map_err(|e| format!("{}: {}", path.display(), e.localizedDescription()))?;
        landed
            .and_then(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()))
            .ok_or_else(|| format!("{}: macOS didn't say where it went", path.display()))
    })
}

/// Renames `from` to `to`, failing if `to` already exists.
pub fn rename_new(from: &Path, to: &Path) -> io::Result<()> {
    // A change of case only: on a case-insensitive volume `to` is the same
    // item, so there's nothing to replace.
    if id(to).is_some() && id(to) == id(from) {
        return std::fs::rename(from, to);
    }
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: both are NUL-terminated paths that outlive the call.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// How far a copy has got, and whether to stop it.
pub struct Meter<'a> {
    pub copied: &'a AtomicU64,
    pub cancel: &'a AtomicBool,
}

/// `clonefile`'s flags, which libc doesn't define: clone a link rather than
/// what it points to, and copy the top item's ACL.
const CLONE_NOFOLLOW: u32 = 0x0001;
const CLONE_ACL: u32 = 0x0004;

/// Copies `from` to the new path `to`, folders whole, keeping metadata.
///
/// On APFS it clones: a file in one `clonefile` call, and a folder in one
/// call per item directly inside it (see `clone_whole`), where `copyfile`
/// clones file by file and took about ten times as long on a big folder.
/// Where cloning isn't possible, on another volume or one without clones,
/// and on any other failure but a cancel, it copies with `copyfile` instead,
/// cloning each file where it can, so an unreadable file still gets that
/// copy's error. A `meter` can stop either partway, leaving what's at `to`
/// incomplete, and sees the bytes `copyfile` has copied so far.
pub fn copy(from: &Path, to: &Path, meter: Option<&Meter>) -> io::Result<()> {
    match clone_whole(from, to, meter.map(|meter| meter.cancel)) {
        Ok(()) => return Ok(()),
        // `to` may be someone else's, and copyfile would refuse it too. A
        // cancel keeps what's partway for the caller to deal with, as
        // copyfile's does.
        Err(error) if matches!(error.raw_os_error(), Some(libc::EEXIST | libc::ECANCELED)) => {
            return Err(error);
        }
        // What a clone left goes to the Trash first, as a failed copy's
        // does; `clone_whole` leaves nothing where cloning isn't possible.
        Err(_) => {
            if id(to).is_some() {
                trash(to).map_err(io::Error::other)?;
            }
        }
    }
    copy_each(&c_path(from)?, &c_path(to)?, meter)
}

fn clone_one(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: two NUL-terminated paths that outlive the call.
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), CLONE_NOFOLLOW | CLONE_ACL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Clones `from` to `to`. A file is one `clonefile` call. A folder is made
/// here and each item directly inside it is one call, with `cancel` checked
/// between them: one call for a 100,000-entry `node_modules` took about two
/// seconds and couldn't be stopped. Then the folder gets its own metadata
/// and `restore` puts back what the clones left out. Where nothing could be
/// cloned, as on another volume or one without clones, it leaves nothing.
/// A pipe or socket directly inside fails it, as copyfile fails on them.
pub(crate) fn clone_whole(from: &Path, to: &Path, cancel: Option<&AtomicBool>) -> io::Result<()> {
    let cancelled = || cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed));
    if cancelled() {
        return Err(io::Error::from_raw_os_error(libc::ECANCELED));
    }
    let meta = std::fs::symlink_metadata(from)?;
    if !meta.is_dir() {
        return clone_one(from, to);
    }
    let into = to
        .parent()
        .ok_or_else(|| io::Error::from_raw_os_error(libc::EINVAL))?;
    if std::fs::symlink_metadata(into)?.dev() != meta.dev() {
        return Err(io::Error::from_raw_os_error(libc::EXDEV));
    }
    std::fs::create_dir(to)?;
    let mut cloned = 0;
    let made = std::fs::read_dir(from)
        .and_then(|items| {
            for item in items {
                if cancelled() {
                    return Err(io::Error::from_raw_os_error(libc::ECANCELED));
                }
                let name = item?.file_name();
                clone_one(&from.join(&name), &to.join(&name))?;
                cloned += 1;
            }
            Ok(())
        })
        .and_then(|()| {
            let (from, to) = (c_path(from)?, c_path(to)?);
            // SAFETY: two NUL-terminated paths; no state.
            let copied = unsafe {
                libc::copyfile(
                    from.as_ptr(),
                    to.as_ptr(),
                    std::ptr::null_mut(),
                    libc::COPYFILE_METADATA,
                )
            };
            if copied == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        })
        .and_then(|()| restore(from, to));
    if made.is_err() && cloned == 0 {
        // The folder this just made, still empty, so a volume without
        // clones doesn't send one to the Trash for each copy.
        // SAFETY: a NUL-terminated path.
        let _ = c_path(to).map(|to| unsafe { libc::rmdir(to.as_ptr()) });
    }
    made
}

/// Puts back what `clonefile` leaves out when it clones a folder, which
/// `copyfile` keeps: each folder's times, which filling it changed, and the
/// ACLs of the items inside, since `CLONE_ACL` copies only the top item's.
fn restore(from: &Path, to: &Path) -> io::Result<()> {
    if !std::fs::symlink_metadata(from)?.is_dir() {
        return Ok(());
    }
    let mut folders = vec![std::path::PathBuf::new()];
    let mut times = Vec::new();
    while let Some(folder) = folders.pop() {
        let meta = std::fs::symlink_metadata(from.join(&folder))?;
        times.push((folder.clone(), meta));
        for item in std::fs::read_dir(from.join(&folder))? {
            let item = item?;
            let inside = folder.join(item.file_name());
            copy_acl(&from.join(&inside), &to.join(&inside))?;
            if item.file_type()?.is_dir() {
                folders.push(inside);
            }
        }
    }
    for (folder, meta) in times {
        let stamps = [
            libc::timespec {
                tv_sec: meta.atime(),
                tv_nsec: meta.atime_nsec(),
            },
            libc::timespec {
                tv_sec: meta.mtime(),
                tv_nsec: meta.mtime_nsec(),
            },
        ];
        let path = c_path(&to.join(&folder))?;
        // SAFETY: a NUL-terminated path and two timespecs.
        let set = unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                path.as_ptr(),
                stamps.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if set != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;

unsafe extern "C" {
    fn acl_get_link_np(path: *const libc::c_char, kind: libc::c_int) -> *mut libc::c_void;
    fn acl_set_link_np(
        path: *const libc::c_char,
        kind: libc::c_int,
        acl: *mut libc::c_void,
    ) -> libc::c_int;
    fn acl_free(acl: *mut libc::c_void) -> libc::c_int;
}

/// Gives `to` the ACL `from` has, if it has one.
fn copy_acl(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // SAFETY: NUL-terminated paths; the ACL is freed below.
    unsafe {
        let acl = acl_get_link_np(from.as_ptr(), ACL_TYPE_EXTENDED);
        if acl.is_null() {
            let error = io::Error::last_os_error();
            // No ACL at all.
            return match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(()),
                _ => Err(error),
            };
        }
        let set = acl_set_link_np(to.as_ptr(), ACL_TYPE_EXTENDED, acl);
        let error = io::Error::last_os_error();
        acl_free(acl);
        if set == 0 { Ok(()) } else { Err(error) }
    }
}

/// `copyfile`, cloning each file where the volume can.
pub(crate) fn copy_each(from: &CString, to: &CString, meter: Option<&Meter>) -> io::Result<()> {
    struct Progress<'a> {
        meter: &'a Meter<'a>,
        /// Bytes in the files already finished.
        done: u64,
        current: u64,
    }
    extern "C" fn status(
        what: libc::c_int,
        stage: libc::c_int,
        state: libc::copyfile_state_t,
        _: *const libc::c_char,
        _: *const libc::c_char,
        context: *mut libc::c_void,
    ) -> libc::c_int {
        // SAFETY: `context` is the `Progress` below, alive for the copy.
        let progress = unsafe { &mut *context.cast::<Progress>() };
        if progress.meter.cancel.load(Ordering::Relaxed) {
            return libc::COPYFILE_QUIT;
        }
        if what == libc::COPYFILE_COPY_DATA && stage == libc::COPYFILE_PROGRESS {
            let mut copied: libc::off_t = 0;
            // SAFETY: COPYFILE_STATE_COPIED writes one off_t.
            unsafe {
                libc::copyfile_state_get(
                    state,
                    libc::COPYFILE_STATE_COPIED as u32,
                    (&raw mut copied).cast(),
                )
            };
            progress.current = copied.max(0) as u64;
        } else if what == libc::COPYFILE_RECURSE_FILE && stage == libc::COPYFILE_FINISH {
            progress.done += progress.current;
            progress.current = 0;
        }
        progress
            .meter
            .copied
            .store(progress.done + progress.current, Ordering::Relaxed);
        libc::COPYFILE_CONTINUE
    }
    // COPYFILE_CLONE copies data, stat and extended attributes, never
    // replaces `to`, and copies links rather than what they point to. ACLs
    // need their own flag.
    let flags = libc::COPYFILE_CLONE | libc::COPYFILE_ACL | libc::COPYFILE_RECURSIVE;
    let mut progress = meter.map(|meter| Progress {
        meter,
        done: 0,
        current: 0,
    });
    // SAFETY: the state is freed below; the callback's context outlives
    // the copy, and both paths are NUL-terminated and outlive the call.
    unsafe {
        let state = libc::copyfile_state_alloc();
        if let Some(progress) = &mut progress {
            let callback = status
                as extern "C" fn(
                    libc::c_int,
                    libc::c_int,
                    libc::copyfile_state_t,
                    *const libc::c_char,
                    *const libc::c_char,
                    *mut libc::c_void,
                ) -> libc::c_int;
            libc::copyfile_state_set(
                state,
                libc::COPYFILE_STATE_STATUS_CB as u32,
                callback as *const libc::c_void,
            );
            libc::copyfile_state_set(
                state,
                libc::COPYFILE_STATE_STATUS_CTX as u32,
                std::ptr::from_mut(progress).cast(),
            );
        }
        let copied = libc::copyfile(from.as_ptr(), to.as_ptr(), state, flags);
        let error = io::Error::last_os_error();
        libc::copyfile_state_free(state);
        if copied == 0 { Ok(()) } else { Err(error) }
    }
}

/// How many items are inside `path`, and the bytes in its files: what a
/// complete copy has the same of. A cloud placeholder folder counts as
/// empty, as in `stamp`.
pub fn tally(path: &Path) -> io::Result<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    let mut tally = (0, if meta.is_file() { meta.len() } else { 0 });
    let mut folders = Vec::new();
    if listable(&meta) {
        folders.push(path.to_path_buf());
    }
    while let Some(folder) = folders.pop() {
        each_item(&folder, |item| {
            let item = item?;
            tally.0 += 1;
            if item.kind == Kind::File {
                tally.1 += item.size;
            } else if item.kind == Kind::Dir && Availability::from_flags(item.flags).is_local() {
                folders.push(folder.join(OsStr::from_bytes(item.name)));
            }
            Ok(())
        })?;
    }
    Ok(tally)
}

/// The bytes this user may still write on the volume `path` is on.
pub fn free(path: &Path) -> io::Result<u64> {
    let path = c_path(path)?;
    // SAFETY: an all-zero statfs is valid, and the call writes only into it.
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut fs) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fs.f_bavail.saturating_mul(u64::from(fs.f_bsize)))
}

/// The volume `path` is on.
pub fn volume(path: &Path) -> io::Result<u64> {
    std::fs::symlink_metadata(path).map(|meta| meta.dev())
}

/// Whether names in `folder` match whatever their case, as on APFS by
/// default. A volume that won't say is taken to.
pub fn ignores_case(folder: &Path) -> bool {
    let Ok(folder) = c_path(folder) else {
        return true;
    };
    // SAFETY: a NUL-terminated path that outlives the call.
    unsafe { libc::pathconf(folder.as_ptr(), libc::_PC_CASE_SENSITIVE) != 1 }
}

/// Looks through the Trash of the volume `original` was on for the item
/// `id`: after a crash, the only record of where it went.
pub fn find_in_trash(id: FileId, original: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(|home| Path::new(&home).join(".Trash"));
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let volumes = original
        .ancestors()
        .map(|dir| dir.join(".Trashes").join(uid.to_string()));
    home.into_iter()
        .chain(volumes)
        .filter(|trash| self::id(trash).is_some_and(|t| t.dev == id.dev))
        .find_map(|trash| {
            std::fs::read_dir(trash)
                .ok()?
                .filter_map(Result::ok)
                .map(|item| item.path())
                .find(|path| self::id(path) == Some(id))
        })
}

/// A folder whose items are on this Mac, so listing it downloads nothing.
fn listable(meta: &std::fs::Metadata) -> bool {
    meta.is_dir() && crate::cloud::Availability::of(meta).is_local()
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `stamp` and `tally` as they were before the bulk walk: `read_dir` and
    /// an `lstat` per item. Stamps the journal kept were made this way.
    fn by_lstat(path: &Path) -> (Stamp, (u64, u64)) {
        let meta = std::fs::symlink_metadata(path).unwrap();
        let mut stamp = Stamp {
            entries: 0,
            size: meta.size(),
            digest: mix(&meta),
        };
        let mut tally = (0, 0);
        let mut folders = vec![path.to_path_buf()];
        while let Some(folder) = folders.pop() {
            let Ok(items) = std::fs::read_dir(&folder) else {
                continue;
            };
            for item in items.filter_map(Result::ok) {
                let meta = item.metadata().unwrap();
                stamp.entries += 1;
                stamp.size += meta.size();
                stamp.digest = stamp.digest.wrapping_add(mix(&meta));
                tally.0 += 1;
                if meta.is_file() {
                    tally.1 += meta.len();
                }
                if listable(&meta) {
                    folders.push(item.path());
                }
            }
        }
        (stamp, tally)
    }

    #[test]
    fn the_bulk_walk_stamps_and_tallies_as_lstat_did() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("tree");
        for folder in ["a/b/c", "a/empty", "d"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        for (i, file) in [
            "a/one.txt",
            "a/b/two.bin",
            "a/b/c/three",
            "d/four.md",
            "five",
        ]
        .iter()
        .enumerate()
        {
            std::fs::write(root.join(file), vec![7u8; 1_000 * i + 3]).unwrap();
        }
        std::os::unix::fs::symlink("one.txt", root.join("a/link")).unwrap();
        std::os::unix::fs::symlink("a long way to nowhere", root.join("dangling")).unwrap();
        let fifo = c_path(&root.join("d/pipe")).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
        // Enough items in one folder for more than one bulk call's buffer.
        for i in 0..3_000 {
            std::fs::write(
                root.join(format!("d/item with a fairly long name {i}")),
                "x",
            )
            .unwrap();
        }
        let (stamp_before, tally_before) = by_lstat(&root);
        let (_, now) = stamp(&root).unwrap();
        assert_eq!(now, stamp_before);
        assert_eq!(tally(&root).unwrap(), tally_before);
        assert_eq!(now.entries, 3_013);
    }
}
