//! What takes up space: folder_size, overview, storage_scan, duplicates
//! and cleanup_duplicates. Each stops at its timeout and says when its
//! answer is partial.

use super::{Call, Preview, local, local_one, scan_timeout, short_timeout};
use crate::cloud;
use crate::mcp::contract::{Planned, Reply, ToolError, shown};
use crate::mcp::server::Server;
use crate::overview::{self, LocationKind};
use crate::sizes;
use crate::storage::{self, Cleanup, Duplicates as Found, Match, Report, Stop, Usage};
use crate::volumes;
use crate::worktrees::Git;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn deadline(seconds: u64) -> Option<Instant> {
    Instant::now().checked_add(Duration::from_secs(seconds))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FolderSize {
    path: String,
    #[serde(default = "short_timeout")]
    timeout_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Size {
    pub path: String,
    pub bytes: u64,
    pub on_disk: u64,
    pub files: u64,
    pub folders: u64,
    /// Folders that couldn't be read; what's in them isn't counted.
    pub unreadable: u64,
    /// Cloud placeholders, counted at their size without downloading.
    pub not_downloaded: u64,
    /// It stopped at the timeout; these are what it had counted.
    pub partial: bool,
}

impl Call for FolderSize {
    type Out = Size;

    fn run(self, _: &mut Server) -> Result<Reply<Size>, ToolError> {
        let folder = local_one(&self.path, "folder_size")?;
        let size = sizes::measure(
            &folder,
            &AtomicBool::new(false),
            deadline(self.timeout_seconds),
        )?;
        let mut text = format!("{}/\n", folder.display());
        let _ = writeln!(
            text,
            "size: {} bytes ({} on disk)\nfiles: {}\nfolders: {}",
            size.bytes, size.on_disk, size.files, size.folders
        );
        if size.unreadable > 0 {
            let _ = writeln!(
                text,
                "unreadable folders: {} (what couldn't be read isn't counted)",
                size.unreadable
            );
        }
        placeholders(&mut text, size.placeholders);
        if !size.complete {
            let _ = writeln!(
                text,
                "(stopped after {} s: these are partial totals; pass a larger timeout_seconds)",
                self.timeout_seconds
            );
        }
        Ok(Reply::new(
            text,
            Size {
                path: shown(&folder),
                bytes: size.bytes,
                on_disk: size.on_disk,
                files: size.files,
                folders: size.folders,
                unreadable: size.unreadable,
                not_downloaded: size.placeholders,
                partial: !size.complete,
            },
        ))
    }
}

/// How many placeholders a total counted without downloading them.
fn placeholders(out: &mut String, n: u64) {
    if n > 0 {
        let _ = writeln!(
            out,
            "not downloaded: {n} placeholders, counted at their size; a placeholder folder's items aren't"
        );
    }
}

const fn overview_timeout() -> u64 {
    5
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Overview {
    #[serde(default = "overview_timeout")]
    timeout_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Disk {
    pub name: String,
    pub free: u64,
    pub total: u64,
    pub low: bool,
    /// What emptying the Trash would free, once measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trash: Option<u64>,
    /// What its used space is made of, as the window's bar shows it; Other
    /// holds what isn't measured yet.
    pub used_by: Vec<UsedBy>,
}

#[derive(Serialize, JsonSchema)]
pub struct UsedBy {
    pub kind: String,
    pub on_disk: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Suggestion {
    pub path: String,
    /// What trashing it frees once the Trash is emptied, at least.
    pub frees: u64,
    pub title: String,
    pub why: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Spot {
    pub group: String,
    pub path: String,
    pub location: String,
    /// Absent until measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_disk: Option<u64>,
}

#[derive(Serialize, JsonSchema)]
pub struct Location {
    pub name: String,
    /// drive, share or sftp.
    pub kind: String,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// On disk at its last scan; absent when never scanned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scanned: Option<u64>,
}

#[derive(Serialize, JsonSchema)]
pub struct Overviewed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<Disk>,
    pub suggestions: Vec<Suggestion>,
    pub stuff: Vec<Spot>,
    pub locations: Vec<Location>,
    /// Some folders aren't measured yet; ask again with a longer timeout.
    pub partial: bool,
}

impl Call for Overview {
    type Out = Overviewed;

    fn run(self, server: &mut Server) -> Result<Reply<Overviewed>, ToolError> {
        let env = server.overview_env();
        let end = deadline(self.timeout_seconds);
        let progress = storage::Progress::default();
        let volumes = volumes::list();
        let read = |now| {
            let connected = |id: &str| server.remote.is_connected(id);
            overview::read(&env, &volumes, &cloud::folders(&env.home), &connected, now)
        };
        // Only an index this session already started: looking at the
        // Overview must never start one.
        if let Some(index) = server.started_index() {
            let git = Git::find().ok();
            let _ = overview::discover(&env, git.as_ref(), |name, limit| index.find(name, limit));
        }
        let now = overview::now();
        for folder in read(now).due {
            if end.is_some_and(|end| Instant::now() >= end) {
                break;
            }
            overview::measure(&env, &folder, &progress, end, now);
        }
        let about = read(now);
        let text = overview_text(&about, now);
        let measured = |size: &Option<overview::Measured>| size.as_ref().map(|size| size.on_disk);
        let data = Overviewed {
            disk: about.headline.as_ref().map(|disk| Disk {
                name: disk.name.clone(),
                free: disk.free,
                total: disk.total,
                low: disk.low,
                trash: disk.trash.map(|trash| trash.unshared),
                used_by: about
                    .parts()
                    .into_iter()
                    .map(|(part, on_disk)| UsedBy {
                        kind: part.name().to_string(),
                        on_disk,
                    })
                    .collect(),
            }),
            suggestions: about
                .suggestions
                .iter()
                .map(|suggestion| Suggestion {
                    path: shown(&suggestion.path),
                    frees: suggestion.size.unshared,
                    title: suggestion.evidence.title(),
                    why: suggestion.evidence.why(),
                })
                .collect(),
            stuff: about
                .stuff
                .iter()
                .flat_map(|group| {
                    group.spots.iter().map(|spot| Spot {
                        group: group.name.clone(),
                        path: shown(&spot.path),
                        location: spot.location.clone(),
                        on_disk: measured(&spot.size),
                    })
                })
                .collect(),
            locations: about
                .locations
                .iter()
                .map(|location| Location {
                    name: location.name.clone(),
                    kind: location_kind(&location.kind).into(),
                    connected: location.connected,
                    free: location.capacity.map(|(_, free)| free),
                    total: location.capacity.map(|(total, _)| total),
                    scanned: location.scan.as_ref().map(|scan| scan.total.on_disk),
                })
                .collect(),
            partial: !about.due.is_empty(),
        };
        Ok(Reply::new(text, data))
    }
}

fn location_kind(kind: &LocationKind) -> &'static str {
    match kind {
        LocationKind::Drive | LocationKind::Disconnected => "drive",
        LocationKind::Share { .. } => "share",
        LocationKind::Sftp { .. } => "sftp",
    }
}

/// Finder's decimal units, to a size worth reading for a disk.
fn bytes(n: u64) -> String {
    let n = n as f64;
    match n {
        ..1e9 => format!("{:.0} MB", n / 1e6),
        ..1e12 => format!("{:.1} GB", n / 1e9),
        _ => format!("{:.2} TB", n / 1e12),
    }
}

/// The Overview in lines: the startup disk, then each section that has anything.
fn overview_text(about: &overview::Overview, now: u64) -> String {
    let mut out = String::new();
    let aged = |size: &overview::Measured| {
        let short = if size.complete { "" } else { " at least" };
        match size.age(now) {
            Some(age) => format!("{short} {} on disk, {age}", bytes(size.on_disk)),
            None => format!("{short} {} on disk", bytes(size.on_disk)),
        }
    };
    if let Some(disk) = &about.headline {
        let _ = write!(
            out,
            "{}: {} free of {}",
            disk.name,
            bytes(disk.free),
            bytes(disk.total)
        );
        if disk.low {
            out.push_str(", low");
        }
        if let Some(trash) = disk.trash.filter(|trash| trash.unshared >= 1_000_000) {
            let short = if trash.complete { "" } else { "at least " };
            let _ = write!(
                out,
                ", {short}{} more after emptying the Trash",
                bytes(trash.unshared)
            );
            if let Some(age) = trash.age(now) {
                let _ = write!(out, " ({age})");
            }
        }
        out.push('\n');
        let parts: Vec<String> = about
            .parts()
            .into_iter()
            .map(|(part, on_disk)| format!("{} {}", part.name(), bytes(on_disk)))
            .collect();
        if !parts.is_empty() {
            let _ = writeln!(out, "used by: {}", parts.join(", "));
        }
    }
    if !about.suggestions.is_empty() {
        out.push_str("suggestions:\n");
    }
    for suggestion in &about.suggestions {
        let short = if suggestion.size.complete {
            ""
        } else {
            "at least "
        };
        let age = suggestion
            .size
            .age(now)
            .map_or(String::new(), |age| format!(", {age}"));
        let _ = writeln!(
            out,
            "frees {short}{}{age} {}: {}. {}",
            bytes(suggestion.size.unshared),
            suggestion.path.display(),
            suggestion.evidence.title(),
            suggestion.evidence.why()
        );
    }
    if !about.stuff.is_empty() {
        out.push_str("stuff:\n");
    }
    for group in &about.stuff {
        for spot in &group.spots {
            let size = spot.size.as_ref().map_or(" not measured".to_string(), aged);
            let _ = writeln!(
                out,
                "{}/ ({}, on {}):{size}",
                spot.path.display(),
                group.name,
                spot.location
            );
        }
    }
    if !about.locations.is_empty() {
        out.push_str("locations:\n");
    }
    for location in &about.locations {
        let state = if location.connected {
            "connected"
        } else {
            "not connected"
        };
        let _ = write!(
            out,
            "{} ({}): {state}",
            location.name,
            location_kind(&location.kind)
        );
        if let Some((total, free)) = location.capacity {
            let _ = write!(out, ", {} free of {}", bytes(free), bytes(total));
        }
        match &location.scan {
            Some(scan) => {
                let _ = writeln!(out, ", last scan{}", aged(&scan.total));
                for row in &scan.rows {
                    let _ = writeln!(out, "{}/:{}", row.path.display(), aged(&row.size));
                }
            }
            None if location.mount.is_some() => out.push_str(", not scanned\n"),
            None => out.push('\n'),
        }
    }
    out
}

const fn largest() -> usize {
    20
}

const fn depth() -> usize {
    2
}

const fn per_level() -> usize {
    10
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StorageScan {
    path: String,
    /// How many of the largest files.
    #[serde(default = "largest")]
    largest: usize,
    /// Levels of folders.
    #[serde(default = "depth")]
    depth: usize,
    /// Folders a level.
    #[serde(default = "per_level")]
    limit: usize,
    #[serde(default = "scan_timeout")]
    timeout_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Used {
    pub path: String,
    pub bytes: u64,
    pub on_disk: u64,
    /// What trashing it frees once the Trash is emptied.
    pub unshared: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Scanned {
    pub path: String,
    pub bytes: u64,
    pub on_disk: u64,
    pub unshared: u64,
    pub files: u64,
    pub folders: usize,
    pub largest: Vec<Used>,
    /// The folders holding the most, a level at a time, most first.
    pub folders_by_size: Vec<Used>,
    /// Folders or items that couldn't be read, with why.
    pub unreadable: Vec<String>,
    /// It stopped early; totals are what it got to.
    pub partial: bool,
}

fn used(path: &std::path::Path, usage: &Usage) -> Used {
    Used {
        path: shown(path),
        bytes: usage.bytes,
        on_disk: usage.on_disk,
        unshared: usage.unshared,
    }
}

impl Call for StorageScan {
    type Out = Scanned;

    fn run(self, server: &mut Server) -> Result<Reply<Scanned>, ToolError> {
        let folder = local_one(&self.path, "storage_scan")?;
        let report = storage::scan(
            &folder,
            &storage::Progress::default(),
            deadline(self.timeout_seconds),
        )?;
        // A whole drive's scan is what the Overview shows for it.
        let volumes = volumes::list();
        if let Some(volume) = overview::volume_at(&volumes, &report.root().path) {
            let _ = overview::record_scan(&server.overview_env(), volume, &report, overview::now());
        }
        let root = report.root();
        let mut folders = Vec::new();
        tree(&report, 0, self.depth, self.limit, &mut folders);
        let data = Scanned {
            path: shown(&root.path),
            bytes: root.usage.bytes,
            on_disk: root.usage.on_disk,
            unshared: root.usage.unshared,
            files: root.usage.files,
            folders: report.folders.len() - 1,
            largest: report
                .largest(self.largest)
                .iter()
                .map(|file| used(&file.path, &file.usage))
                .collect(),
            folders_by_size: folders,
            unreadable: report.unreadable.clone(),
            partial: report.stopped.is_some(),
        };
        let text = storage_text(
            &report,
            self.largest,
            self.depth,
            self.limit,
            self.timeout_seconds,
        );
        Ok(Reply::new(text, data))
    }
}

fn tree(report: &Report, at: usize, depth: usize, limit: usize, out: &mut Vec<Used>) {
    if depth == 0 {
        return;
    }
    for &child in report.folders[at].children.iter().take(limit) {
        let folder = &report.folders[child];
        out.push(used(&folder.path, &folder.usage));
        tree(report, child, depth - 1, limit, out);
    }
}

/// A scan's totals, its unreadable folders, the largest files, then its
/// biggest folders as a tree, `depth` levels and `limit` a level.
fn storage_text(
    report: &Report,
    largest: usize,
    depth: usize,
    limit: usize,
    timeout: u64,
) -> String {
    let root = report.root();
    let total = root.usage;
    let mut out = format!(
        "{}/\nsize: {} bytes ({} on disk, {} unshared)\nfiles: {}\nfolders: {}\n",
        root.path.display(),
        total.bytes,
        total.on_disk,
        total.unshared,
        total.files,
        report.folders.len() - 1
    );
    placeholders(&mut out, total.placeholders);
    listed(
        &mut out,
        "unreadable",
        &report.unreadable,
        "what couldn't be read isn't counted",
    );
    let large = report.largest(largest);
    if !large.is_empty() {
        out.push_str("largest files:\n");
    }
    for file in &large {
        let cloud = if file.usage.placeholders > 0 {
            " (not downloaded)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{}  {}{cloud}",
            file.path.display(),
            usage(&file.usage)
        );
    }
    if !root.children.is_empty() && depth > 0 {
        out.push_str("folders, most on disk first:\n");
        folder_tree(&mut out, report, 0, depth, limit, "");
    }
    if let Some(stop) = report.stopped {
        out.push_str(&stopped_scan(stop, timeout));
    }
    out
}

fn folder_tree(
    out: &mut String,
    report: &Report,
    at: usize,
    depth: usize,
    limit: usize,
    indent: &str,
) {
    let children = &report.folders[at].children;
    for &child in children.iter().take(limit) {
        let folder = &report.folders[child];
        let note = if folder.unreadable {
            ", partly unreadable"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{indent}{}/  {}{note}",
            folder.path.display(),
            usage(&folder.usage)
        );
        if depth > 1 {
            folder_tree(out, report, child, depth - 1, limit, &format!("{indent}  "));
        }
    }
    if children.len() > limit {
        let _ = writeln!(
            out,
            "{indent}({} more; pass a larger limit)",
            children.len() - limit
        );
    }
}

/// Bytes, on disk, and what's unshared when clones or snapshots share some.
fn usage(usage: &Usage) -> String {
    let mut line = format!("{} bytes, {} on disk", usage.bytes, usage.on_disk);
    if usage.unshared < usage.on_disk {
        let _ = write!(line, ", {} unshared", usage.unshared);
    }
    line
}

/// How many were left out as `label`, and the first few.
fn listed(out: &mut String, label: &str, why: &[String], meaning: &str) {
    const SHOWN: usize = 10;
    if why.is_empty() {
        return;
    }
    let _ = writeln!(out, "{label}: {} ({meaning})", why.len());
    for line in why.iter().take(SHOWN) {
        let _ = writeln!(out, "{line}");
    }
    if why.len() > SHOWN {
        let _ = writeln!(out, "({} more)", why.len() - SHOWN);
    }
}

fn stopped_scan(stop: Stop, timeout: u64) -> String {
    match stop {
        Stop::Deadline => format!(
            "(Stopped after {timeout}s: these results are partial; pass a larger timeout_seconds.)\n"
        ),
        Stop::Budget => format!(
            "(Stopped after {} files and folders: these results are partial; scan a smaller folder.)\n",
            storage::MAX_ITEMS
        ),
        Stop::Cancelled => "(Cancelled: these results are partial.)\n".into(),
    }
}

const fn min_size() -> u64 {
    1
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Duplicates {
    path: String,
    /// Smallest file size to compare, in bytes.
    #[serde(default = "min_size")]
    min_size: u64,
    /// Groups.
    #[serde(default = "largest")]
    limit: usize,
    #[serde(default = "scan_timeout")]
    timeout_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct Copies {
    /// Each copy's length.
    pub size: u64,
    /// Read whole and found the same; only these can be cleaned up.
    pub verified: bool,
    pub paths: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Duplicated {
    pub path: String,
    pub groups: Vec<Copies>,
    /// Bytes in extra verified copies.
    pub wasted: u64,
    /// Cloud placeholders never read, so unverified.
    pub not_downloaded: Vec<String>,
    pub unreadable: Vec<String>,
    /// More groups than limit.
    pub truncated: bool,
    /// It stopped early.
    pub partial: bool,
}

impl Call for Duplicates {
    type Out = Duplicated;

    fn run(self, _: &mut Server) -> Result<Reply<Duplicated>, ToolError> {
        let folder = local_one(&self.path, "duplicates")?;
        let end = deadline(self.timeout_seconds);
        let progress = storage::Progress::default();
        let report = storage::scan(&folder, &progress, end)?;
        let min_size = self.min_size;
        let found = report.duplicates(&|size| size >= min_size, &progress, end);
        let text = duplicate_text(&report, &found, self.limit, self.timeout_seconds);
        let data = Duplicated {
            path: shown(&report.root().path),
            groups: found
                .groups
                .iter()
                .take(self.limit)
                .map(|group| Copies {
                    size: group.size,
                    verified: group.matched == Match::Content,
                    paths: group
                        .members
                        .iter()
                        .map(|member| shown(&member.path))
                        .collect(),
                })
                .collect(),
            wasted: found
                .groups
                .iter()
                .filter(|group| group.matched == Match::Content)
                .map(storage::Group::wasted)
                .sum(),
            not_downloaded: found
                .not_downloaded
                .iter()
                .map(|path| shown(path))
                .collect(),
            unreadable: found.unreadable.clone(),
            truncated: found.groups.len() > self.limit,
            partial: found.stopped.is_some(),
        };
        Ok(Reply::new(text, data))
    }
}

/// Verified groups and then candidates, each a header and its paths.
fn duplicate_text(report: &Report, found: &Found, limit: usize, timeout: u64) -> String {
    const COPIES: usize = 20;
    let verified: Vec<_> = found
        .groups
        .iter()
        .filter(|group| group.matched == Match::Content)
        .collect();
    let mut out = format!(
        "{}/\n{} verified, {} candidates; {} bytes in extra verified copies\n",
        report.root().path.display(),
        verified.len(),
        found.groups.len() - verified.len(),
        verified.iter().map(|group| group.wasted()).sum::<u64>(),
    );
    for group in found.groups.iter().take(limit) {
        let what = match group.matched {
            Match::Content => "verified",
            Match::Sample => "candidate, same size, start, middle and end",
            Match::Size => "candidate, same size",
        };
        let _ = writeln!(
            out,
            "{what}: {} copies of {} bytes",
            group.members.len(),
            group.size
        );
        for member in group.members.iter().take(COPIES) {
            let shared = if member.unshared < member.on_disk {
                format!(
                    "  ({} unshared; a clone or snapshot shares the rest)",
                    member.unshared
                )
            } else {
                String::new()
            };
            let _ = writeln!(out, "{}{shared}", member.path.display());
        }
        if group.members.len() > COPIES {
            let _ = writeln!(out, "({} more copies)", group.members.len() - COPIES);
        }
    }
    if found.groups.len() > limit {
        let _ = writeln!(
            out,
            "({} more; pass a larger limit)",
            found.groups.len() - limit
        );
    }
    if found
        .groups
        .iter()
        .any(|group| group.matched != Match::Content)
    {
        out.push_str("(Candidates aren't verified; only verified copies can be cleaned up.)\n");
    }
    let cloud: Vec<String> = found
        .not_downloaded
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    listed(
        &mut out,
        "not downloaded",
        &cloud,
        "the size of another file but never read, so unverified; download them to check",
    );
    listed(
        &mut out,
        "unreadable",
        &found.unreadable,
        "left out of the groups",
    );
    if let Some(stop) = found.stopped {
        out.push_str(&stopped_scan(stop, timeout));
    }
    out
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CleanupDuplicates {
    /// The folder duplicates scanned.
    path: Option<String>,
    /// Verified copies to move to the Trash.
    #[serde(default)]
    trash: Vec<String>,
    #[serde(default)]
    dry_run: bool,
    /// A dry run's token: trashes exactly its plan.
    confirm: Option<String>,
    #[serde(default = "scan_timeout")]
    timeout_seconds: u64,
    request_id: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct CleanupPlan {
    pub keep: Vec<String>,
    pub trash: Vec<String>,
    /// Bytes freed once the Trash is emptied, at least.
    pub frees: u64,
}

impl Call for CleanupDuplicates {
    type Out = Planned<CleanupPlan>;

    fn run(self, server: &mut Server) -> Result<Reply<Self::Out>, ToolError> {
        if let Some(token) = &self.confirm {
            let Preview::Cleanup(plan) = server.kept(token, self.request_id.as_deref())? else {
                return Err(ToolError::invalid("That token is another tool's plan."));
            };
            // Kept when the owner refuses it whole, such as while agents
            // may not change files, so it can be confirmed again.
            let done = server.change(plan.request(), self.request_id.as_deref())?;
            server.spent(token, self.request_id.as_deref());
            return Ok(done.ran());
        }
        let (Some(path), true) = (&self.path, self.dry_run) else {
            return Err(ToolError::invalid(
                "Dry run first with path, trash and dry_run: true, then confirm its token.",
            ));
        };
        let plan = preview(path, &self.trash, self.timeout_seconds)?;
        let text = cleanup_text(&plan);
        let data = CleanupPlan {
            keep: plan
                .groups
                .iter()
                .flat_map(|copies| copies.keep.iter().map(|seen| shown(&seen.path)))
                .collect(),
            trash: plan
                .groups
                .iter()
                .flat_map(|copies| copies.trash.iter().map(|seen| shown(&seen.path)))
                .collect(),
            frees: plan.frees,
        };
        let token = server.keep(Preview::Cleanup(plan));
        Ok(Reply::new(format!("{text}token: {token}\n"), data).planned(Some(token)))
    }
}

/// Scans `path` again, verifies the copies of `trash` there, and plans
/// trashing exactly those.
fn preview(path: &str, trash: &[String], timeout: u64) -> Result<Cleanup, ToolError> {
    // By their folders' real paths, as the scan reports them.
    let (trash, sizes): (Vec<PathBuf>, Vec<u64>) = local(trash, "cleanup_duplicates")?
        .into_iter()
        .map(|path| {
            let fail = |e: std::io::Error| format!("{}: {e}", path.display());
            let meta = std::fs::symlink_metadata(&path).map_err(fail)?;
            let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
                return Err(format!("{}: not a file.", path.display()));
            };
            Ok((folder.canonicalize().map_err(fail)?.join(name), meta.len()))
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .unzip();
    let end = deadline(timeout);
    let progress = storage::Progress::default();
    let report = storage::scan(&local_one(path, "cleanup_duplicates")?, &progress, end)?;
    let found = report.duplicates(&|size| sizes.contains(&size), &progress, end);
    Ok(found.cleanup(&trash).map_err(|why| match found.stopped {
        Some(stop) => format!("{why}\n{}", stopped_scan(stop, timeout)),
        None => why,
    })?)
}

/// Each group a copy a line, kept or trashed, then what it frees.
fn cleanup_text(plan: &Cleanup) -> String {
    let mut out = String::new();
    let (mut kept, mut trashed) = (0, 0);
    for copies in &plan.groups {
        for seen in &copies.keep {
            kept += 1;
            let _ = writeln!(out, "keep: {}", seen.path.display());
        }
        for seen in &copies.trash {
            trashed += 1;
            let _ = writeln!(out, "trash: {}", seen.path.display());
        }
    }
    let _ = writeln!(
        out,
        "{trashed} to trash, {kept} kept; frees at least {} bytes once the Trash is emptied. Undo puts them back.",
        plan.frees
    );
    out
}
