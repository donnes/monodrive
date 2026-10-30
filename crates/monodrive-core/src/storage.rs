//! Storage inspection: what takes up space under a folder the user picks,
//! and which files there are copies of each other. A scan reads metadata
//! the way `sizes` does, through its walk; finding duplicates reads file
//! contents, two files at a time. Both block and stop when asked, so
//! callers run them off the UI thread. Nothing here changes the disk: a
//! cleanup is a plan the operation owner checks again before it trashes
//! anything. docs/storage.md records why the numbers mean what they do.

use crate::ops::{self, FileId, Request, Seen, Stamp, disk};
use crate::sizes::{self, Met};
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsString};
use std::fs::{File, Metadata};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// A scan stops after meeting this many files and folders, which bounds
/// what it holds: about 100 bytes each.
pub const MAX_ITEMS: usize = 500_000;
/// Files read at once while looking for duplicates.
const READERS: usize = 2;
/// How much of a file's start, middle and end the first look reads.
const SAMPLE: u64 = 64 * 1024;
const CHUNK: usize = 256 * 1024;
/// How often, in items met or bytes read, `Progress` calls its notify.
const NOTIFY_ITEMS: u64 = 4096;
const NOTIFY_BYTES: u64 = 64 * 1024 * 1024;

/// Shared with whoever runs a scan on another thread: set `cancel` to stop
/// it, and read the counts when `notify` says they've grown.
#[derive(Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    /// Files and folders the scan has met.
    pub items: AtomicU64,
    /// Bytes looking for duplicates has read.
    pub read: AtomicU64,
    notify: Option<Box<dyn Fn() + Send + Sync>>,
}

impl Progress {
    /// Calls `notify` on the scanning thread every few thousand items or
    /// tens of megabytes, so a view can redraw without polling.
    pub fn notifying(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            notify: Some(Box::new(notify)),
            ..Self::default()
        }
    }

    fn add(&self, counter: &AtomicU64, n: u64, every: u64) {
        let before = counter.fetch_add(n, Ordering::Relaxed);
        if let Some(notify) = &self.notify
            && (before + n) / every != before / every
        {
            notify();
        }
    }

    fn stop(&self, deadline: Option<Instant>) -> Option<Stop> {
        if self.cancel.load(Ordering::Relaxed) {
            Some(Stop::Cancelled)
        } else if deadline.is_some_and(|end| Instant::now() >= end) {
            Some(Stop::Deadline)
        } else {
            None
        }
    }
}

/// Why a scan or a search for duplicates stopped before the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
    Deadline,
    /// It met `MAX_ITEMS`.
    Budget,
}

/// What files take up. Each hard-linked file counts once, in the first
/// folder it was met in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Their lengths, as Finder's Size.
    pub bytes: u64,
    /// The blocks they take up.
    pub on_disk: u64,
    /// The part of `on_disk` no APFS clone or snapshot shares: what
    /// trashing each file alone frees once the Trash is emptied. Where a
    /// volume doesn't say, all of `on_disk`.
    pub unshared: u64,
    /// Files, links and special files.
    pub files: u64,
}

impl Usage {
    fn add(&mut self, other: &Usage) {
        self.bytes += other.bytes;
        self.on_disk += other.on_disk;
        self.unshared += other.unshared;
        self.files += other.files;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    /// `None` for the folder scanned.
    pub parent: Option<usize>,
    /// Everything under it.
    pub usage: Usage,
    /// Folders directly inside it, most on disk first, then by path.
    pub children: Vec<usize>,
    /// It, or some of its items, couldn't be read, so its usage is short.
    pub unreadable: bool,
}

/// One of the largest files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Large {
    pub path: PathBuf,
    pub usage: Usage,
}

/// A regular file the scan met, kept small: its folder holds the path.
struct Record {
    folder: u32,
    name: OsString,
    usage: Usage,
    id: FileId,
    modified: (i64, i64),
}

impl Record {
    /// Whether `meta` is still the file the scan met.
    fn same(&self, meta: &Metadata) -> bool {
        FileId::of(meta) == self.id
            && meta.len() == self.usage.bytes
            && (meta.mtime(), meta.mtime_nsec()) == self.modified
    }
}

pub struct Report {
    /// The folder scanned first, its links resolved; its usage is the total.
    pub folders: Vec<Folder>,
    /// Each folder or item that couldn't be read, with macOS's reason.
    pub unreadable: Vec<String>,
    /// `None` when it met everything.
    pub stopped: Option<Stop>,
    files: Vec<Record>,
}

/// Scans `folder`: every file under it, without following links, added up
/// per folder. Stops early when cancelled, at `deadline` or after
/// `MAX_ITEMS`, keeping what it had met. Fails only when `folder` can't be
/// found or listed.
pub fn scan(
    folder: &Path,
    progress: &Progress,
    deadline: Option<Instant>,
) -> Result<Report, String> {
    let root = folder
        .canonicalize()
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    let mut report = Report {
        folders: vec![Folder {
            path: root.clone(),
            parent: None,
            usage: Usage::default(),
            children: Vec::new(),
            unreadable: false,
        }],
        unreadable: Vec::new(),
        stopped: None,
        files: Vec::new(),
    };
    let met = std::cell::Cell::new(0usize);
    let mut stopped = None;
    let mut check = || {
        stopped = stopped
            .or_else(|| progress.stop(deadline))
            .or_else(|| (met.get() >= MAX_ITEMS).then_some(Stop::Budget));
        stopped.is_some()
    };
    let complete = sizes::walk(&root, &sizes::gated(), &mut check, &mut |event| {
        met.set(met.get() + 1);
        progress.add(&progress.items, 1, NOTIFY_ITEMS);
        match event {
            Met::Folder { parent, path } => report.folders.push(Folder {
                path: path.to_path_buf(),
                parent: Some(parent),
                usage: Usage::default(),
                children: Vec::new(),
                unreadable: false,
            }),
            Met::Item { at, name, meta } => {
                let mut usage = Usage {
                    bytes: meta.len(),
                    on_disk: meta.blocks() * 512,
                    unshared: meta.blocks() * 512,
                    files: 1,
                };
                if meta.is_file() {
                    let path = report.folders[at].path.join(&name);
                    if let Some(unshared) = unshared(&path) {
                        usage.unshared = unshared.min(usage.on_disk);
                    }
                    report.files.push(Record {
                        folder: at as u32,
                        name,
                        usage,
                        id: FileId::of(meta),
                        modified: (meta.mtime(), meta.mtime_nsec()),
                    });
                }
                report.folders[at].usage.add(&usage);
            }
            Met::Unreadable { at, path, why } => {
                report.folders[at].unreadable = true;
                report.unreadable.push(format!("{}: {why}", path.display()));
            }
        }
    })?;
    report.stopped = if complete { None } else { stopped };
    // A folder is numbered after the one holding it, so adding each to its
    // parent from the last makes every total whole.
    for at in (1..report.folders.len()).rev() {
        let Some(parent) = report.folders[at].parent else {
            continue;
        };
        let (usage, unreadable) = (report.folders[at].usage, report.folders[at].unreadable);
        report.folders[parent].usage.add(&usage);
        report.folders[parent].unreadable |= unreadable;
        report.folders[parent].children.push(at);
    }
    for at in 0..report.folders.len() {
        let mut children = std::mem::take(&mut report.folders[at].children);
        let folders = &report.folders;
        children.sort_by(|&x, &y| {
            let (x, y) = (&folders[x], &folders[y]);
            y.usage
                .on_disk
                .cmp(&x.usage.on_disk)
                .then(x.path.cmp(&y.path))
        });
        report.folders[at].children = children;
    }
    Ok(report)
}

/// What APFS says deleting the file at `path` would free: the bytes no
/// clone or snapshot shares. `None` where the volume doesn't say.
fn unshared(path: &Path) -> Option<u64> {
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        returned: libc::attribute_set_t,
        private: libc::off_t,
    }
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut asked = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE,
    };
    // SAFETY: all-zero is a valid `Reply`, and getattrlist writes at most
    // its size into it; the path is NUL-terminated and outlives the call.
    let mut reply: Reply = unsafe { std::mem::zeroed() };
    let got = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut asked).cast(),
            (&raw mut reply).cast(),
            size_of::<Reply>(),
            libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_PACK_INVAL_ATTRS,
        )
    };
    let returned = reply.returned;
    if got != 0 || returned.forkattr & libc::ATTR_CMNEXT_PRIVATESIZE == 0 {
        return None;
    }
    u64::try_from(reply.private).ok()
}

impl Report {
    /// The folder scanned.
    pub fn root(&self) -> &Folder {
        &self.folders[0]
    }

    /// The `n` files that take up the most on disk, largest first, then by
    /// length, then in the order the scan met them.
    pub fn largest(&self, n: usize) -> Vec<Large> {
        let mut order: Vec<usize> = (0..self.files.len()).collect();
        let key = |&i: &usize| {
            let usage = &self.files[i].usage;
            (std::cmp::Reverse((usage.on_disk, usage.bytes)), i)
        };
        if n < order.len() {
            order.select_nth_unstable_by_key(n, key);
            order.truncate(n);
        }
        order.sort_unstable_by_key(key);
        order
            .into_iter()
            .map(|i| Large {
                path: self.path(&self.files[i]),
                usage: self.files[i].usage,
            })
            .collect()
    }

    fn path(&self, file: &Record) -> PathBuf {
        self.folders[file.folder as usize].path.join(&file.name)
    }

    /// Groups the files whose size `wanted` takes that have the same
    /// contents. Files of one size are candidates; reading 64 KiB at each
    /// one's start, middle and end splits them; reading them whole, and
    /// finding them unchanged after, verifies them. A file no larger than
    /// its three samples is read whole the first time. Hard links to one
    /// file are that file, never its duplicates. Stops when cancelled or at
    /// `deadline`; a group it hadn't finished reading stays as it was.
    pub fn duplicates(
        &self,
        wanted: &dyn Fn(u64) -> bool,
        progress: &Progress,
        deadline: Option<Instant>,
    ) -> Duplicates {
        let mut by_size: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, file) in self.files.iter().enumerate() {
            if file.usage.bytes > 0 && wanted(file.usage.bytes) {
                by_size.entry(file.usage.bytes).or_default().push(i);
            }
        }
        let mut groups: Vec<(Match, Vec<usize>)> = by_size
            .into_values()
            .map(|mut files| {
                files.sort_by_key(|&i| self.files[i].id);
                files.dedup_by_key(|&mut i| self.files[i].id);
                (Match::Size, files)
            })
            .filter(|(_, files)| files.len() > 1)
            .collect();
        let mut unreadable = Vec::new();
        let mut found: HashMap<usize, (FileId, Stamp)> = HashMap::new();
        let mut stopped = None;
        for from in [Match::Size, Match::Sample] {
            let picked: Vec<usize> = groups
                .iter()
                .filter(|(matched, _)| *matched == from)
                .flat_map(|(_, files)| files.iter().copied())
                .collect();
            let (read, stop) = self.read(&picked, from == Match::Sample, progress, deadline);
            let mut digests: HashMap<usize, blake3::Hash> = HashMap::new();
            let mut failed = HashSet::new();
            for (i, result) in read {
                match result {
                    Ok(read) => {
                        digests.insert(i, read.hash);
                        if let Some(stamp) = read.stamp {
                            found.insert(i, stamp);
                        }
                    }
                    Err(why) => {
                        failed.insert(i);
                        unreadable.push(why);
                    }
                }
            }
            groups = groups
                .into_iter()
                .flat_map(|(matched, mut files)| {
                    if matched != from {
                        return vec![(matched, files)];
                    }
                    files.retain(|i| !failed.contains(i));
                    if files.len() < 2 {
                        return Vec::new();
                    }
                    if files.iter().any(|i| !digests.contains_key(i)) {
                        return vec![(matched, files)];
                    }
                    let small = self.files[files[0]].usage.bytes <= 3 * SAMPLE;
                    let next = if small || from == Match::Sample {
                        Match::Content
                    } else {
                        Match::Sample
                    };
                    split(next, files, &digests)
                })
                .filter(|(_, files)| files.len() > 1)
                .collect();
            if stop.is_some() {
                stopped = stop;
                break;
            }
        }
        let mut groups: Vec<Group> = groups
            .into_iter()
            .map(|(matched, files)| {
                let size = self.files[files[0]].usage.bytes;
                let mut members: Vec<Member> = files
                    .into_iter()
                    .map(|i| Member {
                        path: self.path(&self.files[i]),
                        on_disk: self.files[i].usage.on_disk,
                        unshared: self.files[i].usage.unshared,
                        found: found.get(&i).copied().filter(|_| matched == Match::Content),
                    })
                    .collect();
                members.sort_by(|a, b| a.path.cmp(&b.path));
                Group {
                    size,
                    matched,
                    members,
                }
            })
            .collect();
        groups.sort_by(|a, b| {
            (b.matched == Match::Content)
                .cmp(&(a.matched == Match::Content))
                .then(b.wasted().cmp(&a.wasted()))
                .then(a.members[0].path.cmp(&b.members[0].path))
        });
        unreadable.sort();
        Duplicates {
            groups,
            unreadable,
            stopped: stopped.or(self.stopped),
        }
    }

    /// Hashes each of `picked`, `READERS` at a time, whole or its three
    /// samples, and says why it stopped if it did. What it hadn't read by
    /// then has no result.
    fn read(
        &self,
        picked: &[usize],
        whole: bool,
        progress: &Progress,
        deadline: Option<Instant>,
    ) -> (Hashes, Option<Stop>) {
        let next = AtomicUsize::new(0);
        let (picked, next) = (&picked, &next);
        let results: Vec<(Hashes, Option<Stop>)> = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..READERS)
                .map(|_| {
                    scope.spawn(move || {
                        let mut done = Vec::new();
                        let mut buffer = vec![0; CHUNK];
                        while let Some(&i) = picked.get(next.fetch_add(1, Ordering::Relaxed)) {
                            let file = &self.files[i];
                            let path = self.path(file);
                            match digest(&path, file, whole, &mut buffer, progress, deadline) {
                                Ok(result) => done.push((i, result)),
                                Err(stop) => return (done, Some(stop)),
                            }
                        }
                        (done, None)
                    })
                })
                .collect();
            readers
                .into_iter()
                .map(|reader| reader.join().unwrap_or_default())
                .collect()
        });
        let stop = results.iter().find_map(|(_, stop)| *stop);
        (
            results.into_iter().flat_map(|(done, _)| done).collect(),
            stop,
        )
    }
}

/// Each file read, by its place in `Report::files`, with its hash or why
/// it has none.
type Hashes = Vec<(usize, Result<Hashed, String>)>;

/// One file's hash, and its stamp before it was read whole.
struct Hashed {
    hash: blake3::Hash,
    stamp: Option<(FileId, Stamp)>,
}

/// Splits `files` by their digests into groups that match as `matched`.
/// Files without one have dropped out.
fn split(
    matched: Match,
    files: Vec<usize>,
    digests: &HashMap<usize, blake3::Hash>,
) -> Vec<(Match, Vec<usize>)> {
    let mut by: HashMap<blake3::Hash, Vec<usize>> = HashMap::new();
    for i in files {
        if let Some(hash) = digests.get(&i) {
            by.entry(*hash).or_default().push(i);
        }
    }
    by.into_values().map(|files| (matched, files)).collect()
}

/// Reads `file` at `path`, whole or its three samples, into one hash.
/// `Err` once stopped; `Ok(Err)` when it can't be read or isn't the file
/// the scan met, with why.
fn digest(
    path: &Path,
    file: &Record,
    whole: bool,
    buffer: &mut [u8],
    progress: &Progress,
    deadline: Option<Instant>,
) -> Result<Result<Hashed, String>, Stop> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let changed = || format!("{} changed since the scan; scan again.", path.display());
    let size = file.usage.bytes;
    let whole = whole || size <= 3 * SAMPLE;
    if let Some(stop) = progress.stop(deadline) {
        return Err(stop);
    }
    // Taken first, so an edit while it's read shows as a change.
    let stamp = match whole.then(|| disk::stamp(path)).transpose() {
        Ok(stamp) => stamp,
        Err(e) => return Ok(Err(fail(e))),
    };
    let (mut f, meta) = match File::open(path).and_then(|f| Ok((f.metadata()?, f))) {
        Ok((meta, f)) => (f, meta),
        Err(e) => return Ok(Err(fail(e))),
    };
    if !file.same(&meta) || stamp.is_some_and(|(id, _)| id != file.id) {
        return Ok(Err(changed()));
    }
    let mut hasher = blake3::Hasher::new();
    if whole {
        loop {
            if let Some(stop) = progress.stop(deadline) {
                return Err(stop);
            }
            let n = match f.read(buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Ok(Err(fail(e))),
            };
            hasher.update(&buffer[..n]);
            progress.add(&progress.read, n as u64, NOTIFY_BYTES);
        }
    } else {
        let sample = &mut buffer[..SAMPLE as usize];
        for at in [0, size / 2 - SAMPLE / 2, size - SAMPLE] {
            if let Err(e) = f.read_exact_at(sample, at) {
                return Ok(Err(fail(e)));
            }
            hasher.update(sample);
            progress.add(&progress.read, SAMPLE, NOTIFY_BYTES);
        }
    }
    match f.metadata() {
        Ok(now) if file.same(&now) => Ok(Ok(Hashed {
            hash: hasher.finalize(),
            stamp,
        })),
        Ok(_) => Ok(Err(changed())),
        Err(e) => Ok(Err(fail(e))),
    }
}

/// How much two or more files were found to have in common.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// The same size; not read.
    Size,
    /// The same size, start, middle and end; not read whole.
    Sample,
    /// Read whole and found the same: verified, and so offered for cleanup.
    Content,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub path: PathBuf,
    pub on_disk: u64,
    /// What trashing it alone frees once the Trash is emptied: less than
    /// its size when it's an APFS clone or a snapshot holds it.
    pub unshared: u64,
    /// The file and its stamp, as verified, which a cleanup checks again.
    found: Option<(FileId, Stamp)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Each member's length.
    pub size: u64,
    pub matched: Match,
    /// By path.
    pub members: Vec<Member>,
}

impl Group {
    /// The bytes all but one of them hold.
    pub fn wasted(&self) -> u64 {
        self.size * (self.members.len() as u64 - 1)
    }
}

pub struct Duplicates {
    /// Verified groups first, then candidates; each by the space all but
    /// one copy take, most first.
    pub groups: Vec<Group>,
    /// Files left out because they couldn't be read or changed since the
    /// scan, with why.
    pub unreadable: Vec<String>,
    /// Why the scan or the reading stopped early, if it did.
    pub stopped: Option<Stop>,
}

impl Duplicates {
    /// A plan to trash exactly `trash`, each a member of a verified group.
    /// Refused whole when a path isn't one, or when it would leave a group
    /// without a copy.
    pub fn cleanup(&self, trash: &[PathBuf]) -> Result<Cleanup, String> {
        if trash.is_empty() {
            return Err("Name at least one copy to trash.".into());
        }
        let mut problems = Vec::new();
        for path in trash {
            let verified = self.groups.iter().any(|group| {
                group.matched == Match::Content && group.members.iter().any(|m| m.path == *path)
            });
            if !verified {
                problems.push(format!(
                    "{}: not a verified duplicate in this scan.",
                    path.display()
                ));
            }
        }
        let mut cleanup = Cleanup {
            groups: Vec::new(),
            frees: 0,
        };
        for group in self.groups.iter().filter(|g| g.matched == Match::Content) {
            let seen = |member: &Member| {
                member.found.map(|found| Seen {
                    path: member.path.clone(),
                    found,
                })
            };
            let (gone, kept): (Vec<&Member>, Vec<&Member>) =
                group.members.iter().partition(|m| trash.contains(&m.path));
            if gone.is_empty() {
                continue;
            }
            if kept.is_empty() {
                problems.push(format!(
                    "Keep at least one of {}.",
                    group
                        .members
                        .iter()
                        .map(|m| m.path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                continue;
            }
            cleanup.frees += gone.iter().map(|m| m.unshared).sum::<u64>();
            cleanup.groups.push(ops::Copies {
                keep: kept.into_iter().filter_map(seen).collect(),
                trash: gone.into_iter().filter_map(seen).collect(),
            });
        }
        if problems.is_empty() {
            Ok(cleanup)
        } else {
            Err(problems.join("\n"))
        }
    }
}

/// Which verified copies go to the Trash and which stay, as a preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cleanup {
    pub groups: Vec<ops::Copies>,
    /// What trashing them frees once the Trash is emptied, at least: copies
    /// sharing blocks only with each other free those too.
    pub frees: u64,
}

impl Cleanup {
    /// One trash operation for the owner, which refuses each copy that
    /// changed since, and every copy of a group none of whose kept copies
    /// is as it was.
    pub fn request(&self) -> Request {
        Request::TrashDuplicates {
            groups: self.groups.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
