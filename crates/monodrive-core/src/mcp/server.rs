//! The protocol: JSON-RPC on stdin and stdout, one message a line.

use super::contract::{self, Profile};
use super::session::Session;
use super::tools;
use crate::ops;
use crate::places;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;

/// Newest first. A client asking for another version gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Short: Codex repeats it after every tool's description.
const INSTRUCTIONS: &str = "Monodrive: an always-current index of this Mac's files, faster than \
Spotlight or find. Paths are absolute or start with ~; a saved SSH server's are id:/path. Changes \
share the Monodrive window's undo history. A failed call ends with (error: code).";

/// Runs the server for this user until stdin closes. `profile` is the
/// command line's; without one, Settings' Agents page says.
pub fn serve_stdio(profile: Option<Profile>) -> std::io::Result<()> {
    let paths = ops::Paths::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let places = places::Store::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let profile = profile.unwrap_or(if saved(&paths.settings).agents_all_tools {
        Profile::All
    } else {
        Profile::Core
    });
    let settings = paths.settings.clone();
    let mut server = Server::new(
        ops::Client::new(paths, std::env::current_exe().ok()),
        places,
        profile,
    )
    .following_settings(settings);
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

/// What the app's saved settings say about a session's tools, by the
/// names `Preferences` gives them.
#[derive(Deserialize, Default)]
struct Saved {
    #[serde(default)]
    agents_all_tools: bool,
    /// LocalSend is on in Settings.
    #[serde(default)]
    localsend: bool,
}

fn saved(settings: &Path) -> Saved {
    std::fs::read(settings)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub struct Server {
    profile: Profile,
    /// What the tools share.
    pub(super) session: Session,
    /// Settings has LocalSend on, so its tools are offered.
    offers_localsend: bool,
    /// The app's saved settings, read again on each request for whether
    /// LocalSend is on, so turning it off reaches a session under way.
    settings: Option<std::path::PathBuf>,
}

impl Server {
    pub fn new(ops: ops::Client, places: places::Store, profile: Profile) -> Self {
        Self {
            profile,
            session: Session::new(ops, places),
            offers_localsend: false,
            settings: None,
        }
    }

    /// Follows the LocalSend switch in the app's saved `settings`.
    pub fn following_settings(mut self, settings: std::path::PathBuf) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Offers LocalSend's tools, as Settings does once it's on.
    pub fn offering_localsend(mut self, on: bool) -> Self {
        self.offers_localsend = on;
        self
    }

    fn localsend_on(&self) -> bool {
        match &self.settings {
            Some(settings) => saved(settings).localsend,
            None => self.offers_localsend,
        }
    }

    /// Whether this session offers `tool`: its profile does, and it isn't
    /// LocalSend's while that's off.
    fn offers(&self, tool: &tools::Tool, localsend: bool) -> bool {
        self.profile.offers(tool.profile) && (localsend || !tool.is_localsend())
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
                self.session.client = params
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
                        "capabilities": {
                            "tools": {},
                            "experimental": {"monodrive/contract": {"version": contract::VERSION}},
                        },
                        "serverInfo": {"name": "monodrive", "version": crate::VERSION},
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => {
                let localsend = self.localsend_on();
                result(
                    id,
                    json!({ "tools": tools::list(|tool| self.offers(tool, localsend)) }),
                )
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let localsend = self.localsend_on();
                let tool = tools::TOOLS
                    .iter()
                    .find(|tool| tool.name == name && self.offers(tool, localsend));
                match tool {
                    Some(tool) => {
                        // MCP lets a call without parameters leave out `arguments`.
                        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                        result(id, tools::answer(tool.run(&mut self.session, arguments)))
                    }
                    None => error(id, -32602, &format!("unknown tool {name}")),
                }
            }
            _ => error(id, -32601, &format!("unknown method {method}")),
        })
    }
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
