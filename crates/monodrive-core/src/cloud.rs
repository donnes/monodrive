//! Items iCloud Drive or a File Provider app, such as Dropbox, Google
//! Drive or OneDrive, keeps in the cloud. A placeholder is a dataless item:
//! `lstat` gives its name, size and dates, but reading it makes macOS
//! download it first, and listing a placeholder folder fetches its items.
//! Nothing here reads contents; downloading and removing a download ask
//! macOS through NSFileManager. docs/cloud.md records what reads what.

use crate::ops::{self, Origin};
use objc2_foundation::{
    NSArray, NSFileManager, NSNumber, NSString, NSURL, NSURLIsUbiquitousItemKey, NSURLResourceKey,
    NSURLUbiquitousItemHasUnresolvedConflictsKey, NSURLUbiquitousItemIsUploadedKey,
};
use serde::Serialize;
use std::fs::Metadata;
use std::os::macos::fs::MetadataExt;
use std::path::Path;

/// `SF_DATALESS` from <sys/stat.h>, which libc doesn't export: the kernel
/// sets it on every placeholder, whichever provider made it.
const SF_DATALESS: u32 = 0x4000_0000;

/// Whether an item's contents are on this Mac.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Availability {
    #[default]
    Local,
    /// Only its metadata is here.
    Placeholder,
    /// A placeholder macOS is downloading. Only `info` asks; a listing
    /// says `Placeholder`.
    Downloading,
    /// Not read, such as for a search result: the index keeps no flags.
    Unknown,
}

impl Availability {
    /// What the metadata from `lstat` says, which a listing already has.
    pub fn of(meta: &Metadata) -> Self {
        Self::from_flags(meta.st_flags())
    }

    fn from_flags(flags: u32) -> Self {
        if flags & SF_DATALESS == 0 {
            Availability::Local
        } else {
            Availability::Placeholder
        }
    }

    pub fn is_local(self) -> bool {
        self == Availability::Local
    }
}

/// Why `path` isn't read: reading it would download it unseen.
pub fn not_downloaded(path: &Path) -> String {
    format!("{} isn't downloaded. Download it first.", path.display())
}

/// Refuses `path` when it or anything in it is a placeholder, for work that
/// reads contents, such as a copy. Links aren't followed, and a placeholder
/// folder isn't listed. Folders that can't be read are left to that work.
pub fn refuse_placeholders(path: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !Availability::of(&meta).is_local() {
        return Err(not_downloaded(path));
    }
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
            if !Availability::of(&meta).is_local() {
                return Err(not_downloaded(&item.path()));
            }
            if meta.is_dir() {
                folders.push(item.path());
            }
        }
    }
    Ok(())
}

/// What asking macOS to download or remove a download came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asked {
    /// macOS took the request and does the work in the background; `info`
    /// shows when it's done and why it failed if it does.
    Requested,
    /// Nothing to do: it's already downloaded, or already a placeholder.
    Unneeded,
}

/// Asks iCloud Drive or the File Provider app holding `path` to download
/// it. A folder's request covers what the provider puts under it.
pub fn download(path: &Path, origin: &Origin, ops: &ops::Client) -> Result<Asked, String> {
    allowed(origin, ops)?;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let local = Availability::of(&meta).is_local();
    if local && !meta.is_dir() {
        return Ok(Asked::Unneeded);
    }
    with_url(path, |url| {
        // SAFETY: Foundation's constant key.
        let [cloud] = flags(url, [unsafe { NSURLIsUbiquitousItemKey }]);
        if !cloud {
            return Err(if local {
                not_in_cloud(path)
            } else {
                format!(
                    "{}: downloading is unsupported for this provider.",
                    path.display()
                )
            });
        }
        NSFileManager::defaultManager()
            .startDownloadingUbiquitousItemAtURL_error(url)
            .map(|()| Asked::Requested)
            .map_err(|e| format!("{}: {}", path.display(), e.localizedDescription()))
    })
}

/// Asks the provider holding `path` to remove its downloaded copy and keep
/// a placeholder, as Finder's Remove Download does. Refuses an item that
/// isn't uploaded yet or has conflicts, whose only copy may be here.
pub fn remove_download(path: &Path, origin: &Origin, ops: &ops::Client) -> Result<Asked, String> {
    allowed(origin, ops)?;
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !Availability::of(&meta).is_local() && !meta.is_dir() {
        return Ok(Asked::Unneeded);
    }
    with_url(path, |url| {
        // SAFETY: Foundation's constant keys.
        let [cloud, uploaded, conflicts] = flags(url, unsafe {
            [
                NSURLIsUbiquitousItemKey,
                NSURLUbiquitousItemIsUploadedKey,
                NSURLUbiquitousItemHasUnresolvedConflictsKey,
            ]
        });
        if !cloud {
            return Err(not_in_cloud(path));
        }
        if !uploaded {
            return Err(format!(
                "{} isn't uploaded yet, so its only copy is on this Mac.",
                path.display()
            ));
        }
        if conflicts {
            return Err(format!(
                "{} has versions in conflict; resolve them first.",
                path.display()
            ));
        }
        NSFileManager::defaultManager()
            .evictUbiquitousItemAtURL_error(url)
            .map(|()| Asked::Requested)
            .map_err(|e| format!("{}: {}", path.display(), e.localizedDescription()))
    })
}

fn allowed(origin: &Origin, ops: &ops::Client) -> Result<(), String> {
    if matches!(origin, Origin::Agent { .. }) && !ops.agents_change_files() {
        return Err(
            "Agents can't download or remove downloads: Let agents change files is off in Monodrive's Agents settings."
                .into(),
        );
    }
    Ok(())
}

fn not_in_cloud(path: &Path) -> String {
    format!(
        "{} isn't in iCloud Drive or a File Provider folder.",
        path.display()
    )
}

fn with_url<T>(path: &Path, f: impl FnOnce(&NSURL) -> T) -> T {
    objc2::rc::autoreleasepool(|_| {
        f(&NSURL::fileURLWithPath(&NSString::from_str(
            &path.to_string_lossy(),
        )))
    })
}

/// Yes-or-no resource values, in one query; no where macOS doesn't say.
fn flags<const N: usize>(url: &NSURL, keys: [&NSURLResourceKey; N]) -> [bool; N] {
    let values = url
        .resourceValuesForKeys_error(&NSArray::from_slice(&keys))
        .ok();
    keys.map(|key| {
        values
            .as_ref()
            .and_then(|values| values.objectForKey(key))
            .and_then(|n| n.downcast::<NSNumber>().ok())
            .is_some_and(|n| n.boolValue())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_dataless_flag_makes_a_placeholder() {
        // What `ls -lO` shows as "compressed,dataless" on a placeholder.
        const UF_COMPRESSED: u32 = 0x20;
        assert_eq!(
            Availability::from_flags(UF_COMPRESSED | SF_DATALESS),
            Availability::Placeholder
        );
        assert_eq!(Availability::from_flags(UF_COMPRESSED), Availability::Local);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        let meta = std::fs::symlink_metadata(dir.path().join("a.txt")).unwrap();
        assert_eq!(Availability::of(&meta), Availability::Local);
        assert_eq!(refuse_placeholders(dir.path()), Ok(()));
    }
}
