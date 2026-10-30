//! `monodrive mcp`: a Model Context Protocol server on stdin/stdout, one
//! JSON-RPC message per line. Tools only decode arguments and call one
//! service method; the behavior lives in `entries`, `index` and `ops`.

use crate::entries::{self, Kind};
use crate::expand_home;
use crate::index::{self, GrepMode, Index, Status};
use crate::ops::{self, Conflict, Done, Job, Origin, Request};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Newest first. A client asking for another version gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Monodrive: an always-current index of this Mac's files, \
much faster than Spotlight or walking folders with find. Use find_files to locate files \
by name, grep to search inside files, and list_dir to see one folder. index_status says \
whether the index is ready. trash, put_back, rename, create_folder, create_folder_with, \
create_file, duplicate, undo and redo change files through the same undo history as the Monodrive window.";

pub fn serve_stdio() -> std::io::Result<()> {
    let paths = ops::Paths::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let mut server = Server::new(ops::Client::new(paths, std::env::current_exe().ok()));
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
}

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
}

fn folders_first() -> bool {
    true
}

fn default_limit() -> usize {
    50
}

impl Server {
    pub fn new(ops: ops::Client) -> Self {
        Self {
            index: None,
            ops,
            client: None,
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
            ToolCall::FindFiles { query, limit } => self
                .index()
                .and_then(|index| index.find(&query, limit))
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
                limit,
            } => {
                let within = path.as_deref().map(expand_home);
                self.index()
                    .and_then(|index| index.grep(&pattern, mode, within.as_deref(), limit))
                    .map(|found| {
                        let mut out = String::new();
                        for file in &found.files {
                            for m in &file.lines {
                                let _ =
                                    writeln!(out, "{}:{}: {}", file.path.display(), m.line, m.text);
                            }
                        }
                        if !found.complete {
                            out.push_str("(search stopped early; narrow it with path)\n");
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
                            let _ = writeln!(out, "{}{}", e.name, suffix(e.kind));
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

pub const TOOLS: [Tool; 18] = [
    Tool {
        name: "find_files",
        description: "Find files and folders by name anywhere on disk. Words are fuzzy and typo-tolerant. \
    Filters go inline: ext:rs, in:~/Work, type:image, size:>5mb, mtime:<7d, src/**/*.ts, !node_modules/. \
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
    regex, or symbol (where a name is defined). Pass path to search one folder.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "mode": {"type": "string", "enum": ["literal", "regex", "symbol"], "default": "literal"},
                    "path": {"type": "string", "description": "Folder to search in; ~ is your home."},
                    "limit": {"type": "integer", "minimum": 1, "default": 50, "description": "Most files to return."}
                },
                "required": ["pattern"]
            })
        },
    },
    Tool {
        name: "list_dir",
        description: "List one folder. Folders end in /, links in @. Names sort as people read \
    them (file2 before file10). For the newest or largest first, sort by modified or size with descending: true.",
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
        name: "job_status",
        description: "A copy or move job's state and progress, then each finished item's resulting \
    path or error. Without job, one line per recent job.",
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
        Server::new(ops::Client::new(paths, None))
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
        let mut server = Server::new(ops::Client::new(paths, None));
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
            (serving, Server::new(ops::Client::new(paths.clone(), None)))
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
        };
        assert_eq!(
            describe(&job),
            "job 7 Move into /dest: waiting, 2 of 3 items, 10 of 30 bytes, by agent claude-code\n\
             /dest/a\n/dest/b already exists.\n"
        );
    }
}
