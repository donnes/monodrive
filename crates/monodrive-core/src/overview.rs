//! The Overview: what could free space, where the user's things are, and what
//! the locations can say about themselves. The window and the MCP tool read
//! the same `Overview`.
//!
//! Reading one walks nothing: it takes capacity from the volumes and sizes from
//! the saved summary, so it answers at once and connects to nothing. Sizes are
//! measured separately, a folder at a time, and each is saved with its date.
//! The summary holds totals and the folders they're for, never a file list.

use crate::cloud::CloudFolder;
use crate::entries::Entry;
use crate::ops::{self, Origin, Outcome, Request};
use crate::places;
use crate::remote::Source;
use crate::servers;
use crate::storage::{self, Report, Stop, Usage};
use crate::volumes::{Volume, VolumeId, holding};
use crate::worktrees::{self, Git};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const VERSION: u32 = 1;
/// Suggestions appear only for a location with less than this share free.
const LOW_FREE: f64 = 0.2;
const MAX_SUGGESTIONS: usize = 3;
/// A total this old is measured again when the page opens.
const REMEASURE: u64 = 60 * 60;
/// A total this old says how old it is.
const CURRENT: u64 = 5 * 60;
/// What a drive's scan keeps: its largest folders, and the rest as one number.
const SCAN_ROWS: usize = 12;
/// How many of what the index finds for a name one look goes through, and how
/// many new repositories it adds.
const MAX_SEARCHED: usize = 500;
const MAX_FOUND: usize = 200;
const KNOWN_FOLDERS: [&str; 6] = [
    "Desktop",
    "Documents",
    "Downloads",
    "Movies",
    "Music",
    "Pictures",
];
/// Downloads each tool fetches again when it needs them, and whose cleanup it
/// doesn't own. The pnpm store isn't here: `node_modules` folders hard-link
/// into it, and only pnpm knows what's unused.
const TOOL_CACHES: [(&str, &str); 6] = [
    ("Homebrew", "Library/Caches/Homebrew"),
    ("Xcode", "Library/Developer/Xcode/DerivedData"),
    ("pip", "Library/Caches/pip"),
    ("Yarn", "Library/Caches/Yarn"),
    ("CocoaPods", "Library/Caches/CocoaPods"),
    ("Cargo", ".cargo/registry/cache"),
];
/// How every cache directory tag starts, by the Cache Directory Tagging spec.
const CACHE_TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Where the Overview reads and saves.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub trash: PathBuf,
    pub store: places::Store,
}

impl Env {
    /// For the Trash at `trash`, which is in the home folder.
    pub fn new(trash: PathBuf, store: places::Store) -> Self {
        Self {
            home: trash.parent().map_or_else(PathBuf::new, Path::to_path_buf),
            trash,
            store,
        }
    }

    fn saved(&self) -> PathBuf {
        self.store.places.with_file_name("overview.json")
    }
}

/// What a folder took up when it was measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measured {
    pub on_disk: u64,
    /// The part of `on_disk` no clone or snapshot shares: what trashing it
    /// frees once the Trash is emptied.
    pub unshared: u64,
    /// When, in seconds since 1970.
    pub at: u64,
    /// False when part of it couldn't be read or the walk stopped early, so
    /// the totals are what it got to.
    pub complete: bool,
}

impl Measured {
    fn of(usage: Usage, complete: bool, at: u64) -> Self {
        Self {
            on_disk: usage.on_disk,
            unshared: usage.unshared,
            at,
            complete,
        }
    }

    /// How old it is, when it isn't current: "3 days ago".
    pub fn age(&self, now: u64) -> Option<String> {
        age(self.at, now)
    }
}

/// How long ago `at` was, when that's long enough to say.
pub fn age(at: u64, now: u64) -> Option<String> {
    let ago = now.saturating_sub(at);
    if ago < CURRENT {
        return None;
    }
    let (n, unit) = if ago < 3_600 {
        (ago / 60, "minute")
    } else if ago < 86_400 {
        (ago / 3_600, "hour")
    } else {
        (ago / 86_400, "day")
    };
    Some(format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" }))
}

/// The startup disk, which the page's headline is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Headline {
    pub name: String,
    pub total: u64,
    pub free: u64,
    /// What Empty Trash would free, once measured. The Trash can't be read
    /// without Full Disk Access.
    pub trash: Option<Measured>,
    /// Under the share where suggestions appear.
    pub low: bool,
}

impl Headline {
    /// Free space, as it will be once the Trash is emptied.
    pub fn free_after_trash(&self) -> u64 {
        self.free + self.trash.map_or(0, |trash| trash.unshared)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A build's output folder, tagged as a cache, beside its manifest.
    BuildOutput { project: String },
    /// A tool's download cache, at the path that tool uses.
    ToolCache { tool: String },
}

impl Evidence {
    pub fn title(&self) -> String {
        match self {
            Evidence::BuildOutput { project } => format!("Clean {project}'s build"),
            Evidence::ToolCache { tool } => format!("Clear {tool}'s cache"),
        }
    }

    /// What its button says.
    pub fn action(&self) -> &'static str {
        match self {
            Evidence::BuildOutput { .. } => "Clean",
            Evidence::ToolCache { .. } => "Clear",
        }
    }

    /// Why it's safe.
    pub fn why(&self) -> String {
        match self {
            Evidence::BuildOutput { .. } => {
                "Tagged as a cache. The next build makes it again.".to_string()
            }
            Evidence::ToolCache { tool } => {
                format!("{tool} downloads it again when it needs it.")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub path: PathBuf,
    pub evidence: Evidence,
    pub size: Measured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupKind {
    Folder,
    Project,
    Cloud,
}

/// Where a group has things.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot {
    /// The location's name.
    pub location: String,
    pub path: PathBuf,
    pub size: Option<Measured>,
    /// Its location is mounted, so the path can be opened.
    pub reachable: bool,
}

/// A row of "your stuff": a kind of thing or a project, in one place or, for a
/// project with the same remote in several, each of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub kind: GroupKind,
    pub spots: Vec<Spot>,
}

impl Group {
    /// All it takes up, across the places it's in, which ranks it. `None` until
    /// one of them is measured.
    pub fn on_disk(&self) -> Option<u64> {
        self.spots
            .iter()
            .filter_map(|spot| spot.size.map(|size| size.on_disk))
            .reduce(|all, size| all + size)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocationKind {
    Drive,
    /// `address` is saved, without any password.
    Share {
        address: Option<String>,
    },
    Sftp {
        id: String,
    },
    /// A drive that isn't mounted, known by its last scan.
    Disconnected,
}

/// A folder at the top of a drive's scan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRow {
    pub name: String,
    pub path: PathBuf,
    pub size: Measured,
    /// Its normalized `origin`, when it's a Git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// Which volume a scan was of: its identity, or where it was mounted when it
/// has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum VolumeKey {
    Id(VolumeId),
    Mount(PathBuf),
}

impl VolumeKey {
    fn of(volume: &Volume) -> Self {
        match &volume.id {
            Some(id) => Self::Id(id.clone()),
            None => Self::Mount(volume.mount.clone()),
        }
    }
}

/// A drive's or share's last scan, as saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scan {
    volume: VolumeKey,
    name: String,
    pub total: Measured,
    /// The largest folders, largest first.
    pub rows: Vec<ScanRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub name: String,
    pub kind: LocationKind,
    /// Where it's mounted. `None` means there's nothing to open or scan.
    pub mount: Option<PathBuf>,
    /// Total and free. Each location's stands alone: free space on a backup
    /// doesn't help a full startup disk, so it's never added up.
    pub capacity: Option<(u64, u64)>,
    pub connected: bool,
    /// `None` until someone asks for a scan.
    pub scan: Option<Scan>,
    /// Items here can go to its Trash: a local drive that can be written to.
    pub takes_trash: bool,
}

/// What the page and the `overview` tool show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub headline: Option<Headline>,
    pub suggestions: Vec<Suggestion>,
    /// Largest first; what hasn't been measured comes last.
    pub stuff: Vec<Group>,
    /// What the startup disk holds that no group explains, once all are
    /// measured.
    pub everything_else: Option<u64>,
    /// Every location but the startup disk.
    pub locations: Vec<Location>,
    /// Folders to measure now, because no total is saved or it's old.
    pub due: Vec<PathBuf>,
}

/// One file `overview.json` holds. Totals for the startup disk's folders by
/// path, drives' scans by volume, and what the index last found.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    version: u32,
    #[serde(default)]
    totals: HashMap<PathBuf, Measured>,
    #[serde(default)]
    scans: Vec<Scan>,
    /// Repositories' main folders the index found and Git confirmed.
    #[serde(default)]
    projects: Vec<PathBuf>,
    /// Folders the index found with a cache directory tag.
    #[serde(default)]
    tagged: Vec<PathBuf>,
}

impl Saved {
    /// A file from another version or one that doesn't read means nothing is
    /// saved yet: it's only a cache of what can be measured again.
    fn read(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .filter(|saved| saved.version == VERSION)
            .unwrap_or_default()
    }

    fn write(mut self, path: &Path) -> Result<(), String> {
        self.version = VERSION;
        let json = serde_json::to_vec_pretty(&self).map_err(|e| e.to_string())?;
        let next = path.with_extension("json.tmp");
        std::fs::write(&next, json)
            .and_then(|()| std::fs::rename(&next, path))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The window and each agent's server change the file, so the lock covers
    /// the read as well as the write, and goes with the file it's held on.
    fn change(path: &Path, change: impl FnOnce(&mut Self)) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let lock = path.with_extension("lock");
        let _held = std::fs::File::create(&lock)
            .and_then(|file| file.lock().map(|()| file))
            .map_err(|e| format!("{}: {e}", lock.display()))?;
        let mut saved = Self::read(path);
        change(&mut saved);
        saved.write(path)
    }
}

/// Now, in seconds since 1970.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The name of the last part of `path`.
fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// `url` as the same text for every way of writing one remote:
/// `git@github.com:a/b.git` and `https://github.com/a/b` are both
/// `github.com/a/b`.
fn normalized_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?,
        // scp's form, which a local path can't be mistaken for: it has a colon
        // before any slash.
        None => match url.split_once(':') {
            Some((host, path)) if !host.contains('/') => (host, path),
            _ => return None,
        },
    };
    let host = host.rsplit('@').next()?.to_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| format!("{host}/{path}"))
}

/// The normalized `origin` of the repository at `dir`. Reads its local config
/// and nothing else, so it needs no network and no Git.
fn remote_of(dir: &Path) -> Option<String> {
    let config = std::fs::read_to_string(dir.join(".git/config")).ok()?;
    let mut in_origin = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            return normalized_remote(value);
        }
    }
    None
}

/// The output folder's project when `dir` is a Cargo build's: named `target`,
/// tagged as a cache, with the manifest that makes it beside it.
fn build_output(dir: &Path) -> Option<Evidence> {
    let project = dir.parent()?;
    let tagged =
        std::fs::read(dir.join("CACHEDIR.TAG")).is_ok_and(|tag| tag.starts_with(CACHE_TAG));
    (dir.file_name()? == "target" && tagged && project.join("Cargo.toml").is_file()).then(|| {
        Evidence::BuildOutput {
            project: name_of(project),
        }
    })
}

/// The proof `dir` can go to the Trash, as it stands now.
fn evidence(env: &Env, dir: &Path) -> Option<Evidence> {
    let cache = TOOL_CACHES
        .iter()
        .find(|(_, at)| env.home.join(at) == dir)
        .filter(|_| dir.is_dir())
        .map(|(tool, _)| Evidence::ToolCache {
            tool: (*tool).to_string(),
        });
    cache.or_else(|| build_output(dir))
}

/// Whether Cargo holds a lock in a profile of the output folder `target`,
/// `target/debug` or `target/<triple>/debug`. Tries for a shared lock on each
/// `.cargo-lock` without waiting, and lets it go at once.
fn cargo_building(target: &Path) -> bool {
    let locked = |lock: PathBuf| {
        std::fs::File::open(lock).is_ok_and(|file| {
            // SAFETY: the descriptor is open for this call, and closing the
            // file releases what it took.
            let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
            taken != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK)
        })
    };
    let folders = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect()
    };
    folders(target).iter().any(|profile| {
        locked(profile.join(".cargo-lock"))
            || folders(profile)
                .into_iter()
                .any(|nested| locked(nested.join(".cargo-lock")))
    })
}

/// Why `dir` can't go to the Trash now, if it can't: its proof is gone, or
/// moving it would break a build underway.
pub fn check(env: &Env, dir: &Path) -> Result<Evidence, String> {
    let evidence = evidence(env, dir)
        .ok_or_else(|| format!("{} is no longer safe to clean up.", dir.display()))?;
    if let Evidence::BuildOutput { project } = &evidence
        && cargo_building(dir)
    {
        return Err(format!(
            "Cargo is building in {project}. Try again when it finishes."
        ));
    }
    Ok(evidence)
}

/// Rechecks a suggestion and moves its folder to the Trash through the owner,
/// so it can be undone.
pub fn clean(
    env: &Env,
    dir: &Path,
    origin: Origin,
    ops: &mut ops::Client,
) -> Result<Outcome, String> {
    check(env, dir)?;
    ops.submit(
        origin,
        Request::Trash {
            paths: vec![dir.to_path_buf()],
        },
    )
}

/// Whether `folder`'s own entries changed after `at`, seconds since 1970.
fn changed_since(folder: &Path, at: u64) -> bool {
    std::fs::metadata(folder)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|changed| changed.duration_since(UNIX_EPOCH).ok())
        .is_some_and(|changed| changed.as_secs() > at)
}

/// Outermost folders only, so nested ones don't count twice.
fn outermost<'a>(paths: impl Iterator<Item = (&'a Path, u64)>) -> u64 {
    let spots: Vec<_> = paths.collect();
    spots
        .iter()
        .filter(|(path, _)| {
            !spots
                .iter()
                .any(|(other, _)| other != path && path.starts_with(other))
        })
        .map(|(_, size)| size)
        .sum()
}

/// What's connected, for a saved SFTP source. Each process has its own
/// connections, so the caller says.
pub type Connected<'a> = &'a dyn Fn(&str) -> bool;

/// The Overview as it stands, from `volumes` and what's saved. Walks nothing
/// and connects to nothing, but reads saved lists and stats a few paths, so
/// call it off the UI thread.
pub fn read(
    env: &Env,
    volumes: &[Volume],
    cloud: &[CloudFolder],
    connected: Connected,
    now: u64,
) -> Overview {
    let saved = Saved::read(&env.saved());
    let total = |path: &Path| saved.totals.get(path).copied();
    let mut due = Vec::new();
    let mut wanted = |path: &Path, size: Option<Measured>| {
        if size.is_none_or(|size| now.saturating_sub(size.at) >= REMEASURE) {
            due.push(path.to_path_buf());
        }
    };

    let startup = volumes.iter().find(|volume| volume.startup);
    let startup_name = startup.map_or("This Mac".to_string(), |volume| volume.name.clone());
    let spot_here = |path: &Path, size| Spot {
        location: startup_name.clone(),
        path: path.to_path_buf(),
        size,
        reachable: true,
    };

    // Elsewhere, a folder is measured only by a scan someone asks for, and
    // cleaning it wouldn't free space on the startup disk.
    let here = |path: &Path| holding(volumes, path).is_none_or(|volume| volume.startup);

    let mut stuff = Vec::new();
    for name in KNOWN_FOLDERS {
        let path = env.home.join(name);
        if path.is_dir() {
            wanted(&path, total(&path));
            stuff.push(Group {
                name: name.to_string(),
                kind: GroupKind::Folder,
                spots: vec![spot_here(&path, total(&path))],
            });
        }
    }
    // Placeholders take no blocks, so a cloud folder's total is its downloads.
    for folder in cloud {
        wanted(&folder.path, total(&folder.path));
        stuff.push(Group {
            name: folder.name.clone(),
            kind: GroupKind::Cloud,
            spots: vec![spot_here(&folder.path, total(&folder.path))],
        });
    }
    let registered = env.store.repositories().unwrap_or_default();
    let mut seen = HashSet::new();
    let projects: Vec<PathBuf> = registered
        .into_iter()
        .map(|repo| repo.path)
        .chain(saved.projects.iter().cloned())
        .filter(|path| here(path) && path.join(".git").exists() && seen.insert(path.clone()))
        .collect();
    // A project's remote is what says two copies are one project.
    let mut by_remote: HashMap<String, usize> = HashMap::new();
    for path in &projects {
        wanted(path, total(path));
        let remote = remote_of(path);
        let spot = spot_here(path, total(path));
        if let Some(at) = remote.as_ref().and_then(|remote| by_remote.get(remote)) {
            let group: &mut Group = &mut stuff[*at];
            group.spots.push(spot);
            continue;
        }
        if let Some(remote) = remote {
            by_remote.insert(remote, stuff.len());
        }
        stuff.push(Group {
            name: name_of(path),
            kind: GroupKind::Project,
            spots: vec![spot],
        });
    }

    // The Trash changes whenever anything goes in or comes out, so a total from
    // before that says nothing.
    let trash = total(&env.trash).filter(|size| !changed_since(&env.trash, size.at));
    let headline = startup.map(|volume| Headline {
        name: volume.name.clone(),
        total: volume.total,
        free: volume.free,
        trash,
        low: (volume.free as f64) < volume.total as f64 * LOW_FREE,
    });
    wanted(&env.trash, trash);

    // Worth measuring only when suggestions would show.
    let mut suggestions = Vec::new();
    if headline.as_ref().is_some_and(|headline| headline.low) {
        let caches = TOOL_CACHES.iter().map(|(_, at)| env.home.join(at));
        let targets = projects.iter().map(|project| project.join("target"));
        let mut seen = HashSet::new();
        for dir in caches.chain(targets).chain(saved.tagged.iter().cloned()) {
            if !here(&dir) || !seen.insert(dir.clone()) {
                continue;
            }
            let Some(evidence) = evidence(env, &dir) else {
                continue;
            };
            wanted(&dir, total(&dir));
            if let Some(size) = total(&dir).filter(|size| size.unshared > 0) {
                suggestions.push(Suggestion {
                    path: dir,
                    evidence,
                    size,
                });
            }
        }
        suggestions.sort_by(|a, b| {
            b.size
                .unshared
                .cmp(&a.size.unshared)
                .then(a.path.cmp(&b.path))
        });
        suggestions.truncate(MAX_SUGGESTIONS);
    }

    // Everything mounted, then what's only saved, then what could be connected.
    let mut locations = Vec::new();
    let mut matched = Vec::new();
    for volume in volumes.iter().filter(|volume| !volume.startup) {
        let key = VolumeKey::of(volume);
        // It may mount somewhere else now, and another drive may have taken
        // the old path, which is what its rows would otherwise open and trash.
        let scan = saved
            .scans
            .iter()
            .find(|scan| scan.volume == key)
            .map(|scan| {
                let mut scan = scan.clone();
                for row in &mut scan.rows {
                    row.path = volume.mount.join(&row.name);
                }
                scan
            });
        if scan.is_some() {
            matched.push(key);
        }
        locations.push(Location {
            name: volume.name.clone(),
            kind: if volume.network {
                LocationKind::Share { address: None }
            } else {
                LocationKind::Drive
            },
            mount: Some(volume.mount.clone()),
            capacity: Some((volume.total, volume.free)),
            connected: true,
            scan,
            takes_trash: !volume.network && !volume.read_only,
        });
    }
    for scan in saved.scans.iter().filter(|s| !matched.contains(&s.volume)) {
        locations.push(Location {
            name: scan.name.clone(),
            kind: LocationKind::Disconnected,
            mount: None,
            capacity: None,
            connected: false,
            scan: Some(scan.clone()),
            takes_trash: false,
        });
    }
    for address in env.store.servers().unwrap_or_default() {
        if servers::mounted(&address, volumes).is_none() {
            locations.push(Location {
                name: servers::without_password(&address),
                kind: LocationKind::Share {
                    address: Some(servers::without_password(&address)),
                },
                mount: None,
                capacity: None,
                connected: false,
                scan: None,
                takes_trash: false,
            });
        }
    }
    let sources: Vec<Source> = env.store.remotes().unwrap_or_default();
    for source in sources {
        locations.push(Location {
            name: source.id.clone(),
            connected: connected(&source.id),
            kind: LocationKind::Sftp { id: source.id },
            mount: None,
            capacity: None,
            scan: None,
            takes_trash: false,
        });
    }

    // A project on a scanned drive joins its copy here, and is one row.
    for location in &mut locations {
        let Some(scan) = &mut location.scan else {
            continue;
        };
        let reachable = location.mount.is_some();
        let name = location.name.clone();
        scan.rows.retain(|row| {
            let at = row.remote.as_ref().and_then(|remote| by_remote.get(remote));
            let Some(at) = at else {
                return true;
            };
            stuff[*at].spots.push(Spot {
                location: name.clone(),
                path: row.path.clone(),
                size: Some(row.size),
                reachable,
            });
            false
        });
    }

    stuff.sort_by(|a, b| {
        b.on_disk()
            .cmp(&a.on_disk())
            .then_with(|| a.name.cmp(&b.name))
    });
    let everything_else = headline.as_ref().and_then(|headline| {
        let here: Vec<_> = stuff
            .iter()
            .flat_map(|group| &group.spots)
            .filter(|spot| spot.location == headline.name)
            .collect();
        let sized: Option<Vec<_>> = here
            .iter()
            .map(|spot| Some((spot.path.as_path(), spot.size?.on_disk)))
            .collect();
        let used = headline.total.saturating_sub(headline.free);
        sized.map(|sized| used.saturating_sub(outermost(sized.into_iter())))
    });
    Overview {
        headline,
        suggestions,
        stuff,
        everything_else,
        locations,
        due,
    }
}

/// Measures `folder` and saves the total, for those of `read`'s `due` list.
/// `None` when it was cancelled, or when it can't be read at all, as the Trash
/// can't without Full Disk Access. Only that folder's walk reads the disk, so
/// call it off the UI thread.
pub fn measure(
    env: &Env,
    folder: &Path,
    progress: &storage::Progress,
    deadline: Option<Instant>,
    now: u64,
) -> Option<Measured> {
    let report = storage::scan(folder, progress, deadline).ok()?;
    if report.stopped == Some(Stop::Cancelled) {
        return None;
    }
    let root = report.root();
    let size = Measured::of(
        root.usage,
        report.stopped.is_none() && !root.unreadable,
        now,
    );
    let _ = Saved::change(&env.saved(), |saved| {
        saved.totals.insert(folder.to_path_buf(), size);
    });
    Some(size)
}

/// Keeps what `report` says about `volume`, whole or as far as it got, as that
/// volume's last scan. Only a scan someone asked for gets here.
pub fn record_scan(env: &Env, volume: &Volume, report: &Report, now: u64) -> Result<Scan, String> {
    let scan = scan_of(volume, report, now);
    let kept = scan.clone();
    Saved::change(&env.saved(), move |saved| {
        saved.scans.retain(|old| old.volume != scan.volume);
        saved.scans.push(scan);
    })?;
    Ok(kept)
}

/// Drops the total saved for `folder`, which something just changed, so it's
/// measured again.
pub fn forget(env: &Env, folder: &Path) {
    let _ = Saved::change(&env.saved(), |saved| {
        saved.totals.remove(folder);
    });
}

/// Leaves a folder that went to the Trash out of its drive's last scan.
pub fn forget_scan_row(env: &Env, path: &Path) -> Result<(), String> {
    Saved::change(&env.saved(), |saved| {
        for scan in &mut saved.scans {
            scan.rows.retain(|row| row.path != path);
        }
    })
}

/// If `folder` is where a volume other than the startup disk is mounted, that
/// volume.
pub fn volume_at<'a>(volumes: &'a [Volume], folder: &Path) -> Option<&'a Volume> {
    volumes
        .iter()
        .find(|volume| !volume.startup && volume.mount == folder)
}

fn scan_of(volume: &Volume, report: &Report, now: u64) -> Scan {
    let root = report.root();
    let row = |at: &usize| {
        let folder = &report.folders[*at];
        ScanRow {
            name: name_of(&folder.path),
            path: folder.path.clone(),
            size: Measured::of(folder.usage, !folder.unreadable, now),
            remote: remote_of(&folder.path),
        }
    };
    Scan {
        volume: VolumeKey::of(volume),
        name: volume.name.clone(),
        total: Measured::of(
            root.usage,
            report.stopped.is_none() && !root.unreadable,
            now,
        ),
        rows: root.children.iter().take(SCAN_ROWS).map(row).collect(),
    }
}

/// Adds what `search` finds, the index's answers for a name, to the
/// repositories and tagged folders saved, each once Git or its tag confirms it.
/// Runs Git, so call it off the UI thread.
pub fn discover(
    env: &Env,
    git: Option<&Git>,
    search: impl Fn(&str, usize) -> Result<Vec<Entry>, String>,
) -> Result<(), String> {
    let saved = Saved::read(&env.saved());
    let found = |name: &str| -> Result<Vec<PathBuf>, String> {
        Ok(search(name, MAX_SEARCHED)?
            .into_iter()
            .filter(|entry| entry.name == name)
            .filter_map(|entry| entry.path.parent().map(Path::to_path_buf))
            .collect())
    };
    let tagged: Vec<PathBuf> = found("CACHEDIR.TAG")?
        .into_iter()
        .filter(|dir| build_output(dir).is_some() && !saved.tagged.contains(dir))
        .collect();
    let projects: Vec<PathBuf> = found(".git")?
        .into_iter()
        .filter(|dir| !hidden_or_tool_folder(dir) && !saved.projects.contains(dir))
        .filter(|dir| git.is_some_and(|git| is_main_checkout(git, dir)))
        .take(MAX_FOUND)
        .collect();
    if projects.is_empty() && tagged.is_empty() {
        return Ok(());
    }
    Saved::change(&env.saved(), |saved| {
        saved.projects.extend(projects);
        saved.tagged.extend(tagged);
    })
}

/// A repository's own checkout: Git knows it, and it isn't a linked worktree.
fn is_main_checkout(git: &Git, dir: &Path) -> bool {
    let (Ok(common), Ok(real)) = (worktrees::common_dir(git, dir), dir.canonicalize()) else {
        return false;
    };
    common == real.join(".git")
}

/// Repositories under these belong to tools, not the user's projects.
fn hidden_or_tool_folder(path: &Path) -> bool {
    path.components().any(|part| {
        let part = part.as_os_str().to_string_lossy();
        part.starts_with('.') || part == "node_modules" || part == "Library"
    })
}

#[cfg(test)]
mod tests;
