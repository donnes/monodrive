//! Get Info: what Finder's info window says about one item. Reads the item
//! itself, never a link's target, and never asks Spotlight, so each call is
//! a few system calls and one LaunchServices lookup for the kind.

use crate::entries::Kind;
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSString, NSURL, NSURLIsPackageKey,
    NSURLLocalizedTypeDescriptionKey, NSURLResourceKey, NSURLVolumeLocalizedNameKey,
    NSURLVolumeURLKey,
};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    /// What Finder calls it, such as "Plain Text Document" or "Folder".
    pub kind_name: String,
    /// Files only. A folder's size is `sizes::measure`'s to find.
    pub size: Option<Size>,
    /// The folder it's in; `None` for `/`.
    pub parent: Option<PathBuf>,
    pub volume: Option<OnVolume>,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    /// When it was last opened through LaunchServices, as Finder, the Dock
    /// and `open` do. `None` if never, and for most folders.
    pub opened: Option<SystemTime>,
    pub permissions: Permissions,
    /// An app or document package, which opens instead of being entered.
    pub package: bool,
    /// Where a symbolic link points, as written in the link.
    pub link: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub bytes: u64,
    pub on_disk: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnVolume {
    pub name: String,
    pub mount: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permissions {
    pub owner: String,
    pub group: String,
    /// The permission bits, such as `0o644`, with setuid, setgid and sticky.
    pub mode: u32,
    /// What the current user may do with it.
    pub readable: bool,
    pub writable: bool,
}

/// What Get Info shows for `path`. Fails only when the item can't be
/// stat'ed, with the path and macOS's reason.
pub fn info(path: &Path) -> Result<Info, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let meta = std::fs::symlink_metadata(path).map_err(fail)?;
    let kind = Kind::of(meta.file_type());
    let link = match kind {
        Kind::Link => Some(std::fs::read_link(path).map_err(fail)?),
        _ => None,
    };
    let known = Resources::of(path);
    Ok(Info {
        name: path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        kind,
        kind_name: known.kind_name.unwrap_or_else(|| {
            match kind {
                Kind::Dir => "Folder",
                Kind::Link => "Alias",
                Kind::File => "Document",
                Kind::Other => "Item",
            }
            .into()
        }),
        size: (kind == Kind::File).then(|| Size {
            bytes: meta.len(),
            on_disk: meta.blocks() * 512,
        }),
        parent: path.parent().map(Path::to_path_buf),
        volume: known.volume,
        created: meta.created().ok(),
        modified: meta.modified().ok(),
        opened: last_opened(path),
        permissions: Permissions {
            owner: user_name(meta.uid()).unwrap_or_else(|| meta.uid().to_string()),
            group: group_name(meta.gid()).unwrap_or_else(|| meta.gid().to_string()),
            mode: meta.mode() & 0o7777,
            readable: can(path, libc::R_OK),
            writable: can(path, libc::W_OK),
        },
        package: known.package,
        link,
        path: path.to_path_buf(),
    })
}

/// Whether `path` is an app or document package by macOS's own reckoning,
/// so Open launches it rather than going inside. Reads the disk.
pub fn is_package(path: &Path) -> bool {
    Resources::of(path).package
}

/// What Foundation knows about an item beyond `lstat`.
#[derive(Default)]
struct Resources {
    kind_name: Option<String>,
    package: bool,
    volume: Option<OnVolume>,
}

impl Resources {
    fn of(path: &Path) -> Self {
        objc2::rc::autoreleasepool(|_| {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
            // SAFETY: these are Foundation's constant keys.
            let keys = unsafe {
                [
                    NSURLLocalizedTypeDescriptionKey,
                    NSURLIsPackageKey,
                    NSURLVolumeLocalizedNameKey,
                    NSURLVolumeURLKey,
                ]
            };
            let Ok(values) = url.resourceValuesForKeys_error(&NSArray::from_slice(&keys)) else {
                return Self::default();
            };
            let get = |key: &NSURLResourceKey| values.objectForKey(key);
            let text = |key| get(key)?.downcast::<NSString>().ok().map(|s| s.to_string());
            // SAFETY: as above.
            unsafe {
                let mount = get(NSURLVolumeURLKey)
                    .and_then(|url| url.downcast::<NSURL>().ok())
                    .and_then(|url| url.path())
                    .map(|path| PathBuf::from(path.to_string()));
                Self {
                    kind_name: text(NSURLLocalizedTypeDescriptionKey),
                    package: flag(&values, NSURLIsPackageKey),
                    volume: mount.map(|mount| OnVolume {
                        name: text(NSURLVolumeLocalizedNameKey)
                            .unwrap_or_else(|| mount.display().to_string()),
                        mount,
                    }),
                }
            }
        })
    }
}

fn flag(values: &NSDictionary<NSURLResourceKey, AnyObject>, key: &NSURLResourceKey) -> bool {
    values
        .objectForKey(key)
        .and_then(|n| n.downcast::<NSNumber>().ok())
        .is_some_and(|n| n.boolValue())
}

/// The date LaunchServices records in the item's own extended attribute
/// when it opens it, which Spotlight copies into `kMDItemLastUsedDate`.
/// Reading the attribute needs no Spotlight round trip and works on
/// volumes Spotlight skips. The access time isn't used: anything that
/// reads the file, a backup or a search, moves it.
fn last_opened(path: &Path) -> Option<SystemTime> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stamp = [0u8; 16];
    // SAFETY: both strings are NUL-terminated, and the buffer is valid for
    // the length given.
    let n = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"com.apple.lastuseddate#PS".as_ptr(),
            stamp.as_mut_ptr().cast(),
            stamp.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    if n != 16 {
        return None;
    }
    // A `timespec`: signed seconds, then nanoseconds, each 8 bytes
    // little-endian. Anything else written there is no date.
    let (seconds, nanos) = stamp.split_at(8);
    let seconds = i64::from_le_bytes(seconds.try_into().ok()?);
    let nanos = u64::from_le_bytes(nanos.try_into().ok()?);
    if nanos >= 1_000_000_000 {
        return None;
    }
    let whole = Duration::from_secs(seconds.unsigned_abs());
    let second = if seconds < 0 {
        UNIX_EPOCH.checked_sub(whole)
    } else {
        UNIX_EPOCH.checked_add(whole)
    };
    second?.checked_add(Duration::from_nanos(nanos))
}

/// Whether the current user may read or write the item itself, as the
/// kernel decides, ACLs included.
fn can(path: &Path, mode: libc::c_int) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: the path is NUL-terminated.
    unsafe {
        libc::faccessat(
            libc::AT_FDCWD,
            path.as_ptr(),
            mode,
            libc::AT_SYMLINK_NOFOLLOW,
        ) == 0
    }
}

fn user_name(uid: u32) -> Option<String> {
    let mut buffer = [0 as libc::c_char; 1024];
    // SAFETY: an all-zero passwd is valid; the call fills it with pointers
    // into `buffer`, which outlives their use here.
    unsafe {
        let mut entry: libc::passwd = std::mem::zeroed();
        let mut found = std::ptr::null_mut();
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        );
        (!found.is_null() && !entry.pw_name.is_null()).then(|| {
            std::ffi::CStr::from_ptr(entry.pw_name)
                .to_string_lossy()
                .into_owned()
        })
    }
}

fn group_name(gid: u32) -> Option<String> {
    let mut buffer = [0 as libc::c_char; 1024];
    // SAFETY: as in `user_name`.
    unsafe {
        let mut entry: libc::group = std::mem::zeroed();
        let mut found = std::ptr::null_mut();
        libc::getgrgid_r(
            gid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        );
        (!found.is_null() && !entry.gr_name.is_null()).then(|| {
            std::ffi::CStr::from_ptr(entry.gr_name)
                .to_string_lossy()
                .into_owned()
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_file_folder_link_and_package_each_say_what_they_are() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "hello").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        let folder = dir.path().join("docs");
        std::fs::create_dir(&folder).unwrap();
        let link = dir.path().join("shortcut");
        std::os::unix::fs::symlink("docs", &link).unwrap();
        let app = dir.path().join("Tool.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), "<plist/>").unwrap();

        let about = info(&file).unwrap();
        assert_eq!(about.name, "notes.txt");
        assert_eq!(about.kind, Kind::File);
        assert_eq!(about.kind_name, "Plain Text Document");
        assert_eq!(about.size.map(|size| size.bytes), Some(5));
        assert!(about.size.unwrap().on_disk > 0);
        assert_eq!(about.parent.as_deref(), Some(dir.path()));
        assert!(about.volume.is_some_and(|volume| !volume.name.is_empty()));
        assert!(about.created.is_some() && about.modified.is_some());
        // Nothing opened it through LaunchServices.
        assert_eq!(about.opened, None);
        assert_eq!(about.permissions.mode, 0o640);
        assert_eq!(
            about.permissions.owner,
            user_name(unsafe { libc::getuid() }).unwrap()
        );
        assert!(about.permissions.readable && about.permissions.writable);
        assert!(!about.package && about.link.is_none());

        let about = info(&folder).unwrap();
        assert_eq!(
            (about.kind, about.kind_name.as_str()),
            (Kind::Dir, "Folder")
        );
        assert_eq!(about.size, None);
        assert!(!about.package);

        // The link itself, not the folder it points to.
        let about = info(&link).unwrap();
        assert_eq!(about.kind, Kind::Link);
        assert_eq!(about.link.as_deref(), Some(Path::new("docs")));
        assert!(!about.package);

        // Its extension alone makes it a package; LaunchServices needn't
        // have registered it.
        let about = info(&app).unwrap();
        assert_eq!(about.kind, Kind::Dir);
        assert!(about.package, "{about:?}");
        assert!(is_package(&app) && !is_package(&folder) && !is_package(&file));

        let missing = dir.path().join("missing");
        assert_eq!(
            info(&missing),
            Err(format!(
                "{}: No such file or directory (os error 2)",
                missing.display()
            ))
        );
    }

    /// Writes the attribute LaunchServices keeps the last-opened date in.
    pub(crate) fn record_opened(file: &Path, seconds: i64, nanos: u64) {
        let mut stamp = [0u8; 16];
        stamp[..8].copy_from_slice(&seconds.to_le_bytes());
        stamp[8..].copy_from_slice(&nanos.to_le_bytes());
        let path = CString::new(file.as_os_str().as_bytes()).unwrap();
        // SAFETY: NUL-terminated strings and a buffer of the length given.
        let set = unsafe {
            libc::setxattr(
                path.as_ptr(),
                c"com.apple.lastuseddate#PS".as_ptr(),
                stamp.as_ptr().cast(),
                stamp.len(),
                0,
                0,
            )
        };
        assert_eq!(set, 0);
    }

    #[test]
    fn opened_is_the_date_launch_services_recorded_when_its_valid() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.pdf");
        std::fs::write(&file, "%PDF").unwrap();
        let opened = |seconds, nanos| {
            record_opened(&file, seconds, nanos);
            info(&file).unwrap().opened
        };
        // 2026-10-29 22:56:28.405290008 UTC, as macOS writes it.
        assert_eq!(
            opened(1_793_314_588, 405_290_008),
            Some(UNIX_EPOCH + Duration::new(1_793_314_588, 405_290_008))
        );
        // The seconds are signed, as in a timespec.
        assert_eq!(opened(-1, 0), Some(UNIX_EPOCH - Duration::from_secs(1)));
        assert_eq!(opened(0, 1_000_000_000), None);
        assert_eq!(opened(0, u64::MAX), None);
    }
}
