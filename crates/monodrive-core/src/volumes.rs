//! Mounted volumes as Finder lists them: what's mounted, a watch that hears
//! mounts and unmounts, and Eject. Reads volume metadata only, never a
//! volume's contents, so nothing here raises a privacy prompt.

use crate::ops::{self, Origin};
use block2::RcBlock;
use objc2::runtime::AnyObject;
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSFileManager,
    NSFileManagerUnmountDissentingProcessIdentifierErrorKey, NSFileManagerUnmountOptions, NSNumber,
    NSString, NSURL, NSURLResourceKey, NSURLVolumeAvailableCapacityForImportantUsageKey,
    NSURLVolumeAvailableCapacityKey, NSURLVolumeIsEjectableKey, NSURLVolumeIsLocalKey,
    NSURLVolumeIsReadOnlyKey, NSURLVolumeIsRemovableKey, NSURLVolumeIsRootFileSystemKey,
    NSURLVolumeLocalizedNameKey, NSURLVolumeTotalCapacityKey, NSURLVolumeURLForRemountingKey,
    NSURLVolumeUUIDStringKey,
};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString, c_char, c_void};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// Which volume this is, whatever it's called and wherever it's mounted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeId {
    /// A local file system's UUID.
    Uuid(String),
    /// A network share's address, such as `smb://nas/photos`. macOS gives
    /// shares no lasting UUID.
    Url(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub name: String,
    pub mount: PathBuf,
    /// The physical disk it's on, as IOKit's id for it. Ejecting the disk
    /// unmounts every volume with the same one. `None` for a network share.
    pub disk: Option<u64>,
    /// `None` when macOS knows none, as for some FAT drives.
    pub id: Option<VolumeId>,
    pub total: u64,
    /// What Finder calls available, purgeable space included.
    pub free: u64,
    pub read_only: bool,
    pub network: bool,
    /// Its media can be ejected, as a disk image or a card can.
    pub ejectable: bool,
    /// Its drive can be unplugged, as a USB drive can.
    pub removable: bool,
    pub startup: bool,
}

/// The volumes Finder shows, the startup disk first. Each read can wait on
/// a slow network share, so call it off the UI thread.
pub fn list() -> Vec<Volume> {
    mounted(false)
}

/// Every mounted volume, or with `hidden` false only those Finder shows.
fn mounted(hidden: bool) -> Vec<Volume> {
    objc2::rc::autoreleasepool(|_| {
        // SAFETY: these are Foundation's constant keys.
        let keys = unsafe {
            [
                NSURLVolumeLocalizedNameKey,
                NSURLVolumeUUIDStringKey,
                NSURLVolumeURLForRemountingKey,
                NSURLVolumeTotalCapacityKey,
                NSURLVolumeAvailableCapacityForImportantUsageKey,
                NSURLVolumeAvailableCapacityKey,
                NSURLVolumeIsReadOnlyKey,
                NSURLVolumeIsLocalKey,
                NSURLVolumeIsEjectableKey,
                NSURLVolumeIsRemovableKey,
                NSURLVolumeIsRootFileSystemKey,
            ]
        };
        let keys = NSArray::from_slice(&keys);
        let mut volumes: Vec<Volume> = mounts(hidden)
            .into_iter()
            .filter_map(|(mount, device)| {
                let url = NSURL::fileURLWithPath(&NSString::from_str(&mount.to_string_lossy()));
                let values = url.resourceValuesForKeys_error(&keys).ok()?;
                let disk = device.as_deref().and_then(physical_disk);
                Some(volume(mount, disk, &values))
            })
            .collect();
        volumes.sort_by_key(|volume| !volume.startup);
        volumes
    })
}

/// Where the kernel has volumes mounted, hidden ones only if asked, and the
/// device it's mounted from, such as `disk4s1`. Read from the mount table,
/// not NSFileManager, whose list lags a mount by a moment, so a `watch`
/// callback that lists again sees the new volume.
fn mounts(hidden: bool) -> Vec<(PathBuf, Option<CString>)> {
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    // Room for a few mounted between the two calls.
    let mut table: Vec<libc::statfs> = Vec::with_capacity(count.max(0) as usize + 8);
    let size = table.capacity() * std::mem::size_of::<libc::statfs>();
    // SAFETY: the buffer holds `size` bytes, and the call fills at most
    // that many entries before `set_len` covers them.
    let filled = unsafe {
        let filled = libc::getfsstat(table.as_mut_ptr(), size as i32, libc::MNT_NOWAIT);
        table.set_len(filled.max(0) as usize);
        filled
    };
    if filled < 0 {
        return Vec::new();
    }
    table
        .iter()
        .filter(|fs| hidden || fs.f_flags & libc::MNT_DONTBROWSE as u32 == 0)
        .map(|fs| {
            // SAFETY: the kernel NUL-terminates both names.
            let (path, from) = unsafe {
                (
                    CStr::from_ptr(fs.f_mntonname.as_ptr()),
                    CStr::from_ptr(fs.f_mntfromname.as_ptr()),
                )
            };
            let device = from.to_bytes().strip_prefix(b"/dev/");
            (
                PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())),
                device.and_then(|name| CString::new(name).ok()),
            )
        })
        .collect()
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOBSDNameMatching(port: libc::mach_port_t, options: u32, name: *const c_char)
    -> *mut c_void;
    fn IOServiceGetMatchingService(port: libc::mach_port_t, matching: *mut c_void) -> u32;
    fn IORegistryEntryGetParentEntry(entry: u32, plane: *const c_char, parent: *mut u32) -> i32;
    fn IORegistryEntryGetRegistryEntryID(entry: u32, id: *mut u64) -> i32;
    fn IOObjectConformsTo(object: u32, class: *const c_char) -> libc::boolean_t;
    fn IOObjectRelease(object: u32) -> i32;
}

/// The physical disk under the device `name`: the outermost storage medium
/// above it in IOKit's registry. An APFS volume sits in a container that
/// sits in a partition of the disk, beside the disk's other partitions.
fn physical_disk(name: &CStr) -> Option<u64> {
    let mut disk = None;
    // SAFETY: port 0 is the default main port; the matching dictionary is
    // consumed by the lookup; each entry is released once it's been asked
    // for its parent.
    unsafe {
        let matching = IOBSDNameMatching(0, 0, name.as_ptr());
        if matching.is_null() {
            return None;
        }
        let mut entry = IOServiceGetMatchingService(0, matching);
        while entry != 0 {
            let mut id = 0;
            if IOObjectConformsTo(entry, c"IOMedia".as_ptr()) != 0
                && IORegistryEntryGetRegistryEntryID(entry, &mut id) == 0
            {
                disk = Some(id);
            }
            let mut parent = 0;
            let found = IORegistryEntryGetParentEntry(entry, c"IOService".as_ptr(), &mut parent);
            IOObjectRelease(entry);
            entry = if found == 0 { parent } else { 0 };
        }
    }
    disk
}

fn volume(
    mount: PathBuf,
    disk: Option<u64>,
    values: &NSDictionary<NSURLResourceKey, AnyObject>,
) -> Volume {
    let get = |key: &NSURLResourceKey| values.objectForKey(key);
    let text = |key| get(key)?.downcast::<NSString>().ok().map(|s| s.to_string());
    let number = |key| get(key)?.downcast::<NSNumber>().ok();
    let flag = |key| number(key).is_some_and(|n| n.boolValue());
    let bytes = |key| number(key).map(|n| n.unsignedLongLongValue());
    // SAFETY: these are Foundation's constant keys.
    unsafe {
        let network = !flag(NSURLVolumeIsLocalKey);
        let share = get(NSURLVolumeURLForRemountingKey)
            .and_then(|url| url.downcast::<NSURL>().ok())
            .and_then(|url| url.absoluteString())
            .map(|url| VolumeId::Url(url.to_string()));
        let uuid = text(NSURLVolumeUUIDStringKey).map(VolumeId::Uuid);
        Volume {
            name: text(NSURLVolumeLocalizedNameKey).unwrap_or_else(|| mount.display().to_string()),
            id: if network { share.or(uuid) } else { uuid },
            total: bytes(NSURLVolumeTotalCapacityKey).unwrap_or(0),
            free: bytes(NSURLVolumeAvailableCapacityForImportantUsageKey)
                .filter(|&free| free > 0)
                .or_else(|| bytes(NSURLVolumeAvailableCapacityKey))
                .unwrap_or(0),
            read_only: flag(NSURLVolumeIsReadOnlyKey),
            network,
            ejectable: flag(NSURLVolumeIsEjectableKey),
            removable: flag(NSURLVolumeIsRemovableKey),
            startup: flag(NSURLVolumeIsRootFileSystemKey),
            disk,
            mount,
        }
    }
}

/// The volume `path` is on: the one mounted deepest above it.
pub fn holding<'a>(volumes: &'a [Volume], path: &Path) -> Option<&'a Volume> {
    volumes
        .iter()
        .filter(|volume| path.starts_with(&volume.mount))
        .max_by_key(|volume| volume.mount.components().count())
}

/// Ejects the volume mounted at `mount`, as Finder's Eject does: the whole
/// disk when its media can be ejected or unplugged, otherwise only this
/// volume. Refuses the startup disk and a volume a Monodrive job still has
/// work on, and passes on macOS's refusal, such as a busy volume. Never
/// forces. Waits for macOS, so call it off the UI thread.
pub fn eject(mount: &Path, origin: &Origin, ops: &mut ops::Client) -> Result<String, String> {
    if matches!(origin, Origin::Agent { .. }) && !ops.agents_change_files() {
        return Err(
            "Agents can't eject volumes: Let agents change files is off in Monodrive's Agents settings."
                .into(),
        );
    }
    // Hidden ones too: a whole-disk eject takes them along, jobs and all.
    let volumes = mounted(true);
    let volume = volumes
        .iter()
        .find(|volume| volume.mount == mount)
        .ok_or_else(|| format!("{} isn't a mounted volume.", mount.display()))?;
    ready(volume, &volumes, ops)?;
    unmount(volume)?;
    Ok(format!("Ejected {}.", volume.name))
}

/// Ejecting takes the whole disk, every volume on it, when its media can
/// be ejected or unplugged; otherwise it unmounts only this volume.
fn whole_disk(volume: &Volume) -> bool {
    volume.ejectable || volume.removable
}

/// Whether `volume` may be ejected now: neither it nor another volume
/// ejecting it takes along is the startup disk, and no job queued, running
/// or waiting at a conflict reads or writes any of them.
fn ready(volume: &Volume, volumes: &[Volume], ops: &mut ops::Client) -> Result<(), String> {
    let along = volumes.iter().filter(|other| {
        whole_disk(volume)
            && other.mount != volume.mount
            && other.disk.is_some()
            && other.disk == volume.disk
    });
    let leaving: Vec<&Volume> = std::iter::once(volume).chain(along).collect();
    // Why one of them can't go, said of the volume asked about.
    let refuse = |going: &Volume, why: &str| {
        if going.mount == volume.mount {
            format!("{} is {why}", going.name)
        } else {
            format!("Ejecting {} also ejects {}, {why}", volume.name, going.name)
        }
    };
    if let Some(startup) = leaving.iter().find(|going| going.startup) {
        return Err(refuse(startup, "the startup disk."));
    }
    let jobs = ops.jobs()?;
    let active = jobs.iter().filter(|job| {
        matches!(
            job.state,
            ops::State::Queued | ops::State::Running | ops::State::Waiting
        )
    });
    for job in active {
        let touches = |mount: &Path| {
            job.into.starts_with(mount)
                || job.tasks.iter().any(|task| task.source.starts_with(mount))
        };
        if let Some(going) = leaving.iter().find(|going| touches(&going.mount)) {
            let which = if going.mount == volume.mount {
                ""
            } else {
                "which is "
            };
            let why = format!(
                "{which}in use by Monodrive's job {} ({} into {}, {}). Let it finish or cancel it first.",
                job.id,
                job.title,
                job.into.display(),
                job.state.label()
            );
            return Err(refuse(going, &why));
        }
    }
    Ok(())
}

fn unmount(volume: &Volume) -> Result<(), String> {
    let mut options = NSFileManagerUnmountOptions::WithoutUI;
    if whole_disk(volume) {
        options |= NSFileManagerUnmountOptions::AllPartitionsAndEjectDisk;
    }
    let (sent, done) = std::sync::mpsc::channel();
    let finished = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: Foundation passes a valid error or null.
        let _ = sent.send(unsafe { error.as_ref() }.map(refusal));
    });
    objc2::rc::autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&volume.mount.to_string_lossy()));
        // SAFETY: the block only sends on a channel, which is thread-safe.
        unsafe {
            NSFileManager::defaultManager()
                .unmountVolumeAtURL_options_completionHandler(&url, options, &finished);
        }
    });
    match done.recv() {
        Ok(None) => Ok(()),
        Ok(Some(why)) => Err(format!("{}: {why}", volume.mount.display())),
        Err(_) => Err(format!("{}: macOS didn't answer.", volume.mount.display())),
    }
}

/// macOS's reason, naming the app that kept the volume busy when it says.
fn refusal(error: &NSError) -> String {
    let why = error.localizedDescription().to_string();
    // SAFETY: the key is Foundation's constant.
    let key = unsafe { NSFileManagerUnmountDissentingProcessIdentifierErrorKey };
    let pid = error
        .userInfo()
        .objectForKey(key)
        .and_then(|pid| pid.downcast::<NSNumber>().ok())
        .map(|pid| pid.intValue());
    match pid.map(|pid| (pid, process_name(pid))) {
        Some((pid, Some(name))) => format!("{why} {name} (pid {pid}) is using it."),
        Some((pid, None)) => format!("{why} Process {pid} is using it."),
        None => why,
    }
}

fn process_name(pid: i32) -> Option<String> {
    let mut name = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length.
    let n = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
    (n > 0).then(|| String::from_utf8_lossy(&name[..n as usize]).into_owned())
}

/// What `VQ_*` events in `sys/mount.h` mean a volume came, went or changed.
const VQ_MOUNT: u32 = 0x0008;
const VQ_UNMOUNT: u32 = 0x0010;
const VQ_UPDATE: u32 = 0x0100;

/// Calls `changed` on its own thread each time a volume is mounted,
/// unmounted or changes, through the kernel's file system events: no
/// polling, and nothing read. Hidden volumes count too, so `list` again to
/// see whether what's shown changed. Dropping it stops the thread.
pub struct Watch {
    queue: OwnedFd,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn watch(changed: impl Fn() + Send + 'static) -> io::Result<Watch> {
    // SAFETY: kqueue has no preconditions; the descriptor is owned here.
    let queue = match unsafe { libc::kqueue() } {
        -1 => return Err(io::Error::last_os_error()),
        fd => unsafe { OwnedFd::from_raw_fd(fd) },
    };
    let raw = queue.as_raw_fd();
    change(raw, libc::EVFILT_USER, libc::EV_ADD | libc::EV_CLEAR, 0)?;
    change(raw, libc::EVFILT_FS, libc::EV_ADD | libc::EV_CLEAR, 0)?;
    let thread = std::thread::spawn(move || {
        loop {
            // SAFETY: an all-zero kevent is valid, and the call writes at
            // most the one it's given.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let n =
                unsafe { libc::kevent(raw, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) };
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if n <= 0 || event.filter == libc::EVFILT_USER {
                return;
            }
            if event.fflags & (VQ_MOUNT | VQ_UNMOUNT | VQ_UPDATE) != 0 {
                changed();
            }
        }
    });
    Ok(Watch {
        queue,
        thread: Some(thread),
    })
}

impl Drop for Watch {
    fn drop(&mut self) {
        let _ = change(
            self.queue.as_raw_fd(),
            libc::EVFILT_USER,
            0,
            libc::NOTE_TRIGGER,
        );
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn change(queue: i32, filter: i16, flags: u16, fflags: u32) -> io::Result<()> {
    // SAFETY: an all-zero kevent is valid.
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    event.filter = filter;
    event.flags = flags;
    event.fflags = fflags;
    // SAFETY: the change list holds one event and the queue is open.
    let n = unsafe {
        libc::kevent(
            queue,
            &raw mut event,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_startup_disk_is_listed_first_with_its_capacity() {
        let volumes = list();
        let startup = volumes.first().expect("a volume");
        assert!(startup.startup, "{volumes:?}");
        assert_eq!(startup.mount, Path::new("/"));
        assert!(startup.total > 0 && startup.free <= startup.total);
        assert!(matches!(startup.id, Some(VolumeId::Uuid(_))));
        assert!(!startup.network);
        assert!(startup.disk.is_some());
    }

    #[test]
    fn eject_refuses_the_startup_disk_and_a_volume_a_job_still_has_work_on() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ops::Paths {
            state: dir.path().join("state"),
            settings: dir.path().join("settings.json"),
            trash: dir.path().join("Trash"),
        };
        let (file, into) = (dir.path().join("a.txt"), dir.path().join("into"));
        std::fs::write(&file, "a").unwrap();
        std::fs::create_dir(&into).unwrap();
        std::fs::write(into.join("a.txt"), "taken").unwrap();
        // The copy stops at the taken name and waits; the owner exits once
        // it has, and the next one reads the job back from its journal.
        let owner = ops::Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut client = ops::Client::new(paths.clone(), None);
        let copy = ops::Request::Copy {
            paths: vec![file],
            into: into.clone(),
            conflict: ops::Conflict::Stop,
        };
        client.submit(Origin::Window, copy).unwrap();
        drop(client);
        serving.join().unwrap().unwrap();

        let owner = ops::Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut client = ops::Client::new(paths, None);
        let volume = |name: &str, mount: &Path, disk: u64, ejectable| Volume {
            name: name.into(),
            mount: mount.into(),
            disk: Some(disk),
            id: None,
            total: 0,
            free: 0,
            read_only: false,
            network: false,
            ejectable,
            removable: false,
            startup: mount == Path::new("/"),
        };
        let volumes = [
            volume("Macintosh HD", "/".as_ref(), 0, false),
            volume("Scratch", "/Volumes/Scratch".as_ref(), 0, false),
            volume("Backup", "/Volumes/Backup".as_ref(), 4, true),
            volume("Photos", dir.path(), 4, true),
            volume("Other", "/Volumes/Other".as_ref(), 5, true),
        ];
        let check = |name: &str, client: &mut ops::Client| {
            let volume = volumes.iter().find(|v| v.name == name).unwrap();
            ready(volume, &volumes, client)
        };
        let busy = check("Photos", &mut client).unwrap_err();
        assert!(
            busy.starts_with("Photos is in use by Monodrive's job 1 (Copy into "),
            "{busy}"
        );
        assert!(busy.contains("waiting"), "{busy}");
        // Ejecting a disk unmounts all its volumes, so each one counts.
        let sibling = check("Backup", &mut client).unwrap_err();
        assert!(
            sibling.starts_with(
                "Ejecting Backup also ejects Photos, which is in use by Monodrive's job 1"
            ),
            "{sibling}"
        );
        assert_eq!(check("Other", &mut client), Ok(()));
        // A volume that isn't ejectable is only unmounted, alone.
        assert_eq!(check("Scratch", &mut client), Ok(()));
        assert_eq!(
            check("Macintosh HD", &mut client),
            Err("Macintosh HD is the startup disk.".into())
        );
        let booted_from_external = [
            volume("External", "/".as_ref(), 7, true),
            volume("Extra", "/Volumes/Extra".as_ref(), 7, true),
        ];
        assert_eq!(
            ready(&booted_from_external[1], &booted_from_external, &mut client),
            Err("Ejecting Extra also ejects External, the startup disk.".into())
        );
        drop(client);
        serving.join().unwrap().unwrap();
    }
}
