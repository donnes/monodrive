use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
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
    entries.sort_by(|a, b| {
        let folders = if sort.folders_first {
            (b.kind == Kind::Dir).cmp(&(a.kind == Kind::Dir))
        } else {
            Ordering::Equal
        };
        let order = match sort.by {
            SortBy::Name => Ordering::Equal,
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Size => a.listed_size().cmp(&b.listed_size()),
            SortBy::Kind => a.kind_label().cmp(&b.kind_label()),
        }
        .then_with(|| natural(&a.name, &b.name));
        folders.then(if sort.descending {
            order.reverse()
        } else {
            order
        })
    });
}

/// Compares names as Finder does: ignoring case, with runs of digits by value.
fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let number = |chars: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = chars.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        chars.next();
                    }
                    digits
                };
                let (x, y) = (number(&mut a), number(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
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
            let kind = Kind::of(meta.file_type());
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            Some(Entry {
                path: item.path(),
                name,
                kind,
                size: meta.len(),
                modified,
            })
        })
        .collect();
    sort(&mut entries, order);
    Ok(entries)
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
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn names_sort_as_people_read_them() {
        let mut entries = ["file10.txt", "File2.txt", "file1.txt", "a", "B"]
            .map(|name| entry(name, Kind::File, 0, 0));
        sort(&mut entries, Sort::default());
        assert_eq!(
            names(&entries),
            ["a", "B", "file1.txt", "File2.txt", "file10.txt"]
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
