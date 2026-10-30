//! The whole-disk index, backed by fsearch. fsearch lets the first
//! process own the index files while later ones follow, so the app,
//! `monodrive mcp` and the fsearch CLI all share one index in
//! `~/Library/Application Support/FSearch`.

use crate::entries::{Entry, Kind};
use crate::search::{self, Query, Scope, Term};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::SystemTime;

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
    /// The line's first 400 bytes, trailing space trimmed.
    pub text: String,
    /// Where the matches are in `text`, in bytes.
    pub matches: Vec<Range<usize>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GrepFile {
    pub path: PathBuf,
    pub lines: Vec<LineMatch>,
}

#[derive(Clone, Debug, Serialize)]
pub struct GrepResults {
    pub files: Vec<GrepFile>,
    /// False when the time budget ran out before every candidate was read,
    /// or `Caps::lines` left matches out.
    pub complete: bool,
}

/// How much a content search returns.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub files: usize,
    pub per_file: usize,
    /// Matched lines in all.
    pub lines: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            files: 50,
            per_file: 5,
            lines: 250,
        }
    }
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

    /// Name search for what was typed, everywhere. See `search`.
    pub fn find(&self, query: &str, limit: usize) -> Result<Vec<Entry>, String> {
        self.search(&Query::parse(query)?, &Scope::Everywhere, limit)
    }

    /// Name search, best matches first.
    pub fn search(&self, query: &Query, scope: &Scope, limit: usize) -> Result<Vec<Entry>, String> {
        let mut q = fsearch_query(query, scope, self.engine.home(), now())?;
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
    /// `filters` picks the files read, with the same meaning as in `search`.
    pub fn grep(
        &self,
        pattern: &str,
        mode: GrepMode,
        filters: &Query,
        scope: &Scope,
        caps: Caps,
    ) -> Result<GrepResults, String> {
        let mut q = fsearch_query(filters, scope, self.engine.home(), now())?;
        // One more of each than the caps take, so a cut shows.
        q.limit = caps.files + 1;
        let mode = match mode {
            GrepMode::Literal => fsearch::GrepMode::Literal,
            GrepMode::Regex => fsearch::GrepMode::Regex,
            GrepMode::Symbol => fsearch::GrepMode::Symbol,
        };
        let mut grep = fsearch::Grep::new(pattern, mode)?;
        grep.max_per_file = caps.per_file + 1;
        let (result, _indexed) = self.engine.grep(&q, &grep)?;
        Ok(bounded(result, caps))
    }
}

/// `query` as fsearch's own at `now`, in seconds from the epoch: the words
/// through fsearch's parser, and each filter onto the field fsearch tests.
fn fsearch_query(
    query: &Query,
    scope: &Scope,
    home: &str,
    now: i64,
) -> Result<fsearch::Query, String> {
    query.check()?;
    let words: Vec<String> = query
        .terms
        .iter()
        .filter(|term| matches!(term, Term::Word(_)))
        .map(Term::to_string)
        .collect();
    let mut q = fsearch::Query::parse(&words.join(" "), home)?;
    let mut kind_exts: Option<Vec<Vec<u8>>> = None;
    let mut exts: Option<Vec<Vec<u8>>> = None;
    for term in &query.terms {
        match term {
            Term::Word(_) | Term::In(_) => {}
            Term::Kind(kinds) => {
                for kind in kinds {
                    let entry = match kind {
                        search::Kind::Folder => Some("dir"),
                        search::Kind::File => Some("file"),
                        search::Kind::Link => Some("link"),
                        _ => None,
                    };
                    if let Some(entry) = entry {
                        q.filter("kind", entry, home)?;
                        continue;
                    }
                    let group = match kind {
                        search::Kind::Document => "doc",
                        group => group.name(),
                    };
                    // On a query of its own, so its extensions narrow the
                    // words' instead of joining them.
                    let mut types = fsearch::Query::default();
                    types.filter("type", group, home)?;
                    (q.kind, q.apps) = (types.kind.or(q.kind), q.apps || types.apps);
                    kind_exts.get_or_insert_default().extend(types.exts);
                }
            }
            Term::Ext(list) => exts
                .get_or_insert_default()
                .extend(list.iter().map(|ext| ext.as_bytes().to_vec())),
            Term::Size(size) => {
                let (first, last) = size.bytes();
                let bytes = |n: i64| u64::try_from(n).unwrap_or(0);
                q.size = (bytes(first), bytes(last));
            }
            Term::Modified(modified) => {
                let (first, last) = modified.seconds(now);
                let second = |n: i64| u32::try_from(n.clamp(0, u32::MAX.into())).unwrap_or(0);
                q.mtime = (second(first), second(last));
            }
        }
    }
    for wanted in [kind_exts, exts].into_iter().flatten() {
        if q.exts.is_empty() {
            q.exts = wanted;
        } else {
            q.exts.retain(|ext| wanted.contains(ext));
            if q.exts.is_empty() {
                // fsearch's own way to match nothing: no name has a '/'.
                q.exts.push(b"/".to_vec());
            }
        }
    }
    if let Some(folder) = query.folder(scope)? {
        q.filter("in", &folder.to_string_lossy(), home)?;
    }
    Ok(q)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        })
}

/// fsearch's matches cut to `caps`, incomplete when that left any out.
fn bounded(result: fsearch::GrepResult, caps: Caps) -> GrepResults {
    let mut complete = result.complete && result.files.len() <= caps.files;
    let mut left = caps.lines;
    let files = result
        .files
        .into_iter()
        .take(caps.files)
        .map_while(|file| {
            let take = file.lines.len().min(caps.per_file).min(left);
            complete &= take == file.lines.len();
            left -= take;
            let mut hits = file.hits.into_iter();
            (take > 0).then(|| GrepFile {
                path: <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(file.path)
                    .into(),
                lines: file
                    .lines
                    .into_iter()
                    .take(take)
                    .map(|(line, text)| LineMatch {
                        line,
                        text,
                        matches: hits
                            .next()
                            .map(|hit| hit.ranges.into_iter().map(|(a, b)| a..b).collect())
                            .unwrap_or_default(),
                    })
                    .collect(),
            })
        })
        .collect();
    GrepResults { files, complete }
}

#[cfg(test)]
mod tests {
    use super::*;

    use fsearch::walk::{KIND_DIR, KIND_FILE, KIND_LINK};

    const NOW: i64 = 1_800_000_000;

    /// Whether fsearch's own matcher takes an entry for `text` in `scope`.
    fn takes(text: &str, scope: &Scope, (path, kind, size, mtime): (&str, u8, u64, i64)) -> bool {
        let home = std::env::var("HOME").unwrap();
        let q = fsearch_query(&Query::parse(text).unwrap(), scope, &home, NOW).unwrap();
        let mtime = u32::try_from(mtime).unwrap();
        q.match_path(path.as_bytes(), kind, size, mtime).is_some()
    }

    fn file(path: &str) -> (&str, u8, u64, i64) {
        (path, KIND_FILE, 1000, NOW)
    }

    #[test]
    fn kinds_map_onto_fsearchs_types_and_an_extension_narrows_them() {
        let all = &Scope::Everywhere;
        let (png, jpg, mov, rs) = (
            file("/f/a.png"),
            file("/f/a.JPG"),
            file("/f/a.mov"),
            file("/f/a.rs"),
        );
        assert!(takes("kind:image", all, png) && takes("kind:image", all, jpg));
        assert!(!takes("kind:image", all, rs) && !takes("kind:image", all, mov));
        assert!(takes("kind:image,video", all, mov));
        assert!(takes("kind:document", all, file("/f/report.pdf")));
        assert!(takes("kind:image ext:png", all, png));
        assert!(!takes("kind:image ext:png", all, jpg));
        assert!(takes("ext:rs ext:toml", all, rs));
        // A kind narrows the extensions a glob already asks for.
        assert!(takes("*.png kind:image", all, png));
        assert!(!takes("*.png kind:image", all, jpg));
        for nothing in [png, rs] {
            assert!(!takes("*.rs kind:image", all, nothing));
        }
        for nothing in [png, rs] {
            assert!(!takes("kind:image ext:rs", all, nothing));
        }

        let folder = ("/f/a.png", KIND_DIR, 0, NOW);
        assert!(takes("kind:folder", all, folder) && !takes("kind:folder", all, png));
        assert!(takes("kind:file", all, png) && !takes("kind:file", all, folder));
        assert!(takes("kind:link", all, ("/f/l", KIND_LINK, 0, NOW)));
        for app in [KIND_DIR, KIND_LINK] {
            assert!(takes(
                "kind:app",
                all,
                ("/Applications/Safari.app", app, 0, NOW)
            ));
        }
        assert!(!takes("kind:app", all, file("/f/Safari.app")));
        // Words still go to fsearch.
        assert!(takes("report kind:document", all, file("/f/report.pdf")));
        assert!(!takes("report kind:document", all, file("/f/notes.pdf")));
        assert!(!takes("!report kind:document", all, file("/f/report.pdf")));
    }

    #[test]
    fn size_and_modified_bounds_reach_fsearch_exactly() {
        let all = &Scope::Everywhere;
        let sized = |size| ("/f/a.bin", KIND_FILE, size, NOW);
        assert!(takes("size:>1mb", all, sized(1_000_001)));
        assert!(!takes("size:>1mb", all, sized(1_000_000)));
        assert!(takes("size:>=1mb", all, sized(1_000_000)));
        assert!(takes("size:1kb..2kb", all, sized(2000)));
        assert!(!takes("size:1kb..2kb", all, sized(2001)));
        assert!(!takes("size:<1kb", all, sized(1000)));

        let aged = |seconds| ("/f/a.txt", KIND_FILE, 1, NOW - seconds);
        assert!(takes("modified:7d", all, aged(7 * 86_400)));
        assert!(!takes("modified:7d", all, aged(7 * 86_400 + 1)));
        assert!(takes("modified:>7d", all, aged(8 * 86_400)));
        assert!(!takes("modified:>7d", all, aged(86_400)));
        let first = |text: &str| match &Query::parse(text).unwrap().terms[0] {
            Term::Modified(modified) => modified.seconds(NOW),
            other => panic!("{other:?}"),
        };
        let (midnight, _) = first("modified:today");
        assert!(takes("modified:today", all, aged(NOW - midnight)));
        assert!(!takes("modified:today", all, aged(NOW - midnight + 1)));
        assert!(takes("modified:yesterday", all, aged(NOW - midnight + 1)));
        let (_, last) = first("modified:<2026-01-01");
        assert!(takes("modified:<2026-01-01", all, aged(NOW - last)));
        assert!(!takes("modified:<2026-01-01", all, aged(NOW - last - 1)));
    }

    #[test]
    fn a_folder_scope_takes_what_is_under_it_and_in_overrides_it() {
        let work = Scope::Folder("/nonexistent/Work".into());
        assert!(takes("", &work, file("/nonexistent/Work/a.txt")));
        assert!(takes("", &work, file("/nonexistent/Work/deep/down/a.txt")));
        assert!(!takes("", &work, file("/nonexistent/Workshop/a.txt")));
        assert!(!takes("", &work, ("/nonexistent/Work", KIND_DIR, 0, NOW)));
        assert!(!takes("", &work, file("/elsewhere/a.txt")));
        assert!(takes("", &Scope::Everywhere, file("/elsewhere/a.txt")));
        assert!(takes("in:src", &work, file("/nonexistent/Work/src/a.rs")));
        assert!(!takes("in:src", &work, file("/nonexistent/Work/a.rs")));
        assert!(takes("in:/elsewhere", &work, file("/elsewhere/a.txt")));
        let home = std::env::var("HOME").unwrap();
        assert!(
            fsearch_query(
                &Query::parse("in:src").unwrap(),
                &Scope::Everywhere,
                &home,
                NOW
            )
            .is_err()
        );
    }

    #[test]
    fn content_matches_stop_at_the_cap_on_lines_in_all() {
        let file = |path: &str, lines: usize| fsearch::FileMatches {
            path: path.as_bytes().to_vec(),
            lines: (1..=lines).map(|n| (n, format!("line {n}"))).collect(),
            hits: (0..lines)
                .map(|_| fsearch::Hit {
                    ranges: vec![(0, 4)],
                    ..Default::default()
                })
                .collect(),
        };
        let result = || fsearch::GrepResult {
            files: vec![file("/a", 3), file("/b", 3), file("/c", 1)],
            complete: true,
            ..Default::default()
        };
        let caps = |lines| Caps {
            lines,
            ..Caps::default()
        };
        let cut = bounded(result(), caps(4));
        let shape: Vec<(String, usize)> = cut
            .files
            .iter()
            .map(|f| (f.path.display().to_string(), f.lines.len()))
            .collect();
        assert_eq!(shape, [("/a".into(), 3), ("/b".into(), 1)]);
        assert_eq!(cut.files[1].lines[0].text, "line 1");
        assert_eq!(
            cut.files[1].lines[0].matches,
            vec![Range { start: 0, end: 4 }]
        );
        assert!(!cut.complete);
        assert!(bounded(result(), caps(7)).complete);
    }

    #[test]
    fn content_matches_past_the_file_or_per_file_cap_make_it_incomplete() {
        let file = |path: &str, lines: usize| fsearch::FileMatches {
            path: path.as_bytes().to_vec(),
            lines: (1..=lines).map(|n| (n, format!("line {n}"))).collect(),
            hits: Vec::new(),
        };
        let result = |files| fsearch::GrepResult {
            files,
            complete: true,
            ..Default::default()
        };
        let caps = Caps {
            files: 2,
            per_file: 5,
            lines: 100,
        };
        // fsearch is asked for one more of each than the caps take.
        let extra_line = bounded(result(vec![file("/a", 6)]), caps);
        assert_eq!(extra_line.files[0].lines.len(), 5);
        assert!(!extra_line.complete);
        let extra_file = bounded(
            result(vec![file("/a", 1), file("/b", 1), file("/c", 1)]),
            caps,
        );
        assert_eq!(extra_file.files.len(), 2);
        assert!(!extra_file.complete);
        let full = bounded(result(vec![file("/a", 5), file("/b", 5)]), caps);
        assert!(full.complete);
    }

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
