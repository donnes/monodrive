//! `monodrive mcp`: a Model Context Protocol server on stdin/stdout, one
//! JSON-RPC message per line. Tools only decode arguments and call one
//! service method; the behavior lives in `archives`, `catalogs`, `cloud`,
//! `compare`, `entries`, `index`, `info`, `ops`, `places`, `search`, `sizes`,
//! `storage` and `volumes`.

use crate::archives::{self, Archive};
use crate::catalogs::{self, Catalog, Found, Resolved};
use crate::cloud::{self, Asked};
use crate::compare::{
    self, Action, Block, Comparison, CopyPlan, Ending, LineKind, Newer, TextDiff,
};
use crate::entries::{self, Kind};
use crate::expand_home;
use crate::index::{self, Caps, GrepMode, Index, Status};
use crate::info::{self, Info};
use crate::ops::{self, Conflict, Done, Job, Origin, Request, rename};
use crate::places::{self, Availability, Place, SavedSearch};
use crate::search::{Query, Scope};
use crate::sizes::{self, FolderSize};
use crate::storage::{self, Cleanup, Duplicates, Match, Report, Stop, Usage};
use crate::volumes::{self, Volume, VolumeId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime};

/// Newest first. A client asking for another version gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Monodrive: an always-current index of this Mac's files, \
much faster than Spotlight or walking folders with find. Use find_files to locate files \
by name, grep to search inside files, and list_dir to see one folder. index_status says \
whether the index is ready. stat describes items, and folder_size adds up a folder. \
trash, put_back, rename, rename_many, create_folder, create_folder_with, create_file, duplicate, \
undo and redo change files through the same undo history as the Monodrive window; rename_preview \
shows what rename_many would do. copy, move, archive_create and archive_extract run as jobs that \
job_status follows; archive_list shows what a ZIP holds without extracting it. \
compare compares two files or folders, diff shows two text files' differences, and compare_copy \
copies differences one way as a job. storage_scan shows what takes up space in a folder, duplicates \
finds identical files there, and cleanup_duplicates trashes the copies you choose. \
list_places shows the sidebar's favorites and the mounted volumes; pin, unpin and eject manage them. \
list_searches, save_search, rename_search and remove_search manage the sidebar's saved searches. \
Items iCloud Drive or a cloud app hasn't downloaded show (not downloaded); nothing but download \
fetches them, and remove_download frees a downloaded copy. catalog_create keeps a searchable list \
of a drive's or folder's names, so catalog_search finds them while it's disconnected; catalog_list, \
catalog_refresh and catalog_forget manage those catalogs.";

pub fn serve_stdio() -> std::io::Result<()> {
    let paths = ops::Paths::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let places = places::Store::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let catalogs =
        catalogs::Store::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let mut server = Server::new(
        ops::Client::new(paths, std::env::current_exe().ok()),
        places,
        catalogs,
    );
    let mut stdout = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => server.handle(&message),
            Err(e) => Some(error(&Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

pub struct Server {
    /// Started on the first tool that needs it, so connecting an agent
    /// never kicks off a disk crawl by itself.
    index: Option<Result<Index, String>>,
    /// Connects to the operation owner on the first change.
    ops: ops::Client,
    /// The name the agent's MCP client gave, to attribute its changes.
    client: Option<String>,
    places: places::Store,
    catalogs: catalogs::Store,
    /// What compare_copy and cleanup_duplicates previewed, oldest first,
    /// so a confirm runs only that.
    plans: Vec<Previewed>,
}

struct Previewed {
    token: String,
    made: Instant,
    plan: Plan,
}

enum Plan {
    Copy(CopyPlan),
    Cleanup(Cleanup, String),
}

/// How many previews a session keeps, and for how long.
const PLANS_KEPT: usize = 16;
const PLAN_LIFE: Duration = Duration::from_secs(600);

#[derive(Deserialize)]
#[serde(tag = "name", content = "arguments", rename_all = "snake_case")]
enum ToolCall {
    FindFiles {
        query: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    Grep {
        pattern: String,
        #[serde(default)]
        mode: GrepMode,
        path: Option<String>,
        #[serde(default)]
        filters: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    ListDir {
        path: String,
        #[serde(default)]
        show_hidden: bool,
        #[serde(default)]
        sort: entries::SortBy,
        #[serde(default)]
        descending: bool,
        #[serde(default = "folders_first")]
        folders_first: bool,
    },
    IndexStatus {},
    Stat {
        paths: Vec<String>,
    },
    FolderSize {
        path: String,
        #[serde(default = "default_timeout")]
        timeout_seconds: u64,
    },
    Trash {
        paths: Vec<String>,
    },
    PutBack {
        paths: Vec<String>,
    },
    Rename {
        path: String,
        name: String,
    },
    RenamePreview {
        paths: Vec<String>,
        rule: rename::Rule,
    },
    RenameMany {
        paths: Vec<String>,
        rule: rename::Rule,
        #[serde(default)]
        skip_problems: bool,
    },
    CreateFolder {
        path: String,
    },
    CreateFolderWith {
        path: String,
        paths: Vec<String>,
    },
    CreateFile {
        path: String,
    },
    Duplicate {
        paths: Vec<String>,
    },
    Undo {},
    Redo {},
    Copy {
        paths: Vec<String>,
        into: String,
        #[serde(default)]
        conflict: Conflict,
    },
    Move {
        paths: Vec<String>,
        into: String,
        #[serde(default)]
        conflict: Conflict,
    },
    ArchiveList {
        path: String,
        #[serde(default)]
        folder: String,
        #[serde(default = "archive_limit")]
        limit: usize,
    },
    ArchiveCreate {
        paths: Vec<String>,
    },
    ArchiveExtract {
        path: String,
        into: String,
        #[serde(default)]
        entries: Vec<String>,
        #[serde(default)]
        conflict: Conflict,
    },
    Compare {
        left: String,
        right: String,
        #[serde(default)]
        verify: bool,
        #[serde(default = "compare_limit")]
        limit: usize,
        #[serde(default = "default_timeout")]
        timeout_seconds: u64,
    },
    Diff {
        left: String,
        right: String,
        #[serde(default = "diff_context")]
        context: usize,
        #[serde(default = "diff_limit")]
        limit: usize,
    },
    CompareCopy {
        left: Option<String>,
        right: Option<String>,
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        confirm: bool,
        plan: Option<String>,
        #[serde(default = "compare_limit")]
        limit: usize,
        #[serde(default = "default_timeout")]
        timeout_seconds: u64,
    },
    StorageScan {
        path: String,
        #[serde(default = "largest")]
        largest: usize,
        #[serde(default = "depth")]
        depth: usize,
        #[serde(default = "folder_limit")]
        limit: usize,
        #[serde(default = "scan_timeout")]
        timeout_seconds: u64,
    },
    Duplicates {
        path: String,
        #[serde(default = "min_size")]
        min_size: u64,
        #[serde(default = "largest")]
        limit: usize,
        #[serde(default = "scan_timeout")]
        timeout_seconds: u64,
    },
    CleanupDuplicates {
        path: Option<String>,
        #[serde(default)]
        trash: Vec<String>,
        #[serde(default)]
        confirm: bool,
        plan: Option<String>,
        #[serde(default = "scan_timeout")]
        timeout_seconds: u64,
    },
    JobStatus {
        job: Option<u64>,
    },
    CancelJob {
        job: u64,
    },
    ContinueJob {
        job: u64,
        conflict: Option<Conflict>,
    },
    ListPlaces {},
    Pin {
        path: String,
        at: Option<usize>,
    },
    Unpin {
        path: String,
    },
    Eject {
        path: String,
    },
    Download {
        paths: Vec<String>,
    },
    RemoveDownload {
        paths: Vec<String>,
    },
    ListSearches {},
    SaveSearch {
        name: String,
        query: String,
        path: Option<String>,
        at: Option<usize>,
    },
    RenameSearch {
        name: String,
        to: String,
    },
    RemoveSearch {
        name: String,
    },
    CatalogCreate {
        path: String,
        #[serde(default = "catalog_timeout")]
        timeout_seconds: u64,
    },
    CatalogList {},
    CatalogRefresh {
        catalog: String,
        #[serde(default = "catalog_timeout")]
        timeout_seconds: u64,
    },
    CatalogForget {
        catalog: String,
    },
    CatalogSearch {
        query: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
}

fn folders_first() -> bool {
    true
}

fn default_limit() -> usize {
    50
}

fn default_timeout() -> u64 {
    10
}

fn archive_limit() -> usize {
    200
}

fn compare_limit() -> usize {
    200
}

fn largest() -> usize {
    20
}

fn depth() -> usize {
    2
}

fn folder_limit() -> usize {
    10
}

fn min_size() -> u64 {
    1
}

fn scan_timeout() -> u64 {
    30
}

fn diff_context() -> usize {
    3
}

fn diff_limit() -> usize {
    500
}

fn catalog_timeout() -> u64 {
    300
}

impl Server {
    pub fn new(ops: ops::Client, places: places::Store, catalogs: catalogs::Store) -> Self {
        Self {
            index: None,
            ops,
            client: None,
            places,
            catalogs,
            plans: Vec::new(),
        }
    }

    /// Handles one message and returns the reply. Notifications and
    /// responses get none.
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let method = message.get("method")?.as_str()?;
        let id = message.get("id")?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                self.client = params
                    .pointer("/clientInfo/name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let version = PROTOCOL_VERSIONS
                    .iter()
                    .find(|v| Some(**v) == asked)
                    .unwrap_or(&PROTOCOL_VERSIONS[0]);
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "monodrive", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": tools() })),
            "tools/call" => match serde_json::from_value::<ToolCall>(with_arguments(params)) {
                Ok(call) => result(id, self.call(call)),
                Err(e) => error(id, -32602, &format!("invalid tool call: {e}")),
            },
            _ => error(id, -32601, &format!("unknown method {method}")),
        })
    }

    fn call(&mut self, call: ToolCall) -> Value {
        let text = match call {
            ToolCall::FindFiles { query, limit } => Query::parse(&query)
                .and_then(|query| self.index()?.search(&query, &Scope::Everywhere, limit))
                .map(|hits| {
                    hits.iter().fold(String::new(), |mut out, hit| {
                        let _ = writeln!(out, "{}{}", hit.path.display(), suffix(hit.kind));
                        out
                    })
                }),
            ToolCall::Grep {
                pattern,
                mode,
                path,
                filters,
                limit,
            } => {
                let scope =
                    path.map_or(Scope::Everywhere, |path| Scope::Folder(expand_home(&path)));
                let caps = Caps {
                    files: limit,
                    ..Caps::default()
                };
                Query::parse(&filters)
                    .and_then(|filters| self.index()?.grep(&pattern, mode, &filters, &scope, caps))
                    .map(|found| {
                        let mut out = String::new();
                        for file in &found.files {
                            for m in &file.lines {
                                let _ =
                                    writeln!(out, "{}:{}: {}", file.path.display(), m.line, m.text);
                            }
                        }
                        if !found.complete {
                            out.push_str(
                                "(search stopped early; narrow it with path or filters)\n",
                            );
                        }
                        out
                    })
            }
            ToolCall::ListDir {
                path,
                show_hidden,
                sort,
                descending,
                folders_first,
            } => {
                let order = entries::Sort {
                    by: sort,
                    descending,
                    folders_first,
                };
                entries::list_dir(&expand_home(&path), show_hidden, order)
                    .map_err(|e| format!("{path}: {e}"))
                    .map(|list| {
                        list.iter().fold(String::new(), |mut out, e| {
                            let _ = writeln!(
                                out,
                                "{}{}{}",
                                e.name,
                                suffix(e.kind),
                                marker(e.availability)
                            );
                            out
                        })
                    })
            }
            ToolCall::IndexStatus {} => self.status().map(|status| {
                let mut out = format!("{}\n", status.progress);
                if status.full_disk_access == Some(false) {
                    out.push_str("No Full Disk Access: consent-gated folders are skipped.\n");
                }
                out
            }),
            ToolCall::Stat { paths } => {
                let mut failed = false;
                let blocks: Vec<String> = paths
                    .iter()
                    .map(|path| match info::info(&expand_home(path)) {
                        Ok(about) => stat(&about),
                        Err(e) => {
                            failed = true;
                            format!("{e}\n")
                        }
                    })
                    .collect();
                let text = blocks.join("\n");
                if failed { Err(text) } else { Ok(text) }
            }
            ToolCall::FolderSize {
                path,
                timeout_seconds,
            } => {
                let folder = expand_home(&path);
                let deadline = Instant::now().checked_add(Duration::from_secs(timeout_seconds));
                sizes::measure(&folder, &AtomicBool::new(false), deadline)
                    .map(|size| folder_size(&folder, &size, timeout_seconds))
            }
            ToolCall::Trash { paths } => self.change(Request::Trash {
                paths: expand(&paths),
            }),
            ToolCall::PutBack { paths } => self.change(Request::PutBack {
                paths: expand(&paths),
            }),
            ToolCall::Rename { path, name } => self.change(Request::Rename {
                path: expand_home(&path),
                name,
            }),
            ToolCall::RenamePreview { paths, rule } => {
                Ok(rename_table(&rename::Plan::new(&expand(&paths), &rule)))
            }
            ToolCall::RenameMany {
                paths,
                rule,
                skip_problems,
            } => self.change(Request::RenameMany {
                paths: expand(&paths),
                rule,
                preview: None,
                skip_problems,
            }),
            ToolCall::CreateFolder { path } => self.change(Request::NewFolder {
                path: expand_home(&path),
            }),
            ToolCall::CreateFolderWith { path, paths } => self.change(Request::NewFolderWith {
                path: expand_home(&path),
                items: expand(&paths),
            }),
            ToolCall::CreateFile { path } => self.change(Request::NewFile {
                path: expand_home(&path),
            }),
            ToolCall::Duplicate { paths } => self.change(Request::Duplicate {
                paths: expand(&paths),
            }),
            ToolCall::Undo {} => self.change(Request::Undo),
            ToolCall::Redo {} => self.change(Request::Redo),
            ToolCall::Copy {
                paths,
                into,
                conflict,
            } => self.queue(Request::Copy {
                paths: expand(&paths),
                into: expand_home(&into),
                conflict,
            }),
            ToolCall::Move {
                paths,
                into,
                conflict,
            } => self.queue(Request::Move {
                paths: expand(&paths),
                into: expand_home(&into),
                conflict,
            }),
            ToolCall::ArchiveList {
                path,
                folder,
                limit,
            } => Archive::open(&expand_home(&path))
                .map(|archive| archive_list(&archive, &folder, limit)),
            ToolCall::ArchiveCreate { paths } => self.queue(Request::Compress {
                paths: expand(&paths),
            }),
            ToolCall::ArchiveExtract {
                path,
                into,
                entries,
                conflict,
            } => self.queue(Request::Extract {
                archive: expand_home(&path),
                entries,
                into: expand_home(&into),
                conflict,
            }),
            ToolCall::Compare {
                left,
                right,
                verify,
                limit,
                timeout_seconds,
            } => {
                let deadline = Instant::now().checked_add(Duration::from_secs(timeout_seconds));
                let progress = compare::Progress::default();
                compare::compare(
                    &expand_home(&left),
                    &expand_home(&right),
                    &progress,
                    deadline,
                )
                .map(|mut comparison| {
                    let read =
                        verify.then(|| comparison.verify(&[PathBuf::new()], &progress, deadline));
                    compared(&comparison, limit, read, timeout_seconds)
                })
            }
            ToolCall::Diff {
                left,
                right,
                context,
                limit,
            } => {
                let (left, right) = (expand_home(&left), expand_home(&right));
                compare::diff(&left, &right, context)
                    .map(|diff| unified(&diff, &left, &right, limit))
            }
            ToolCall::CompareCopy {
                left,
                right,
                paths,
                confirm,
                plan,
                limit,
                timeout_seconds,
            } => match (confirm, left, right) {
                (true, ..) => self.run_plan(plan.as_deref(), limit),
                (false, Some(left), Some(right)) => {
                    self.preview_copy(&left, &right, &paths, limit, timeout_seconds)
                }
                (false, ..) => Err("Pass left and right to preview a copy.".into()),
            },
            ToolCall::StorageScan {
                path,
                largest,
                depth,
                limit,
                timeout_seconds,
            } => {
                let deadline = Instant::now().checked_add(Duration::from_secs(timeout_seconds));
                storage::scan(&expand_home(&path), &storage::Progress::default(), deadline)
                    .map(|report| storage_report(&report, largest, depth, limit, timeout_seconds))
            }
            ToolCall::Duplicates {
                path,
                min_size,
                limit,
                timeout_seconds,
            } => {
                let deadline = Instant::now().checked_add(Duration::from_secs(timeout_seconds));
                let progress = storage::Progress::default();
                storage::scan(&expand_home(&path), &progress, deadline).map(|report| {
                    let found = report.duplicates(&|size| size >= min_size, &progress, deadline);
                    duplicate_groups(&report, &found, limit, timeout_seconds)
                })
            }
            ToolCall::CleanupDuplicates {
                path,
                trash,
                confirm,
                plan,
                timeout_seconds,
            } => match (confirm, path) {
                (true, _) => self.run_cleanup(plan.as_deref()),
                (false, Some(path)) => self.preview_cleanup(&path, &trash, timeout_seconds),
                (false, None) => Err("Pass path and trash to preview a cleanup.".into()),
            },
            ToolCall::ContinueJob { job, conflict } => self
                .submit(Request::Retry { job, conflict })
                .map(|outcome| format!("job {} queued again\n", outcome.op)),
            ToolCall::CancelJob { job } => self
                .submit(Request::Cancel { job })
                .map(|_| format!("Cancelling job {job}; it keeps the items it finished.\n")),
            ToolCall::JobStatus { job } => self.ops.jobs().and_then(|jobs| match job {
                Some(id) => jobs
                    .iter()
                    .find(|job| job.id == id)
                    .map(describe)
                    .ok_or_else(|| format!("No job {id}.")),
                None => Ok(jobs.iter().fold(String::new(), |mut out, job| {
                    let _ = writeln!(out, "{}", summary(job));
                    out
                })),
            }),
            ToolCall::ListPlaces {} => {
                let volumes = volumes::list();
                self.places
                    .places(&volumes)
                    .map(|places| list_places(&places, &volumes))
            }
            ToolCall::Pin { path, at } => {
                let path = expand_home(&path);
                self.places
                    .pin(&path, at, &volumes::list())
                    .map(|pinned| format!("{}\n", pinned.display()))
            }
            ToolCall::Unpin { path } => {
                let path = expand_home(&path);
                self.places
                    .unpin(&path, &volumes::list())
                    .map(|()| format!("Unpinned {}; the folder is untouched.\n", path.display()))
            }
            ToolCall::Eject { path } => {
                let origin = Origin::Agent {
                    client: self.client.clone(),
                };
                volumes::eject(&expand_home(&path), &origin, &mut self.ops).map(|done| done + "\n")
            }
            ToolCall::Download { paths } => self.ask_cloud(
                &paths,
                cloud::download,
                "downloading; stat shows when it's done",
                "already downloaded",
            ),
            ToolCall::RemoveDownload { paths } => self.ask_cloud(
                &paths,
                cloud::remove_download,
                "removing the download; download brings it back",
                "already not downloaded",
            ),
            ToolCall::ListSearches {} => self.places.searches().map(|searches| {
                if searches.is_empty() {
                    return "No saved searches.\n".into();
                }
                searches.iter().fold(String::new(), |mut out, search| {
                    let _ = writeln!(out, "{}", saved_search(search));
                    out
                })
            }),
            ToolCall::SaveSearch {
                name,
                query,
                path,
                at,
            } => {
                let scope =
                    path.map_or(Scope::Everywhere, |path| Scope::Folder(expand_home(&path)));
                let search = SavedSearch { name, query, scope };
                self.places
                    .save_search(search, at)
                    .map(|saved| format!("{}\n", saved_search(&saved)))
            }
            ToolCall::RenameSearch { name, to } => self
                .places
                .rename_search(&name, &to)
                .map(|()| format!("Renamed {name} to {}.\n", to.trim())),
            ToolCall::RemoveSearch { name } => self
                .places
                .remove_search(&name)
                .map(|()| format!("Removed the saved search {name}.\n")),
            ToolCall::CatalogCreate {
                path,
                timeout_seconds,
            } => {
                let volumes = volumes::all();
                let deadline = Instant::now().checked_add(Duration::from_secs(timeout_seconds));
                self.catalogs
                    .create(
                        &expand_home(&path),
                        &volumes,
                        &storage::Progress::default(),
                        deadline,
                    )
                    .map(|catalog| catalog_line(&catalog, &volumes, index::now()))
            }
            ToolCall::CatalogList {} => self.catalogs.list().map(|catalogs| {
                if catalogs.is_empty() {
                    return "No catalogs.\n".into();
                }
                let volumes = volumes::all();
                catalogs
                    .iter()
                    .map(|catalog| catalog_line(catalog, &volumes, index::now()))
                    .collect()
            }),
            ToolCall::CatalogRefresh {
                catalog,
                timeout_seconds,
            } => {
                let volumes = volumes::all();
                let deadline = Instant::now().checked_add(Duration::from_secs(timeout_seconds));
                self.catalogs
                    .refresh(&catalog, &volumes, &storage::Progress::default(), deadline)
                    .map(|catalog| catalog_line(&catalog, &volumes, index::now()))
            }
            ToolCall::CatalogForget { catalog } => self.catalogs.forget(&catalog).map(|()| {
                format!("Forgot catalog {catalog}. Only Monodrive's catalog file went; no files changed.\n")
            }),
            ToolCall::CatalogSearch { query, limit } => {
                let volumes = volumes::all();
                Query::parse(&query)
                    .and_then(|query| self.catalogs.search(&query, &volumes, limit))
                    .map(|found| catalog_hits(&found, &volumes, index::now()))
            }
        };
        match text {
            Ok(text) if text.is_empty() => tool_text("No results.", false),
            Ok(text) => tool_text(&text, false),
            Err(e) => tool_text(&e, true),
        }
    }

    /// Runs a change through the operation owner: each resulting path on
    /// its own line, and any item that failed with its reason.
    fn submit(&mut self, request: Request) -> Result<ops::Outcome, String> {
        let origin = Origin::Agent {
            client: self.client.clone(),
        };
        self.ops.submit(origin, request)
    }

    /// Queues a job and replies with its id at once; job_status follows it.
    fn queue(&mut self, request: Request) -> Result<String, String> {
        let outcome = self.submit(request)?;
        Ok(format!("job {} queued\n", outcome.op))
    }

    fn change(&mut self, request: Request) -> Result<String, String> {
        let outcome = self.submit(request)?;
        let mut out = String::new();
        if outcome.title.starts_with("Undo ") || outcome.title.starts_with("Redo ") {
            let _ = writeln!(out, "{}", outcome.title);
        }
        for item in &outcome.items {
            let _ = match &item.result {
                Ok(path) => writeln!(out, "{}", path.display()),
                Err(e) => writeln!(out, "{e}"),
            };
        }
        if outcome.interrupted {
            out.push_str(
                "(Monodrive stopped partway; these results were read back from the disk.)\n",
            );
        }
        if outcome.failures().next().is_some() {
            Err(out)
        } else {
            Ok(out)
        }
    }

    /// Compares the two folders now and previews a one-way copy of the
    /// differences at `paths`, kept under a token for `run_plan`.
    fn preview_copy(
        &mut self,
        left: &str,
        right: &str,
        paths: &[String],
        limit: usize,
        timeout: u64,
    ) -> Result<String, String> {
        let deadline = Instant::now().checked_add(Duration::from_secs(timeout));
        let comparison = compare::compare(
            &expand_home(left),
            &expand_home(right),
            &compare::Progress::default(),
            deadline,
        )?;
        if !comparison.complete {
            return Err(format!(
                "{}So there's no plan; nothing was copied.\n",
                stopped(timeout)
            ));
        }
        let under: Vec<PathBuf> = match paths {
            [] => vec![PathBuf::new()],
            paths => paths.iter().map(PathBuf::from).collect(),
        };
        let plan = comparison.plan(&under);
        let preview = copy_plan(&plan, limit);
        if plan.request().is_none() {
            return Ok(preview + "Nothing to copy.\n");
        }
        let token = self.keep(Plan::Copy(plan));
        Ok(format!(
            "{preview}plan: {token}\nPass confirm: true and this plan within 10 minutes to copy it.\n"
        ))
    }

    /// Keeps a previewed plan under a new token.
    fn keep(&mut self, plan: Plan) -> String {
        let token = format!("{:016x}", RandomState::new().hash_one(self.plans.len()));
        self.plans.retain(|kept| kept.made.elapsed() < PLAN_LIFE);
        if self.plans.len() >= PLANS_KEPT {
            self.plans.remove(0);
        }
        self.plans.push(Previewed {
            token: token.clone(),
            made: Instant::now(),
            plan,
        });
        token
    }

    /// Where the plan a preview kept under `token` is, if it's still kept
    /// and `is` the kind asked for.
    fn kept(
        &mut self,
        token: Option<&str>,
        is: fn(&Plan) -> bool,
        again: &str,
    ) -> Result<usize, String> {
        self.plans.retain(|kept| kept.made.elapsed() < PLAN_LIFE);
        token
            .and_then(|token| {
                self.plans
                    .iter()
                    .position(|kept| kept.token == token && is(&kept.plan))
            })
            .ok_or_else(|| {
                format!("No previewed plan with that token in this session. A plan runs once, within 10 minutes; preview again with {again}.")
            })
    }

    /// Queues exactly the plan a preview kept, once. The owner refuses each
    /// item whose source or destination changed since.
    fn run_plan(&mut self, token: Option<&str>, limit: usize) -> Result<String, String> {
        let at = self.kept(
            token,
            |plan| matches!(plan, Plan::Copy(_)),
            "left and right",
        )?;
        let Plan::Copy(plan) = &self.plans[at].plan else {
            return Err("Not a copy plan.".into());
        };
        let preview = copy_plan(plan, limit);
        let request = plan.request().ok_or("Nothing to copy.")?;
        let queued = self.queue(request)?;
        self.plans.remove(at);
        Ok(preview + &queued)
    }

    /// Scans `path` again, verifies the copies of `trash` there, and
    /// previews trashing exactly those, kept under a token for
    /// `run_cleanup`.
    fn preview_cleanup(
        &mut self,
        path: &str,
        trash: &[String],
        timeout: u64,
    ) -> Result<String, String> {
        // By their folders' real paths, as the scan reports them.
        let (trash, sizes): (Vec<PathBuf>, Vec<u64>) = expand(trash)
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
        let deadline = Instant::now().checked_add(Duration::from_secs(timeout));
        let progress = storage::Progress::default();
        let report = storage::scan(&expand_home(path), &progress, deadline)?;
        let found = report.duplicates(&|size| sizes.contains(&size), &progress, deadline);
        let plan = found.cleanup(&trash).map_err(|why| match found.stopped {
            Some(stop) => format!("{why}\n{}", stopped_scan(stop, timeout)),
            None => why,
        })?;
        let preview = cleanup_plan(&plan);
        let token = self.keep(Plan::Cleanup(plan, preview.clone()));
        Ok(format!(
            "{preview}plan: {token}\nPass confirm: true and this plan within 10 minutes to trash them.\n"
        ))
    }

    /// Trashes exactly what a preview kept, once, as one change undo
    /// reverses. The owner refuses each copy that changed since.
    fn run_cleanup(&mut self, token: Option<&str>) -> Result<String, String> {
        let at = self.kept(
            token,
            |plan| matches!(plan, Plan::Cleanup(..)),
            "path and trash",
        )?;
        let Plan::Cleanup(plan, preview) = &self.plans[at].plan else {
            return Err("Not a cleanup plan.".into());
        };
        let (request, preview) = (plan.request(), preview.clone());
        // Kept when the owner refuses it whole, such as while agents may
        // not change files, so it can be confirmed again.
        let outcome = self.submit(request)?;
        self.plans.remove(at);
        let mut out = preview;
        for item in &outcome.items {
            let _ = match &item.result {
                Ok(path) => writeln!(out, "trashed: {} → {}", item.path.display(), path.display()),
                Err(why) => writeln!(out, "kept: {why}"),
            };
        }
        if outcome.failures().next().is_some() {
            Err(out)
        } else {
            Ok(out)
        }
    }

    /// Asks macOS to download or remove the download of each path, a line
    /// each saying what it came to.
    fn ask_cloud(
        &self,
        paths: &[String],
        ask: fn(&Path, &Origin, &ops::Client) -> Result<Asked, String>,
        requested: &str,
        unneeded: &str,
    ) -> Result<String, String> {
        let origin = Origin::Agent {
            client: self.client.clone(),
        };
        let mut failed = false;
        let mut lines: Vec<String> = expand(paths)
            .iter()
            .map(|path| match ask(path, &origin, &self.ops) {
                Ok(Asked::Requested) => format!("{}: {requested}", path.display()),
                Ok(Asked::Unneeded) => format!("{}: {unneeded}", path.display()),
                Err(e) => {
                    failed = true;
                    e
                }
            })
            .collect();
        // The Agents setting refuses every path alike.
        lines.dedup();
        let text = lines.join("\n") + "\n";
        if failed { Err(text) } else { Ok(text) }
    }

    /// Where the index is, without starting it.
    fn status(&self) -> Result<Status, String> {
        match &self.index {
            None => Ok(Status::not_started(index::saved())),
            Some(index) => index.as_ref().map(Index::status).map_err(Clone::clone),
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "each agent session's server starts the index on its first search"
    )]
    fn index(&mut self) -> Result<&Index, String> {
        self.index
            .get_or_insert_with(Index::start)
            .as_ref()
            .map_err(Clone::clone)
    }
}

/// One item's Get Info, a field a line after its path. Opened appears only
/// when macOS recorded it.
fn stat(about: &Info) -> String {
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
    let availability = match about.availability {
        cloud::Availability::Local if about.in_cloud => Some("downloaded"),
        cloud::Availability::Local | cloud::Availability::Unknown => None,
        cloud::Availability::Placeholder => Some("not downloaded"),
        cloud::Availability::Downloading => Some("downloading"),
    };
    if let Some(availability) = availability {
        field("availability", availability.into());
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

/// A folder's totals, saying so when it stopped before counting everything.
fn folder_size(folder: &std::path::Path, size: &FolderSize, timeout: u64) -> String {
    let mut out = format!("{}/\n", folder.display());
    let _ = writeln!(
        out,
        "size: {} bytes ({} on disk)\nfiles: {}\nfolders: {}",
        size.bytes, size.on_disk, size.files, size.folders
    );
    if size.unreadable > 0 {
        let _ = writeln!(
            out,
            "unreadable folders: {} (what couldn't be read isn't counted)",
            size.unreadable
        );
    }
    placeholders(&mut out, size.placeholders);
    if !size.complete {
        let _ = writeln!(
            out,
            "(stopped after {timeout} s: these are partial totals; pass a larger timeout_seconds)"
        );
    }
    out
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

/// What list_dir and others print after an item that isn't downloaded.
fn marker(availability: cloud::Availability) -> &'static str {
    match availability {
        cloud::Availability::Placeholder => " (not downloaded)",
        cloud::Availability::Downloading => " (downloading)",
        cloud::Availability::Local | cloud::Availability::Unknown => "",
    }
}

/// ISO 8601 in UTC, to the second. A date too far out for a calendar year
/// prints as seconds from the epoch.
fn utc(time: SystemTime) -> String {
    let seconds: libc::time_t = match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        Err(before) => 0i64
            .checked_sub_unsigned(before.duration().as_secs())
            .unwrap_or(i64::MIN),
    };
    // SAFETY: an all-zero tm is valid, and gmtime_r writes only into it.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        (!libc::gmtime_r(&seconds, &mut tm).is_null()).then_some(tm)
    };
    let Some(tm) = tm else {
        return format!("{seconds} seconds from 1970-01-01T00:00:00Z");
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        i64::from(tm.tm_year) + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// `name: query`, and the folder it searches when it has one.
fn saved_search(search: &SavedSearch) -> String {
    match &search.scope {
        Scope::Everywhere => format!("{}: {}", search.name, search.query),
        Scope::Folder(folder) => {
            format!(
                "{}: {} (in {})",
                search.name,
                search.query,
                folder.display()
            )
        }
    }
}

/// Favorites, then volumes: one path a line, with what's worth knowing
/// about it in brackets.
fn list_places(places: &[Place], volumes: &[Volume]) -> String {
    let mut out = String::from("Favorites\n");
    for place in places {
        let _ = match &place.availability {
            Availability::Available(path) => writeln!(out, "{}", path.display()),
            Availability::Unavailable(why) => {
                writeln!(
                    out,
                    "{} (unavailable: {why})",
                    place.favorite.path.display()
                )
            }
        };
    }
    out.push_str("Volumes\n");
    for volume in volumes {
        let mut about = vec![
            volume.name.clone(),
            format!("{} free of {}", bytes(volume.free), bytes(volume.total)),
        ];
        for (yes, label) in [
            (volume.startup, "startup disk"),
            (volume.read_only, "read-only"),
            (volume.network, "network"),
            (volume.ejectable || volume.removable, "removable"),
        ] {
            if yes {
                about.push(label.into());
            }
        }
        match &volume.id {
            Some(VolumeId::Uuid(uuid)) => about.push(format!("uuid {uuid}")),
            Some(VolumeId::Url(url)) => about.push(url.clone()),
            None => {}
        }
        let _ = writeln!(out, "{} ({})", volume.mount.display(), about.join(", "));
    }
    out
}

/// A catalog's source where it is now or was last seen, then what's worth
/// knowing about it in brackets, and a line for anything it's missing.
fn catalog_line(catalog: &Catalog, volumes: &[Volume], now: i64) -> String {
    let now = u64::try_from(now).unwrap_or(0);
    let (path, state) = match catalog.source.locate(volumes) {
        Availability::Available(path) => (path, "connected"),
        Availability::Unavailable(_) => (catalog.source.path.clone(), "offline"),
    };
    let mut out = format!(
        "{} (catalog {}, {}, {state}, refreshed {}, {} entries, {} on disk)\n",
        path.display(),
        catalog.id,
        catalog.source.volume.name,
        ago(catalog.refreshed, now),
        index::thousands(catalog.entries as usize),
        small_bytes(catalog.bytes),
    );
    if catalog.unreadable > 0 {
        let _ = writeln!(
            out,
            "  incomplete: {} folders couldn't be read, and what's in them is missing",
            catalog.unreadable
        );
    }
    if catalog.truncated {
        let _ = writeln!(
            out,
            "  incomplete: stopped at {} entries",
            index::thousands(catalogs::MAX_ENTRIES)
        );
    }
    if let Some(failed) = &catalog.failed {
        let _ = writeln!(
            out,
            "  last refresh failed {}: {} The entries are from the refresh before.",
            ago(failed.at, now),
            failed.why
        );
    }
    out
}

/// Each catalog a line, saying how old it is and whether its source is
/// connected, then its hits a path a line: as cataloged when it's offline,
/// and where they are now, checked, when it's connected.
fn catalog_hits(found: &Found, volumes: &[Volume], now: i64) -> String {
    let now = u64::try_from(now).unwrap_or(0);
    let mut out = String::new();
    for (at, searched) in found.catalogs.iter().enumerate() {
        let catalog = &searched.catalog;
        let state = match &searched.source {
            Availability::Available(path) => format!("connected at {}", path.display()),
            Availability::Unavailable(why) => format!("offline: {why}"),
        };
        let _ = writeln!(
            out,
            "{} (catalog {}): cataloged {}, {state}",
            catalog.source.volume.name,
            catalog.id,
            ago(catalog.refreshed, now)
        );
        for hit in found.hits.iter().filter(|hit| hit.catalog == at) {
            let mark = suffix(hit.entry.kind);
            let _ = match catalog.resolve(hit, volumes) {
                Resolved::Current(path) => writeln!(out, "{}{mark}", path.display()),
                Resolved::Changed(path, why) => writeln!(out, "{}{mark} ({why})", path.display()),
                Resolved::Offline(_) => writeln!(out, "{}{mark}", hit.entry.path.display()),
            };
        }
    }
    out
}

/// How long before `now` a moment in seconds from the epoch was.
fn ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    let (n, unit) = match seconds {
        0..60 => return "just now".into(),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    let s = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{s} ago")
}

/// Finder's decimal units, for files that may be small.
fn small_bytes(n: u64) -> String {
    match n {
        ..1_000_000 => format!("{} KB", n.div_ceil(1000)),
        _ => format!("{:.1} MB", n as f64 / 1e6),
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

/// One line: what a job is doing, how far it's got, and who asked.
fn summary(job: &Job) -> String {
    let done = job.tasks.iter().filter(|task| task.done.is_some()).count();
    let by = match &job.origin {
        Origin::Window => "the window".to_string(),
        Origin::Agent { client: Some(name) } => format!("agent {name}"),
        Origin::Agent { client: None } => "an agent".to_string(),
    };
    format!(
        "job {} {} into {}: {}, {done} of {} items, {} of {} bytes, by {by}",
        job.id,
        job.title,
        job.into.display(),
        job.state.label(),
        job.tasks.len(),
        job.bytes,
        job.total_bytes,
    )
}

/// The summary, then each finished item: where it went, or why not.
fn describe(job: &Job) -> String {
    let mut out = format!("{}\n", summary(job));
    for done in job.tasks.iter().filter_map(|task| task.done.as_ref()) {
        let _ = match done {
            Done::To(path) => writeln!(out, "{}", path.display()),
            Done::Skipped(why) | Done::Failed(why) => writeln!(out, "{why}"),
        };
    }
    out
}

/// A line an entry: its name as stored, links ending in @, then what it
/// holds, and why it can't be extracted when it can't.
fn archive_list(archive: &Archive, folder: &str, limit: usize) -> String {
    let mut out = String::new();
    let mut shown = 0;
    for entry in archive.under(folder) {
        shown += 1;
        if shown > limit {
            continue;
        }
        let name = archives::printable(&entry.name);
        let _ = match entry.kind {
            Kind::Dir => write!(out, "{name}"),
            Kind::Link => write!(out, "{name}@"),
            Kind::File | Kind::Other => write!(
                out,
                "{name}  {} bytes, {} compressed",
                entry.size, entry.compressed
            ),
        };
        if let Some(modified) = entry.modified {
            let _ = write!(out, "  {}", utc(modified));
        }
        if let Some(why) = entry.problem() {
            let _ = write!(out, "  (can't extract: {why})");
        }
        out.push('\n');
    }
    if shown > limit {
        let _ = writeln!(
            out,
            "({} more; pass folder or a larger limit)",
            shown - limit
        );
    }
    if archive.total > archive.entries.len() as u64 {
        let _ = writeln!(
            out,
            "(the archive lists {} entries; Monodrive read the first {})",
            archive.total,
            archive.entries.len()
        );
    }
    out
}

/// Both roots, the counts, then each difference a line, by its path
/// relative to both. Equal and same-looking entries are only counted.
fn compared(comparison: &Comparison, limit: usize, read: Option<bool>, timeout: u64) -> String {
    use compare::Status;
    let label = |status: &Status| match status {
        Status::OnlyLeft => "only left",
        Status::OnlyRight => "only right",
        Status::TypeConflict => "type conflict",
        Status::SameMetadata => "same size and date",
        Status::MetadataDiffers => "dates differ",
        Status::Equal => "equal",
        Status::Differs => "differs",
        Status::Unknown(_) => "unknown",
    };
    let mut out = format!(
        "left: {}\nright: {}\n",
        comparison.left.display(),
        comparison.right.display()
    );
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for entry in &comparison.entries {
        match counts
            .iter_mut()
            .find(|(name, _)| *name == label(&entry.status))
        {
            Some((_, n)) => *n += 1,
            None => counts.push((label(&entry.status), 1)),
        }
    }
    counts.sort_by_key(|(name, _)| {
        [
            "only left",
            "only right",
            "type conflict",
            "dates differ",
            "differs",
            "unknown",
            "same size and date",
            "equal",
        ]
        .iter()
        .position(|n| n == name)
    });
    let counts: Vec<String> = counts
        .iter()
        .map(|(name, n)| format!("{n} {name}"))
        .collect();
    let _ = writeln!(
        out,
        "{}",
        if counts.is_empty() {
            "Both are empty.".into()
        } else {
            counts.join(", ")
        }
    );
    let one = comparison.entries.len() == 1 && comparison.entries[0].path.as_os_str().is_empty();
    let shown: Vec<&compare::Entry> = comparison
        .entries
        .iter()
        .filter(|entry| one || !matches!(entry.status, Status::Equal | Status::SameMetadata))
        .collect();
    for entry in shown.iter().take(limit) {
        out.push_str(label(&entry.status));
        if !one {
            let kind = entry
                .left
                .as_ref()
                .or(entry.right.as_ref())
                .map(|side| side.kind);
            let _ = write!(out, ": {}{}", entry.path.display(), kind.map_or("", suffix));
        }
        let detail = match (&entry.status, &entry.left, &entry.right) {
            (Status::TypeConflict, Some(l), Some(r)) => format!(
                " ({} on the left, {} on the right)",
                compare::kind_name(l.kind),
                compare::kind_name(r.kind)
            ),
            (Status::Unknown(why), ..) => format!(" ({why})"),
            _ => match entry.newer() {
                Some(Newer::Left) => " (left newer)".into(),
                Some(Newer::Right) => " (right newer)".into(),
                None => String::new(),
            },
        };
        let cloud = |side: &Option<compare::Side>| {
            side.as_ref()
                .is_some_and(|side| !side.availability.is_local())
        };
        let cloud = match (cloud(&entry.left), cloud(&entry.right)) {
            (true, true) => " (not downloaded)",
            (true, false) => " (left not downloaded)",
            (false, true) => " (right not downloaded)",
            (false, false) => "",
        };
        let _ = writeln!(out, "{detail}{cloud}");
    }
    if shown.len() > limit {
        let _ = writeln!(out, "({} more; pass a larger limit)", shown.len() - limit);
    }
    let cloud = comparison
        .entries
        .iter()
        .filter(|entry| entry.not_downloaded())
        .count();
    if cloud > 0 {
        let _ = writeln!(
            out,
            "({cloud} not downloaded on a side, so verify doesn't read them; download them first.)"
        );
    }
    if comparison
        .entries
        .iter()
        .any(|entry| entry.status == Status::SameMetadata)
    {
        out.push_str(
            "(Same size and date doesn't prove equal contents; verify: true reads them.)\n",
        );
    }
    if read == Some(false) {
        let _ = writeln!(
            out,
            "(Reading stopped after {timeout}s; what it didn't read is still unverified.)"
        );
    }
    if !comparison.complete {
        out.push_str(&stopped(timeout));
    }
    out
}

/// Why a comparison isn't whole. Either limit can stop it.
fn stopped(timeout: u64) -> String {
    format!(
        "(Stopped before comparing everything, after {timeout}s or {} items; raise timeout_seconds or compare smaller folders.)\n",
        compare::MAX_ITEMS
    )
}

/// A unified diff: `@@ -start,count +start,count @@` before each hunk, and
/// `\\ No newline at end of file` after a last line without one. When the
/// changed lines end differently, ␍ marks those ending in CRLF.
fn unified(diff: &TextDiff, left: &Path, right: &Path, limit: usize) -> String {
    if diff.additions + diff.deletions == 0 {
        return "No differences in the text.\n".into();
    }
    let mut out = format!("--- {}\n+++ {}\n", left.display(), right.display());
    let mut endings = diff
        .lines
        .iter()
        .filter(|l| l.kind != LineKind::Context)
        .map(|l| l.ending);
    let first = endings.next();
    let mixed = endings.any(|ending| Some(ending) != first);
    let mut marked = false;
    let mut lines = Vec::new();
    for block in &diff.blocks {
        let Block::Hunk(range) = block else { continue };
        let side = |kind: LineKind, before: &[compare::Line], hunk: &[compare::Line]| {
            let count = hunk.iter().filter(|l| l.kind != kind).count();
            let start = before.iter().filter(|l| l.kind != kind).count();
            (start + usize::from(count > 0), count)
        };
        let (before, hunk) = (&diff.lines[..range.start], &diff.lines[range.clone()]);
        let (old, new) = (
            side(LineKind::Add, before, hunk),
            side(LineKind::Del, before, hunk),
        );
        lines.push(format!("@@ -{},{} +{},{} @@", old.0, old.1, new.0, new.1));
        for line in hunk {
            let mark = match line.kind {
                LineKind::Add => '+',
                LineKind::Del => '-',
                LineKind::Context => ' ',
            };
            let cr = mixed && line.kind != LineKind::Context && line.ending == Ending::CrLf;
            marked |= cr;
            lines.push(format!("{mark}{}{}", line.text, if cr { "␍" } else { "" }));
            if line.ending == Ending::None {
                lines.push("\\ No newline at end of file".into());
            }
        }
    }
    for line in lines.iter().take(limit) {
        let _ = writeln!(out, "{line}");
    }
    if lines.len() > limit {
        let _ = writeln!(
            out,
            "({} more lines; pass a larger limit)",
            lines.len() - limit
        );
    }
    if marked {
        out.push_str("(␍ marks a line that ends in CRLF.)\n");
    }
    out
}

/// A scan's totals, its unreadable folders, the largest files, then its
/// biggest folders as a tree, `depth` levels and `limit` a level.
fn storage_report(
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

/// Verified groups and then candidates, each a header and its paths.
fn duplicate_groups(report: &Report, found: &Duplicates, limit: usize, timeout: u64) -> String {
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

/// Each group a copy a line, kept or trashed, then what it frees.
fn cleanup_plan(plan: &Cleanup) -> String {
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

/// What a one-way copy does, a line an item, then the totals.
fn copy_plan(plan: &CopyPlan, limit: usize) -> String {
    let mut out = format!(
        "from: {}\ninto: {}\n",
        plan.from.display(),
        plan.into.display()
    );
    if plan.rows.is_empty() {
        out.push_str("No differences to copy there.\n");
        return out;
    }
    let (mut copies, mut replaces, mut skips) = (0, 0, 0);
    for (i, row) in plan.rows.iter().enumerate() {
        let line = match &row.action {
            Action::Copy => {
                copies += 1;
                format!(
                    "copy: {} → {}",
                    row.source.display(),
                    row.destination.display()
                )
            }
            Action::Replace => {
                replaces += 1;
                format!(
                    "replace: {} → {}",
                    row.source.display(),
                    row.destination.display()
                )
            }
            Action::Skip(why) => {
                skips += 1;
                format!("skip: {} ({why})", row.path.display())
            }
        };
        if i < limit {
            let _ = writeln!(out, "{line}");
        }
    }
    if plan.rows.len() > limit {
        let _ = writeln!(
            out,
            "({} more; pass a larger limit)",
            plan.rows.len() - limit
        );
    }
    let _ = writeln!(
        out,
        "{copies} to copy, {replaces} to replace, {skips} skipped. What's replaced goes to the Trash; nothing is deleted."
    );
    out
}

/// `old path → new name` a line, with the status unless it's ok.
fn rename_table(plan: &rename::Plan) -> String {
    plan.rows.iter().fold(String::new(), |mut out, row| {
        let status = match &row.status {
            rename::Status::Ok => String::new(),
            rename::Status::Unchanged => " (unchanged)".into(),
            rename::Status::Invalid(why) => format!(" (invalid: {why})"),
            rename::Status::Collision(why) => format!(" (collision: {why})"),
        };
        let _ = writeln!(out, "{} → {}{status}", row.path.display(), row.name);
        out
    })
}

fn expand(paths: &[String]) -> Vec<PathBuf> {
    paths.iter().map(|path| expand_home(path)).collect()
}

/// MCP lets a call to a tool without parameters leave out `arguments`.
fn with_arguments(mut params: Value) -> Value {
    if let Some(call) = params.as_object_mut() {
        call.entry("arguments").or_insert_with(|| json!({}));
    }
    params
}

/// `ls -F` markers, so agents can tell folders and links from files.
fn suffix(kind: Kind) -> &'static str {
    match kind {
        Kind::Dir => "/",
        Kind::Link => "@",
        Kind::File | Kind::Other => "",
    }
}

/// A tool agents can call, as `tools/list` describes it and Settings lists it.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    input_schema: fn() -> Value,
}

pub const TOOLS: [Tool; 46] = [
    Tool {
        name: "find_files",
        description: "Find files and folders by name anywhere on disk. Words are fuzzy and typo-tolerant. \
    Filters go inline: kind:image (or video, audio, document, code, archive, font, app, folder, file, \
    link), ext:rs, size:>5mb or 1mb..10mb, modified:7d, >1y, today or >=2026-01-01, in:~/Work, globs \
    such as src/**/*.ts, and !node_modules/. Repeated kind: and ext: widen; other filters narrow. \
    Best matches first.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "default": 50}
                },
                "required": ["query"]
            })
        },
    },
    Tool {
        name: "grep",
        description: "Search inside files with the content index. Smart case. mode is literal (default), \
    regex, or symbol (where a name is defined). Pass path to search one folder, and filters to pick \
    files with find_files syntax. Prints path:line: text, at most 5 lines a file.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "mode": {"type": "string", "enum": ["literal", "regex", "symbol"], "default": "literal"},
                    "path": {"type": "string", "description": "Folder to search in; ~ is your home."},
                    "filters": {"type": "string", "description": "Which files to read, such as kind:code ext:rs size:<1mb modified:7d."},
                    "limit": {"type": "integer", "minimum": 1, "default": 50, "description": "Most files to return."}
                },
                "required": ["pattern"]
            })
        },
    },
    Tool {
        name: "list_dir",
        description: "List one folder. Folders end in /, links in @, and cloud placeholders that \
    aren't downloaded in (not downloaded). Names sort as people read them (file2 before file10). For \
    the newest or largest first, sort by modified or size with descending: true.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Folder to list; ~ is your home."},
                    "show_hidden": {"type": "boolean", "default": false},
                    "sort": {"type": "string", "enum": ["name", "modified", "size", "kind"], "default": "name"},
                    "descending": {"type": "boolean", "default": false},
                    "folders_first": {"type": "boolean", "default": true}
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "index_status",
        description: "Whether the index is ready, how far its first build has got, and whether it \
    can see the whole disk. Never starts indexing.",
        input_schema: || json!({"type": "object", "properties": {}}),
    },
    Tool {
        name: "stat",
        description: "Get Info for files, folders or links, one block per path: kind, size, where, \
    volume, created, modified, opened (when macOS recorded it), permissions, whether it's a package, \
    a link's target, and for cloud items whether they're downloaded. Dates are UTC. A folder's size \
    comes from folder_size.",
        input_schema: paths_schema,
    },
    Tool {
        name: "folder_size",
        description: "Add up every file under a folder without following links. Counts hard links \
    once and cloud placeholders at their size without downloading them, and reports folders it couldn't \
    read. Stops at timeout_seconds and says when totals are partial.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "A folder. Absolute; ~ is your home."},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "default": 10}
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "trash",
        description: "Move files or folders to the Trash; never deletes. Prints where each went in \
    the Trash. put_back or undo restores them.",
        input_schema: paths_schema,
    },
    Tool {
        name: "put_back",
        description: "Restore items Monodrive moved to the Trash, given their paths in the Trash. \
    Prints where each went back to.",
        input_schema: paths_schema,
    },
    Tool {
        name: "rename",
        description: "Rename a file or folder where it is. name is the new name, not a path. \
    Never replaces an existing item.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute; ~ is your home."},
                    "name": {"type": "string"}
                },
                "required": ["path", "name"]
            })
        },
    },
    Tool {
        name: "rename_preview",
        description: "Show how rename_many would rename items, changing nothing. A replace rule swaps \
    text before the extension (in it too with extension: true); a format rule names items name plus a \
    number in the order given, such as \"Trip 001.jpg\". Prints old path → new name, then any of \
    (unchanged), (invalid: why) or (collision: why).",
        input_schema: || rename_schema(false),
    },
    Tool {
        name: "rename_many",
        description: "Rename items where they are, as rename_preview shows, as one change undo reverses. \
    Refuses if any item is invalid or collides, unless skip_problems renames only the rest. Never \
    replaces an existing item. Prints each new path.",
        input_schema: || rename_schema(true),
    },
    Tool {
        name: "create_folder",
        description: "Create an empty folder at path. Its parent must exist.",
        input_schema: path_schema,
    },
    Tool {
        name: "create_folder_with",
        description: "Create a folder at path and move paths into it, all on one volume, as one \
    change: one undo moves them back and removes the folder. Prints the folder, then each item's new path.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The new folder. Absolute; ~ is your home."},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "What goes in it. Absolute; ~ is your home."}
                },
                "required": ["path", "paths"]
            })
        },
    },
    Tool {
        name: "create_file",
        description: "Create an empty file at path. Its parent must exist; never replaces a file.",
        input_schema: path_schema,
    },
    Tool {
        name: "duplicate",
        description: "Copy files or folders beside themselves, named as Finder does (\"name copy\"), \
    keeping metadata. Prints each copy's path.",
        input_schema: paths_schema,
    },
    Tool {
        name: "undo",
        description: "Undo the last file change, whether this agent, another agent or the Monodrive \
    window made it. Refuses if the files changed since.",
        input_schema: || json!({"type": "object", "properties": {}}),
    },
    Tool {
        name: "redo",
        description: "Redo the last undone file change. Refuses if the files changed since.",
        input_schema: || json!({"type": "object", "properties": {}}),
    },
    Tool {
        name: "copy",
        description: "Copy files or folders into a folder, keeping metadata. Returns a job id at once; \
    job_status follows it. conflict says what to do when a name is taken there.",
        input_schema: transfer_schema,
    },
    Tool {
        name: "move",
        description: "Move files or folders into a folder; across volumes it copies, checks the copy, \
    then trashes the source. Returns a job id at once; job_status follows it.",
        input_schema: transfer_schema,
    },
    Tool {
        name: "archive_list",
        description: "List what a ZIP archive holds without extracting it, an entry a line as stored: \
    folders end in /, links in @, then sizes and modified time (UTC), and why an entry can't be \
    extracted, such as encryption. folder lists one folder inside.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The archive. Absolute; ~ is your home."},
                    "folder": {"type": "string", "description": "A folder inside, such as docs/."},
                    "limit": {"type": "integer", "minimum": 1, "default": 200}
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "archive_create",
        description: "Compress files or folders from one folder into a new ZIP beside them: name.zip \
    for one item, Archive.zip for more, numbered if taken. Links are stored as links. Returns a job \
    id; job_status prints the archive's path.",
        input_schema: paths_schema,
    },
    Tool {
        name: "archive_extract",
        description: "Extract a ZIP archive into the folder into, as a job: everything, or the entries \
    named, each coming out under its own name. Refuses paths leading out, encrypted entries and zip \
    bombs. conflict applies to each item. Returns a job id.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The archive. Absolute; ~ is your home."},
                    "into": {"type": "string", "description": "The folder they go into."},
                    "entries": {"type": "array", "items": {"type": "string"}, "description": "Entries or folders as archive_list prints them; all by default."},
                    "conflict": conflict_schema()
                },
                "required": ["path", "into"]
            })
        },
    },
    Tool {
        name: "compare",
        description: "Compare two files, or two folders whole, by size and modification time, never \
    following links. Prints counts, then each difference by its path in both: only left, only right, \
    type conflict, dates differ, differs, or unknown with why. Same size and date isn't proof; verify: \
    true reads files to settle them as equal or differs, except cloud placeholders. Stops at timeout_seconds.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "left": {"type": "string", "description": "A file or folder. Absolute; ~ is your home."},
                    "right": {"type": "string", "description": "The same kind of item to compare it with."},
                    "verify": {"type": "boolean", "default": false},
                    "limit": {"type": "integer", "minimum": 1, "default": 200},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "default": 10}
                },
                "required": ["left", "right"]
            })
        },
    },
    Tool {
        name: "diff",
        description: "A unified line diff of two text files, with context lines around each change. \
    Refuses binary files and files over 1 MB.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "left": {"type": "string", "description": "The old file. Absolute; ~ is your home."},
                    "right": {"type": "string", "description": "The new file."},
                    "context": {"type": "integer", "minimum": 0, "default": 3},
                    "limit": {"type": "integer", "minimum": 1, "default": 500, "description": "Most lines to print."}
                },
                "required": ["left", "right"]
            })
        },
    },
    Tool {
        name: "compare_copy",
        description: "Copy differences one way, from the left folder into the right: what's only on \
    the left, and files that differ where the left one is newer, whose older copy goes to the Trash. \
    Never deletes what's only on the right. A preview with left and right prints a plan token; \
    confirm: true with that plan runs exactly that preview, once, within 10 minutes, and returns a \
    job id; undo reverses it. Items that changed since the preview are refused.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "left": {"type": "string", "description": "The folder to copy from. Absolute; ~ is your home."},
                    "right": {"type": "string", "description": "The folder to copy into."},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Entries as compare prints them, or folders holding them; all differences by default."},
                    "confirm": {"type": "boolean", "default": false},
                    "plan": {"type": "string", "description": "The token a preview printed; needed with confirm."},
                    "limit": {"type": "integer", "minimum": 1, "default": 200},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "default": 10}
                }
            })
        },
    },
    Tool {
        name: "storage_scan",
        description: "What takes up space in a folder: totals, the largest files, and the folders \
    holding the most, as a tree. Never follows links and counts hard links once. on disk is the blocks \
    files take; unshared is what trashing them frees once the Trash is emptied, less when APFS clones \
    or snapshots share blocks. Reports unreadable folders; stops at timeout_seconds with partial totals.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "A folder. Absolute; ~ is your home."},
                    "largest": {"type": "integer", "minimum": 0, "default": 20, "description": "How many of the largest files to print."},
                    "depth": {"type": "integer", "minimum": 0, "default": 2, "description": "Levels of folders to print."},
                    "limit": {"type": "integer", "minimum": 1, "default": 10, "description": "Most folders to print in each."},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "default": 30}
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "duplicates",
        description: "Find files with identical contents in a folder. Same-size files are candidates; \
    reading their start, middle and end narrows them, and reading them whole verifies them. Hard links \
    are one file, never duplicates, and cloud placeholders are never read. Prints verified groups, then \
    unverified candidates, each copy's path a line. Only verified copies can go to cleanup_duplicates.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "A folder. Absolute; ~ is your home."},
                    "min_size": {"type": "integer", "minimum": 1, "default": 1, "description": "Smallest file size in bytes."},
                    "limit": {"type": "integer", "minimum": 1, "default": 20, "description": "Most groups to print."},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "default": 30}
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "cleanup_duplicates",
        description: "Move chosen duplicate copies to the Trash. A preview with path and trash scans \
    path, verifies those copies again, and prints what's kept and trashed with a plan token; it refuses \
    a copy that isn't verified and a plan that would trash every copy of a group. confirm: true with \
    that plan trashes exactly that, once, within 10 minutes, as one change undo reverses. Copies that \
    changed since the preview stay.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The folder duplicates scanned. Absolute; ~ is your home."},
                    "trash": {"type": "array", "items": {"type": "string"}, "description": "Copies to trash, as duplicates prints them."},
                    "confirm": {"type": "boolean", "default": false},
                    "plan": {"type": "string", "description": "The token a preview printed; needed with confirm."},
                    "timeout_seconds": {"type": "integer", "minimum": 1, "default": 30}
                }
            })
        },
    },
    Tool {
        name: "job_status",
        description: "A copy, move or archive job's state and progress, then each finished item's \
    resulting path or error. Without job, one line per recent job.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"job": {"type": "integer"}}
            })
        },
    },
    Tool {
        name: "cancel_job",
        description: "Stop a job after the item it's on, or one waiting at a conflict. Finished items \
    stay; undo reverses them.",
        input_schema: job_schema,
    },
    Tool {
        name: "continue_job",
        description: "Continue a job that's waiting at a conflict, failed or was interrupted. It stays \
    the same job: finished items are kept and the rest run again, optionally with another conflict \
    choice.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"job": {"type": "integer"}, "conflict": conflict_schema()},
                "required": ["job"]
            })
        },
    },
    Tool {
        name: "list_places",
        description: "The Monodrive sidebar's favorite folders, then the mounted volumes with free space, \
    read-only, network and removable flags, and their identity. Never reads a volume's contents.",
        input_schema: || json!({"type": "object", "properties": {}}),
    },
    Tool {
        name: "pin",
        description: "Add a folder to the sidebar's favorites, or move one already there to position \
    at (0 is first). Changes no files.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "A folder. Absolute; ~ is your home."},
                    "at": {"type": "integer", "minimum": 0, "description": "Position; last by default."}
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "unpin",
        description: "Remove a folder from the sidebar's favorites. Only the bookmark goes; the folder stays.",
        input_schema: path_schema,
    },
    Tool {
        name: "eject",
        description: "Eject or unmount the volume mounted at path, as Finder does. Refuses the startup disk, \
    a volume a Monodrive copy or move is using, and one macOS says is busy; never forces. Undo by \
    reconnecting the drive or share.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "Where it's mounted, as list_places prints it."}},
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "download",
        description: "Ask iCloud Drive or the cloud app holding each path to download it, in the \
    background. Jobs that read contents, such as copy, refuse items that aren't downloaded. stat shows when \
    each is done, or why its download failed.",
        input_schema: paths_schema,
    },
    Tool {
        name: "remove_download",
        description: "Remove the downloaded copy of each path and keep it in the cloud, as Finder's \
    Remove Download does. Refuses an item that isn't uploaded yet or has conflicts. download brings it back.",
        input_schema: paths_schema,
    },
    Tool {
        name: "list_searches",
        description: "The Monodrive sidebar's saved searches in order, one a line: name: query, \
    then (in folder) when it searches one folder. Runs none of them.",
        input_schema: || json!({"type": "object", "properties": {}}),
    },
    Tool {
        name: "save_search",
        description: "Save a search in the sidebar, in find_files syntax, without running it. \
    Saving under a taken name replaces that search; at moves it (0 is first). Prints it as saved.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "query": {"type": "string", "description": "Such as kind:image size:>1mb."},
                    "path": {"type": "string", "description": "Folder it searches; everywhere by default. ~ is your home."},
                    "at": {"type": "integer", "minimum": 0, "description": "Position; last by default."}
                },
                "required": ["name", "query"]
            })
        },
    },
    Tool {
        name: "rename_search",
        description: "Rename a saved search.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}, "to": {"type": "string"}},
                "required": ["name", "to"]
            })
        },
    },
    Tool {
        name: "remove_search",
        description: "Remove a saved search from the sidebar. Changes no files.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            })
        },
    },
    Tool {
        name: "catalog_create",
        description: "Catalog a volume or a folder on one: walk it once and keep its names, \
    kinds, sizes and dates in Monodrive, so catalog_search finds them after it's disconnected. \
    Reads no contents and downloads nothing. Refuses the startup disk. Cataloging a source again \
    refreshes it. Prints the catalog as catalog_list does.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The volume or folder. Absolute; ~ is your home."},
                    "timeout_seconds": {"type": "integer", "minimum": 0, "default": 300, "description": "A walk that runs out of time keeps nothing."}
                },
                "required": ["path"]
            })
        },
    },
    Tool {
        name: "catalog_list",
        description: "Each catalog a line: its source's path, where it is now when connected, its id, \
    volume, connected or offline, age, entries and size on disk, then what it's missing and \
    whether its last refresh failed.",
        input_schema: || json!({"type": "object", "properties": {}}),
    },
    Tool {
        name: "catalog_refresh",
        description: "Walk a catalog's source again, found by its volume's identity wherever it's \
    mounted now. A refresh that fails or runs out of time keeps the last good catalog.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "catalog": {"type": "string", "description": "Its id, as catalog_list prints it."},
                    "timeout_seconds": {"type": "integer", "minimum": 0, "default": 300}
                },
                "required": ["catalog"]
            })
        },
    },
    Tool {
        name: "catalog_forget",
        description: "Delete a catalog from Monodrive. Only Monodrive's catalog file goes; the \
    source's files are untouched.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {"catalog": {"type": "string", "description": "Its id, as catalog_list prints it."}},
                "required": ["catalog"]
            })
        },
    },
    Tool {
        name: "catalog_search",
        description: "Search the catalogs by name, in find_files syntax, connected or not. Prints \
    each catalog's age and whether its source is offline, then its matches a path a line: as \
    cataloged while offline, or checked where they are now, marked when they've changed.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "default": 50}
                },
                "required": ["query"]
            })
        },
    },
];

fn tools() -> Value {
    TOOLS
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": (tool.input_schema)(),
            })
        })
        .collect()
}

fn conflict_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["stop", "skip", "keep_both", "replace"],
        "default": "stop",
        "description": "stop waits for continue_job; replace trashes what's there first."
    })
}

fn transfer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "paths": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Absolute; ~ is your home."
            },
            "into": {"type": "string", "description": "The folder they go into."},
            "conflict": conflict_schema()
        },
        "required": ["paths", "into"]
    })
}

fn rename_schema(skip: bool) -> Value {
    let mut schema = json!({
        "type": "object",
        "properties": {
            "paths": {
                "type": "array",
                "items": {"type": "string"},
                "description": "In the order to number them. Absolute; ~ is your home."
            },
            "rule": {"oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "type": {"const": "replace"},
                        "find": {"type": "string"},
                        "with": {"type": "string", "default": ""},
                        "case_sensitive": {"type": "boolean", "default": false},
                        "extension": {"type": "boolean", "default": false}
                    },
                    "required": ["type", "find"]
                },
                {
                    "type": "object",
                    "properties": {
                        "type": {"const": "format"},
                        "name": {"type": "string"},
                        "start": {"type": "integer", "minimum": 0, "default": 1},
                        "digits": {"type": "integer", "minimum": 0, "default": 0, "description": "Pads the number with zeros."},
                        "place": {"type": "string", "enum": ["after", "before"], "default": "after"}
                    },
                    "required": ["type"]
                }
            ]}
        },
        "required": ["paths", "rule"]
    });
    if skip {
        schema["properties"]["skip_problems"] = json!({"type": "boolean", "default": false});
    }
    schema
}

fn job_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"job": {"type": "integer"}},
        "required": ["job"]
    })
}

fn path_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"path": {"type": "string", "description": "Absolute; ~ is your home."}},
        "required": ["path"]
    })
}

fn paths_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "paths": {
                "type": "array",
                "items": {"type": "string"},
                "description": "Absolute; ~ is your home."
            }
        },
        "required": ["paths"]
    })
}

fn tool_text(text: &str, is_error: bool) -> Value {
    json!({ "content": [{"type": "text", "text": text}], "isError": is_error })
}

fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message} })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(server: &mut Server, params: Value) -> Value {
        server
            .handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params}))
            .expect("a request gets a reply")
    }

    /// A server whose changes go nowhere: read-only tools never connect.
    fn server() -> Server {
        let paths = ops::Paths {
            state: "/nonexistent/monodrive-test".into(),
            settings: "/nonexistent/monodrive-test/settings.json".into(),
            trash: "/nonexistent/monodrive-test/Trash".into(),
        };
        Server::new(
            ops::Client::new(paths, None),
            places("/nonexistent/monodrive-test".as_ref()),
            catalogs::Store {
                dir: "/nonexistent/monodrive-test/catalogs".into(),
            },
        )
    }

    fn places(dir: &std::path::Path) -> places::Store {
        places::Store {
            places: dir.join("places.json"),
            recents: dir.join("recents.json"),
        }
    }

    fn catalog_store(dir: &std::path::Path) -> catalogs::Store {
        catalogs::Store {
            dir: dir.join("catalogs"),
        }
    }

    fn text(reply: &Value) -> &str {
        reply["result"]["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn notifications_get_no_reply() {
        let mut server = server();
        let reply =
            server.handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        assert_eq!(reply, None);
    }

    #[test]
    fn list_dir_lists_folders_first_without_starting_the_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.toml"), "").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let mut server = server();
        let reply = call(
            &mut server,
            json!({"name": "list_dir", "arguments": {"path": dir.path()}}),
        );
        assert_eq!(reply["result"]["isError"], false);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert_eq!(text.lines().collect::<Vec<_>>(), ["src/", "a.toml"]);
        assert!(server.index.is_none());
    }

    #[test]
    fn index_status_reports_without_starting_the_index() {
        let mut server = server();
        let reply = call(
            &mut server,
            json!({"name": "index_status", "arguments": {}}),
        );
        assert_eq!(reply["result"]["isError"], false);
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("Saved index") || text.starts_with("No index yet"),
            "{text}"
        );
        // MCP lets a call without parameters leave out `arguments`.
        let bare = call(&mut server, json!({"name": "index_status"}));
        assert_eq!(bare["result"]["isError"], false, "{bare}");
        assert!(server.index.is_none());
    }

    #[test]
    fn stat_prints_a_block_per_path_and_folder_size_says_when_its_partial() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (file, folder, link) = (
            dir.path().join("notes.txt"),
            dir.path().join("docs"),
            dir.path().join("shortcut"),
        );
        std::fs::write(&file, "hello").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("a.txt"), "0123456789").unwrap();
        std::os::unix::fs::symlink("docs", &link).unwrap();
        let missing = dir.path().join("missing");
        let mut server = server();

        let reply = call(
            &mut server,
            json!({"name": "stat", "arguments": {"paths": [file, link, missing]}}),
        );
        // One path failing marks the reply, and the others still print.
        assert_eq!(reply["result"]["isError"], true);
        let blocks: Vec<Vec<&str>> = text(&reply)
            .split("\n\n")
            .map(|block| block.lines().collect())
            .collect();
        let shown = |path: &std::path::Path| path.display().to_string();
        let notes = &blocks[0];
        assert_eq!(
            notes[..2],
            [shown(&file), "kind: Plain Text Document".into()]
        );
        assert!(notes[2].starts_with("size: 5 bytes ("), "{notes:?}");
        assert_eq!(notes[3], format!("where: {}", shown(dir.path())));
        assert!(notes[4].starts_with("volume: "), "{notes:?}");
        assert!(notes[5].starts_with("created: 20") && notes[5].ends_with('Z'));
        assert!(notes[6].starts_with("modified: 20"));
        assert!(
            notes[7].starts_with("permissions: 644, owner "),
            "{notes:?}"
        );
        assert!(notes[7].ends_with(", you can read and write"));
        assert_eq!(notes.len(), 8);
        assert_eq!(blocks[1][0], format!("{}@", shown(&link)));
        assert_eq!(blocks[1].last(), Some(&"link to: docs"));
        assert_eq!(
            blocks[2],
            [format!(
                "{}: No such file or directory (os error 2)",
                shown(&missing)
            )]
        );
        assert_eq!(
            utc(std::time::UNIX_EPOCH + Duration::from_secs(1_793_314_588)),
            "2026-10-29T22:56:28Z"
        );

        // Any date the attribute holds prints, however odd.
        for (seconds, shown) in [
            (-1, "opened: 1969-12-31T23:59:59Z"),
            (
                i64::MIN,
                "opened: -9223372036854775808 seconds from 1970-01-01T00:00:00Z",
            ),
        ] {
            info::tests::record_opened(&file, seconds, 0);
            let reply = call(
                &mut server,
                json!({"name": "stat", "arguments": {"paths": [file]}}),
            );
            assert_eq!(reply["result"]["isError"], false, "{reply}");
            assert!(text(&reply).lines().any(|line| line == shown), "{reply}");
        }

        let reply = call(
            &mut server,
            json!({"name": "folder_size", "arguments": {"path": folder}}),
        );
        let lines: Vec<&str> = text(&reply).lines().collect();
        assert_eq!(lines[0], format!("{}/", shown(&folder)));
        assert!(lines[1].starts_with("size: 10 bytes ("), "{lines:?}");
        assert_eq!(lines[2..], ["files: 1", "folders: 0"]);
        let reply = call(
            &mut server,
            json!({"name": "folder_size", "arguments": {"path": folder, "timeout_seconds": 0}}),
        );
        assert!(
            text(&reply).ends_with(
                "(stopped after 0 s: these are partial totals; pass a larger timeout_seconds)\n"
            ),
            "{reply}"
        );
        for bad in [
            json!({"name": "stat", "arguments": {"paths": file}}),
            json!({"name": "folder_size", "arguments": {}}),
            json!({"name": "folder_size", "arguments": {"path": folder, "timeout_seconds": -1}}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }
        assert!(server.index.is_none());
    }

    #[test]
    fn placeholders_say_so_and_downloads_never_happen_unasked() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "hello").unwrap();
        let mut about = info::info(&file).unwrap();
        assert_eq!(about.availability, cloud::Availability::Local);
        assert!(!stat(&about).contains("availability"));
        let field = |about: &Info, name: &str| {
            stat(about)
                .lines()
                .find_map(|line| line.strip_prefix(&format!("{name}: ")).map(str::to_owned))
        };
        about.in_cloud = true;
        assert_eq!(field(&about, "availability").as_deref(), Some("downloaded"));
        about.availability = cloud::Availability::Placeholder;
        about.download_error = Some("The server is unavailable.".into());
        assert_eq!(
            field(&about, "availability").as_deref(),
            Some("not downloaded")
        );
        assert_eq!(
            field(&about, "download failed").as_deref(),
            Some("The server is unavailable.")
        );
        let size = FolderSize {
            placeholders: 2,
            complete: true,
            ..FolderSize::default()
        };
        assert!(
            folder_size(dir.path(), &size, 10).contains("\nnot downloaded: 2 placeholders, "),
            "{}",
            folder_size(dir.path(), &size, 10)
        );

        let (left, right) = (dir.path().join("left"), dir.path().join("right"));
        for (side, name, text) in [
            (&left, "only.txt", "a"),
            (&left, "sizes.txt", "a"),
            (&right, "sizes.txt", "abc"),
            (&left, "clash", "a"),
        ] {
            std::fs::create_dir_all(side).unwrap();
            std::fs::write(side.join(name), text).unwrap();
        }
        std::fs::create_dir(right.join("clash")).unwrap();
        let mut comparison =
            compare::compare(&left, &right, &compare::Progress::default(), None).unwrap();
        for entry in &mut comparison.entries {
            match entry.path.to_str() {
                Some("only.txt" | "sizes.txt") => &mut entry.left,
                _ => &mut entry.right,
            }
            .as_mut()
            .unwrap()
            .availability = cloud::Availability::Placeholder;
        }
        let shown = compared(&comparison, 10, None, 10);
        for line in [
            "type conflict: clash (file on the left, folder on the right) (right not downloaded)",
            "only left: only.txt (left not downloaded)",
            "differs: sizes.txt (left not downloaded)",
        ] {
            assert!(shown.lines().any(|shown| shown == line), "{shown}");
        }

        let mut server = server();
        let reply = call(
            &mut server,
            json!({"name": "download", "arguments": {"paths": [file]}}),
        );
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(
            text(&reply),
            format!("{}: already downloaded\n", file.display())
        );
        let reply = call(
            &mut server,
            json!({"name": "remove_download", "arguments": {"paths": [file]}}),
        );
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert_eq!(
            text(&reply),
            format!(
                "{} isn't in iCloud Drive or a File Provider folder.\n",
                file.display()
            )
        );
        assert!(file.is_file());
    }

    #[test]
    fn unknown_tools_are_protocol_errors() {
        let reply = call(&mut server(), json!({"name": "rm_rf", "arguments": {}}));
        assert_eq!(reply["error"]["code"], -32602);
    }

    #[test]
    fn change_tools_print_resulting_paths_and_share_undo_with_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ops::Paths {
            state: dir.path().join("state"),
            settings: dir.path().join("settings.json"),
            trash: dir.path().join("Trash"),
        };
        let owner = ops::Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut server = Server::new(
            ops::Client::new(paths, None),
            places(dir.path()),
            catalog_store(dir.path()),
        );
        let (made, renamed) = (dir.path().join("made"), dir.path().join("renamed"));
        let shown = |path: &std::path::Path| format!("{}\n", path.display());
        let reply = call(
            &mut server,
            json!({"name": "create_folder", "arguments": {"path": made}}),
        );
        assert_eq!(text(&reply), shown(&made));
        let reply = call(
            &mut server,
            json!({"name": "rename", "arguments": {"path": made, "name": "renamed"}}),
        );
        assert_eq!(text(&reply), shown(&renamed));
        let reply = call(&mut server, json!({"name": "undo"}));
        assert_eq!(text(&reply), format!("Undo Rename\n{}", shown(&made)));
        assert!(made.is_dir() && !renamed.exists());

        // Arguments that don't decode are protocol errors; a relative path
        // is the owner's to refuse.
        for bad in [
            json!({"name": "rename", "arguments": {"path": made}}),
            json!({"name": "trash", "arguments": {"paths": made}}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }
        let reply = call(
            &mut server,
            json!({"name": "create_file", "arguments": {"path": "notes.md"}}),
        );
        assert_eq!(reply["result"]["isError"], true);
        assert!(text(&reply).contains("absolute"), "{reply}");

        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"agents_change_files": false}"#,
        )
        .unwrap();
        let reply = call(
            &mut server,
            json!({"name": "create_file", "arguments": {"path": dir.path().join("off.md")}}),
        );
        assert_eq!(reply["result"]["isError"], true);
        assert!(text(&reply).starts_with("Agents can't"), "{reply}");
        let reply = call(
            &mut server,
            json!({"name": "eject", "arguments": {"path": "/"}}),
        );
        assert!(text(&reply).starts_with("Agents can't eject"), "{reply}");
        let reply = call(
            &mut server,
            json!({"name": "download", "arguments": {"paths": [&made, &renamed]}}),
        );
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(text(&reply).lines().count(), 1, "{reply}");
        assert!(text(&reply).starts_with("Agents can't download"), "{reply}");
        drop(server);
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn rename_preview_prints_the_table_and_rename_many_runs_only_what_it_shows() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ops::Paths {
            state: dir.path().join("state"),
            settings: dir.path().join("settings.json"),
            trash: dir.path().join("Trash"),
        };
        let owner = ops::Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut server = Server::new(
            ops::Client::new(paths, None),
            places(dir.path()),
            catalog_store(dir.path()),
        );
        let files = dir.path().join("files");
        std::fs::create_dir(&files).unwrap();
        for name in ["IMG_1.jpg", "IMG_2.jpg", "Trip 2.jpg", "x"] {
            std::fs::write(files.join(name), "").unwrap();
        }
        let batch = ["IMG_1.jpg", "IMG_2.jpg", "x"].map(|name| files.join(name));
        let rule = json!({"type": "format", "name": "Trip"});
        let reply = call(
            &mut server,
            json!({"name": "rename_preview", "arguments": {"paths": batch, "rule": rule}}),
        );
        let shown = |name: &str| files.join(name).display().to_string();
        assert_eq!(
            text(&reply),
            format!(
                "{} → Trip 1.jpg\n{} → Trip 2.jpg (collision: Trip 2.jpg already exists.)\n{} → Trip 3\n",
                shown("IMG_1.jpg"),
                shown("IMG_2.jpg"),
                shown("x"),
            )
        );
        let reply = call(
            &mut server,
            json!({"name": "rename_many", "arguments": {"paths": batch, "rule": rule}}),
        );
        assert_eq!(reply["result"]["isError"], true);
        assert!(
            text(&reply).starts_with("Nothing was renamed:\n"),
            "{reply}"
        );
        let reply = call(
            &mut server,
            json!({"name": "rename_many", "arguments": {"paths": batch, "rule": rule, "skip_problems": true}}),
        );
        assert_eq!(
            text(&reply),
            format!("{}\n{}\n", shown("Trip 1.jpg"), shown("Trip 3"))
        );
        let reply = call(&mut server, json!({"name": "undo"}));
        assert!(text(&reply).starts_with("Undo Batch Rename\n"), "{reply}");
        assert!(batch.iter().all(|path| path.exists()));

        for bad in [
            json!({"type": "regex", "find": "x"}),
            json!({"type": "replace"}),
            json!("Trip"),
        ] {
            let reply = call(
                &mut server,
                json!({"name": "rename_preview", "arguments": {"paths": batch, "rule": bad}}),
            );
            assert_eq!(reply["error"]["code"], -32602, "{reply}");
        }
        drop(server);
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn copy_returns_a_job_id_at_once_and_job_status_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ops::Paths {
            state: dir.path().join("state"),
            settings: dir.path().join("settings.json"),
            trash: dir.path().join("Trash"),
        };
        // An owner outlives its last client until its jobs are done, so a
        // fresh one sees each job as it ended.
        let serve = || {
            let owner = ops::Owner::bind(paths.clone()).unwrap();
            let serving = std::thread::spawn(move || owner.serve());
            (
                serving,
                Server::new(
                    ops::Client::new(paths.clone(), None),
                    places(dir.path()),
                    catalog_store(dir.path()),
                ),
            )
        };
        let (serving, mut server) = serve();
        let (file, into) = (dir.path().join("a.txt"), dir.path().join("into"));
        std::fs::write(&file, "a").unwrap();
        std::fs::create_dir(&into).unwrap();
        let reply = call(
            &mut server,
            json!({"name": "copy", "arguments": {"paths": [file], "into": into}}),
        );
        assert_eq!(text(&reply), "job 1 queued\n");
        let reply = call(
            &mut server,
            json!({"name": "job_status", "arguments": {"job": 1}}),
        );
        let first = format!("job 1 Copy into {}: ", into.display());
        assert!(text(&reply).starts_with(&first), "{reply}");
        for bad in [
            json!({"name": "copy", "arguments": {"paths": [file], "into": into, "conflict": "merge"}}),
            json!({"name": "cancel_job", "arguments": {}}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }
        drop(server);
        serving.join().unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(into.join("a.txt")).unwrap(), "a");

        // Copied again, it waits at the name; continuing keeps its id.
        let (serving, mut server) = serve();
        let reply = call(
            &mut server,
            json!({"name": "copy", "arguments": {"paths": [file], "into": into}}),
        );
        assert_eq!(text(&reply), "job 2 queued\n");
        drop(server);
        serving.join().unwrap().unwrap();
        let (serving, mut server) = serve();
        let status = |server: &mut Server| {
            let reply = call(
                server,
                json!({"name": "job_status", "arguments": {"job": 2}}),
            );
            text(&reply).lines().next().unwrap_or_default().to_string()
        };
        assert!(status(&mut server).contains(": waiting, 1 of 1 items"));
        let reply = call(
            &mut server,
            json!({"name": "continue_job", "arguments": {"job": 2, "conflict": "keep_both"}}),
        );
        assert_eq!(text(&reply), "job 2 queued again\n");
        drop(server);
        serving.join().unwrap().unwrap();
        let (serving, mut server) = serve();
        assert!(status(&mut server).contains(": completed, 1 of 1 items"));
        let reply = call(
            &mut server,
            json!({"name": "continue_job", "arguments": {"job": 2}}),
        );
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(text(&reply), "Job 2 has nothing left to do.");
        drop(server);
        serving.join().unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(into.join("a 2.txt")).unwrap(), "a");
    }

    #[test]
    fn archive_tools_list_without_extracting_and_create_and_extract_as_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ops::Paths {
            state: dir.path().join("state"),
            settings: dir.path().join("settings.json"),
            trash: dir.path().join("Trash"),
        };
        // Each owner runs its jobs to the end before it exits.
        let serve = || {
            let owner = ops::Owner::bind(paths.clone()).unwrap();
            let serving = std::thread::spawn(move || owner.serve());
            (
                serving,
                Server::new(
                    ops::Client::new(paths.clone(), None),
                    places(dir.path()),
                    catalog_store(dir.path()),
                ),
            )
        };
        let (proj, out) = (dir.path().join("proj"), dir.path().join("out"));
        std::fs::create_dir_all(proj.join("docs")).unwrap();
        std::fs::create_dir(&out).unwrap();
        std::fs::write(proj.join("docs/a.txt"), "hello").unwrap();
        std::fs::write(proj.join("docs/b.txt"), "").unwrap();
        let (serving, mut server) = serve();
        let reply = call(
            &mut server,
            json!({"name": "archive_create", "arguments": {"paths": [proj]}}),
        );
        assert_eq!(text(&reply), "job 1 queued\n");
        drop(server);
        serving.join().unwrap().unwrap();

        let (serving, mut server) = serve();
        let archive = dir.path().join("proj.zip");
        let reply = call(
            &mut server,
            json!({"name": "job_status", "arguments": {"job": 1}}),
        );
        assert!(
            text(&reply).ends_with(&format!(
                ": completed, 1 of 1 items, 5 of 5 bytes, by an agent\n{}\n",
                archive.display()
            )),
            "{reply}"
        );
        let reply = call(
            &mut server,
            json!({"name": "archive_list", "arguments": {"path": archive, "folder": "proj/docs", "limit": 1}}),
        );
        let lines: Vec<&str> = text(&reply).lines().collect();
        assert!(
            lines[0].starts_with("proj/docs/a.txt  5 bytes, "),
            "{lines:?}"
        );
        assert!(lines[0].ends_with('Z'), "{lines:?}");
        assert_eq!(lines[1..], ["(1 more; pass folder or a larger limit)"]);
        for bad in [
            json!({"name": "archive_extract", "arguments": {"path": archive}}),
            json!({"name": "archive_extract", "arguments": {"path": archive, "into": out, "conflict": "merge"}}),
            json!({"name": "archive_list", "arguments": {}}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }
        let extract =
            json!({"name": "archive_extract", "arguments": {"path": archive, "into": out}});
        assert_eq!(text(&call(&mut server, extract.clone())), "job 2 queued\n");
        drop(server);
        serving.join().unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("proj/docs/a.txt")).unwrap(),
            "hello"
        );

        // Extracted again, it waits at the name, as conflict defaults to stop.
        let (serving, mut server) = serve();
        assert_eq!(text(&call(&mut server, extract)), "job 3 queued\n");
        drop(server);
        serving.join().unwrap().unwrap();
        let (serving, mut server) = serve();
        let reply = call(
            &mut server,
            json!({"name": "job_status", "arguments": {"job": 3}}),
        );
        assert!(text(&reply).contains(": waiting, 1 of 1 items"), "{reply}");
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"agents_change_files": false}"#,
        )
        .unwrap();
        let reply = call(
            &mut server,
            json!({"name": "archive_create", "arguments": {"paths": [proj]}}),
        );
        assert!(text(&reply).starts_with("Agents can't"), "{reply}");
        drop(server);
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn place_tools_pin_list_and_unpin_favorites_and_eject_refuses_without_ejecting() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let (work, gone) = (real.join("Work"), real.join("Gone"));
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&gone).unwrap();
        let mut server = server();
        server.places = places(dir.path());
        for (path, at) in [(&gone, json!(null)), (&work, json!(0))] {
            let reply = call(
                &mut server,
                json!({"name": "pin", "arguments": {"path": path, "at": at}}),
            );
            assert_eq!(text(&reply), format!("{}\n", path.display()));
        }
        std::fs::rename(&gone, real.join("Moved")).unwrap();
        let reply = call(&mut server, json!({"name": "list_places"}));
        assert_eq!(reply["result"]["isError"], false);
        let lines: Vec<&str> = text(&reply).lines().collect();
        let gone_line = format!("{0} (unavailable: {0} no longer exists.)", gone.display());
        assert_eq!(lines[..2], ["Favorites", &work.display().to_string()]);
        assert_eq!(lines[7], gone_line);
        assert_eq!(lines[8], "Volumes");
        assert!(lines[9].starts_with("/ ("), "{}", lines[9]);
        assert!(lines[9].contains(" free of ") && lines[9].contains("startup disk, uuid "));

        let reply = call(
            &mut server,
            json!({"name": "unpin", "arguments": {"path": gone}}),
        );
        assert_eq!(
            text(&reply),
            format!("Unpinned {}; the folder is untouched.\n", gone.display())
        );
        for bad in [
            json!({"name": "pin", "arguments": {"path": work, "at": -1}}),
            json!({"name": "eject", "arguments": {}}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }
        for (path, refusal) in [
            (dir.path().to_path_buf(), " isn't a mounted volume."),
            ("/".into(), " is the startup disk."),
        ] {
            let reply = call(
                &mut server,
                json!({"name": "eject", "arguments": {"path": path}}),
            );
            assert_eq!(reply["result"]["isError"], true);
            assert!(text(&reply).ends_with(refusal), "{reply}");
        }
    }

    #[test]
    fn compare_diff_and_compare_copy_preview_then_copy_only_on_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = ops::Paths {
            state: root.join("state"),
            settings: root.join("settings.json"),
            trash: root.join("Trash"),
        };
        let owner = ops::Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut server = Server::new(
            ops::Client::new(paths, None),
            places(&root),
            catalog_store(&root),
        );
        let (left, right) = (root.join("left"), root.join("right"));
        let then = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (path, text, at) in [
            (left.join("a.txt"), "one\nnew\n", 3600),
            (left.join("same.txt"), "same", 0),
            (left.join("docs/only.md"), "only", 0),
            (left.join("docs/new.md"), "new", 0),
            (right.join("a.txt"), "one\nold\n", 0),
            (right.join("same.txt"), "same", 0),
            (right.join("docs/extra.md"), "extra", 0),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(then + Duration::from_secs(at))
                .unwrap();
        }
        let both = json!({"left": left, "right": right});
        let reply = call(&mut server, json!({"name": "compare", "arguments": both}));
        let roots = format!("left: {}\nright: {}\n", left.display(), right.display());
        assert_eq!(
            text(&reply),
            roots.clone()
                + "2 only left, 1 only right, 1 dates differ, 1 same size and date\n\
                   dates differ: a.txt (left newer)\n\
                   only right: docs/extra.md\n\
                   only left: docs/new.md\n\
                   only left: docs/only.md\n\
                   (Same size and date doesn't prove equal contents; verify: true reads them.)\n"
        );
        let mut verified = both.clone();
        verified["verify"] = json!(true);
        verified["limit"] = json!(1);
        let reply = call(
            &mut server,
            json!({"name": "compare", "arguments": verified}),
        );
        assert_eq!(
            text(&reply),
            roots
                + "2 only left, 1 only right, 1 differs, 1 equal\n\
                   differs: a.txt (left newer)\n\
                   (3 more; pass a larger limit)\n"
        );

        let reply = call(
            &mut server,
            json!({"name": "diff", "arguments": {"left": right.join("a.txt"), "right": left.join("a.txt")}}),
        );
        assert_eq!(
            text(&reply),
            format!(
                "--- {}\n+++ {}\n@@ -1,2 +1,2 @@\n one\n-old\n+new\n",
                right.join("a.txt").display(),
                left.join("a.txt").display()
            )
        );
        let reply = call(
            &mut server,
            json!({"name": "diff", "arguments": {"left": left.join("same.txt"), "right": right.join("same.txt")}}),
        );
        assert_eq!(text(&reply), "No differences in the text.\n");

        let token = |reply: &Value| {
            text(reply)
                .lines()
                .find_map(|line| line.strip_prefix("plan: "))
                .unwrap_or_default()
                .to_string()
        };
        let reply = call(
            &mut server,
            json!({"name": "compare_copy", "arguments": both}),
        );
        let shown = |path: &str| {
            (
                left.join(path).display().to_string(),
                right.join(path).display().to_string(),
            )
        };
        let ((la, ra), (ln, rn), (lo, ro)) =
            (shown("a.txt"), shown("docs/new.md"), shown("docs/only.md"));
        assert_eq!(
            text(&reply),
            format!(
                "from: {}\ninto: {}\n\
                 replace: {la} → {ra}\n\
                 skip: docs/extra.md (Only on the right; Monodrive never deletes it.)\n\
                 copy: {ln} → {rn}\n\
                 copy: {lo} → {ro}\n\
                 2 to copy, 1 to replace, 1 skipped. What's replaced goes to the Trash; nothing is deleted.\n\
                 plan: {}\n\
                 Pass confirm: true and this plan within 10 minutes to copy it.\n",
                left.display(),
                right.display(),
                token(&reply),
            )
        );
        assert!(!right.join("docs/only.md").exists());

        let mut docs = both.clone();
        docs["paths"] = json!(["docs/"]);
        let reply = call(
            &mut server,
            json!({"name": "compare_copy", "arguments": docs}),
        );
        assert!(
            text(&reply).contains(&format!("copy: {lo} → {ro}\n")),
            "{reply}"
        );
        // Appearing after the preview, older and another size: a plan made
        // again now would replace it.
        std::fs::write(right.join("docs/only.md"), "stale").unwrap();
        std::fs::File::options()
            .write(true)
            .open(right.join("docs/only.md"))
            .unwrap()
            .set_modified(then - Duration::from_secs(3600))
            .unwrap();
        let mut confirmed = docs.clone();
        confirmed["confirm"] = json!(true);
        confirmed["plan"] = json!(token(&reply));
        let settings = root.join("settings.json");
        std::fs::write(&settings, r#"{"agents_change_files": false}"#).unwrap();
        let reply = call(
            &mut server,
            json!({"name": "compare_copy", "arguments": confirmed}),
        );
        assert_eq!(reply["result"]["isError"], true);
        assert!(text(&reply).ends_with("Agents can't change files: Let agents change files is off in Monodrive's Agents settings."), "{reply}");
        std::fs::write(&settings, r#"{"agents_change_files": true}"#).unwrap();
        let reply = call(
            &mut server,
            json!({"name": "compare_copy", "arguments": confirmed}),
        );
        assert!(text(&reply).ends_with("2 to copy, 0 to replace, 1 skipped. What's replaced goes to the Trash; nothing is deleted.\njob 1 queued\n"), "{reply}");
        let mut unknown = confirmed.clone();
        unknown["plan"] = json!("0");
        for refused in [confirmed, unknown] {
            let reply = call(
                &mut server,
                json!({"name": "compare_copy", "arguments": refused}),
            );
            assert_eq!(reply["result"]["isError"], true);
            assert!(text(&reply).starts_with("No previewed plan"), "{reply}");
        }
        for bad in [
            json!({"name": "compare", "arguments": {"left": left}}),
            json!({"name": "compare_copy", "arguments": {"left": left, "right": right, "confirm": "yes"}}),
            json!({"name": "diff", "arguments": {"left": left, "right": right, "context": -1}}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }
        drop(server);
        serving.join().unwrap().unwrap();
        let read = |path: &str| std::fs::read_to_string(right.join(path)).unwrap();
        assert_eq!(read("docs/new.md"), "new");
        assert_eq!(read("docs/only.md"), "stale");
        assert_eq!(read("a.txt"), "one\nold\n");
        assert!(right.join("docs/extra.md").exists());
    }

    #[test]
    fn storage_tools_scan_find_duplicates_and_trash_chosen_copies_only_on_confirm() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = ops::Paths {
            state: root.join("state"),
            settings: root.join("settings.json"),
            trash: root.join("Trash"),
        };
        let owner = ops::Owner::bind(paths.clone()).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let mut server = Server::new(
            ops::Client::new(paths, None),
            places(&root),
            catalog_store(&root),
        );
        let photos = root.join("photos");
        let photo: Vec<u8> = (0..300_000u32).map(|n| (n % 253) as u8).collect();
        let mut other = photo.clone();
        other[100_000] ^= 1;
        let (a, b, c) = (
            photos.join("a/photo.jpg"),
            photos.join("b/photo.jpg"),
            photos.join("other.jpg"),
        );
        for (path, bytes) in [(&a, &photo), (&b, &photo), (&c, &other)] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let on_disk = |path: &Path| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path).unwrap().blocks() * 512
        };
        let line = |path: &Path| {
            format!(
                "{}  300000 bytes, {} on disk",
                path.display(),
                on_disk(path)
            )
        };

        let reply = call(
            &mut server,
            json!({"name": "storage_scan", "arguments": {"path": photos, "largest": 1}}),
        );
        let lines: Vec<&str> = text(&reply).lines().collect();
        assert_eq!(lines[0], format!("{}/", photos.display()));
        assert!(lines[1].starts_with("size: 900000 bytes ("), "{lines:?}");
        assert_eq!(lines[2..5], ["files: 3", "folders: 2", "largest files:"]);
        // All take as much on disk, so the first met comes first.
        assert_eq!(lines[5], line(&c));
        assert_eq!(lines[6], "folders, most on disk first:");
        let folder = |name: &str| {
            let path = photos.join(name);
            format!(
                "{}/  300000 bytes, {} on disk",
                path.display(),
                on_disk(&path.join("photo.jpg"))
            )
        };
        assert_eq!(lines[7..], [folder("a"), folder("b")]);

        let reply = call(
            &mut server,
            json!({"name": "duplicates", "arguments": {"path": photos}}),
        );
        assert_eq!(
            text(&reply),
            format!(
                "{}/\n1 verified, 0 candidates; 300000 bytes in extra verified copies\n\
                 verified: 2 copies of 300000 bytes\n{}\n{}\n",
                photos.display(),
                a.display(),
                b.display()
            )
        );

        let preview = |server: &mut Server, trash: &[&PathBuf]| {
            call(
                server,
                json!({"name": "cleanup_duplicates", "arguments": {"path": photos, "trash": trash}}),
            )
        };
        let refused = preview(&mut server, &[&a, &b]);
        assert_eq!(refused["result"]["isError"], true);
        assert!(
            text(&refused).starts_with("Keep at least one of "),
            "{refused}"
        );
        let refused = preview(&mut server, &[&c]);
        assert!(
            text(&refused).ends_with("not a verified duplicate in this scan."),
            "{refused}"
        );
        let reply = preview(&mut server, &[&b]);
        let token = text(&reply)
            .lines()
            .find_map(|line| line.strip_prefix("plan: "))
            .unwrap_or_default()
            .to_string();
        let plan = format!(
            "keep: {}\ntrash: {}\n1 to trash, 1 kept; frees at least {} bytes once the Trash is emptied. Undo puts them back.\n",
            a.display(),
            b.display(),
            on_disk(&b)
        );
        assert_eq!(
            text(&reply),
            format!(
                "{plan}plan: {token}\nPass confirm: true and this plan within 10 minutes to trash them.\n"
            )
        );

        let confirmed =
            json!({"name": "cleanup_duplicates", "arguments": {"confirm": true, "plan": token}});
        let settings = root.join("settings.json");
        std::fs::write(&settings, r#"{"agents_change_files": false}"#).unwrap();
        let reply = call(&mut server, confirmed.clone());
        assert!(
            text(&reply).starts_with("Agents can't change files"),
            "{reply}"
        );
        std::fs::write(&settings, r#"{"agents_change_files": true}"#).unwrap();
        let reply = call(&mut server, confirmed.clone());
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let trashed = text(&reply).strip_prefix(&plan).unwrap();
        assert!(
            trashed.starts_with(&format!("trashed: {} → ", b.display())),
            "{reply}"
        );
        assert!(!b.exists() && a.exists());
        let reply = call(&mut server, confirmed);
        assert!(text(&reply).starts_with("No previewed plan"), "{reply}");
        let reply = call(&mut server, json!({"name": "undo"}));
        assert_eq!(
            text(&reply),
            format!("Undo Trash Duplicates\n{}\n", b.display())
        );
        assert_eq!(std::fs::read(&b).unwrap(), photo);

        for bad in [
            json!({"name": "storage_scan", "arguments": {}}),
            json!({"name": "duplicates", "arguments": {"path": photos, "min_size": 0.5}}),
            json!({"name": "cleanup_duplicates", "arguments": {"path": photos, "trash": b}}),
            json!({"name": "cleanup_duplicates", "arguments": {"confirm": "yes"}}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }
        drop(server);
        serving.join().unwrap().unwrap();
    }

    #[test]
    fn diff_shows_a_missing_last_newline_and_line_endings_that_differ() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = server();
        let mut diff = |old: &str, new: &str| {
            let (left, right) = (dir.path().join("old"), dir.path().join("new"));
            std::fs::write(&left, old).unwrap();
            std::fs::write(&right, new).unwrap();
            let reply = call(
                &mut server,
                json!({"name": "diff", "arguments": {"left": left, "right": right}}),
            );
            let head = format!("--- {}\n+++ {}\n", left.display(), right.display());
            text(&reply).strip_prefix(&head).unwrap().to_string()
        };
        assert_eq!(
            diff("foo\n", "foo"),
            "@@ -1,1 +1,1 @@\n-foo\n+foo\n\\ No newline at end of file\n"
        );
        assert_eq!(
            diff("a\r\nb\n", "a\nb\n"),
            "@@ -1,2 +1,2 @@\n-a␍\n+a\n b\n(␍ marks a line that ends in CRLF.)\n"
        );
        assert_eq!(
            diff("foo\r\r\n", "foo\r\n"),
            "@@ -1,1 +1,1 @@\n-foo\r\n+foo\n"
        );
    }

    #[test]
    fn a_bad_filter_is_a_tool_error_that_never_starts_the_index() {
        let mut server = server();
        for (call_args, error) in [
            (
                json!({"name": "find_files", "arguments": {"query": "cat kind:bogus"}}),
                "kind:bogus: kinds are image, video",
            ),
            (
                json!({"name": "grep", "arguments": {"pattern": "TODO", "filters": "size:big"}}),
                "size:big: give a size",
            ),
        ] {
            let reply = call(&mut server, call_args);
            assert_eq!(reply["result"]["isError"], true);
            assert!(text(&reply).starts_with(error), "{reply}");
        }
        assert!(server.index.is_none());
    }

    #[test]
    fn search_tools_save_list_rename_and_remove_without_searching() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = server();
        server.places = places(dir.path());
        let home = std::env::var("HOME").unwrap();
        let reply = call(&mut server, json!({"name": "list_searches"}));
        assert_eq!(text(&reply), "No saved searches.\n");
        for (args, printed) in [
            (
                json!({"name": "Photos", "query": "kind:image  size:>1MB", "path": "~/Pictures"}),
                format!("Photos: kind:image size:>1mb (in {home}/Pictures)\n"),
            ),
            (
                json!({"name": "Recent", "query": "modified:<7d", "at": 0}),
                "Recent: modified:7d\n".into(),
            ),
        ] {
            let reply = call(
                &mut server,
                json!({"name": "save_search", "arguments": args}),
            );
            assert_eq!(text(&reply), printed);
        }
        let reply = call(
            &mut server,
            json!({"name": "rename_search", "arguments": {"name": "Recent", "to": "This week"}}),
        );
        assert_eq!(text(&reply), "Renamed Recent to This week.\n");
        let reply = call(&mut server, json!({"name": "list_searches"}));
        assert_eq!(
            text(&reply),
            format!("This week: modified:7d\nPhotos: kind:image size:>1mb (in {home}/Pictures)\n")
        );
        let reply = call(
            &mut server,
            json!({"name": "remove_search", "arguments": {"name": "Photos"}}),
        );
        assert_eq!(text(&reply), "Removed the saved search Photos.\n");
        let reply = call(
            &mut server,
            json!({"name": "save_search", "arguments": {"name": "Bad", "query": "in:~/a in:~/b"}}),
        );
        assert_eq!(reply["result"]["isError"], true);
        let reply = call(
            &mut server,
            json!({"name": "remove_search", "arguments": {"name": "Photos"}}),
        );
        assert_eq!(text(&reply), "Photos isn't a saved search.");
        assert!(server.index.is_none());
    }

    #[test]
    fn a_jobs_status_lists_where_each_finished_item_went_or_why_not() {
        let job = Job {
            id: 7,
            title: "Move".into(),
            origin: Origin::Agent {
                client: Some("claude-code".into()),
            },
            into: "/dest".into(),
            state: ops::State::Waiting,
            tasks: vec![
                ops::Task {
                    source: "/src/a".into(),
                    done: Some(Done::To("/dest/a".into())),
                },
                ops::Task {
                    source: "/src/b".into(),
                    done: Some(Done::Failed("/dest/b already exists.".into())),
                },
                ops::Task {
                    source: "/src/c".into(),
                    done: None,
                },
            ],
            bytes: 10,
            total_bytes: 30,
            started: 0,
            run: 7,
            undoable: false,
        };
        assert_eq!(
            describe(&job),
            "job 7 Move into /dest: waiting, 2 of 3 items, 10 of 30 bytes, by agent claude-code\n\
             /dest/a\n/dest/b already exists.\n"
        );
    }

    #[test]
    fn catalog_tools_search_an_offline_catalog_with_its_age_and_forget_only_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let drive = root.join("Drive");
        std::fs::create_dir_all(drive.join("Photos")).unwrap();
        std::fs::write(drive.join("Photos/report.pdf"), "x").unwrap();
        // No mounted volume has this identity, so it's offline here.
        let unplugged = Volume {
            name: "Drive".into(),
            mount: drive.clone(),
            disk: None,
            id: Some(VolumeId::Uuid("MONODRIVE-TEST-DRIVE".into())),
            total: 0,
            free: 0,
            read_only: false,
            network: false,
            ejectable: true,
            removable: true,
            startup: false,
        };
        let store = catalog_store(&root);
        let made = store
            .create(&drive, &[unplugged], &storage::Progress::default(), None)
            .unwrap();
        let mut server = server();
        server.catalogs = store.clone();
        let id = &made.id;

        let reply = call(&mut server, json!({"name": "catalog_list"}));
        assert_eq!(
            text(&reply),
            format!(
                "{} (catalog {id}, Drive, offline, refreshed just now, 2 entries, 1 KB on disk)\n",
                drive.display()
            )
        );
        let search = json!({"name": "catalog_search", "arguments": {"query": "report"}});
        let reply = call(&mut server, search);
        let report = drive.join("Photos/report.pdf");
        assert_eq!(
            text(&reply),
            format!(
                "Drive (catalog {id}): cataloged just now, offline: Drive isn't connected.\n{}\n",
                report.display()
            )
        );
        let found = store
            .search(&Query::parse("photos").unwrap(), &[], 50)
            .unwrap();
        let later = i64::try_from(made.refreshed).unwrap() + 3 * 86_400;
        assert_eq!(
            catalog_hits(&found, &[], later),
            format!(
                "Drive (catalog {id}): cataloged 3 days ago, offline: Drive isn't connected.\n{}/\n",
                drive.join("Photos").display()
            )
        );

        let refresh = json!({"name": "catalog_refresh", "arguments": {"catalog": id}});
        let reply = call(&mut server, refresh);
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(
            text(&reply),
            "Drive isn't connected. The last good catalog is kept."
        );
        let reply = call(&mut server, json!({"name": "catalog_list"}));
        assert!(
            text(&reply).ends_with(
                "\n  last refresh failed just now: Drive isn't connected. The entries are from the refresh before.\n"
            ),
            "{}",
            text(&reply)
        );
        // A temporary folder is on the startup disk.
        let create = json!({"name": "catalog_create", "arguments": {"path": root}});
        let reply = call(&mut server, create);
        assert!(text(&reply).contains("is on the startup disk"), "{reply}");
        for bad in [
            json!({"name": "catalog_search", "arguments": {}}),
            json!({"name": "catalog_refresh", "arguments": {"catalog": id, "timeout_seconds": -1}}),
            json!({"name": "catalog_forget"}),
        ] {
            assert_eq!(call(&mut server, bad)["error"]["code"], -32602);
        }

        let forget = json!({"name": "catalog_forget", "arguments": {"catalog": id}});
        let reply = call(&mut server, forget.clone());
        assert_eq!(
            text(&reply),
            format!("Forgot catalog {id}. Only Monodrive's catalog file went; no files changed.\n")
        );
        assert!(report.is_file());
        let reply = call(&mut server, json!({"name": "catalog_list"}));
        assert_eq!(text(&reply), "No catalogs.\n");
        let reply = call(&mut server, forget);
        assert_eq!(text(&reply), format!("No catalog {id}."));
    }
}
