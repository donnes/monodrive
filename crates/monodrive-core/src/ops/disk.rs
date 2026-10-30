//! The system calls behind each operation: macOS's own Trash, renames that
//! never replace, and copies that clone where APFS can.

use objc2_foundation::{NSFileManager, NSString, NSURL};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Which item a path holds. Renames, including into the Trash, keep it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    dev: u64,
    ino: u64,
}

/// What an item was like, to tell whether it changed since.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    size: u64,
    modified: (i64, i64),
}

/// The item at `path` itself, not what a link there points to.
pub fn id(path: &Path) -> Option<FileId> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    Some(FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

pub fn stamp(path: &Path) -> io::Result<(FileId, Stamp)> {
    let meta = std::fs::symlink_metadata(path)?;
    let id = FileId {
        dev: meta.dev(),
        ino: meta.ino(),
    };
    Ok((
        id,
        Stamp {
            size: meta.size(),
            modified: (meta.mtime(), meta.mtime_nsec()),
        },
    ))
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

/// Copies `from` to the new path `to`, folders whole, keeping metadata.
/// Each file is an APFS clone where the volume supports it.
pub fn duplicate(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (c_path(from)?, c_path(to)?);
    // COPYFILE_CLONE copies data and metadata, never replaces `to`, and
    // copies links rather than what they point to.
    let flags = libc::COPYFILE_CLONE | libc::COPYFILE_RECURSIVE;
    // SAFETY: both are NUL-terminated paths that outlive the call, and a
    // null state is allowed.
    if unsafe { libc::copyfile(from.as_ptr(), to.as_ptr(), std::ptr::null_mut(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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
