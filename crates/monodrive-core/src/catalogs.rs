//! Offline catalogs: the names, kinds, sizes and dates under a source the
//! user or an agent picks, a volume or a folder on one, kept in Monodrive's
//! own folder so they can be searched while the source is disconnected.
//! fsearch can't keep a volume's entries once it's gone: its crawl never
//! crosses a mount point. docs/catalogs.md has the rules.

use crate::entries::{Entry, Kind};
use crate::index;
use crate::ops;
use crate::places::{Availability, Favorite, Pinned};
use crate::search::{Query, Scope};
use crate::sizes::{self, Met};
use crate::storage::{Progress, Stop};
use crate::volumes::{self, Volume, VolumeId};
use flate2::Compression;
use flate2::bufread::DeflateDecoder;
use flate2::write::DeflateEncoder;
use fsearch::walk::{KIND_DIR, KIND_FILE, KIND_LINK, KIND_OTHER};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::ffi::OsStr;
use std::fs::{File, Metadata};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A refresh stops after this many entries and keeps them, marked
/// incomplete. While it walks it holds only the folders' paths.
pub const MAX_ENTRIES: usize = 2_000_000;

/// Starts every catalog file; the digit is the format's version.
const MAGIC: &[u8; 8] = b"MDCATLG1";
/// The magic and the body's length.
const PREFIX: u64 = 16;
/// How often, in entries, a refresh's `Progress` calls its notify.
const NOTIFY_ENTRIES: u64 = 4096;

/// Where the catalogs are kept: `<id>.catalog` each, beside a `<id>.lock`
/// a refresh holds.
#[derive(Clone, Debug)]
pub struct Store {
    pub dir: PathBuf,
}

/// What a catalog is of: a volume, by its identity, or a folder on one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Where it was at the last good refresh, its links resolved.
    pub path: PathBuf,
    /// Its volume then, found again by identity wherever it mounts.
    pub volume: Pinned,
}

/// One catalog, as its file's header says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(skip)]
    pub id: String,
    pub source: Source,
    /// Seconds from the epoch when the last good refresh finished.
    pub refreshed: u64,
    pub entries: u64,
    /// Folders the last good refresh couldn't read, such as one that needs
    /// Full Disk Access. What's in them is missing.
    pub unreadable: u64,
    /// The last good refresh stopped at `MAX_ENTRIES`.
    pub truncated: bool,
    /// Why the latest refresh failed, if it did. The entries are still the
    /// last good refresh's.
    pub failed: Option<Failed>,
    /// The file's size on disk.
    #[serde(skip)]
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failed {
    /// Seconds from the epoch.
    pub at: u64,
    pub why: String,
}

/// What a search over catalogs found, best first.
#[derive(Debug)]
pub struct Found {
    /// Each catalog a hit came from.
    pub catalogs: Vec<Searched>,
    pub hits: Vec<Hit>,
}

#[derive(Debug)]
pub struct Searched {
    pub catalog: Catalog,
    /// Where its source is now, or why it isn't connected. Hits' paths are
    /// under where it is now, or else where it was last seen.
    pub source: Availability,
}

#[derive(Clone, Debug)]
pub struct Hit {
    /// Into `Found::catalogs`.
    pub catalog: usize,
    /// As cataloged; never downloaded or read.
    pub entry: Entry,
    /// Its path inside the source.
    pub inside: PathBuf,
    score: i32,
}

/// What a hit is on its source now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// There, the same kind and size, at this path.
    Current(PathBuf),
    /// What's at this path now isn't what was cataloged, or it's gone.
    Changed(PathBuf, String),
    /// Its source isn't connected; says which to connect.
    Offline(String),
}

impl Store {
    /// Monodrive's own, in its support folder.
    pub fn user() -> Option<Self> {
        Some(Self {
            dir: crate::support_dir()?.join("catalogs"),
        })
    }

    /// Every catalog, by volume name and then path. Reads only headers.
    pub fn list(&self) -> Result<Vec<Catalog>, String> {
        let listing = match std::fs::read_dir(&self.dir) {
            Ok(listing) => listing,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("{}: {e}", self.dir.display())),
        };
        let mut catalogs = Vec::new();
        for item in listing {
            let item = item.map_err(|e| format!("{}: {e}", self.dir.display()))?;
            let name = item.file_name();
            let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".catalog")) else {
                continue;
            };
            if is_id(id) {
                catalogs.push(self.read(id)?);
            }
        }
        catalogs.sort_by(|a, b| {
            (&a.source.volume.name, &a.source.path).cmp(&(&b.source.volume.name, &b.source.path))
        });
        Ok(catalogs)
    }

    /// Catalogs the folder at `path`, or refreshes the catalog of it that
    /// already exists. A new catalog is kept only once its walk finishes.
    /// Blocks while it walks, so call it off the UI thread.
    pub fn create(
        &self,
        path: &Path,
        volumes: &[Volume],
        progress: &Progress,
        deadline: Option<Instant>,
    ) -> Result<Catalog, String> {
        let source = source(path, volumes)?;
        let id = source.id();
        let _held = self.hold(&id)?;
        let old = self
            .file(&id)?
            .exists()
            .then(|| self.read(&id))
            .transpose()?;
        self.run(&id, source, old, progress, deadline, MAX_ENTRIES)
    }

    /// Walks the catalog's source again where it's mounted now. A refresh
    /// that fails or stops early keeps the last good entries and records
    /// why. Blocks while it walks, so call it off the UI thread.
    pub fn refresh(
        &self,
        id: &str,
        volumes: &[Volume],
        progress: &Progress,
        deadline: Option<Instant>,
    ) -> Result<Catalog, String> {
        self.refresh_within(id, volumes, progress, deadline, MAX_ENTRIES)
    }

    fn refresh_within(
        &self,
        id: &str,
        volumes: &[Volume],
        progress: &Progress,
        deadline: Option<Instant>,
        max: usize,
    ) -> Result<Catalog, String> {
        if !self.file(id)?.exists() {
            return Err(format!("No catalog {id}."));
        }
        let _held = self.hold(id)?;
        let old = self.read(id)?;
        let source = match old.source.locate(volumes) {
            Availability::Available(path) => {
                let pinned = &old.source.volume;
                let volume = volumes
                    .iter()
                    .find(|volume| volume.id.as_ref() == Some(&pinned.id));
                Source {
                    path,
                    volume: Pinned {
                        id: pinned.id.clone(),
                        name: volume.map_or_else(|| pinned.name.clone(), |v| v.name.clone()),
                        mount: volume.map_or_else(|| pinned.mount.clone(), |v| v.mount.clone()),
                    },
                }
            }
            Availability::Unavailable(why) => return Err(self.fail(old, why)),
        };
        self.run(id, source, Some(old), progress, deadline, max)
    }

    /// Removes the catalog's file, and only that: the source is untouched.
    pub fn forget(&self, id: &str) -> Result<(), String> {
        let file = self.file(id)?;
        let held = self.hold(id)?;
        let removed = discard(&file).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => format!("No catalog {id}."),
            _ => format!("{}: {e}", file.display()),
        });
        let _ = discard(&self.lock(id));
        drop(held);
        removed
    }

    /// Runs `query`, in find_files' language, over every catalog, or only
    /// those its `in:` overlaps, best matches first. Reads only Monodrive's
    /// catalog files, never a source.
    pub fn search(&self, query: &Query, volumes: &[Volume], limit: usize) -> Result<Found, String> {
        let home = std::env::var("HOME").unwrap_or_default();
        let q = index::fsearch_query(query, &Scope::Everywhere, &home, index::now())?;
        let catalogs: Vec<Searched> = self
            .list()?
            .into_iter()
            .map(|catalog| Searched {
                source: catalog.source.locate(volumes),
                catalog,
            })
            .collect();
        let mut hits = self.scan(&catalogs, &q, limit)?;
        // As fsearch does: typos past the first letter only when nothing
        // closer matched.
        if hits.is_empty() && q.tokens.iter().any(|t| !t.negate && t.edits > 0) {
            hits = self.scan(&catalogs, &q.deepen(), limit)?;
        }
        // Only the catalogs hits came from, in order of their best.
        let mut kept: Vec<usize> = Vec::new();
        for hit in &mut hits {
            let at = kept
                .iter()
                .position(|&c| c == hit.catalog)
                .unwrap_or_else(|| {
                    kept.push(hit.catalog);
                    kept.len() - 1
                });
            hit.catalog = at;
        }
        let mut catalogs: Vec<Option<Searched>> = catalogs.into_iter().map(Some).collect();
        Ok(Found {
            catalogs: kept.iter().filter_map(|&c| catalogs[c].take()).collect(),
            hits,
        })
    }

    fn scan(
        &self,
        catalogs: &[Searched],
        q: &fsearch::Query,
        limit: usize,
    ) -> Result<Vec<Hit>, String> {
        let forms: Vec<fsearch::Query> = std::iter::once(q.clone())
            .chain(q.alt.as_ref().map(|alt| fsearch::Query {
                tokens: alt.clone(),
                alt: None,
                ..q.clone()
            }))
            .collect();
        let scope = q.scope.as_deref().map(|s| Path::new(OsStr::from_bytes(s)));
        let mut hits: Vec<Hit> = Vec::new();
        let keep = |hits: &mut Vec<Hit>| {
            hits.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then_with(|| a.entry.path.cmp(&b.entry.path))
            });
            hits.truncate(limit);
        };
        for (at, searched) in catalogs.iter().enumerate() {
            let root = match &searched.source {
                Availability::Available(path) => path,
                Availability::Unavailable(_) => &searched.catalog.source.path,
            };
            if scope.is_some_and(|s| !root.starts_with(s) && !s.starts_with(root)) {
                continue;
            }
            let mut path = root.as_os_str().as_bytes().to_vec();
            path.push(b'/');
            let base = path.len();
            self.each(&searched.catalog.id, |inside, kind, size, mtime| {
                path.truncate(base);
                path.extend_from_slice(inside);
                let second = u32::try_from(mtime.clamp(0, u32::MAX.into())).unwrap_or(0);
                let Some(score) = forms
                    .iter()
                    .filter_map(|form| form.match_path(&path, kind, size, second))
                    .max()
                else {
                    return;
                };
                let full = PathBuf::from(OsStr::from_bytes(&path));
                hits.push(Hit {
                    catalog: at,
                    entry: Entry {
                        name: full
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        path: full,
                        kind: kind_of(kind),
                        size,
                        modified: u64::try_from(mtime).unwrap_or(0),
                        availability: crate::cloud::Availability::Unknown,
                    },
                    inside: PathBuf::from(OsStr::from_bytes(inside)),
                    score,
                });
                if hits.len() >= limit.max(64) * 4 {
                    keep(&mut hits);
                }
            })?;
        }
        keep(&mut hits);
        Ok(hits)
    }

    /// Calls `each` with every entry in the catalog's body: its path inside
    /// the source, fsearch's kind, its size and modification second.
    fn each(&self, id: &str, mut each: impl FnMut(&[u8], u8, u64, i64)) -> Result<(), String> {
        let path = self.file(id)?;
        let fail = |e: io::Error| format!("{}: {e}", path.display());
        let (file, len) = open(&path).map_err(fail)?;
        let mut body = BufReader::new(DeflateDecoder::new(BufReader::new(file.take(len))));
        let mut inside = Vec::new();
        loop {
            if body.fill_buf().map_err(fail)?.is_empty() {
                return Ok(());
            }
            let shared = varint(&mut body).map_err(fail)? as usize;
            let more = varint(&mut body).map_err(fail)? as usize;
            if shared > inside.len() {
                return Err(fail(io::ErrorKind::InvalidData.into()));
            }
            inside.truncate(shared);
            let start = inside.len();
            inside.resize(start + more, 0);
            body.read_exact(&mut inside[start..]).map_err(fail)?;
            let mut kind = [0];
            body.read_exact(&mut kind).map_err(fail)?;
            let size = varint(&mut body).map_err(fail)?;
            let mtime = unzigzag(varint(&mut body).map_err(fail)?);
            each(&inside, kind[0], size, mtime);
        }
    }

    /// Walks `source` into a new body and, when the walk finishes or stops
    /// at `max`, puts it in place of the old. Otherwise keeps `old`, if
    /// there is one, recording why.
    fn run(
        &self,
        id: &str,
        source: Source,
        old: Option<Catalog>,
        progress: &Progress,
        deadline: Option<Instant>,
        max: usize,
    ) -> Result<Catalog, String> {
        match self.walk(id, source, progress, deadline, max) {
            Ok(catalog) => Ok(catalog),
            Err(why) => match old {
                Some(old) => Err(self.fail(old, why)),
                None => {
                    let _ = discard(&self.lock(id));
                    Err(why)
                }
            },
        }
    }

    fn walk(
        &self,
        id: &str,
        source: Source,
        progress: &Progress,
        deadline: Option<Instant>,
        max: usize,
    ) -> Result<Catalog, String> {
        let into = self.file(id)?;
        let fail = |e: io::Error| format!("{}: {e}", into.display());
        let draft = Draft::new(into.clone()).map_err(fail)?;
        let mut body = DeflateEncoder::new(BufWriter::new(&draft.file), Compression::default());
        let entries = Cell::new(0u64);
        let broken = Cell::new(false);
        let mut stop = None;
        let mut stopped = || {
            stop = stop
                .or_else(|| progress.stop(deadline))
                .or_else(|| (entries.get() >= max as u64).then_some(Stop::Budget));
            stop.is_some() || broken.get()
        };
        // Each folder's path inside the source, by the walk's number for it.
        let mut folders: Vec<Vec<u8>> = vec![Vec::new()];
        let (mut last, mut unreadable, mut counted) = (Vec::new(), 0, None);
        let mut error = None;
        let mut met = |met: Met| {
            let (at, name, meta) = match met {
                Met::Folder {
                    parent, path, meta, ..
                } => (parent, path.file_name().unwrap_or_default(), meta),
                Met::Item {
                    at, ref name, meta, ..
                } => (at, name.as_os_str(), meta),
                Met::Unreadable { at, .. } => {
                    unreadable += u64::from(counted != Some(at));
                    counted = Some(at);
                    return;
                }
            };
            let mut inside = folders[at].clone();
            if !inside.is_empty() {
                inside.push(b'/');
            }
            inside.extend_from_slice(name.as_bytes());
            if let Err(e) = record(&mut body, &mut last, &inside, meta) {
                error = Some(e);
                broken.set(true);
            }
            if meta.is_dir() {
                folders.push(inside);
            }
            entries.set(entries.get() + 1);
            progress.add(&progress.items, 1, NOTIFY_ENTRIES);
        };
        sizes::walk(&source.path, &sizes::gated(), &mut stopped, &mut met)?;
        drop(met);
        if let Some(e) = error {
            return Err(fail(e));
        }
        match stop {
            Some(Stop::Cancelled) => return Err("Cancelled before it finished.".into()),
            Some(Stop::Deadline) => return Err("Ran out of time before it finished.".into()),
            Some(Stop::Budget) | None => {}
        }
        body.finish()
            .and_then(|mut written| written.flush())
            .map_err(fail)?;
        let mut catalog = Catalog {
            id: id.into(),
            source,
            refreshed: index::now().max(0) as u64,
            entries: entries.get(),
            unreadable,
            truncated: stop == Some(Stop::Budget),
            failed: None,
            bytes: 0,
        };
        catalog.bytes = draft.commit(&catalog).map_err(fail)?;
        Ok(catalog)
    }

    /// Records that a refresh of `old` failed, keeping its entries, and
    /// says why with what's kept.
    fn fail(&self, mut old: Catalog, why: String) -> String {
        old.failed = Some(Failed {
            at: index::now().max(0) as u64,
            why: why.clone(),
        });
        let stop = if why.ends_with('.') { "" } else { "." };
        let kept = format!("{why}{stop} The last good catalog is kept.");
        let Ok(into) = self.file(&old.id) else {
            return kept;
        };
        let saved = (|| {
            let (mut file, len) = open(&into)?;
            let mut draft = Draft::new(into.clone())?;
            io::copy(&mut (&mut file).take(len), &mut draft.file)?;
            draft.commit(&old)
        })();
        match saved {
            Ok(_) => kept,
            Err(e) => format!(
                "{kept} Recording the failure failed: {}: {e}",
                into.display()
            ),
        }
    }

    fn read(&self, id: &str) -> Result<Catalog, String> {
        let path = self.file(id)?;
        let fail = |e: io::Error| match e.kind() {
            io::ErrorKind::NotFound => format!("No catalog {id}."),
            _ => format!("{}: {e}", path.display()),
        };
        let (mut file, len) = open(&path).map_err(fail)?;
        file.seek(SeekFrom::Start(PREFIX + len)).map_err(fail)?;
        let mut header = Vec::new();
        file.read_to_end(&mut header).map_err(fail)?;
        let mut catalog: Catalog =
            serde_json::from_slice(&header).map_err(|e| format!("{}: {e}", path.display()))?;
        catalog.id = id.into();
        catalog.bytes = PREFIX + len + header.len() as u64;
        Ok(catalog)
    }

    /// The catalog file for `id`, which must be one this store names.
    fn file(&self, id: &str) -> Result<PathBuf, String> {
        if !is_id(id) {
            return Err(format!("No catalog {id}."));
        }
        Ok(self.dir.join(format!("{id}.catalog")))
    }

    fn lock(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.lock"))
    }

    /// Holds the catalog's lock, so only one refresh or forget runs at once
    /// across the window and every MCP server.
    fn hold(&self, id: &str) -> Result<File, String> {
        let lock = self.lock(id);
        let fail = |e: io::Error| format!("{}: {e}", lock.display());
        std::fs::create_dir_all(&self.dir).map_err(fail)?;
        ops::lock(&lock, libc::LOCK_EX | libc::LOCK_NB).map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock => {
                format!("Catalog {id} is being refreshed. Wait for it to finish or cancel it.")
            }
            _ => fail(e),
        })
    }
}

impl Source {
    /// Where the source is now: on whichever mounted volume has its
    /// identity, maybe mounted somewhere else, never on another volume of
    /// the same name.
    pub fn locate(&self, volumes: &[Volume]) -> Availability {
        Favorite {
            path: self.path.clone(),
            volume: Some(self.volume.clone()),
        }
        .resolve(volumes)
    }

    /// The same for every catalog of this volume and folder, wherever the
    /// volume mounts.
    fn id(&self) -> String {
        let inside = self
            .path
            .strip_prefix(&self.volume.mount)
            .unwrap_or(&self.path);
        let mut hash = blake3::Hasher::new();
        match &self.volume.id {
            VolumeId::Uuid(uuid) => hash.update(b"uuid:").update(uuid.as_bytes()),
            VolumeId::Url(url) => hash.update(b"url:").update(url.as_bytes()),
        };
        hash.update(b"\0").update(inside.as_os_str().as_bytes());
        hash.finalize().to_hex()[..12].to_string()
    }
}

impl Catalog {
    /// Whether the last good refresh met everything.
    pub fn complete(&self) -> bool {
        !self.truncated && self.unreadable == 0
    }

    /// What `hit` is now, once its source is connected: the same kind and
    /// size at the path it has now, or what changed. Reads only that entry.
    pub fn resolve(&self, hit: &Hit, volumes: &[Volume]) -> Resolved {
        let root = match self.source.locate(volumes) {
            Availability::Available(root) => root,
            Availability::Unavailable(why) => return Resolved::Offline(why),
        };
        let path = root.join(&hit.inside);
        let was = &hit.entry;
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Resolved::Changed(path, "gone since cataloged".into());
            }
            Err(e) => return Resolved::Changed(path, e.to_string()),
        };
        let kind = Kind::of(meta.file_type());
        if kind != was.kind {
            let why = format!(
                "changed since cataloged: was {}, now {}",
                kind_name(was.kind),
                kind_name(kind)
            );
            return Resolved::Changed(path, why);
        }
        if kind != Kind::Dir && meta.len() != was.size {
            let why = format!(
                "changed since cataloged: was {} bytes, now {}",
                was.size,
                meta.len()
            );
            return Resolved::Changed(path, why);
        }
        Resolved::Current(path)
    }
}

/// The source for a catalog of the folder at `path`: its real path and the
/// volume holding it, which must have an identity and not be the startup
/// disk.
fn source(path: &Path, volumes: &[Volume]) -> Result<Source, String> {
    if !path.is_absolute() {
        return Err(format!("{}: give an absolute path.", path.display()));
    }
    let path = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !path.is_dir() {
        return Err(format!("{} isn't a folder.", path.display()));
    }
    let volume = volumes::holding(volumes, &path)
        .ok_or_else(|| format!("{} isn't on a mounted volume.", path.display()))?;
    if volume.startup {
        return Err(format!(
            "{} is on the startup disk, which is always connected. Catalog a volume or a folder on one.",
            path.display()
        ));
    }
    let Some(id) = volume.id.clone() else {
        return Err(format!(
            "macOS gives {} no identity, so Monodrive couldn't tell it from another drive once it's disconnected.",
            volume.name
        ));
    };
    let volume = Pinned {
        id,
        name: volume.name.clone(),
        mount: volume.mount.clone(),
    };
    Ok(Source { path, volume })
}

fn is_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Appends one entry: how much of the last path it shares, the rest of its
/// own, its kind, size and modification second.
fn record(
    out: &mut impl Write,
    last: &mut Vec<u8>,
    inside: &[u8],
    meta: &Metadata,
) -> io::Result<()> {
    let shared = last.iter().zip(inside).take_while(|(a, b)| a == b).count();
    let kind = if meta.is_dir() {
        KIND_DIR
    } else if meta.is_symlink() {
        KIND_LINK
    } else if meta.is_file() {
        KIND_FILE
    } else {
        KIND_OTHER
    };
    let size = if meta.is_dir() { 0 } else { meta.len() };
    let mut bytes = Vec::with_capacity(inside.len() - shared + 24);
    put_varint(&mut bytes, shared as u64);
    put_varint(&mut bytes, (inside.len() - shared) as u64);
    bytes.extend_from_slice(&inside[shared..]);
    bytes.push(kind);
    put_varint(&mut bytes, size);
    put_varint(&mut bytes, zigzag(meta.mtime()));
    last.truncate(shared);
    last.extend_from_slice(&inside[shared..]);
    out.write_all(&bytes)
}

fn put_varint(out: &mut Vec<u8>, mut n: u64) {
    while n >= 0x80 {
        out.push(n as u8 | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn varint(input: &mut impl Read) -> io::Result<u64> {
    let mut n = 0u64;
    for shift in (0..64).step_by(7) {
        let mut byte = [0];
        input.read_exact(&mut byte)?;
        n |= u64::from(byte[0] & 0x7f) << shift;
        if byte[0] < 0x80 {
            return Ok(n);
        }
    }
    Err(io::ErrorKind::InvalidData.into())
}

fn zigzag(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

fn unzigzag(n: u64) -> i64 {
    (n >> 1) as i64 ^ -((n & 1) as i64)
}

fn kind_of(kind: u8) -> Kind {
    match kind {
        KIND_FILE => Kind::File,
        KIND_DIR => Kind::Dir,
        KIND_LINK => Kind::Link,
        _ => Kind::Other,
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::File => "a file",
        Kind::Dir => "a folder",
        Kind::Link => "a link",
        Kind::Other => "something else",
    }
}

/// A catalog file open at its body, and the body's length.
fn open(path: &Path) -> io::Result<(File, u64)> {
    let mut file = File::open(path)?;
    let mut prefix = [0; PREFIX as usize];
    file.read_exact(&mut prefix)?;
    if &prefix[..8] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a catalog this Monodrive can read",
        ));
    }
    let len = u64::from_le_bytes(prefix[8..].try_into().unwrap_or_default());
    Ok((file, len))
}

/// A catalog file being written under a name of its own: the magic, room
/// for the body's length, the body, then the header. Dropped before
/// `commit`, it's removed, so a failed refresh leaves nothing behind.
struct Draft {
    file: File,
    tmp: Option<PathBuf>,
    into: PathBuf,
}

impl Draft {
    fn new(into: PathBuf) -> io::Result<Self> {
        static DRAFTS: AtomicU64 = AtomicU64::new(0);
        let n = DRAFTS.fetch_add(1, Ordering::Relaxed);
        let tmp = into.with_extension(format!("catalog.{}-{n}.tmp", std::process::id()));
        let mut file = File::create(&tmp)?;
        file.write_all(MAGIC)?;
        file.write_all(&[0; 8])?;
        Ok(Self {
            file,
            tmp: Some(tmp),
            into,
        })
    }

    /// Appends the header after what's been written, then renames the file
    /// into place. Returns its size.
    fn commit(mut self, catalog: &Catalog) -> io::Result<u64> {
        let body = self.file.seek(SeekFrom::End(0))? - PREFIX;
        serde_json::to_writer(&mut self.file, catalog)?;
        self.file.write_all_at(&body.to_le_bytes(), 8)?;
        let size = self.file.metadata()?.len();
        if let Some(tmp) = self.tmp.take() {
            std::fs::rename(tmp, &self.into)?;
        }
        Ok(size)
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = discard(&tmp);
        }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "catalogs and their drafts are Monodrive's own files, never the user's"
)]
fn discard(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

#[cfg(test)]
mod tests;
