//! ZIP archives: what one holds, read from its central directory without
//! extracting anything; extracting its entries; and compressing items into
//! a new one. The file operations owner runs extraction and compression as
//! jobs; listing changes nothing and runs anywhere.
//!
//! Entry names come from whoever made the archive. They're shown as stored
//! and checked before anything is written: no absolute paths, no `..`, no
//! two entries at one path, no link leading out of what it's extracted
//! with, and no writing through a link, whether the archive made it or it
//! was already there.

use crate::entries::Kind;
use flate2::bufread::DeflateDecoder;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;
use zip::write::FullFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// Reading an archive stops after this many entries, so a huge one can't
/// take the memory of whoever lists it.
pub const MAX_ENTRIES: usize = 200_000;
/// Deflate shrinks data by at most about 1032 to 1. An archive that says it
/// expands by more reuses its own bytes, as zip bombs do.
const MAX_RATIO: u64 = 1_100;
/// macOS's PATH_MAX: no link target is longer.
const MAX_LINK: u64 = 1024;
const CHUNK: usize = 64 * 1024;

const END: &[u8] = b"PK\x05\x06";
const END64: &[u8] = b"PK\x06\x06";
const LOCATOR: &[u8] = b"PK\x06\x07";
const CENTRAL: &[u8] = b"PK\x01\x02";
const LOCAL: &[u8] = b"PK\x03\x04";
const ZIP64: u16 = 0x0001;
const TIMESTAMP: u16 = 0x5455;

/// A ZIP archive's entries, as its central directory lists them.
#[derive(Clone, Debug)]
pub struct Archive {
    pub path: PathBuf,
    /// In the order the archive lists them, at most `MAX_ENTRIES`.
    pub entries: Vec<Entry>,
    /// How many entries the archive says it has: more than `entries` holds
    /// when it has more than Monodrive reads.
    pub total: u64,
    len: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// As the archive stores it, which may not be a safe path. Folders end
    /// in `/`.
    pub name: String,
    /// File, folder or link; never `Other`.
    pub kind: Kind,
    /// Uncompressed, as the archive says.
    pub size: u64,
    pub compressed: u64,
    pub modified: Option<SystemTime>,
    pub encrypted: bool,
    method: u16,
    crc: u32,
    /// Of its local header.
    offset: u64,
    mode: Option<u32>,
}

impl Entry {
    /// Why Monodrive won't extract this entry, if it won't.
    pub fn problem(&self) -> Option<String> {
        if let Err(why) = parts(&self.name) {
            return Some(why);
        }
        if self.encrypted {
            return Some("encrypted, and Monodrive doesn't extract encrypted entries".into());
        }
        let method = match self.method {
            0 | 8 => return None,
            _ if self.kind == Kind::Dir => return None,
            9 => "Deflate64".into(),
            12 => "bzip2".into(),
            14 => "LZMA".into(),
            93 => "Zstandard".into(),
            95 => "XZ".into(),
            98 => "PPMd".into(),
            n => format!("method {n}"),
        };
        Some(format!(
            "compressed with {method}, which Monodrive doesn't extract"
        ))
    }
}

/// `name` with control characters escaped, so it prints on one line.
pub fn printable(name: &str) -> Cow<'_, str> {
    if name.chars().any(char::is_control) {
        Cow::Owned(name.escape_debug().to_string())
    } else {
        Cow::Borrowed(name)
    }
}

/// A name's parts, without empty ones or `.`.
fn split(name: &str) -> Vec<&str> {
    name.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

/// A name's parts, or why it isn't a path inside the destination.
fn parts(name: &str) -> Result<Vec<&str>, String> {
    if name.starts_with('/') {
        return Err("an absolute path, which Monodrive doesn't extract".into());
    }
    if name.contains('\0') {
        return Err("a name with a NUL character, which macOS doesn't allow".into());
    }
    let parts = split(name);
    if parts.contains(&"..") {
        return Err("a path that climbs out with .., which Monodrive doesn't extract".into());
    }
    if parts.is_empty() {
        return Err("an entry without a name".into());
    }
    Ok(parts)
}

impl Archive {
    /// Reads the central directory of the archive at `path`, at most
    /// `MAX_ENTRIES` of it. Nothing is extracted, and nothing is allocated
    /// by the sizes the archive claims.
    pub fn open(path: &Path) -> Result<Self, String> {
        let at = |why: String| format!("{}: {why}", path.display());
        let mut file = File::open(path).map_err(|e| at(e.to_string()))?;
        let len = file.metadata().map_err(|e| at(e.to_string()))?.len();
        let end = find_end(&mut file, len).map_err(at)?;
        let mut reader = BufReader::with_capacity(CHUNK, file);
        let damaged = |e: io::Error| at(format!("isn't a whole ZIP archive: {e}"));
        reader.seek(SeekFrom::Start(end.offset)).map_err(damaged)?;
        let count = end.entries.min(MAX_ENTRIES as u64);
        // Each entry takes at least 46 bytes, whatever the count says.
        let mut entries = Vec::with_capacity(count.min(end.size / 46) as usize);
        for _ in 0..count {
            entries.push(read_entry(&mut reader).map_err(damaged)?);
        }
        Ok(Self {
            path: path.to_path_buf(),
            entries,
            total: end.entries,
            len,
        })
    }

    /// The entries inside `folder`, a folder in the archive; all of them
    /// when it's empty.
    pub fn under<'a>(&'a self, folder: &'a str) -> impl Iterator<Item = &'a Entry> {
        let want = split(folder);
        self.entries.iter().filter(move |entry| {
            want.is_empty() || {
                let parts = split(&entry.name);
                parts.len() > want.len() && parts.starts_with(&want)
            }
        })
    }

    /// The items extracting `selected` makes in the destination, as entry
    /// names: one per selected entry or folder, or one per top-level name
    /// when none are. Refuses the whole extraction when the archive has
    /// more entries than Monodrive reads, when an entry it takes can't be
    /// extracted, when it needs more than the `free` bytes, or when the
    /// archive claims to expand by more than deflate can.
    pub(crate) fn plan(
        &self,
        selected: &[String],
        free: Option<u64>,
    ) -> Result<Vec<String>, String> {
        let at = self.path.display();
        if self.total > self.entries.len() as u64 {
            return Err(format!(
                "{at}: has {} entries, and Monodrive extracts archives of up to {MAX_ENTRIES}.",
                self.total
            ));
        }
        let mut items: Vec<Vec<&str>> = Vec::new();
        if selected.is_empty() {
            let mut seen = HashSet::new();
            for entry in &self.entries {
                if let Ok(parts) = parts(&entry.name)
                    && seen.insert(parts[0])
                {
                    items.push(vec![parts[0]]);
                }
            }
        } else {
            for name in selected {
                let want = split(name);
                let found = !want.is_empty()
                    && self.entries.iter().any(|entry| {
                        parts(&entry.name).is_ok_and(|parts| parts.starts_with(&want))
                    });
                if !found {
                    return Err(format!("{at}: has no entry {}.", printable(name)));
                }
                items.push(want);
            }
            // One inside another comes out with it.
            let inside = |a: &[&str], b: &[&str]| a.len() > b.len() && a.starts_with(b);
            let kept: Vec<Vec<&str>> = items
                .iter()
                .filter(|item| !items.iter().any(|other| inside(item, other)))
                .cloned()
                .collect();
            items = Vec::new();
            for item in kept {
                if !items.contains(&item) {
                    items.push(item);
                }
            }
        }
        let taken = |entry: &&Entry| {
            selected.is_empty()
                || parts(&entry.name)
                    .is_ok_and(|parts| items.iter().any(|item| parts.starts_with(item)))
        };
        let mut bytes = 0u64;
        let mut problems = Vec::new();
        for entry in self.entries.iter().filter(taken) {
            bytes = bytes.saturating_add(entry.size);
            if let Some(why) = entry.problem() {
                problems.push(format!("{} is {why}.", printable(&entry.name)));
            }
        }
        if !problems.is_empty() {
            let more = problems.len().saturating_sub(5);
            problems.truncate(5);
            if more > 0 {
                problems.push(format!("And {more} more."));
            }
            return Err(format!(
                "Nothing was extracted from {at}:\n{}",
                problems.join("\n")
            ));
        }
        if let Some(free) = free
            && bytes > free
        {
            return Err(format!(
                "{at}: needs {bytes} bytes once extracted, more than the {free} free there."
            ));
        }
        let claimed = self
            .entries
            .iter()
            .fold(0u64, |sum, entry| sum.saturating_add(entry.size));
        if claimed > self.len.saturating_mul(MAX_RATIO) {
            return Err(format!(
                "{at}: claims to expand from {} to {claimed} bytes, more than ZIP compression can, \
                 as zip bombs do. Nothing was extracted.",
                self.len
            ));
        }
        Ok(items.into_iter().map(|item| item.join("/")).collect())
    }

    /// The bytes extracting `item` writes, as the archive says.
    pub(crate) fn bytes(&self, item: &str) -> u64 {
        self.taking(item)
            .fold(0, |sum, (entry, _)| sum.saturating_add(entry.size))
    }

    /// The entries under `item`, with their parts below it.
    fn taking<'a>(&'a self, item: &str) -> impl Iterator<Item = (&'a Entry, Vec<&'a str>)> {
        let want: Vec<String> = split(item).into_iter().map(str::to_owned).collect();
        self.entries.iter().filter_map(move |entry| {
            let parts = parts(&entry.name).ok()?;
            (parts.len() >= want.len() && parts.iter().zip(&want).all(|(a, b)| a == b))
                .then(|| (entry, parts[want.len()..].to_vec()))
        })
    }

    /// Extracts `item`, an entry or a folder of them, to the new path
    /// `to`, never through a link, on a volume that `ignores_case` or not.
    /// The destination's free space and the entries' problems were checked
    /// when the extraction was planned; the rest is checked here. Two
    /// entries at one path refuse it before anything is written. Each entry
    /// writes no more than the archive says it holds, and must match its
    /// checksum. `done` counts the bytes written; `cancel` stops partway,
    /// leaving what's at `to` unfinished.
    pub(crate) fn extract(
        &self,
        item: &str,
        to: &Path,
        ignores_case: bool,
        cancel: &AtomicBool,
        done: &AtomicU64,
    ) -> Result<(), String> {
        let taken: Vec<(&Entry, Vec<&str>)> = self.taking(item).collect();
        let keys: Vec<Vec<String>> = taken
            .iter()
            .map(|(_, rest)| key(rest, ignores_case))
            .collect();
        let mut first: HashMap<&[String], &str> = HashMap::new();
        let twice: Vec<String> = taken
            .iter()
            .zip(&keys)
            .filter_map(|((entry, _), key)| {
                let earlier = first.insert(key, &entry.name)?;
                Some(format!(
                    "{} and {}",
                    printable(earlier),
                    printable(&entry.name)
                ))
            })
            .take(5)
            .collect();
        if !twice.is_empty() {
            return Err(format!(
                "{} are the same path once extracted, so nothing of {} was.",
                twice.join(", "),
                printable(item)
            ));
        }
        let links: HashSet<Vec<String>> = taken
            .iter()
            .zip(&keys)
            .filter(|((entry, _), _)| entry.kind == Kind::Link)
            .map(|(_, key)| key.clone())
            .collect();
        let own = taken
            .iter()
            .find(|(_, rest)| rest.is_empty())
            .map(|(entry, _)| *entry);
        let inside: Vec<&(&Entry, Vec<&str>)> =
            taken.iter().filter(|(_, rest)| !rest.is_empty()).collect();
        let (Some(folder), Some(base)) = (to.parent(), to.file_name()) else {
            return Err(format!("{} can't be extracted to.", to.display()));
        };
        let folder = open_dir(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        let base = CString::new(base.as_bytes()).map_err(|e| e.to_string())?;
        let mut writer = Writer {
            archive: self,
            links,
            ignores_case,
            file: File::open(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?,
            buffer: vec![0; CHUNK],
            cancel,
            done,
        };
        let shown = printable(item);
        if inside.is_empty() {
            let entry = own.ok_or_else(|| format!("{shown} isn't in the archive."))?;
            if entry.kind == Kind::Dir {
                let dir =
                    make_dir(folder.as_fd(), &base, true).map_err(|e| format!("{shown}: {e}"))?;
                return finish(dir.as_fd(), entry).map_err(|e| format!("{shown}: {e}"));
            }
            return writer.write(entry, folder.as_fd(), &base, &[]);
        }
        if own.is_some_and(|entry| entry.kind != Kind::Dir) {
            return Err(format!(
                "{shown} is both a file and a folder in the archive."
            ));
        }
        let root = make_dir(folder.as_fd(), &base, true).map_err(|e| format!("{shown}: {e}"))?;
        // Entries come grouped by folder, so the last folder opened is
        // usually the next one's too.
        let mut here: (Vec<&str>, OwnedFd) = (Vec::new(), dup(&root)?);
        let mut folders = Vec::new();
        for (entry, rest) in inside {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let (dirs, name) = rest.split_at(rest.len() - 1);
            if here.0 != dirs {
                here = (dirs.to_vec(), descend(&root, dirs, &entry.name)?);
            }
            let name = CString::new(name[0]).map_err(|e| e.to_string())?;
            if entry.kind == Kind::Dir {
                make_dir(here.1.as_fd(), &name, false)
                    .map_err(|e| format!("{} {}", printable(&entry.name), blocked(&e)))?;
                folders.push((rest, *entry));
            } else {
                writer.write(entry, here.1.as_fd(), &name, rest)?;
            }
        }
        // Folders last, deepest first: writing inside one changes its time,
        // and one that's read-only can't be written into.
        folders.sort_by_key(|(rest, _)| std::cmp::Reverse(rest.len()));
        for (rest, entry) in folders {
            let dir = descend(&root, rest, &entry.name)?;
            finish(dir.as_fd(), entry).map_err(|e| format!("{}: {e}", printable(&entry.name)))?;
        }
        match own {
            Some(entry) => finish(root.as_fd(), entry).map_err(|e| format!("{shown}: {e}")),
            None => Ok(()),
        }
    }
}

/// Where the central directory is, and how many entries it lists.
struct End {
    entries: u64,
    size: u64,
    offset: u64,
}

/// Finds the end of central directory record, and the ZIP64 one after it
/// when the archive needs one.
fn find_end(file: &mut File, len: u64) -> Result<End, String> {
    let tail = len.min(22 + 0xFFFF);
    let mut bytes = vec![0; tail as usize];
    file.seek(SeekFrom::Start(len - tail))
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    let not_zip = || "isn't a ZIP archive.".to_string();
    let at = (0..=bytes.len().checked_sub(22).ok_or_else(not_zip)?)
        .rev()
        .find(|&i| {
            &bytes[i..i + 4] == END && i + 22 + le(&bytes, i + 20, 2) as usize <= bytes.len()
        })
        .ok_or_else(not_zip)?;
    let record = &bytes[at..];
    let mut disks = (le(record, 4, 2), le(record, 6, 2));
    let mut end = End {
        entries: le(record, 10, 2),
        size: le(record, 12, 4),
        offset: le(record, 16, 4),
    };
    if end.entries == 0xFFFF || end.size == 0xFFFF_FFFF || end.offset == 0xFFFF_FFFF {
        let found = (len - tail + at as u64)
            .checked_sub(20)
            .ok_or_else(not_zip)?;
        let mut locator = [0; 20];
        file.seek(SeekFrom::Start(found))
            .and_then(|_| file.read_exact(&mut locator))
            .map_err(|e| e.to_string())?;
        if &locator[..4] == LOCATOR {
            let mut record = [0; 56];
            file.seek(SeekFrom::Start(le(&locator, 8, 8)))
                .and_then(|_| file.read_exact(&mut record))
                .map_err(|e| format!("its ZIP64 record is missing: {e}"))?;
            if &record[..4] != END64 {
                return Err("its ZIP64 record is missing.".into());
            }
            disks = (le(&record, 16, 4), le(&record, 20, 4));
            end = End {
                entries: le(&record, 32, 8),
                size: le(&record, 40, 8),
                offset: le(&record, 48, 8),
            };
        }
    }
    if disks != (0, 0) {
        return Err("is split across several files, which Monodrive doesn't read.".into());
    }
    if end
        .offset
        .checked_add(end.size)
        .is_none_or(|stop| stop > len)
    {
        return Err("isn't a whole ZIP archive: its directory is past its end.".into());
    }
    Ok(end)
}

/// The little-endian number `width` bytes long at `at`.
fn le(bytes: &[u8], at: usize, width: usize) -> u64 {
    bytes[at..at + width]
        .iter()
        .rev()
        .fold(0, |n, b| n << 8 | u64::from(*b))
}

/// One central directory entry.
fn read_entry(reader: &mut impl Read) -> io::Result<Entry> {
    let mut fixed = [0; 46];
    reader.read_exact(&mut fixed)?;
    if &fixed[..4] != CENTRAL {
        return Err(io::Error::other("an entry is damaged"));
    }
    let field = |at, width| le(&fixed, at, width);
    let mut name = vec![0; field(28, 2) as usize];
    let mut extra = vec![0; field(30, 2) as usize];
    let mut comment = vec![0; field(32, 2) as usize];
    reader.read_exact(&mut name)?;
    reader.read_exact(&mut extra)?;
    reader.read_exact(&mut comment)?;
    let (mut size, mut compressed, mut offset) = (field(24, 4), field(20, 4), field(42, 4));
    let mut stamp = None;
    let mut rest = extra.as_slice();
    while rest.len() >= 4 {
        let (id, len) = (le(rest, 0, 2) as u16, le(rest, 2, 2) as usize);
        let Some(data) = rest.get(4..4 + len) else {
            break;
        };
        match id {
            ZIP64 => {
                // Only the fields the header left at their maximum, in order.
                let mut at = 0;
                for value in [&mut size, &mut compressed, &mut offset] {
                    if *value == 0xFFFF_FFFF && at + 8 <= data.len() {
                        *value = le(data, at, 8);
                        at += 8;
                    }
                }
            }
            TIMESTAMP if data.len() >= 5 && data[0] & 1 != 0 => {
                stamp = Some(le(data, 1, 4) as u32 as i32);
            }
            _ => {}
        }
        rest = &rest[4 + len..];
    }
    let name = match String::from_utf8(name) {
        Ok(name) => name,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    };
    let external = field(38, 4) as u32;
    // Made on Unix or macOS: the mode is in the top half.
    let mode = matches!(field(4, 2) >> 8, 3 | 19)
        .then_some(external >> 16)
        .filter(|mode| *mode != 0);
    let format = mode.map(|mode| mode & u32::from(libc::S_IFMT));
    let kind = if name.ends_with('/') || format == Some(u32::from(libc::S_IFDIR)) {
        Kind::Dir
    } else if format == Some(u32::from(libc::S_IFLNK)) {
        Kind::Link
    } else if mode.is_none() && external & 0x10 != 0 {
        Kind::Dir
    } else {
        Kind::File
    };
    let modified = match stamp {
        Some(seconds) => Some(at(i64::from(seconds))),
        None => from_dos(field(14, 2) as u16, field(12, 2) as u16),
    };
    Ok(Entry {
        name,
        kind,
        size,
        compressed,
        modified,
        encrypted: field(8, 2) & 1 != 0,
        method: field(10, 2) as u16,
        crc: field(16, 4) as u32,
        offset,
        mode,
    })
}

fn at(seconds: i64) -> SystemTime {
    match u64::try_from(seconds) {
        Ok(after) => UNIX_EPOCH + Duration::from_secs(after),
        Err(_) => UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs()),
    }
}

/// A DOS date and time, which ZIP stores in local time.
fn from_dos(date: u16, time: u16) -> Option<SystemTime> {
    if date == 0 {
        return None;
    }
    // SAFETY: an all-zero tm is valid, and mktime only reads and fixes it.
    let seconds = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = 80 + i32::from(date >> 9);
        tm.tm_mon = i32::from((date >> 5) & 0xF) - 1;
        tm.tm_mday = i32::from(date & 0x1F);
        tm.tm_hour = i32::from(time >> 11);
        tm.tm_min = i32::from((time >> 5) & 0x3F);
        tm.tm_sec = i32::from(time & 0x1F) * 2;
        tm.tm_isdst = -1;
        libc::mktime(&mut tm)
    };
    (seconds != -1).then(|| at(seconds))
}

/// `time` as a DOS date and time in local time; 1980 for what it can't hold.
fn to_dos(seconds: i64) -> DateTime {
    // SAFETY: an all-zero tm is valid, and localtime_r writes only into it.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::localtime_r(&seconds, &mut tm).is_null()).then_some(tm)
    };
    tm.and_then(|tm| {
        DateTime::from_date_and_time(
            u16::try_from(tm.tm_year + 1900).ok()?,
            u8::try_from(tm.tm_mon + 1).ok()?,
            u8::try_from(tm.tm_mday).ok()?,
            u8::try_from(tm.tm_hour).ok()?,
            u8::try_from(tm.tm_min).ok()?,
            u8::try_from(tm.tm_sec).ok()?,
        )
        .ok()
    })
    .unwrap_or_default()
}

/// A path's parts as a volume compares names: whatever their Unicode
/// normalization, and whatever their case where it `ignores_case`.
fn key(parts: &[&str], ignores_case: bool) -> Vec<String> {
    parts
        .iter()
        .map(|part| match ignores_case {
            true => part.to_lowercase().nfd().collect(),
            false => part.nfd().collect(),
        })
        .collect()
}

/// Writes an archive's files and links where `extract` puts them.
struct Writer<'a> {
    archive: &'a Archive,
    /// The item's links, keyed as its volume compares names.
    links: HashSet<Vec<String>>,
    ignores_case: bool,
    file: File,
    buffer: Vec<u8>,
    cancel: &'a AtomicBool,
    done: &'a AtomicU64,
}

impl Writer<'_> {
    /// Writes the file or link `entry` as `name` in `dir`, at the parts `at`
    /// inside the item, never replacing or following anything there.
    fn write(
        &mut self,
        entry: &Entry,
        dir: BorrowedFd,
        name: &CStr,
        at: &[&str],
    ) -> Result<(), String> {
        let shown = printable(&entry.name);
        if let Some(why) = entry.problem() {
            return Err(format!("{shown} is {why}."));
        }
        if entry.kind == Kind::Link {
            if entry.size > MAX_LINK {
                return Err(format!(
                    "{shown} is a link with a target too long to be real."
                ));
            }
            let mut target = Vec::new();
            self.pour(entry, &mut target)?;
            if !self.stays(&target, at) {
                return Err(format!(
                    "{shown} is a link that leads out of what's extracted with it, or through \
                     another link, so it wasn't extracted."
                ));
            }
            let target = CString::new(target).map_err(|e| e.to_string())?;
            // SAFETY: both are NUL-terminated and outlive the call, and
            // `dir` is open.
            if unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) } != 0 {
                return Err(format!("{shown} {}", blocked(&io::Error::last_os_error())));
            }
            if let Some(modified) = entry.modified {
                let times = [omit(), timespec(modified)];
                // SAFETY: as above; `times` holds two timespecs.
                unsafe {
                    libc::utimensat(
                        dir.as_raw_fd(),
                        name.as_ptr(),
                        times.as_ptr(),
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                };
            }
            return Ok(());
        }
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `name` is NUL-terminated and `dir` is open; the descriptor
        // is owned here.
        let fd =
            unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, 0o600 as libc::c_uint) };
        if fd < 0 {
            return Err(format!("{shown} {}", blocked(&io::Error::last_os_error())));
        }
        let out = File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let mut out = BufWriter::with_capacity(CHUNK, out);
        self.pour(entry, &mut out)?;
        let out = out
            .into_inner()
            .map_err(|e| format!("{shown}: {}", e.error()))?;
        finish(out.as_fd(), entry).map_err(|e| format!("{shown}: {e}"))
    }

    /// Decompresses `entry` into `out`, refusing more bytes than it claims
    /// or a checksum that doesn't match.
    fn pour(&mut self, entry: &Entry, out: &mut impl Write) -> Result<(), String> {
        let shown = printable(&entry.name);
        let damaged = |e: io::Error| format!("{shown} is damaged: {e}");
        let mut header = [0; 30];
        self.file
            .seek(SeekFrom::Start(entry.offset))
            .and_then(|_| self.file.read_exact(&mut header))
            .map_err(damaged)?;
        if &header[..4] != LOCAL {
            return Err(format!("{shown} is damaged: its header is missing."));
        }
        let start = entry.offset + 30 + le(&header, 26, 2) + le(&header, 28, 2);
        if start
            .checked_add(entry.compressed)
            .is_none_or(|stop| stop > self.archive.len)
        {
            return Err(format!(
                "{shown} is damaged: it runs past the archive's end."
            ));
        }
        self.file.seek(SeekFrom::Start(start)).map_err(damaged)?;
        let raw = BufReader::with_capacity(CHUNK, (&self.file).take(entry.compressed));
        let mut data: Box<dyn Read + '_> = match entry.method {
            8 => Box::new(DeflateDecoder::new(raw)),
            _ => Box::new(raw),
        };
        let mut crc = crc32fast::Hasher::new();
        let mut written = 0u64;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let n = data.read(&mut self.buffer).map_err(damaged)?;
            if n == 0 {
                break;
            }
            written += n as u64;
            if written > entry.size {
                return Err(format!(
                    "{shown} holds more than the archive says, as zip bombs do, so extraction stopped."
                ));
            }
            crc.update(&self.buffer[..n]);
            out.write_all(&self.buffer[..n])
                .map_err(|e| format!("{shown}: {e}"))?;
            self.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        if written != entry.size || crc.finalize() != entry.crc {
            return Err(format!(
                "{shown} is damaged: it doesn't match the archive's size or checksum."
            ));
        }
        Ok(())
    }

    /// Whether a link at `at` inside the item, to `target`, leads somewhere
    /// inside the item. A relative target may climb with `..` as long as it
    /// stays inside, but it can't pass through one of the item's links,
    /// whose own target would move it: what `..` means after one isn't what
    /// it reads as. A link at the top of the item would lead among the
    /// destination's own items, which the archive doesn't decide.
    fn stays(&self, target: &[u8], at: &[&str]) -> bool {
        let (Ok(target), Some((_, folder))) = (std::str::from_utf8(target), at.split_last()) else {
            return false;
        };
        if target.starts_with('/') || target.contains('\0') {
            return false;
        }
        let parts: Vec<&str> = split(target);
        let mut here = key(folder, self.ignores_case);
        for (i, part) in parts.iter().enumerate() {
            if *part == ".." {
                if here.pop().is_none() {
                    return false;
                }
                continue;
            }
            here.extend(key(&[part], self.ignores_case));
            if i + 1 < parts.len() && self.links.contains(&here) {
                return false;
            }
        }
        true
    }
}

/// What stopped an entry being written: a link or file in the way.
fn blocked(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) => {
            "goes through a link or a file, so it wasn't extracted.".into()
        }
        Some(libc::EEXIST) => "appears twice in the archive.".into(),
        _ => format!("couldn't be written: {e}"),
    }
}

/// Opens the folder `dirs` below `root`, making what's missing, never
/// through a link.
fn descend(root: &OwnedFd, dirs: &[&str], entry: &str) -> Result<OwnedFd, String> {
    let mut here = dup(root)?;
    for dir in dirs {
        let name = CString::new(*dir).map_err(|e| e.to_string())?;
        here = make_dir(here.as_fd(), &name, false)
            .map_err(|e| format!("{} {}", printable(entry), blocked(&e)))?;
    }
    Ok(here)
}

/// Makes the folder `name` in `dir`, unless it's there and needn't be
/// `fresh`, and opens it, refusing a link or a file there.
fn make_dir(dir: BorrowedFd, name: &CStr, fresh: bool) -> io::Result<OwnedFd> {
    // SAFETY: `name` is NUL-terminated and `dir` is open.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
        let e = io::Error::last_os_error();
        if fresh || e.raw_os_error() != Some(libc::EEXIST) {
            return Err(e);
        }
    }
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: as above; the descriptor is owned here.
    match unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) } {
        -1 => Err(io::Error::last_os_error()),
        fd => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    }
}

/// The destination folder itself, which may be a link the person chose.
fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC;
    // SAFETY: a NUL-terminated path; the descriptor is owned here.
    match unsafe { libc::open(path.as_ptr(), flags) } {
        -1 => Err(io::Error::last_os_error()),
        fd => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
    }
}

fn dup(fd: &OwnedFd) -> Result<OwnedFd, String> {
    fd.try_clone().map_err(|e| e.to_string())
}

/// Gives what `fd` has open the entry's permissions and modification time.
fn finish(fd: BorrowedFd, entry: &Entry) -> io::Result<()> {
    if let Some(mode) = entry.mode {
        // SAFETY: `fd` is open.
        if unsafe { libc::fchmod(fd.as_raw_fd(), (mode & 0o777) as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    if let Some(modified) = entry.modified {
        let times = [omit(), timespec(modified)];
        // SAFETY: `fd` is open and `times` holds two timespecs.
        if unsafe { libc::futimens(fd.as_raw_fd(), times.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn omit() -> libc::timespec {
    libc::timespec {
        tv_sec: 0,
        tv_nsec: libc::UTIME_OMIT,
    }
}

fn timespec(time: SystemTime) -> libc::timespec {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => 0i64
            .checked_sub_unsigned(before.duration().as_secs())
            .unwrap_or(i64::MIN),
    };
    libc::timespec {
        tv_sec: seconds,
        tv_nsec: 0,
    }
}

/// Compresses `items`, all in one folder, into a new archive at `to`, each
/// under its own name. Folders go in whole and links as links, never
/// followed; sockets, pipes and devices hold no data and are left out. Each
/// entry keeps its permissions and modification time. `done` counts the
/// bytes read; `cancel` stops partway, leaving what's at `to` unfinished.
pub(crate) fn create(
    items: &[PathBuf],
    to: &Path,
    cancel: &AtomicBool,
    done: &AtomicU64,
) -> Result<(), String> {
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| format!("{}: {e}", to.display()))?;
    let mut zip = ZipWriter::new(BufWriter::with_capacity(CHUNK, file));
    let mut buffer = vec![0; CHUNK];
    let mut written = 0u64;
    for item in items {
        let name = item
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{}: can't be named in an archive.", item.display()))?;
        let mut pending = vec![(item.clone(), name.to_string())];
        while let Some((path, name)) = pending.pop() {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
            let meta = std::fs::symlink_metadata(&path).map_err(|e| fail(&e))?;
            let options = options(&meta).map_err(|e| fail(&e))?;
            let kind = meta.file_type();
            if kind.is_dir() {
                zip.add_directory(name.as_str(), options)
                    .map_err(|e| fail(&e))?;
                let mut inside = std::fs::read_dir(&path)
                    .and_then(|items| {
                        items
                            .map(|item| item.map(|item| item.file_name()))
                            .collect::<io::Result<Vec<_>>>()
                    })
                    .map_err(|e| fail(&e))?;
                // Popped in name order, each folder before what it holds.
                inside.sort_unstable_by(|a, b| b.cmp(a));
                for child in inside {
                    let label = child
                        .to_str()
                        .ok_or_else(|| fail(&"a name inside can't be stored"))?;
                    pending.push((path.join(&child), format!("{name}/{label}")));
                }
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&path).map_err(|e| fail(&e))?;
                let target = target
                    .to_str()
                    .ok_or_else(|| fail(&"its target can't be stored"))?;
                zip.add_symlink(name.as_str(), target, options)
                    .map_err(|e| fail(&e))?;
            } else if kind.is_file() {
                let options = options
                    .compression_method(CompressionMethod::Deflated)
                    .large_file(meta.len() >= 0xFFFF_FFFF);
                zip.start_file(name.as_str(), options)
                    .map_err(|e| fail(&e))?;
                let mut source = File::open(&path).map_err(|e| fail(&e))?;
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return Err("Cancelled.".into());
                    }
                    let n = source.read(&mut buffer).map_err(|e| fail(&e))?;
                    if n == 0 {
                        break;
                    }
                    zip.write_all(&buffer[..n]).map_err(|e| fail(&e))?;
                    done.fetch_add(n as u64, Ordering::Relaxed);
                }
            } else {
                continue;
            }
            written += 1;
        }
    }
    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", to.display());
    zip.finish()
        .map_err(|e| fail(&e))?
        .into_inner()
        .map_err(|e| fail(e.error()))?;
    let made = Archive::open(to)?;
    if made.total != written {
        let why = format!("lists {} entries instead of {written}", made.total);
        return Err(fail(&why));
    }
    Ok(())
}

/// An entry's permissions and time: in local time as ZIP keeps it, and to
/// the second in an extended timestamp where it fits.
fn options(meta: &std::fs::Metadata) -> zip::result::ZipResult<FullFileOptions<'static, 'static>> {
    let mut options = FullFileOptions::default()
        .last_modified_time(to_dos(meta.mtime()))
        .unix_permissions(meta.mode() & 0o777);
    if let Ok(seconds) = i32::try_from(meta.mtime()) {
        let mut stamp = vec![1];
        stamp.extend_from_slice(&seconds.to_le_bytes());
        options.add_extra_field(TIMESTAMP, stamp, false)?;
    }
    Ok(options)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// What a test archive holds at one name.
    pub enum Item<'a> {
        File(&'a [u8]),
        Dir,
        Link(&'a str),
    }

    /// Writes an archive of `items` to `path`, names as given, unchecked.
    pub fn zip(path: &Path, items: &[(&str, Item)]) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let options = FullFileOptions::default().unix_permissions(0o644);
        for (name, item) in items {
            match item {
                Item::File(data) => {
                    zip.start_file(*name, options.clone()).unwrap();
                    zip.write_all(data).unwrap();
                }
                Item::Dir => zip
                    .add_directory(*name, FullFileOptions::default())
                    .unwrap(),
                Item::Link(target) => zip.add_symlink(*name, *target, options.clone()).unwrap(),
            }
        }
        zip.finish().unwrap();
    }

    /// Changes the central directory entry for `name` in the archive at
    /// `path`: `fixed` is its 46-byte header, `extra` its extra fields.
    pub fn patch(path: &Path, name: &str, change: impl FnOnce(&mut [u8], &mut [u8])) {
        let mut bytes = std::fs::read(path).unwrap();
        let mut at = 0;
        loop {
            let found = bytes[at..]
                .windows(4)
                .position(|w| w == CENTRAL)
                .expect("the entry is in the archive");
            at += found;
            let (n, m) = (
                le(&bytes, at + 28, 2) as usize,
                le(&bytes, at + 30, 2) as usize,
            );
            if &bytes[at + 46..at + 46 + n] == name.as_bytes() {
                let (fixed, rest) = bytes[at..].split_at_mut(46);
                change(fixed, &mut rest[n..n + m]);
                break;
            }
            at += 4;
        }
        std::fs::write(path, bytes).unwrap();
    }

    pub fn set(bytes: &mut [u8], at: usize, value: u64, width: usize) {
        bytes[at..at + width].copy_from_slice(&value.to_le_bytes()[..width]);
    }

    #[test]
    fn listing_shows_names_as_stored_and_what_cant_be_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("odd.zip");
        zip(
            &path,
            &[
                ("ok/", Item::Dir),
                ("ok/a.txt", Item::File(b"hello")),
                ("../up.txt", Item::File(b"up")),
                ("/abs.txt", Item::File(b"abs")),
                ("ok/new\nline", Item::File(b"")),
                ("ok/link", Item::Link("a.txt")),
                ("secret.txt", Item::File(b"secret")),
                ("packed.txt", Item::File(b"packed")),
            ],
        );
        patch(&path, "secret.txt", |fixed, _| fixed[8] |= 1);
        patch(&path, "packed.txt", |fixed, _| set(fixed, 10, 14, 2));
        let archive = Archive::open(&path).unwrap();
        let listed: Vec<(&str, Kind, Option<String>)> = archive
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.kind, e.problem()))
            .collect();
        assert_eq!(
            listed,
            [
                ("ok/", Kind::Dir, None),
                ("ok/a.txt", Kind::File, None),
                (
                    "../up.txt",
                    Kind::File,
                    Some("a path that climbs out with .., which Monodrive doesn't extract".into())
                ),
                (
                    "/abs.txt",
                    Kind::File,
                    Some("an absolute path, which Monodrive doesn't extract".into())
                ),
                ("ok/new\nline", Kind::File, None),
                ("ok/link", Kind::Link, None),
                (
                    "secret.txt",
                    Kind::File,
                    Some("encrypted, and Monodrive doesn't extract encrypted entries".into())
                ),
                (
                    "packed.txt",
                    Kind::File,
                    Some("compressed with LZMA, which Monodrive doesn't extract".into())
                ),
            ]
        );
        assert!(archive.entries[6].encrypted && !archive.entries[1].encrypted);
        assert_eq!((archive.entries[1].size, archive.total), (5, 8));
        assert_eq!(printable("ok/new\nline"), "ok/new\\nline");
        let inside: Vec<&str> = archive.under("ok").map(|e| e.name.as_str()).collect();
        assert_eq!(inside, ["ok/a.txt", "ok/new\nline", "ok/link"]);
        let not_zip = dir.path().join("not.zip");
        std::fs::write(&not_zip, "hello").unwrap();
        assert!(
            Archive::open(&not_zip)
                .unwrap_err()
                .ends_with("isn't a ZIP archive.")
        );
    }

    #[test]
    fn listing_reads_zip64_and_stops_at_the_most_entries_it_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("many.zip");
        let mut zip = ZipWriter::new(BufWriter::new(File::create(&path).unwrap()));
        let options = FullFileOptions::default().compression_method(CompressionMethod::Stored);
        for n in 0..=MAX_ENTRIES {
            zip.start_file(n.to_string(), options.clone()).unwrap();
        }
        zip.finish().unwrap();
        let archive = Archive::open(&path).unwrap();
        assert_eq!(archive.entries.len(), MAX_ENTRIES);
        assert_eq!(archive.total, MAX_ENTRIES as u64 + 1);
        let refused = archive.plan(&[], None).unwrap_err();
        assert!(refused.contains("has 200001 entries"), "{refused}");
    }

    #[test]
    fn cancelling_stops_extraction_and_compression_partway() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.zip");
        zip(&path, &[("a/", Item::Dir), ("a/x", Item::File(b"x"))]);
        let archive = Archive::open(&path).unwrap();
        let (cancel, done) = (AtomicBool::new(true), AtomicU64::new(0));
        let to = dir.path().join("out");
        assert_eq!(
            archive.extract("a", &to, true, &cancel, &done),
            Err("Cancelled.".into())
        );
        assert!(to.is_dir() && !to.join("x").exists());
        let made = dir.path().join("made.zip");
        assert_eq!(
            create(&[dir.path().join("out")], &made, &cancel, &done),
            Err("Cancelled.".into())
        );
        assert_eq!(done.load(Ordering::Relaxed), 0);
    }
}
