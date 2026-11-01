use crate::cloud::Availability;
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
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
        match self.kind {
            Kind::Dir => "Folder".into(),
            Kind::Link => "Alias".into(),
            Kind::Other => "--".into(),
            Kind::File => self.path.extension().map_or_else(
                || "Document".into(),
                |ext| format!("{} file", ext.to_string_lossy().to_uppercase()),
            ),
        }
    }

    /// Its size as listed: only files have one.
    pub fn listed_size(&self) -> u64 {
        if self.kind == Kind::File {
            self.size
        } else {
            0
        }
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
pub fn sort(entries: &mut [Entry], sort: Sort) {
    let column = |entry: &Entry| {
        let (number, label) = match sort.by {
            SortBy::Name => (0, String::new()),
            SortBy::Modified => (entry.modified, String::new()),
            SortBy::Size => (entry.listed_size(), String::new()),
            SortBy::Kind => (0, entry.kind_label()),
        };
        (number, label, natural_key(&entry.name))
    };
    let after_folders = |entry: &Entry| sort.folders_first && entry.kind != Kind::Dir;
    if sort.descending {
        entries.sort_by_cached_key(|entry| (after_folders(entry), Reverse(column(entry))));
    } else {
        entries.sort_by_cached_key(|entry| (after_folders(entry), column(entry)));
    }
}

/// A name as bytes that order as Finder does: ignoring case, with runs of
/// digits by value. A run becomes its length then its digits, without leading
/// zeros, so a longer number sorts after a shorter one. Each starts with `0`,
/// which sits among other characters where any digit would.
fn natural_key(name: &str) -> Vec<u8> {
    let name = name.to_lowercase();
    let mut key = Vec::with_capacity(name.len());
    let mut rest = name.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if !byte.is_ascii_digit() {
            key.push(byte);
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
    key
}

/// A folder's entries in `order`. Entries that vanish or can't be read
/// mid-listing are skipped, so one bad item never hides the rest.
pub fn list_dir(dir: &Path, show_hidden: bool, order: Sort) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|item| {
            let name = item.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            // DirEntry::metadata doesn't follow symlinks, so links stay links.
            let meta = item.metadata().ok()?;
            Some(entry(item.path(), name, &meta))
        })
        .collect();
    sort(&mut entries, order);
    Ok(entries)
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
