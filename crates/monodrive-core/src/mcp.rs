//! `monodrive mcp`: a Model Context Protocol server on stdin/stdout, one
//! JSON-RPC message per line. Tools only decode arguments and call one
//! service method; the behavior lives in `entries` and `index`.

use crate::entries::{self, Kind};
use crate::expand_home;
use crate::index::{self, GrepMode, Index, Status};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::{BufRead, Write};

/// Newest first. A client asking for another version gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "Monodrive: an always-current index of this Mac's files, \
much faster than Spotlight or walking folders with find. Use find_files to locate files \
by name, grep to search inside files, and list_dir to see one folder. index_status says \
whether the index is ready.";

pub fn serve_stdio() -> std::io::Result<()> {
    let mut server = Server::default();
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

#[derive(Default)]
pub struct Server {
    /// Started on the first tool that needs it, so connecting an agent
    /// never kicks off a disk crawl by itself.
    index: Option<Result<Index, String>>,
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
}

fn folders_first() -> bool {
    true
}

fn default_limit() -> usize {
    50
}

impl Server {
    /// Handles one message and returns the reply. Notifications and
    /// responses get none.
    pub fn handle(&mut self, message: &Value) -> Option<Value> {
        let method = message.get("method")?.as_str()?;
        let id = message.get("id")?;
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
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
        };
        match text {
            Ok(text) if text.is_empty() => tool_text("No results.", false),
            Ok(text) => tool_text(&text, false),
            Err(e) => tool_text(&e, true),
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

fn tools() -> Value {
    json!([
        {
            "name": "find_files",
            "description": "Find files and folders by name anywhere on disk. Words are fuzzy and typo-tolerant. \
    Filters go inline: ext:rs, in:~/Work, type:image, size:>5mb, mtime:<7d, src/**/*.ts, !node_modules/. \
    Best matches first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "default": 50}
                },
                "required": ["query"]
            }
        },
        {
            "name": "grep",
            "description": "Search inside files with the content index. Smart case. mode is literal (default), \
    regex, or symbol (where a name is defined). Pass path to search one folder.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "mode": {"type": "string", "enum": ["literal", "regex", "symbol"], "default": "literal"},
                    "path": {"type": "string", "description": "Folder to search in; ~ is your home."},
                    "limit": {"type": "integer", "minimum": 1, "default": 50, "description": "Most files to return."}
                },
                "required": ["pattern"]
            }
        },
        {
            "name": "list_dir",
            "description": "List one folder. Folders end in /, links in @. Names sort as people read \
    them (file2 before file10). For the newest or largest first, sort by modified or size with descending: true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Folder to list; ~ is your home."},
                    "show_hidden": {"type": "boolean", "default": false},
                    "sort": {"type": "string", "enum": ["name", "modified", "size", "kind"], "default": "name"},
                    "descending": {"type": "boolean", "default": false},
                    "folders_first": {"type": "boolean", "default": true}
                },
                "required": ["path"]
            }
        },
        {
            "name": "index_status",
            "description": "Whether the index is ready, how far its first build has got, and whether it \
    can see the whole disk. Never starts indexing.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
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

    #[test]
    fn notifications_get_no_reply() {
        let mut server = Server::default();
        let reply =
            server.handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        assert_eq!(reply, None);
    }

    #[test]
    fn list_dir_lists_folders_first_without_starting_the_index() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.toml"), "").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        let mut server = Server::default();
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
        let mut server = Server::default();
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
        let reply = call(
            &mut Server::default(),
            json!({"name": "rm_rf", "arguments": {}}),
        );
        assert_eq!(reply["error"]["code"], -32602);
    }
}
