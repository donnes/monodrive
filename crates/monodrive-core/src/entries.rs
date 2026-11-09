use crate::cloud::Availability;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Dir,
    Link,
    Other,
}

impl Kind {
    pub fn of(file_type: std::fs::FileType) -> Self {
        if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_symlink() {
            Kind::Link
        } else if file_type.is_file() {
            Kind::File
        } else {
            Kind::Other
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    /// Seconds since the Unix epoch; 0 when unknown.
    pub modified: u64,
    /// From the listing's own `lstat`, so never `Downloading`.
    pub availability: Availability,
}

impl Entry {
    /// What the entry is, as people say it: "Folder", "PDF file".
    pub fn kind_label(&self) -> String {
        kind_label(self.kind, &self.path)
    }

    /// Its size as listed: only files have one.
    pub fn listed_size(&self) -> u64 {
        listed_size(self.kind, self.size)
    }
}

/// What an item of `kind` named `name` is, as people say it.
pub fn kind_label(kind: Kind, name: &Path) -> String {
    match kind {
        Kind::Dir => "Folder".into(),
        Kind::Link => "Alias".into(),
        Kind::Other => "--".into(),
        Kind::File => name.extension().map_or_else(
            || "Document".into(),
            |ext| format!("{} file", ext.to_string_lossy().to_uppercase()),
        ),
    }
}

fn listed_size(kind: Kind, size: u64) -> u64 {
    if kind == Kind::File { size } else { 0 }
}

/// What a listing sorts on, so this Mac's entries and a server's sort alike.
pub trait Sorted {
    fn name(&self) -> &str;
    fn kind(&self) -> Kind;
    fn size(&self) -> u64;
    fn modified(&self) -> u64;
    fn kind_label(&self) -> String {
        kind_label(self.kind(), Path::new(self.name()))
    }
}

impl Sorted for Entry {
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> Kind {
        self.kind
    }
    fn size(&self) -> u64 {
        self.size
    }
    fn modified(&self) -> u64 {
        self.modified
    }
    fn kind_label(&self) -> String {
        Entry::kind_label(self)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortBy {
    #[default]
    Name,
    Modified,
    Size,
    Kind,
}

/// How a listing is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub by: SortBy,
    pub descending: bool,
    /// Folders before files, whatever the order.
    pub folders_first: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            by: SortBy::Name,
            descending: false,
            folders_first: true,
        }
    }
}

/// Orders `entries`. Names compare as people read them: case aside, and
/// numbers by value, so "file2" comes before "file10". Ties fall back to the
/// name, so a sort by size or date is stable and predictable.
///
/// Each entry's key goes into one shared buffer, and its first eight bytes
/// into a number, so most comparisons are one integer compare and a sort by
/// name of ASCII names allocates nothing per entry.
pub fn sort<T: Sorted>(entries: &mut [T], sort: Sort) {
    let mut bytes = Vec::with_capacity(entries.len() * 24);
    let keys: Vec<Key> = entries
        .iter()
        .map(|entry| {
            let start = bytes.len();
            let number = match sort.by {
                SortBy::Name | SortBy::Kind => 0,
                SortBy::Modified => entry.modified(),
                SortBy::Size => listed_size(entry.kind(), entry.size()),
            };
            if sort.by == SortBy::Kind {
                // NUL sorts before any byte a label has, so a shorter label
                // still comes first.
                bytes.extend_from_slice(entry.kind_label().as_bytes());
                bytes.push(0);
            }
            push_natural_key(entry.name(), &mut bytes);
            let mut head = [0; 8];
            let len = (bytes.len() - start).min(8);
            head[..len].copy_from_slice(&bytes[start..start + len]);
            Key {
                after_folders: sort.folders_first && entry.kind() != Kind::Dir,
                number,
                head: u64::from_be_bytes(head),
                start: start as u32,
                end: bytes.len() as u32,
            }
        })
        .collect();
    let mut order: Vec<u32> = (0..entries.len() as u32).collect();
    // Stable, so names that read the same keep the order they came in.
    order.sort_by(|&a, &b| {
        let (a, b) = (&keys[a as usize], &keys[b as usize]);
        a.after_folders.cmp(&b.after_folders).then_with(|| {
            let column = (a.number, a.head)
                .cmp(&(b.number, b.head))
                .then_with(|| a.bytes(&bytes).cmp(b.bytes(&bytes)));
            if sort.descending {
                column.reverse()
            } else {
                column
            }
        })
    });
    // Puts each entry where `order` says, as `sort_by_cached_key` does: the
    // entry for slot `i` may already have been swapped out of its place, so
    // follow where it went.
    for i in 0..order.len() {
        let mut from = order[i] as usize;
        while from < i {
            from = order[from] as usize;
        }
        order[i] = from as u32;
        entries.swap(i, from);
    }
}

/// What a listing sorts on for one entry; its bytes are `start..end` of the
/// shared buffer.
struct Key {
    after_folders: bool,
    number: u64,
    head: u64,
    start: u32,
    end: u32,
}

impl Key {
    fn bytes<'a>(&self, all: &'a [u8]) -> &'a [u8] {
        &all[self.start as usize..self.end as usize]
    }
}

/// A name as bytes that order as Finder does: ignoring case, with runs of
/// digits by value. A run becomes its length then its digits, without leading
/// zeros, so a longer number sorts after a shorter one. Each starts with `0`,
/// which sits among other characters where any digit would.
fn push_natural_key(name: &str, key: &mut Vec<u8>) {
    let lower;
    let name = if name.is_ascii() {
        name
    } else {
        lower = name.to_lowercase();
        &lower
    };
    let mut rest = name.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if !byte.is_ascii_digit() {
            key.push(byte.to_ascii_lowercase());
            rest = after;
            continue;
        }
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        let (run, after) = rest.split_at(digits);
        let zeros = run.iter().take_while(|&&b| b == b'0').count();
        let value = &run[zeros..];
        key.push(b'0');
        key.extend_from_slice(&value.len().to_be_bytes());
        key.extend_from_slice(value);
        rest = after;
    }
}

/// A folder's entries in `order`. Entries that vanish or can't be read
/// mid-listing are skipped, so one bad item never hides the rest. macOS's own
/// `.DS_Store` and `.localized` stay out even with `show_hidden`.
pub fn list_dir(dir: &Path, show_hidden: bool, order: Sort) -> std::io::Result<Vec<Entry>> {
    let mut entries = read_folder(dir, |name| {
        !(name == b".DS_Store"
            || name == b".localized"
            || (!show_hidden && name.first() == Some(&b'.')))
    })?;
    sort(&mut entries, order);
    Ok(entries)
}

/// `ATTR_CMN_ERROR` and the `vtype`s from <sys/attr.h> and <sys/vnode.h>,
/// which libc doesn't export.
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

/// The entries of `dir` whose names `keep` accepts. Each `getattrlistbulk`
/// call hands back the name, type, size, date and flags of hundreds of
/// entries, where `read_dir` takes an `lstat` per entry, which is most of
/// the time a listing takes. Like `lstat`, it doesn't follow links.
fn read_folder(dir: &Path, keep: impl Fn(&[u8]) -> bool) -> std::io::Result<Vec<Entry>> {
    use std::os::fd::AsRawFd;
    let folder = std::fs::File::open(dir)?;
    let mut wanted = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_MODTIME
            | libc::ATTR_CMN_FLAGS,
        volattr: 0,
        dirattr: 0,
        fileattr: libc::ATTR_FILE_DATALENGTH,
        forkattr: 0,
    };
    let mut buffer = vec![0u8; 256 * 1024];
    let mut entries = Vec::new();
    loop {
        // SAFETY: `wanted` and `buffer` outlive the call, and the kernel
        // writes no more than `buffer.len()` bytes.
        let count = unsafe {
            libc::getattrlistbulk(
                folder.as_raw_fd(),
                (&raw mut wanted).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
            )
        };
        match count {
            0 => return Ok(entries),
            ..0 => {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            _ => {
                let mut rest = buffer.as_slice();
                for _ in 0..count {
                    let Some(length) = u32_at(rest, 0).map(|n| n as usize) else {
                        break;
                    };
                    let Some((record, after)) = rest.split_at_checked(length) else {
                        break;
                    };
                    rest = after;
                    entries.extend(parse_record(dir, record, &keep));
                }
            }
        }
    }
}

/// One entry from `getattrlistbulk`'s buffer: its length, the attributes
/// returned, then each one asked for that came back, in the order of their
/// bits, 4-byte aligned.
fn parse_record(dir: &Path, record: &[u8], keep: &impl Fn(&[u8]) -> bool) -> Option<Entry> {
    use std::os::unix::ffi::OsStrExt;
    let returned = u32_at(record, 4)?;
    let file_returned = u32_at(record, 16)?;
    // After the length and the five words of `attribute_set_t`.
    let mut at = 24;
    let has = |bit: u32| returned & bit != 0;
    if has(ATTR_CMN_ERROR) {
        if u32_at(record, at)? != 0 {
            return None;
        }
        at += 4;
    }
    if !has(libc::ATTR_CMN_NAME) {
        return None;
    }
    // An `attrreference_t`: where the name starts, from here, and its
    // length with the closing NUL.
    let start = at.checked_add_signed(i32::from_ne_bytes(word(record, at)?) as isize)?;
    let length = u32_at(record, at + 4)? as usize;
    let name = record.get(start..start + length.checked_sub(1)?)?;
    if !keep(name) {
        return None;
    }
    at += 8;
    let mut kind = Kind::Other;
    if has(libc::ATTR_CMN_OBJTYPE) {
        kind = match u32_at(record, at)? {
            VREG => Kind::File,
            VDIR => Kind::Dir,
            VLNK => Kind::Link,
            _ => Kind::Other,
        };
        at += 4;
    }
    let mut modified = 0;
    if has(libc::ATTR_CMN_MODTIME) {
        // A `timespec`: whole seconds, then nanoseconds.
        modified = u64::try_from(i64_at(record, at)?).unwrap_or(0);
        at += 16;
    }
    let mut availability = Availability::Local;
    if has(libc::ATTR_CMN_FLAGS) {
        availability = Availability::from_flags(u32_at(record, at)?);
        at += 4;
    }
    let size = if file_returned & libc::ATTR_FILE_DATALENGTH != 0 {
        u64::try_from(i64_at(record, at)?).unwrap_or(0)
    } else {
        0
    };
    // Sized once: `join` copies `dir` and then grows to fit the name.
    let mut path = PathBuf::with_capacity(dir.as_os_str().len() + 1 + name.len());
    path.push(dir);
    // From the bytes, so a name that isn't UTF-8 still reaches its file.
    path.push(std::ffi::OsStr::from_bytes(name));
    Some(Entry {
        path,
        name: String::from_utf8_lossy(name).into_owned(),
        kind,
        size,
        modified,
        availability,
    })
}

fn word(bytes: &[u8], at: usize) -> Option<[u8; 4]> {
    bytes.get(at..at + 4)?.try_into().ok()
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    word(bytes, at).map(u32::from_ne_bytes)
}

fn i64_at(bytes: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_ne_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The entry at `path`, as a listing of its folder would show it.
pub fn entry_at(path: &Path) -> std::io::Result<Entry> {
    let meta = std::fs::symlink_metadata(path)?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    Ok(entry(path.into(), name, &meta))
}

fn entry(path: PathBuf, name: String, meta: &std::fs::Metadata) -> Entry {
    Entry {
        path,
        name,
        kind: Kind::of(meta.file_type()),
        size: meta.len(),
        modified: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs()),
        availability: Availability::of(meta),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: Kind, size: u64, modified: u64) -> Entry {
        Entry {
            path: PathBuf::from("/x").join(name),
            name: name.into(),
            kind,
            size,
            modified,
            availability: Availability::Local,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn showing_hidden_files_leaves_out_the_ones_macos_keeps_for_itself() {
        let dir = tempfile::tempdir().unwrap();
        for name in [".DS_Store", ".localized", ".env", "a.txt"] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let listed = |show_hidden| list_dir(dir.path(), show_hidden, Sort::default()).unwrap();
        assert_eq!(names(&listed(true)), [".env", "a.txt"]);
        assert_eq!(names(&listed(false)), ["a.txt"]);
    }

    #[test]
    fn a_listing_says_what_lstat_says_about_each_entry() {
        let dir = tempfile::tempdir().unwrap();
        // More than one batch of `getattrlistbulk`'s buffer holds.
        for i in 0..4_000 {
            std::fs::write(dir.path().join(format!("file {i}.txt")), "x".repeat(i % 7)).unwrap();
        }
        std::fs::create_dir(dir.path().join("folder")).unwrap();
        std::fs::write(dir.path().join("Ünïcødé ✓.md"), "hello").unwrap();
        std::os::unix::fs::symlink("folder", dir.path().join("to folder")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.path().join("broken")).unwrap();
        let fifo = std::ffi::CString::new(
            dir.path()
                .join("pipe")
                .into_os_string()
                .into_encoded_bytes(),
        );
        // SAFETY: a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(fifo.unwrap().as_ptr(), 0o644) }, 0);
        let listed = list_dir(dir.path(), true, Sort::default()).unwrap();
        assert_eq!(listed.len(), 4_005);
        for entry in &listed {
            let by_lstat = entry_at(&entry.path).unwrap();
            assert_eq!(entry.name, by_lstat.name);
            assert_eq!(entry.kind, by_lstat.kind, "{}", entry.name);
            assert_eq!(
                entry.listed_size(),
                by_lstat.listed_size(),
                "{}",
                entry.name
            );
            assert_eq!(entry.modified, by_lstat.modified, "{}", entry.name);
            assert_eq!(entry.availability, by_lstat.availability, "{}", entry.name);
        }
        let kind = |name: &str| listed.iter().find(|e| e.name == name).unwrap().kind;
        assert_eq!(kind("folder"), Kind::Dir);
        assert_eq!(kind("to folder"), Kind::Link);
        assert_eq!(kind("broken"), Kind::Link);
        assert_eq!(kind("pipe"), Kind::Other);
    }

    #[test]
    fn names_sort_as_people_read_them() {
        let mut entries = [
            "file10.txt",
            "File2.txt",
            "file1.txt",
            "file02.txt",
            "file0.txt",
            "file 3.txt",
            "file_1.txt",
            "a",
            "B",
            "Écran 2",
            "écran 10",
            "photo99",
            "photo100",
        ]
        .map(|name| entry(name, Kind::File, 0, 0));
        sort(&mut entries, Sort::default());
        assert_eq!(
            names(&entries),
            [
                "a",
                "B",
                "file 3.txt",
                "file0.txt",
                "file1.txt",
                "File2.txt",
                "file02.txt",
                "file10.txt",
                "file_1.txt",
                "photo99",
                "photo100",
                "Écran 2",
                "écran 10",
            ]
        );
    }

    #[test]
    fn folders_lead_in_either_direction_unless_mixed() {
        let mut entries = [
            entry("big.bin", Kind::File, 900, 1),
            entry("docs", Kind::Dir, 0, 3),
            entry("small.txt", Kind::File, 10, 2),
        ];
        let by = |by, descending, folders_first| Sort {
            by,
            descending,
            folders_first,
        };
        sort(&mut entries, by(SortBy::Size, true, true));
        assert_eq!(names(&entries), ["docs", "big.bin", "small.txt"]);
        sort(&mut entries, by(SortBy::Modified, false, false));
        assert_eq!(names(&entries), ["big.bin", "small.txt", "docs"]);
        sort(&mut entries, by(SortBy::Kind, false, true));
        assert_eq!(names(&entries), ["docs", "big.bin", "small.txt"]);
        // Mixed in, a folder sorts by its kind's name and counts as no size.
        sort(&mut entries, by(SortBy::Kind, false, false));
        assert_eq!(names(&entries), ["big.bin", "docs", "small.txt"]);
        sort(&mut entries, by(SortBy::Size, false, false));
        assert_eq!(names(&entries), ["docs", "small.txt", "big.bin"]);
    }
}
