//! The system calls behind each operation: macOS's own Trash, renames that
//! never replace, and copies that clone where APFS can.

use objc2_foundation::{NSFileManager, NSString, NSURL};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Which item a path holds. Renames, including into the Trash, keep it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId {
    dev: u64,
    ino: u64,
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
    let meta = std::fs::symlink_metadata(path)?;
    Ok(FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// The item at `path` and its stamp. A folder is walked whole, without
/// following links; a subfolder that can't be read counts as empty.
pub fn stamp(path: &Path) -> io::Result<(FileId, Stamp)> {
    let meta = std::fs::symlink_metadata(path)?;
    let id = FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    };
    let mut stamp = Stamp {
        entries: 0,
        size: meta.size(),
        digest: mix(&meta),
    };
    let mut folders = Vec::new();
    if meta.is_dir() {
        folders.push(path.to_path_buf());
    }
    while let Some(folder) = folders.pop() {
        let Ok(items) = std::fs::read_dir(&folder) else {
            continue;
        };
        for item in items.filter_map(Result::ok) {
            // DirEntry::metadata doesn't follow links.
            let Ok(meta) = item.metadata() else { continue };
            stamp.entries += 1;
            stamp.size += meta.size();
            stamp.digest = stamp.digest.wrapping_add(mix(&meta));
            if meta.is_dir() {
                folders.push(item.path());
            }
        }
    }
    Ok((id, stamp))
}

/// One item's share of a stamp's digest. Summed, so the order a folder lists
/// in doesn't matter; std's hasher isn't stable across releases, and the
/// journal keeps stamps.
fn mix(meta: &std::fs::Metadata) -> u64 {
    [
        meta.ino(),
        meta.size(),
        meta.mtime() as u64,
        meta.mtime_nsec() as u64,
    ]
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

/// Copies `from` to the new path `to`, folders whole, keeping metadata.
/// Each file is an APFS clone where the volume supports it. A `meter` sees
/// the bytes copied so far and can stop the copy partway, leaving what's
/// at `to` incomplete.
pub fn copy(from: &Path, to: &Path, meter: Option<&Meter>) -> io::Result<()> {
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
    let (from, to) = (c_path(from)?, c_path(to)?);
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
/// complete copy has the same of.
pub fn tally(path: &Path) -> io::Result<(u64, u64)> {
    let meta = std::fs::symlink_metadata(path)?;
    let mut tally = (0, if meta.is_file() { meta.len() } else { 0 });
    let mut folders = Vec::new();
    if meta.is_dir() {
        folders.push(path.to_path_buf());
    }
    while let Some(folder) = folders.pop() {
        for item in std::fs::read_dir(&folder)? {
            let item = item?;
            let meta = item.metadata()?;
            tally.0 += 1;
            if meta.is_file() {
                tally.1 += meta.len();
            } else if meta.is_dir() {
                folders.push(item.path());
            }
        }
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

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}
