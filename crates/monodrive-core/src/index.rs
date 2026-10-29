//! The whole-disk index, backed by fsearch. fsearch lets the first
//! process own the index files while later ones follow, so the app,
//! `monodrive mcp` and the fsearch CLI all share one index in
//! `~/Library/Application Support/FSearch`.

use crate::entries::{Entry, Kind};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrepMode {
    #[default]
    Literal,
    Regex,
    /// Where a name is defined (`fn name`, `class Name`, ...).
    Symbol,
}

#[derive(Clone, Debug, Serialize)]
pub struct LineMatch {
    pub line: usize,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct GrepFile {
    pub path: PathBuf,
    pub lines: Vec<LineMatch>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GrepResults {
    pub files: Vec<GrepFile>,
    /// False when the time budget ran out before every candidate was read.
    pub complete: bool,
}

pub struct Index {
    engine: fsearch::Engine,
    /// Roughly how many entries the first crawl will list, if it can be told.
    estimate: Option<usize>,
}

/// How far the name index has got. Content indexing isn't tracked: fsearch
/// has no reliable total for it, and grep reads files directly until then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// This process hasn't started the index. `saved` says whether one is on
    /// disk to load.
    NotStarted {
        saved: bool,
    },
    /// Another process owns the first crawl; its count isn't visible here.
    Following,
    Listing {
        listed: usize,
        estimate: Option<usize>,
    },
    Ready {
        entries: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub progress: Progress,
    /// Unknown until the index starts.
    pub full_disk_access: Option<bool>,
}

impl Status {
    pub fn not_started(saved: bool) -> Self {
        Self {
            progress: Progress::NotStarted { saved },
            full_disk_access: None,
        }
    }

    /// How full a progress bar is, when that can be told.
    pub fn fraction(&self) -> Option<f32> {
        match self.progress {
            Progress::Ready { .. } => Some(1.),
            // Listing ends with building the index, so it stops short of full.
            Progress::Listing {
                listed,
                estimate: Some(estimate),
            } if estimate > 0 => Some((listed as f32 / estimate as f32).min(0.99)),
            _ => None,
        }
    }
}

/// One line for people and agents, such as "Indexing 3.1M of ~8.2M files".
impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Progress::NotStarted { saved: true } => {
                f.write_str("Saved index, loads on first search")
            }
            Progress::NotStarted { saved: false } => {
                f.write_str("No index yet, the first search builds it")
            }
            Progress::Following => f.write_str("Another app is building the index"),
            Progress::Listing {
                listed,
                estimate: Some(estimate),
            } => write!(
                f,
                "Indexing {} of ~{} files",
                millions(listed),
                millions(estimate)
            ),
            Progress::Listing { listed, .. } => write!(f, "Indexing {} files", millions(listed)),
            Progress::Ready { entries } => write!(f, "{} files indexed", thousands(entries)),
        }
    }
}

fn millions(n: usize) -> String {
    format!("{:.1}M", n as f64 / 1e6)
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Whether an index is saved on disk. Doesn't load or start it.
pub fn saved() -> bool {
    std::env::var("HOME").is_ok_and(|home| fsearch::default_dir(&home).join("index.bin").is_file())
}

/// Used inodes on the data volume, which is close to what the first crawl
/// lists (it skips what it can't read, so it can fall short).
fn disk_entries() -> Option<usize> {
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a statfs buffer to fill.
    let ok = unsafe { libc::statfs(c"/System/Volumes/Data".as_ptr(), &mut stats) } == 0;
    let used = stats.f_files.saturating_sub(stats.f_ffree);
    (ok && used > 0)
        .then(|| usize::try_from(used).ok())
        .flatten()
}

impl Index {
    /// Starts indexing in the background and returns at once. Until the
    /// first whole-disk crawl finishes, searches return an "indexing" error
    /// with its progress.
    pub fn start() -> Result<Self, String> {
        let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
        let engine = fsearch::Engine::start(fsearch::Options {
            dir: fsearch::default_dir(&home),
            home,
            skip: None,
        })?;
        Ok(Self {
            engine,
            estimate: disk_entries(),
        })
    }

    /// Calls `f` with the folders each batch of disk changes touched, as
    /// FSEvents names them (real paths: /private/var, not /var), and whether
    /// anything below them may have changed too. `f` runs on fsearch's own
    /// thread, so it should only hand the paths on.
    pub fn on_change(&self, f: impl Fn(Vec<(PathBuf, bool)>) + Send + Sync + 'static) {
        self.engine.on_change(move |changed| {
            f(changed
                .iter()
                .map(|(path, below)| {
                    let path = <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(path);
                    (PathBuf::from(path), *below)
                })
                .collect())
        });
    }

    /// Where the name index is. Cheap enough to poll while it builds.
    pub fn status(&self) -> Status {
        let status = self.engine.status();
        let progress = if status.ready {
            Progress::Ready {
                entries: status.entries,
            }
        } else if !status.owner {
            Progress::Following
        } else {
            Progress::Listing {
                listed: fsearch::walk::LISTED.load(Ordering::Relaxed),
                // Without Full Disk Access the crawl skips folders the count
                // includes, so the bar would stall short.
                estimate: self.estimate.filter(|_| status.full_disk_access),
            }
        };
        Status {
            progress,
            full_disk_access: Some(status.full_disk_access),
        }
    }

    /// Name search in fsearch's query language: fuzzy words plus filters such
    /// as `ext:rs`, `in:~/Work`, `type:image`, `size:>5mb` and `mtime:<7d`.
    pub fn find(&self, query: &str, limit: usize) -> Result<Vec<Entry>, String> {
        let mut q = fsearch::Query::parse(query, self.engine.home())?;
        q.limit = limit;
        Ok(self
            .engine
            .search(&q)?
            .into_iter()
            .map(|found| Entry {
                name: found.path.file_name().map_or_else(
                    || found.path.to_string_lossy().into_owned(),
                    |name| name.to_string_lossy().into_owned(),
                ),
                path: found.path,
                kind: match found.kind & 0b11 {
                    0 => Kind::File,
                    1 => Kind::Dir,
                    2 => Kind::Link,
                    _ => Kind::Other,
                },
                size: found.size,
                modified: found.mtime.into(),
            })
            .collect())
    }

    /// Content search, smart-case, ranked by how likely each file is wanted.
    pub fn grep(
        &self,
        pattern: &str,
        mode: GrepMode,
        within: Option<&Path>,
        limit: usize,
    ) -> Result<GrepResults, String> {
        let home = self.engine.home();
        let mut q = fsearch::Query::parse("", home)?;
        q.limit = limit;
        if let Some(dir) = within {
            q.filter("in", &dir.to_string_lossy(), home)?;
        }
        let mode = match mode {
            GrepMode::Literal => fsearch::GrepMode::Literal,
            GrepMode::Regex => fsearch::GrepMode::Regex,
            GrepMode::Symbol => fsearch::GrepMode::Symbol,
        };
        let grep = fsearch::Grep::new(pattern, mode)?;
        let (result, _indexed) = self.engine.grep(&q, &grep)?;
        Ok(GrepResults {
            files: result
                .files
                .into_iter()
                .map(|file| GrepFile {
                    path: <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(
                        file.path,
                    )
                    .into(),
                    lines: file
                        .lines
                        .into_iter()
                        .map(|(line, text)| LineMatch { line, text })
                        .collect(),
                })
                .collect(),
            complete: result.complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_fills_up_to_but_not_past_almost_full() {
        let listing = |listed, estimate| Status {
            progress: Progress::Listing { listed, estimate },
            full_disk_access: Some(true),
        };
        assert_eq!(listing(2_000_000, Some(8_000_000)).fraction(), Some(0.25));
        assert_eq!(listing(9_000_000, Some(8_000_000)).fraction(), Some(0.99));
        assert_eq!(listing(2_000_000, None).fraction(), None);
        assert_eq!(listing(2_000_000, Some(0)).fraction(), None);
        assert_eq!(Status::not_started(true).fraction(), None);
    }

    #[test]
    fn progress_reads_as_one_short_line() {
        let listing = Progress::Listing {
            listed: 3_140_000,
            estimate: Some(8_237_417),
        };
        assert_eq!(listing.to_string(), "Indexing 3.1M of ~8.2M files");
        let ready = Progress::Ready { entries: 8_237_417 };
        assert_eq!(ready.to_string(), "8,237,417 files indexed");
        assert_eq!(
            Progress::Ready { entries: 999 }.to_string(),
            "999 files indexed"
        );
    }
}
