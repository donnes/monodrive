//! Comparing two files or folders. A walk reads both trees' metadata, so
//! size and modification time pick candidates; only reading the bytes says
//! two files are equal. A plan turns selected differences into a one-way
//! copy the operation owner checks again before it copies anything.
//! Everything here reads the disk and blocks, so callers run it off the UI
//! thread.

use crate::entries::Kind;
use crate::ops::{FileId, Planned, Request, Stamp, disk};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// A walk stops after reading this many items, both sides' and folders'
/// counted, which bounds what it holds.
pub const MAX_ITEMS: usize = 200_000;
/// Files read at once while verifying.
const READERS: usize = 2;
const CHUNK: usize = 64 * 1024;
/// Times closer than this count as the same: FAT and HFS+ keep them to
/// one or two seconds, so a copy there is otherwise always older.
const SLACK: Duration = Duration::from_secs(2);
/// A text diff reads at most this much of each file.
pub const DIFF_BYTES: u64 = 1024 * 1024;
/// How long the line diff may take before it settles for a coarser one.
const DIFF_TIME: Duration = Duration::from_millis(500);

/// Shared with whoever runs a walk or verification on another thread.
#[derive(Debug, Default)]
pub struct Progress {
    pub cancel: AtomicBool,
    /// Entries the walk has compared.
    pub entries: AtomicU64,
    /// Bytes verification has read, both sides counted.
    pub bytes: AtomicU64,
}

impl Progress {
    fn stopped(&self, deadline: Option<Instant>) -> bool {
        self.cancel.load(Ordering::Relaxed) || deadline.is_some_and(|end| Instant::now() >= end)
    }
}

/// One side of an entry, as the walk found it. Links aren't followed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Side {
    pub kind: Kind,
    /// A file's length; a folder's own size, not what it holds.
    pub size: u64,
    pub modified: Option<SystemTime>,
    id: FileId,
}

impl Side {
    fn of(meta: &Metadata) -> Self {
        Self {
            kind: Kind::of(meta.file_type()),
            size: meta.len(),
            modified: meta.modified().ok(),
            id: FileId::of(meta),
        }
    }

    fn read(path: &Path) -> Result<Self, String> {
        std::fs::symlink_metadata(path)
            .map(|meta| Self::of(&meta))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    OnlyLeft,
    OnlyRight,
    /// A different kind of item on each side, such as a folder and a file.
    TypeConflict,
    /// The same size and modification time: likely equal, not proven.
    SameMetadata,
    /// The same size, modified at different times.
    MetadataDiffers,
    /// Read and found equal byte for byte, or links to the same target.
    Equal,
    /// Different sizes or bytes, or links to different targets.
    Differs,
    /// Couldn't be read or compared: why, with the path.
    Unknown(String),
}

/// Which side was modified later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Newer {
    Left,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The same on both sides, relative to each; empty for two files.
    pub path: PathBuf,
    pub left: Option<Side>,
    pub right: Option<Side>,
    pub status: Status,
}

impl Entry {
    /// `None` when either time is unknown or they're within `SLACK`.
    pub fn newer(&self) -> Option<Newer> {
        newer(
            self.left.as_ref()?.modified?,
            self.right.as_ref()?.modified?,
        )
    }

    /// Files on both sides that only reading can tell apart.
    pub fn unverified(&self) -> bool {
        matches!(
            (&self.left, &self.right),
            (Some(l), Some(r)) if l.kind == Kind::File && r.kind == Kind::File && l.size == r.size
        ) && !matches!(self.status, Status::Equal | Status::Differs)
    }

    fn under(&self, paths: &[PathBuf]) -> bool {
        paths.iter().any(|path| self.path.starts_with(path))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    /// Both roots with links in them resolved.
    pub left: PathBuf,
    pub right: PathBuf,
    /// By path. Folders on both sides aren't entries; what they hold is.
    /// A folder on one side is one entry, not walked.
    pub entries: Vec<Entry>,
    /// False when it was cancelled, ran out of time or read `MAX_ITEMS`;
    /// `entries` holds the folders it finished.
    pub complete: bool,
}

/// Compares two files, or two folders whole, reading only metadata. Fails
/// when a root can't be found, or is a folder that can't be listed.
pub fn compare(
    left: &Path,
    right: &Path,
    progress: &Progress,
    deadline: Option<Instant>,
) -> Result<Comparison, String> {
    walk(
        left,
        right,
        progress,
        deadline,
        MAX_ITEMS,
        &crate::sizes::gated(),
    )
}

/// Compares without opening `gated` folders or anything in them.
fn walk(
    left: &Path,
    right: &Path,
    progress: &Progress,
    deadline: Option<Instant>,
    items: usize,
    gated: &[PathBuf],
) -> Result<Comparison, String> {
    let root = |path: &Path| {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Side::read(&path).map(|side| (path, side))
    };
    let ((left, l), (right, r)) = (root(left)?, root(right)?);
    for root in [&left, &right] {
        if let Some(why) = crate::sizes::guarded(root, gated) {
            return Err(format!("{}: {why}", root.display()));
        }
    }
    let mut comparison = Comparison {
        left,
        right,
        entries: Vec::new(),
        complete: true,
    };
    if l.kind != Kind::Dir || r.kind != Kind::Dir {
        let status = status(&comparison.left, &comparison.right, &l, &r);
        comparison.entries.push(Entry {
            path: PathBuf::new(),
            left: Some(l),
            right: Some(r),
            status,
        });
        progress.entries.fetch_add(1, Ordering::Relaxed);
        return Ok(comparison);
    }
    let mut budget = Budget {
        items,
        progress,
        deadline,
        gated,
    };
    let mut pending = vec![(PathBuf::new(), l, r)];
    while let Some((folder, l, r)) = pending.pop() {
        // Empty folders take nothing from the budget, so check here too.
        if progress.stopped(deadline) {
            comparison.complete = false;
            break;
        }
        let (left, right) = (
            at(&comparison.left, &folder),
            at(&comparison.right, &folder),
        );
        let mut names: BTreeMap<OsString, (Option<Listed>, Option<Listed>)> = BTreeMap::new();
        match (list(&left, &mut budget), list(&right, &mut budget)) {
            // Half a folder would show the rest as only on one side.
            (Ok(None), _) | (_, Ok(None)) => {
                comparison.complete = false;
                break;
            }
            (Ok(Some(on_left)), Ok(Some(on_right))) => {
                for (name, side) in on_left {
                    names.entry(name).or_default().0 = Some(side);
                }
                for (name, side) in on_right {
                    names.entry(name).or_default().1 = Some(side);
                }
            }
            (Err(why), _) | (_, Err(why)) if folder.as_os_str().is_empty() => return Err(why),
            (Err(why), _) | (_, Err(why)) => {
                comparison.entries.push(Entry {
                    path: folder,
                    left: Some(l),
                    right: Some(r),
                    status: Status::Unknown(why),
                });
                continue;
            }
        }
        for (name, sides) in names {
            let path = folder.join(&name);
            progress.entries.fetch_add(1, Ordering::Relaxed);
            let (l, r, status) = match sides {
                (Some(Ok(l)), Some(Ok(r))) if l.kind == Kind::Dir && r.kind == Kind::Dir => {
                    pending.push((path, l, r));
                    continue;
                }
                (Some(Ok(l)), Some(Ok(r))) => {
                    let status = status(&left.join(&name), &right.join(&name), &l, &r);
                    (Some(l), Some(r), status)
                }
                (Some(Ok(l)), None) => (Some(l), None, Status::OnlyLeft),
                (None, Some(Ok(r))) => (None, Some(r), Status::OnlyRight),
                (Some(Err(why)), r) => (None, r.and_then(Result::ok), Status::Unknown(why)),
                (l, Some(Err(why))) => (l.and_then(Result::ok), None, Status::Unknown(why)),
                (None, None) => continue,
            };
            comparison.entries.push(Entry {
                path,
                left: l,
                right: r,
                status,
            });
        }
    }
    comparison.entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(comparison)
}

type Listed = Result<Side, String>;

/// What a walk may still read.
struct Budget<'a> {
    items: usize,
    progress: &'a Progress,
    deadline: Option<Instant>,
    gated: &'a [PathBuf],
}

impl Budget<'_> {
    /// False once the walk has to stop.
    fn take(&mut self) -> bool {
        if self.items == 0 || self.progress.stopped(self.deadline) {
            return false;
        }
        self.items -= 1;
        true
    }
}

/// A folder's items by name, without following links. `None` once the
/// budget runs out partway.
fn list(folder: &Path, budget: &mut Budget) -> Result<Option<Vec<(OsString, Listed)>>, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", folder.display());
    if let Some(why) = crate::sizes::guarded(folder, budget.gated) {
        return Err(fail(why));
    }
    let mut listed = Vec::new();
    for item in std::fs::read_dir(folder).map_err(fail)? {
        if !budget.take() {
            return Ok(None);
        }
        let item = item.map_err(fail)?;
        // DirEntry::metadata doesn't follow links.
        let side = match item.metadata() {
            Ok(meta) => Ok(Side::of(&meta)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => Err(format!("{}: {e}", item.path().display())),
        };
        listed.push((item.file_name(), side));
    }
    Ok(Some(listed))
}

fn newer(left: SystemTime, right: SystemTime) -> Option<Newer> {
    match left.duration_since(right) {
        Ok(by) if by > SLACK => Some(Newer::Left),
        Err(e) if e.duration() > SLACK => Some(Newer::Right),
        _ => None,
    }
}

/// What metadata says about two items of one name; links by their targets.
fn status(left: &Path, right: &Path, l: &Side, r: &Side) -> Status {
    if l.kind != r.kind {
        return Status::TypeConflict;
    }
    match l.kind {
        Kind::File if l.size != r.size => Status::Differs,
        Kind::File => match (l.modified, r.modified) {
            (Some(a), Some(b)) if newer(a, b).is_none() => Status::SameMetadata,
            _ => Status::MetadataDiffers,
        },
        Kind::Link => match (std::fs::read_link(left), std::fs::read_link(right)) {
            (Ok(a), Ok(b)) if a == b => Status::Equal,
            (Ok(_), Ok(_)) => Status::Differs,
            (Err(e), _) => Status::Unknown(format!("{}: {e}", left.display())),
            (_, Err(e)) => Status::Unknown(format!("{}: {e}", right.display())),
        },
        Kind::Dir | Kind::Other => Status::Unknown(format!(
            "{}: not a file, folder or link, so it can't be compared.",
            left.display()
        )),
    }
}

/// `root/path`, or `root` itself for the empty path two files compare at.
fn at(root: &Path, path: &Path) -> PathBuf {
    if path.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(path)
    }
}

impl Comparison {
    /// Reads every unverified entry at or under one of `paths` (an empty
    /// path is everything), `READERS` files at a time, and settles it as
    /// equal, differing or unknown. False if it stopped before the end;
    /// what it hadn't read stays as it was.
    pub fn verify(
        &mut self,
        paths: &[PathBuf],
        progress: &Progress,
        deadline: Option<Instant>,
    ) -> bool {
        let picked: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.entries[i].unverified() && self.entries[i].under(paths))
            .collect();
        let next = AtomicUsize::new(0);
        let (picked, next, this) = (&picked, &next, &*self);
        let read: Vec<(usize, Status)> = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..READERS)
                .map(|_| {
                    scope.spawn(move || {
                        let mut read = Vec::new();
                        let mut buffers = (vec![0; CHUNK], vec![0; CHUNK]);
                        while let Some(&i) = picked.get(next.fetch_add(1, Ordering::Relaxed)) {
                            let entry = &this.entries[i];
                            let (Some(l), Some(r)) = (&entry.left, &entry.right) else {
                                continue;
                            };
                            let left = (at(&this.left, &entry.path), l);
                            let right = (at(&this.right, &entry.path), r);
                            match same_bytes(left, right, &mut buffers, progress, deadline) {
                                Some(status) => read.push((i, status)),
                                None => break,
                            }
                        }
                        read
                    })
                })
                .collect();
            readers
                .into_iter()
                .flat_map(|reader| reader.join().unwrap_or_default())
                .collect()
        });
        let finished = read.len() == picked.len();
        for (i, status) in read {
            self.entries[i].status = status;
        }
        finished
    }

    /// A one-way copy from left to right of the differences at or under
    /// `paths` (an empty path is everything). What's only on the left is
    /// copied; a file that differs is replaced when the left one is newer.
    /// The rest is skipped, and nothing is ever deleted. It reads the disk
    /// to check each item is as the comparison found it.
    pub fn plan(&self, paths: &[PathBuf]) -> CopyPlan {
        let rows = self
            .entries
            .iter()
            .filter(|entry| {
                entry.under(paths) && !matches!(entry.status, Status::Equal | Status::SameMetadata)
            })
            .map(|entry| {
                let source = at(&self.left, &entry.path);
                let destination = at(&self.right, &entry.path);
                let planned = planned(entry, &source, &destination);
                Row {
                    path: entry.path.clone(),
                    action: match &planned {
                        Ok(Planned { replaces: None, .. }) => Action::Copy,
                        Ok(Planned { .. }) => Action::Replace,
                        Err(why) => Action::Skip(why.clone()),
                    },
                    planned: planned.ok(),
                    source,
                    destination,
                }
            })
            .collect();
        CopyPlan {
            from: self.left.clone(),
            into: self.right.clone(),
            rows,
        }
    }
}

/// Whether two files hold the same bytes. `None` once stopped.
fn same_bytes(
    (left, l): (PathBuf, &Side),
    (right, r): (PathBuf, &Side),
    (a, b): &mut (Vec<u8>, Vec<u8>),
    progress: &Progress,
    deadline: Option<Instant>,
) -> Option<Status> {
    let open = |path: &Path, side: &Side| -> Result<File, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        unchanged(&file, path, side, "since the comparison; compare again")?;
        Ok(file)
    };
    let (mut lf, mut rf) = match (open(&left, l), open(&right, r)) {
        (Ok(lf), Ok(rf)) => (lf, rf),
        (Err(why), _) | (_, Err(why)) => return Some(Status::Unknown(why)),
    };
    loop {
        if progress.stopped(deadline) {
            return None;
        }
        let read = fill(&mut lf, a, &left).and_then(|n| Ok((n, fill(&mut rf, b, &right)?)));
        let (n, m) = match read {
            Ok(read) => read,
            Err(why) => return Some(Status::Unknown(why)),
        };
        progress.bytes.fetch_add((n + m) as u64, Ordering::Relaxed);
        if n != m || a[..n] != b[..m] {
            return Some(Status::Differs);
        }
        if n == 0 {
            break;
        }
    }
    let changed = [(&lf, &left, l), (&rf, &right, r)]
        .into_iter()
        .find_map(|(file, path, side)| unchanged(file, path, side, "while it was read").err());
    Some(changed.map_or(Status::Equal, Status::Unknown))
}

fn unchanged(file: &File, path: &Path, side: &Side, when: &str) -> Result<(), String> {
    let now = file
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if Side::of(&now) == *side {
        Ok(())
    } else {
        Err(format!("{} changed {when}.", path.display()))
    }
}

/// Reads until `buffer` is full or the file ends.
fn fill(file: &mut File, buffer: &mut [u8], path: &Path) -> Result<usize, String> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(filled)
}

/// What one row of a plan copies, or why it doesn't.
fn planned(entry: &Entry, source: &Path, destination: &Path) -> Result<Planned, String> {
    let found = |path: &Path, side: &Side| -> Result<(FileId, Stamp), String> {
        if Side::read(path)? != *side {
            return Err(format!(
                "{} changed since the comparison. Compare again.",
                path.display()
            ));
        }
        disk::stamp(path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let replaces = match (&entry.status, &entry.left, &entry.right) {
        (Status::OnlyLeft, Some(_), None) => {
            if disk::id(destination).is_some() {
                return Err(format!(
                    "{} appeared since the comparison. Compare again.",
                    destination.display()
                ));
            }
            None
        }
        (Status::Differs | Status::MetadataDiffers, Some(l), Some(r))
            if matches!(l.kind, Kind::File | Kind::Link) =>
        {
            match entry.newer() {
                Some(Newer::Left) => Some(found(destination, r)?),
                Some(Newer::Right) => return Err("The one on the right is newer.".into()),
                None => return Err("Neither is newer, so neither replaces the other.".into()),
            }
        }
        (Status::OnlyRight, ..) => {
            return Err("Only on the right; Monodrive never deletes it.".into());
        }
        (Status::TypeConflict, Some(l), Some(r)) => {
            return Err(format!(
                "A {} on the left and a {} on the right.",
                kind_name(l.kind),
                kind_name(r.kind)
            ));
        }
        (Status::Unknown(why), ..) => return Err(why.clone()),
        _ => return Err("Nothing to copy.".into()),
    };
    let side = entry.left.as_ref().ok_or("Nothing to copy.")?;
    Ok(Planned {
        source: source.to_path_buf(),
        to: destination.to_path_buf(),
        found: found(source, side)?,
        replaces,
    })
}

pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "file",
        Kind::Dir => "folder",
        Kind::Link => "link",
        Kind::Other => "special file",
    }
}

/// A one-way copy, as a preview: what it copies, replaces and skips.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyPlan {
    pub from: PathBuf,
    pub into: PathBuf,
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// As the comparison's entry has it.
    pub path: PathBuf,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub action: Action,
    planned: Option<Planned>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing is at the destination.
    Copy,
    /// What's at the destination goes to the Trash first.
    Replace,
    /// Left alone: why.
    Skip(String),
}

impl CopyPlan {
    /// The copy job for the owner, which refuses each item whose source or
    /// destination changed since the plan. `None` when nothing is copied.
    pub fn request(&self) -> Option<Request> {
        let items: Vec<Planned> = self
            .rows
            .iter()
            .filter_map(|row| row.planned.clone())
            .collect();
        (!items.is_empty()).then(|| Request::CopyPlanned {
            into: self.into.clone(),
            items,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Add,
    Del,
    Context,
}

/// How a line ends. Only the last line of a file can have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    None,
    Lf,
    CrLf,
}

/// One line of a text diff. Numbers count from 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// Without its ending; any other `\r` stays.
    pub text: String,
    pub ending: Ending,
    pub old_number: Option<usize>,
    pub new_number: Option<usize>,
}

/// A run of `TextDiff::lines`: changes with their context, or unchanged
/// lines folded away between them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Hunk(Range<usize>),
    Fold(Range<usize>),
}

/// Monocode's unified diff model, without its Git staging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextDiff {
    pub additions: usize,
    pub deletions: usize,
    pub lines: Vec<Line>,
    pub blocks: Vec<Block>,
}

/// A line diff of two text files, `context` unchanged lines around each
/// change. Refuses a file that's binary, not UTF-8, or larger than
/// `DIFF_BYTES`, without reading it whole.
pub fn diff(left: &Path, right: &Path, context: usize) -> Result<TextDiff, String> {
    let (old, new) = (text(left)?, text(right)?);
    // similar's own line split also ends a line at a lone \r.
    let (old, new): (Vec<&str>, Vec<&str>) = (
        old.split_inclusive('\n').collect(),
        new.split_inclusive('\n').collect(),
    );
    let diff = similar::TextDiff::configure()
        .timeout(DIFF_TIME)
        .diff_slices(&old, &new);
    let lines: Vec<Line> = diff
        .iter_all_changes()
        .map(|change| {
            let line = change.value();
            let (text, ending) = match (line.strip_suffix("\r\n"), line.strip_suffix('\n')) {
                (Some(text), _) => (text, Ending::CrLf),
                (None, Some(text)) => (text, Ending::Lf),
                (None, None) => (line, Ending::None),
            };
            Line {
                kind: match change.tag() {
                    similar::ChangeTag::Insert => LineKind::Add,
                    similar::ChangeTag::Delete => LineKind::Del,
                    similar::ChangeTag::Equal => LineKind::Context,
                },
                text: text.to_string(),
                ending,
                old_number: change.old_index().map(|i| i + 1),
                new_number: change.new_index().map(|i| i + 1),
            }
        })
        .collect();
    Ok(TextDiff {
        additions: lines.iter().filter(|l| l.kind == LineKind::Add).count(),
        deletions: lines.iter().filter(|l| l.kind == LineKind::Del).count(),
        blocks: fold(&lines, context),
        lines,
    })
}

fn text(path: &Path) -> Result<String, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    let file = File::open(path).map_err(fail)?;
    let meta = file.metadata().map_err(fail)?;
    if !meta.is_file() {
        return Err(format!("{}: not a file.", path.display()));
    }
    if meta.len() > DIFF_BYTES {
        return Err(format!(
            "{} is {} bytes, more than the {DIFF_BYTES} a text diff reads.",
            path.display(),
            meta.len()
        ));
    }
    let mut bytes = Vec::new();
    file.take(DIFF_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(fail)?;
    if bytes.len() as u64 > DIFF_BYTES {
        return Err(format!("{} grew while it was read.", path.display()));
    }
    // Git's test: a NUL early on means binary.
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return Err(format!(
            "{} is binary, so it has no text diff.",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "{} isn't UTF-8 text, so it has no text diff.",
            path.display()
        )
    })
}

/// Monocode's `fold_unified_lines`: unchanged lines more than `context`
/// from any change fold away.
fn fold(lines: &[Line], context: usize) -> Vec<Block> {
    let mut shown: Vec<bool> = lines.iter().map(|l| l.kind != LineKind::Context).collect();
    for (i, line) in lines.iter().enumerate() {
        if line.kind != LineKind::Context {
            let to = (i + context).min(lines.len() - 1);
            shown[i.saturating_sub(context)..=to].fill(true);
        }
    }
    let mut blocks = Vec::new();
    let mut start = 0;
    for i in 1..=lines.len() {
        if i == lines.len() || shown[i] != shown[start] {
            blocks.push(if shown[start] {
                Block::Hunk(start..i)
            } else {
                Block::Fold(start..i)
            });
            start = i;
        }
    }
    blocks
}

#[cfg(test)]
mod tests;
