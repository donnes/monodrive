//! Finding and reading: find_files, grep, list_dir, stat, read_file,
//! thumbnail and index_status.

use super::{Call, Where, default_limit, local_one, locate};
use crate::archives::{self, Archive};
use crate::cloud;
use crate::entries;
use crate::index::{Caps, GrepMode, Progress, Status};
use crate::info::{self, Info};
use crate::mcp::contract::{
    Code, Kind, Page, Reply, ToolError, more_line, shown, suffix, utc, utc_seconds,
};
use crate::mcp::server::Server;
use crate::remote::{self, RemoteEntry};
use crate::search::{Query, Scope};
use crate::thumbnails;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Serialize, JsonSchema)]
pub struct Hit {
    pub path: String,
    pub kind: Kind,
}

#[derive(Serialize, JsonSchema)]
pub struct Hits {
    pub items: Vec<Hit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// More exist than are here: pass next_cursor, or narrow the call.
    pub truncated: bool,
}

/// One item in a folder. Some clients show models this, not the text, so
/// it holds what the text does: a name, not a path, and no dates.
#[derive(Serialize, JsonSchema)]
pub struct Item {
    pub name: String,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// not_downloaded, partly_downloaded or downloading, for cloud items.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    /// Why an archive entry can't be extracted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Listing {
    /// The folder; each item's path is this, a slash, and its name.
    pub path: String,
    pub items: Vec<Item>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// More exist than are here: pass next_cursor.
    pub truncated: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindFiles {
    query: String,
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1))]
    limit: usize,
    cursor: Option<String>,
}

impl Call for FindFiles {
    type Out = Hits;

    fn run(self, server: &mut Server) -> Result<Reply<Hits>, ToolError> {
        let page = Page::new(self.cursor.as_deref(), self.limit)?;
        let query = Query::parse(&self.query).map_err(ToolError::invalid)?;
        let hits = server
            .index()?
            .search(&query, &Scope::Everywhere, page.wanted())?;
        let (hits, next) = page.take(hits);
        let mut text = String::new();
        for hit in &hits {
            let _ = writeln!(text, "{}{}", hit.path.display(), suffix(hit.kind));
        }
        text.push_str(&more_line(next.as_ref()));
        let items = hits
            .iter()
            .map(|hit| Hit {
                path: shown(&hit.path),
                kind: hit.kind.into(),
            })
            .collect();
        Ok(Reply::new(
            text,
            Hits {
                items,
                truncated: next.is_some(),
                next_cursor: next,
            },
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Grep {
    pattern: String,
    #[serde(default)]
    mode: GrepMode,
    /// A folder to search in.
    path: Option<String>,
    /// find_files filters picking the files to search.
    #[serde(default)]
    filters: String,
    /// Files, not lines.
    #[serde(default = "default_limit")]
    #[schemars(range(min = 1))]
    limit: usize,
    cursor: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Line {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Lines {
    pub items: Vec<Line>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// More files match than are here, or the search stopped early.
    pub truncated: bool,
}

impl Call for Grep {
    type Out = Lines;

    fn run(self, server: &mut Server) -> Result<Reply<Lines>, ToolError> {
        let page = Page::new(self.cursor.as_deref(), self.limit)?;
        let scope = match &self.path {
            Some(path) => Scope::Folder(local_one(path, "grep")?),
            None => Scope::Everywhere,
        };
        let filters = Query::parse(&self.filters).map_err(ToolError::invalid)?;
        let caps = Caps {
            files: page.wanted(),
            ..Caps::default()
        };
        let found = server
            .index()?
            .grep(&self.pattern, self.mode, &filters, &scope, caps)?;
        let (files, next) = page.take(found.files);
        let items: Vec<Line> = files
            .iter()
            .flat_map(|file| {
                file.lines.iter().map(|m| Line {
                    path: shown(&file.path),
                    line: m.line,
                    text: m.text.clone(),
                })
            })
            .collect();
        let mut text = String::new();
        for line in &items {
            let _ = writeln!(text, "{}:{}: {}", line.path, line.line, line.text);
        }
        if !found.complete {
            text.push_str("(search stopped early; narrow it with path or filters)\n");
        }
        text.push_str(&more_line(next.as_ref()));
        Ok(Reply::new(
            text,
            Lines {
                items,
                truncated: next.is_some() || !found.complete,
                next_cursor: next,
            },
        ))
    }
}

const fn listed() -> usize {
    200
}

const fn yes() -> bool {
    true
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDir {
    path: String,
    #[serde(default)]
    show_hidden: bool,
    #[serde(default)]
    sort: entries::SortBy,
    #[serde(default)]
    descending: bool,
    #[serde(default = "yes")]
    folders_first: bool,
    #[serde(default = "listed")]
    #[schemars(range(min = 1))]
    limit: usize,
    cursor: Option<String>,
}

impl Call for ListDir {
    type Out = Listing;

    fn run(self, server: &mut Server) -> Result<Reply<Listing>, ToolError> {
        let page = Page::new(self.cursor.as_deref(), self.limit)?;
        let (path, items, next, note) = match locate(&self.path) {
            Where::Remote(path) => remote_list(server, &path, &page)?,
            Where::Archive { archive, inside } => archive_list(&archive, &inside, &page)?,
            Where::Local(folder) if is_zip(&folder) => archive_list(&folder, "", &page)?,
            Where::Local(folder) => {
                let order = entries::Sort {
                    by: self.sort,
                    descending: self.descending,
                    folders_first: self.folders_first,
                };
                let list = entries::list_dir(&folder, self.show_hidden, order)
                    .map_err(|e| ToolError::from(e).at(self.path.clone()))?;
                let partly = cloud::partly_downloaded(&list);
                let (list, next) = page.take(list);
                let items = list
                    .into_iter()
                    .map(|e| Item {
                        availability: if partly.contains(&e.path) {
                            Some("partly_downloaded".into())
                        } else {
                            availability(e.availability)
                        },
                        size: (e.kind == entries::Kind::File).then_some(e.size),
                        name: e.name,
                        kind: e.kind.into(),
                        problem: None,
                    })
                    .collect();
                (shown(&folder), items, next, String::new())
            }
        };
        let mut text = String::new();
        for item in &items {
            let name = &item.name;
            let marker = match item.kind {
                Kind::Folder => "/",
                Kind::Link => "@",
                Kind::File | Kind::Other => "",
            };
            let _ = write!(text, "{name}{marker}");
            match item.availability.as_deref() {
                Some("not_downloaded") => text.push_str(" (not downloaded)"),
                Some("downloading") => text.push_str(" (downloading)"),
                Some("partly_downloaded") => text.push_str(" (not all downloaded)"),
                _ => {}
            }
            if let Some(why) = &item.problem {
                let _ = write!(text, " (can't extract: {why})");
            }
            text.push('\n');
        }
        text.push_str(&note);
        text.push_str(&more_line(next.as_ref()));
        Ok(Reply::new(
            text,
            Listing {
                path,
                items,
                truncated: next.is_some(),
                next_cursor: next,
            },
        ))
    }
}

fn is_zip(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        && path.is_file()
}

fn availability(availability: cloud::Availability) -> Option<String> {
    match availability {
        cloud::Availability::Placeholder => Some("not_downloaded".into()),
        cloud::Availability::Downloading => Some("downloading".into()),
        cloud::Availability::Local | cloud::Availability::Unknown => None,
    }
}

/// The folder's path, a page of its items, the next page's cursor, and a
/// note for the text.
type Listed = (String, Vec<Item>, Option<String>, String);

fn remote_list(server: &Server, path: &str, page: &Page) -> Result<Listed, ToolError> {
    let (open, at) = server.remote_at(path)?;
    let listed = open.list(&at, page.offset, page.limit)?;
    let next = listed
        .more
        .then(|| (page.offset + listed.entries.len()).to_string());
    let items = listed.entries.iter().map(remote_item).collect();
    let note = if listed.left_out > 0 {
        format!(
            "({} names left out: not UTF-8, or not plain names)\n",
            listed.left_out
        )
    } else {
        String::new()
    };
    Ok((at.to_string(), items, next, note))
}

fn remote_item(entry: &RemoteEntry) -> Item {
    Item {
        name: entry.name.clone(),
        kind: entry.kind.into(),
        size: (entry.kind == entries::Kind::File).then_some(entry.size),
        availability: None,
        problem: None,
    }
}

/// One folder of a ZIP, read without extracting anything.
fn archive_list(archive: &Path, inside: &str, page: &Page) -> Result<Listed, ToolError> {
    let opened = Archive::open(archive).map_err(|e| ToolError::from(e).at(shown(archive)))?;
    let folder = inside.trim_matches('/');
    let depth = folder.split('/').filter(|part| !part.is_empty()).count();
    // Its own items, not theirs: a folder's show when it's listed. An
    // archive may name a folder only through the paths inside it.
    let mut items: Vec<Item> = Vec::new();
    for entry in opened.under(folder) {
        let name = archives::printable(&entry.name);
        let parts: Vec<&str> = name.split('/').filter(|part| !part.is_empty()).collect();
        let Some(&own) = parts.get(depth) else {
            continue;
        };
        if items.iter().any(|item| item.name == own) {
            continue;
        }
        let deeper = parts.len() > depth + 1;
        items.push(Item {
            name: own.to_owned(),
            kind: if deeper {
                Kind::Folder
            } else {
                entry.kind.into()
            },
            size: (!deeper && entry.kind == entries::Kind::File).then_some(entry.size),
            availability: None,
            problem: if deeper { None } else { entry.problem() },
        });
    }
    let (items, next) = page.take(items);
    let note = if opened.total > opened.entries.len() as u64 {
        format!(
            "(the archive lists {} entries; Monodrive read the first {})\n",
            opened.total,
            opened.entries.len()
        )
    } else {
        String::new()
    };
    let path = match folder {
        "" => shown(archive),
        folder => format!("{}/{folder}", archive.display()),
    };
    Ok((path, items, next, note))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Stat {
    paths: Vec<String>,
}

#[derive(Serialize, JsonSchema, Default)]
pub struct About {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// What Finder calls it, such as "Plain Text Document".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_disk: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened: Option<String>,
    /// Octal, such as "644".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub package: bool,
    /// Where a link points, as written in it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Abouts {
    pub items: Vec<About>,
}

impl Call for Stat {
    type Out = Abouts;

    fn run(self, server: &mut Server) -> Result<Reply<Abouts>, ToolError> {
        let mut blocks = Vec::new();
        let mut items = Vec::new();
        let mut first_error = None;
        for path in &self.paths {
            let found = match locate(path) {
                Where::Local(at) => info::info(&at)
                    .map(|about| (stat_text(&about), local_about(&about)))
                    .map_err(ToolError::from),
                Where::Remote(qualified) => server
                    .remote_at(&qualified)
                    .and_then(|(open, at)| open.metadata(&at))
                    .map(|entry| (remote_text(&entry), remote_about(&entry)))
                    .map_err(ToolError::from),
                Where::Archive { .. } => Err(ToolError::new(
                    Code::UnsupportedOnSource,
                    format!("{path}: stat can't reach inside a ZIP; list_dir shows its entries."),
                )),
            };
            match found {
                Ok((text, about)) => {
                    blocks.push(text);
                    items.push(about);
                }
                Err(e) => {
                    blocks.push(format!("{}\n", e.message));
                    items.push(About {
                        path: path.clone(),
                        error: Some(e.message.clone()),
                        ..About::default()
                    });
                    first_error.get_or_insert(e.at(path.clone()));
                }
            }
        }
        if let Some(error) = first_error.filter(|_| items.iter().all(|about| about.error.is_some()))
        {
            return Err(ToolError {
                message: blocks.join("\n"),
                ..error
            });
        }
        Ok(Reply::new(blocks.join("\n"), Abouts { items }))
    }
}

fn local_about(about: &Info) -> About {
    let p = &about.permissions;
    About {
        path: shown(&about.path),
        kind: Some(about.kind.into()),
        kind_name: Some(about.kind_name.clone()),
        size: about.size.map(|size| size.bytes),
        on_disk: about.size.map(|size| size.on_disk),
        availability: cloud_state(about).map(|state| state.replace(' ', "_")),
        download_error: about.download_error.clone(),
        volume: about.volume.as_ref().map(|volume| shown(&volume.mount)),
        created: about.created.map(utc),
        modified: about.modified.map(utc),
        opened: about.opened.map(utc),
        mode: Some(format!("{:03o}", p.mode)),
        owner: Some(p.owner.clone()),
        group: Some(p.group.clone()),
        writable: Some(p.writable),
        package: about.package,
        link: about.link.as_deref().map(shown),
        error: None,
    }
}

fn cloud_state(about: &Info) -> Option<&'static str> {
    match about.availability {
        cloud::Availability::Local if about.partly_downloaded => Some("partly downloaded"),
        cloud::Availability::Local if about.in_cloud => Some("downloaded"),
        cloud::Availability::Local | cloud::Availability::Unknown => None,
        cloud::Availability::Placeholder => Some("not downloaded"),
        cloud::Availability::Downloading => Some("downloading"),
    }
}

/// One item's Get Info, a field a line after its path. Opened appears only
/// when macOS recorded it.
fn stat_text(about: &Info) -> String {
    let mut out = format!("{}{}\n", about.path.display(), suffix(about.kind));
    let mut field = |name: &str, value: String| {
        let _ = writeln!(out, "{name}: {value}");
    };
    field("kind", about.kind_name.clone());
    if let Some(size) = about.size {
        field(
            "size",
            format!("{} bytes ({} on disk)", size.bytes, size.on_disk),
        );
    }
    if let Some(state) = cloud_state(about) {
        field("availability", state.into());
    }
    if let Some(why) = &about.download_error {
        field("download failed", why.clone());
    }
    if let Some(parent) = &about.parent {
        field("where", parent.display().to_string());
    }
    if let Some(volume) = &about.volume {
        field(
            "volume",
            format!("{} at {}", volume.name, volume.mount.display()),
        );
    }
    for (name, date) in [
        ("created", about.created),
        ("modified", about.modified),
        ("opened", about.opened),
    ] {
        if let Some(date) = date {
            field(name, utc(date));
        }
    }
    let p = &about.permissions;
    let access = match (p.readable, p.writable) {
        (true, true) => "you can read and write",
        (true, false) => "you can read, not write",
        (false, true) => "you can write, not read",
        (false, false) => "you can't read or write",
    };
    field(
        "permissions",
        format!(
            "{:03o}, owner {}, group {}, {access}",
            p.mode, p.owner, p.group
        ),
    );
    if about.package {
        field("package", "yes, opens as one item".into());
    }
    if let Some(target) = &about.link {
        field("link to", target.display().to_string());
    }
    out
}

fn remote_about(entry: &RemoteEntry) -> About {
    About {
        path: entry.path.to_string(),
        kind: Some(entry.kind.into()),
        size: (entry.kind == entries::Kind::File).then_some(entry.size),
        modified: (entry.modified > 0).then(|| utc_seconds(entry.modified)),
        mode: entry.permissions.map(|mode| format!("{mode:o}")),
        link: entry.link.clone(),
        ..About::default()
    }
}

fn remote_text(entry: &RemoteEntry) -> String {
    let mut out = format!("{}{}\n", entry.path, suffix(entry.kind));
    if entry.kind == entries::Kind::File {
        let _ = writeln!(out, "size: {} bytes", entry.size);
    }
    if entry.modified > 0 {
        let _ = writeln!(out, "modified: {}", utc_seconds(entry.modified));
    }
    if let Some(mode) = entry.permissions {
        let _ = writeln!(out, "permissions: {mode:o}");
    }
    if let Some(link) = &entry.link {
        let _ = writeln!(out, "link to: {}", link.escape_default());
    }
    out
}

const fn read_limit() -> u32 {
    64 << 10
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadFile {
    path: String,
    #[serde(default)]
    offset: u64,
    /// Bytes, at most 1 MB.
    #[serde(default = "read_limit")]
    #[schemars(range(min = 1, max = 1048576))]
    limit: u32,
}

#[derive(Serialize, JsonSchema)]
pub struct Chunk {
    pub path: String,
    pub offset: u64,
    /// The file's whole length.
    pub size: u64,
    /// Absent for a binary file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Where the next read starts, when there's more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<u64>,
}

impl Call for ReadFile {
    type Out = Chunk;

    fn run(self, server: &mut Server) -> Result<Reply<Chunk>, ToolError> {
        let limit = self.limit.min(remote::MOST_READ);
        let (path, data, size) = match locate(&self.path) {
            Where::Remote(qualified) => {
                let (open, at) = server.remote_at(&qualified)?;
                let (data, size) = open.read(&at, self.offset, limit)?;
                (at.to_string(), data, size)
            }
            Where::Local(path) => {
                let (data, size) = read_local(&path, self.offset, limit)
                    .map_err(|e| ToolError::from(e).at(self.path.clone()))?;
                (shown(&path), data, size)
            }
            Where::Archive { .. } => {
                return Err(ToolError::new(
                    Code::UnsupportedOnSource,
                    format!(
                        "{}: read_file can't reach inside a ZIP; archive_extract takes it out.",
                        self.path
                    ),
                ));
            }
        };
        let text = match std::str::from_utf8(&data) {
            Ok(text) if !text.contains('\0') => Some(text.to_owned()),
            // A range can end partway through a character: that character
            // starts the next read. At the end of the file it never ends.
            Err(e)
                if e.error_len().is_none()
                    && self.offset.saturating_add(data.len() as u64) < size
                    && !data[..e.valid_up_to()].contains(&0) =>
            {
                Some(String::from_utf8_lossy(&data[..e.valid_up_to()]).into_owned())
            }
            _ => None,
        };
        let read = text.as_ref().map_or(data.len(), String::len);
        let end = self.offset.saturating_add(read as u64);
        let head = format!("{path}: bytes {} to {end} of {size}\n", self.offset);
        let body = text
            .clone()
            .unwrap_or_else(|| "(binary, not shown; copy takes it whole)\n".into());
        Ok(Reply::new(
            head + &body,
            Chunk {
                path,
                offset: self.offset,
                size,
                text,
                next_offset: (end < size).then_some(end),
            },
        ))
    }
}

fn read_local(path: &Path, offset: u64, limit: u32) -> std::io::Result<(Vec<u8>, u64)> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    file.seek(SeekFrom::Start(offset))?;
    let mut data = Vec::new();
    file.take(u64::from(limit)).read_to_end(&mut data)?;
    Ok((data, size))
}

const fn side() -> u32 {
    256
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Thumbnail {
    path: String,
    /// The longer edge in pixels.
    #[serde(default = "side")]
    #[schemars(range(min = 16, max = 1024))]
    side: u32,
}

#[derive(Serialize, JsonSchema)]
pub struct Picture {
    pub path: String,
    pub mime_type: String,
}

/// MCP's image content: an agent can't watch a video or read a HEIC, and
/// this shows it one. The side is capped so a reply stays cheap.
impl Call for Thumbnail {
    type Out = Picture;

    fn run(self, _: &mut Server) -> Result<Reply<Picture>, ToolError> {
        let path = local_one(&self.path, "thumbnail")?;
        let jpeg = thumbnails::generate(&path, self.side.clamp(16, 1024), &|| false)
            .and_then(|bitmap| thumbnails::jpeg(&bitmap))
            .map_err(|e| ToolError::from(e).at(self.path.clone()))?;
        let mut reply = Reply::new(
            String::new(),
            Picture {
                path: shown(&path),
                mime_type: "image/jpeg".into(),
            },
        );
        reply.image = Some(jpeg);
        Ok(reply)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IndexStatus {}

#[derive(Serialize, JsonSchema)]
pub struct IndexState {
    /// not_started, following (another app builds it), indexing or ready.
    pub state: String,
    /// When not started: whether a saved index will load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entries: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<usize>,
    /// Unknown until the index starts. Without it, consent-gated folders
    /// are skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_disk_access: Option<bool>,
}

impl Call for IndexStatus {
    type Out = IndexState;

    fn run(self, server: &mut Server) -> Result<Reply<IndexState>, ToolError> {
        let status: Status = server.status()?;
        let mut text = format!("{}\n", status.progress);
        if status.full_disk_access == Some(false) {
            text.push_str("No Full Disk Access: consent-gated folders are skipped.\n");
        }
        let (state, saved, entries, estimate) = match status.progress {
            Progress::NotStarted { saved } => ("not_started", Some(saved), None, None),
            Progress::Following => ("following", None, None, None),
            Progress::Listing { listed, estimate } => ("indexing", None, Some(listed), estimate),
            Progress::Ready { entries } => ("ready", None, Some(entries), None),
        };
        Ok(Reply::new(
            text,
            IndexState {
                state: state.into(),
                saved,
                entries,
                estimate,
                full_disk_access: status.full_disk_access,
            },
        ))
    }
}
