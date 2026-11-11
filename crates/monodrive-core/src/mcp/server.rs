//! The protocol: JSON-RPC on stdin and stdout, one message a line, and the
//! session state tools share.

use super::contract::{self, Code, JobView, Method, Profile, Reply, ToolError};
use super::tools::{self, Preview};
use crate::index::{self, Index, Status};
use crate::ops::{self, Job, Origin, Request};
use crate::places;
use crate::remote::{self, Connection, Connections, RemoteError, RemotePath};
use crate::worktrees::Git;
use serde::Deserialize;
use serde_json::{Value, json};
use std::hash::{BuildHasher, RandomState};
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Newest first. A client asking for another version gets the newest.
const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Short: Codex repeats it after every tool's description.
const INSTRUCTIONS: &str = "Monodrive: an always-current index of this Mac's files, faster than \
Spotlight or find. Paths are absolute or start with ~; a saved SSH server's are id:/path. Changes \
share the Monodrive window's undo history. A failed call ends with (error: code).";

/// How long a call may hold its reply for a job.
const MOST_WAIT: u64 = 60;

/// How many previews a session keeps, and for how long.
const PREVIEWS_KEPT: usize = 16;
const PREVIEW_LIFE: Duration = Duration::from_secs(600);

/// Runs the server for this user until stdin closes. `profile` is the
/// command line's; without one, Settings' Agents page says.
pub fn serve_stdio(profile: Option<Profile>) -> std::io::Result<()> {
    let paths = ops::Paths::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let places = places::Store::user().ok_or_else(|| std::io::Error::other("no home folder"))?;
    let profile = profile.unwrap_or_else(|| saved_profile(&paths.settings));
    let mut server = Server::new(
        ops::Client::new(paths, std::env::current_exe().ok()),
        places,
        profile,
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

/// `Preferences::agents_all_tools` in the app, read from its saved file.
fn saved_profile(settings: &Path) -> Profile {
    #[derive(Deserialize)]
    struct Settings {
        #[serde(default)]
        agents_all_tools: bool,
    }
    let all = std::fs::read(settings)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .is_some_and(|settings| settings.agents_all_tools);
    if all { Profile::All } else { Profile::Core }
}

pub struct Server {
    profile: Profile,
    /// Started on the first tool that needs it, so connecting an agent
    /// never kicks off a disk crawl by itself.
    index: Option<Result<Index, String>>,
    /// Connects to the operation owner on the first change.
    pub(super) ops: ops::Client,
    /// The name the agent's MCP client gave, to attribute its changes.
    client: Option<String>,
    pub(super) places: places::Store,
    /// Where the startup disk's own folders are, which the Overview measures.
    pub(super) root: std::path::PathBuf,
    /// Found on the first Git tool, and again until one is found.
    git: Option<Git>,
    /// What dry runs planned, oldest first, so a confirm runs only that.
    previews: Vec<Kept>,
    /// Each opened on the first tool that needs it; each session its own.
    pub(super) remote: Connections,
}

struct Kept {
    token: String,
    made: Instant,
    preview: Preview,
    /// The request id that ran it, so a retry of that call reaches the
    /// owner, which answers with what it did the first time.
    ran: Option<String>,
}

impl Server {
    pub fn new(ops: ops::Client, places: places::Store, profile: Profile) -> Self {
        Self {
            profile,
            index: None,
            ops,
            client: None,
            remote: Connections::new(places.clone(), remote::ssh::connector()),
            places,
            root: "/".into(),
            previews: Vec::new(),
            git: None,
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
            "tools/list" => result(id, json!({ "tools": tools::list(self.profile) })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let tool = tools::TOOLS
                    .iter()
                    .find(|tool| tool.name == name && self.profile.offers(tool.profile));
                match tool {
                    Some(tool) => {
                        // MCP lets a call without parameters leave out `arguments`.
                        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                        result(id, tools::answer(tool.run(self, arguments)))
                    }
                    None => error(id, -32602, &format!("unknown tool {name}")),
                }
            }
            _ => error(id, -32601, &format!("unknown method {method}")),
        })
    }

    pub(super) fn origin(&self) -> Origin {
        Origin::Agent {
            client: self.client.clone(),
        }
    }

    /// Runs a change through the operation owner.
    pub(super) fn submit(
        &mut self,
        request: Request,
        request_id: Option<&str>,
    ) -> Result<ops::Outcome, ToolError> {
        let origin = self.origin();
        Ok(self.ops.submit_as(request_id, origin, request)?)
    }

    /// A change the owner makes at once. The call fails only when no item
    /// changed; otherwise each item says how it went.
    pub(super) fn change(
        &mut self,
        request: Request,
        request_id: Option<&str>,
    ) -> Result<Reply<contract::Change>, ToolError> {
        let outcome = self.submit(request, request_id)?;
        let change = contract::Change::of(&outcome);
        if change.failed > 0 && change.failed == change.items.len() {
            let first = &change.items[0];
            let why = change.text();
            return Err(ToolError::from(why).at(first.path.clone()));
        }
        Ok(Reply::new(change.text(), change))
    }

    /// Queues a job, waits up to `wait_seconds` for it to end, and replies
    /// with where it's got.
    pub(super) fn queue(
        &mut self,
        request: Request,
        request_id: Option<&str>,
        wait_seconds: u64,
    ) -> Result<Reply<JobView>, ToolError> {
        let outcome = self.submit(request, request_id)?;
        self.job_reply(outcome.op, wait_seconds)
    }

    /// The job `id`, once it ends or `wait_seconds` pass, whichever is first.
    pub(super) fn job_reply(
        &mut self,
        id: u64,
        wait_seconds: u64,
    ) -> Result<Reply<JobView>, ToolError> {
        let job = self.wait(id, wait_seconds)?;
        let view = JobView::of_job(&job, |source| method(&job, source));
        Ok(Reply::new(view.text(), view))
    }

    /// Waits on the owner's journal, which it writes as each item ends,
    /// never on a clock.
    fn wait(&mut self, id: u64, wait_seconds: u64) -> Result<Job, ToolError> {
        let deadline = Instant::now() + Duration::from_secs(wait_seconds.min(MOST_WAIT));
        let (tell, told) = std::sync::mpsc::channel();
        let _watch = (wait_seconds > 0)
            .then(|| {
                self.ops.watch(move || {
                    let _ = tell.send(());
                })
            })
            .and_then(Result::ok);
        loop {
            let job = self
                .ops
                .jobs()?
                .into_iter()
                .find(|job| job.id == id)
                .ok_or_else(|| ToolError::new(Code::NotFound, format!("No job {id}.")))?;
            let left = deadline.saturating_duration_since(Instant::now());
            if !job.state.active() || _watch.is_none() || left.is_zero() {
                return Ok(job);
            }
            if told.recv_timeout(left).is_err() {
                return self.wait(id, 0);
            }
        }
    }

    /// Refuses before anything is written when agents may not change files.
    pub(super) fn can_change(&self) -> Result<(), ToolError> {
        if self.ops.agents_change_files() {
            Ok(())
        } else {
            Err(ToolError::new(
                Code::AgentsDisabled,
                "Agents can't change files: Let agents change files is off in Monodrive's Agents settings.",
            ))
        }
    }

    /// The connection for `qualified`, and the path; `source:` alone means
    /// where the source starts.
    pub(super) fn remote_at(
        &self,
        qualified: &str,
    ) -> Result<(Arc<Connection>, RemotePath), RemoteError> {
        if let Some(source) = qualified.strip_suffix(':') {
            let open = self.remote.connect(source)?;
            let at = open.start()?;
            return Ok((open, at));
        }
        let at = RemotePath::parse(qualified)?;
        Ok((self.remote.connect(at.source())?, at))
    }

    /// Keeps a dry run's plan under a new token, for one confirm.
    pub(super) fn keep(&mut self, preview: Preview) -> String {
        let token = format!(
            "{:016x}",
            RandomState::new().hash_one((self.previews.len(), Instant::now()))
        );
        self.previews
            .retain(|kept| kept.made.elapsed() < PREVIEW_LIFE);
        if self.previews.len() >= PREVIEWS_KEPT {
            self.previews.remove(0);
        }
        self.previews.push(Kept {
            token: token.clone(),
            made: Instant::now(),
            preview,
            ran: None,
        });
        token
    }

    /// The plan a dry run kept under `token`, until it runs. It stays kept
    /// when the owner refuses it whole, so it can be confirmed again, and a
    /// run's own request id may send it again.
    pub(super) fn kept(
        &mut self,
        token: &str,
        request_id: Option<&str>,
    ) -> Result<Preview, ToolError> {
        self.previews
            .retain(|kept| kept.made.elapsed() < PREVIEW_LIFE);
        self.previews
            .iter()
            .find(|kept| kept.token == token && (kept.ran.is_none() || kept.ran.as_deref() == request_id))
            .map(|kept| kept.preview.clone())
            .ok_or_else(|| {
                ToolError::new(
                    Code::ChangedSincePreview,
                    "No plan with that token in this session. A plan runs once, within 10 minutes; dry run again.",
                )
            })
    }

    /// Marks the plan under `token` as run by `request_id`.
    pub(super) fn spent(&mut self, token: &str, request_id: Option<&str>) {
        match request_id {
            Some(id) => self
                .previews
                .iter_mut()
                .filter(|kept| kept.token == token)
                .for_each(|kept| kept.ran = Some(id.to_owned())),
            None => self.previews.retain(|kept| kept.token != token),
        }
    }

    pub(super) fn git(&mut self) -> Result<Git, ToolError> {
        let git = match self.git.take() {
            Some(git) => git,
            None => Git::find()?,
        };
        self.git = Some(git.clone());
        Ok(git)
    }

    /// Where the index is, without starting it.
    pub(super) fn overview_env(&self) -> crate::overview::Env {
        crate::overview::Env {
            root: self.root.clone(),
            ..crate::overview::Env::new(self.ops.trash().to_path_buf(), self.places.clone())
        }
    }

    pub(super) fn status(&self) -> Result<Status, ToolError> {
        match &self.index {
            None => Ok(Status::not_started(index::saved())),
            Some(index) => index
                .as_ref()
                .map(Index::status)
                .map_err(|e| ToolError::new(Code::IndexUnavailable, e.clone())),
        }
    }

    /// The index if this session already started it, for tools that must
    /// never start one.
    pub(super) fn started_index(&self) -> Option<&Index> {
        self.index.as_ref().and_then(|index| index.as_ref().ok())
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "each agent session's server starts the index on its first search"
    )]
    pub(super) fn index(&mut self) -> Result<&Index, ToolError> {
        self.index
            .get_or_insert_with(Index::start)
            .as_ref()
            .map_err(|e| ToolError::new(Code::IndexUnavailable, e.clone()))
    }
}

/// How a finished copy's data was written. Copies to or from a server
/// never clone; moves and archives aren't copies.
fn method(job: &Job, source: &Path) -> Option<Method> {
    match job.title.as_str() {
        "Copy" if ops::clones(source, &job.into) => Some(Method::Clone),
        "Copy" | "Download" | "Upload" => Some(Method::Copy),
        _ => None,
    }
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
