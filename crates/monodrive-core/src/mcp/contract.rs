//! What every tool's reply is made of: errors and their codes, the shapes
//! of jobs, changes and lists, and the schemas `tools/list` publishes.
//! `docs/mcp.md` says which of these may change without a new version.

use crate::entries;
use crate::ops::{self, Done, Job, Origin};
use crate::remote::RemoteError;
use crate::user_commands::{Ended, Run, RunState};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::SystemTime;

/// What `initialize` reports as `monodrive/contract`. It counts breaking
/// changes, apart from the app's version.
pub const VERSION: u32 = 1;

/// Which tools `tools/list` offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// Finding, reading and changing files, and the jobs that do it.
    Core,
    /// Core, plus places, servers, saved searches, Git, saved commands,
    /// edits and organize.
    All,
}

impl Profile {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "core" => Some(Profile::Core),
            "all" => Some(Profile::All),
            _ => None,
        }
    }

    /// Whether a tool in `profile` is offered under this one.
    pub fn offers(self, profile: Profile) -> bool {
        self == Profile::All || profile == Profile::Core
    }
}

/// MCP's tool annotations: advice for clients. The operation owner is
/// what enforces the Agents setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hints {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    /// It can reach another machine: an SSH server, a share, GitHub.
    pub open_world: bool,
}

impl Hints {
    pub const READ: Hints = Hints {
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
    };
    /// Only makes something new, or restores something.
    pub const ADD: Hints = Hints {
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
    };
    /// Moves, renames, replaces or removes something.
    pub const CHANGE: Hints = Hints {
        read_only: false,
        destructive: true,
        idempotent: false,
        open_world: false,
    };

    pub const fn net(self) -> Hints {
        Hints {
            open_world: true,
            ..self
        }
    }

    pub const fn idempotent(self) -> Hints {
        Hints {
            idempotent: true,
            ..self
        }
    }

    pub fn to_json(self) -> Value {
        json!({
            "readOnlyHint": self.read_only,
            "destructiveHint": self.destructive,
            "idempotentHint": self.idempotent,
            "openWorldHint": self.open_world,
        })
    }
}

/// Why a call failed, for an agent to decide whether to retry, fix its
/// arguments or give up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    InvalidArgument,
    NotFound,
    PermissionDenied,
    NameTaken,
    ChangedSincePreview,
    NoTrashOnVolume,
    NotDownloaded,
    UnsupportedOnSource,
    /// require_clone was set and the copy can't share blocks.
    CloneUnavailable,
    AgentsDisabled,
    IndexUnavailable,
    AuthRequired,
    Unreachable,
    Busy,
    /// The disk or a service said no, for the reason in the message.
    Failed,
    Internal,
}

impl Code {
    pub fn name(self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|name| name.as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    /// The same call may work later, unchanged.
    fn retryable(self) -> bool {
        matches!(
            self,
            Code::Busy | Code::Unreachable | Code::IndexUnavailable
        )
    }
}

/// A failed call, as `structuredContent.error`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ToolError {
    pub code: Code,
    /// What went wrong, as the service said it.
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ToolError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            path: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(Code::InvalidArgument, message)
    }

    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// The reply's text: the message, then the code an agent acts on.
    pub fn text(&self) -> String {
        let retry = if self.retryable { ", retryable" } else { "" };
        let message = self.message.trim_end();
        format!("{message}\n(error: {}{retry})", self.code.name())
    }
}

/// Services report errors as text; these phrases are theirs, so they're
/// the one place a message turns into a code.
impl From<String> for ToolError {
    fn from(message: String) -> Self {
        let lower = message.to_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|needle| lower.contains(needle));
        let code = if lower.starts_with("agents can't") {
            Code::AgentsDisabled
        } else if has(&["changed since"]) {
            Code::ChangedSincePreview
        } else if has(&["already exists"]) {
            Code::NameTaken
        } else if has(&["isn't downloaded", "not downloaded"]) {
            Code::NotDownloaded
        } else if has(&["no trash", "can't be moved to the trash"]) {
            Code::NoTrashOnVolume
        } else if has(&["permission denied", "not permitted"]) {
            Code::PermissionDenied
        } else if has(&[
            "no such file",
            "doesn't exist",
            "not found",
            "no job ",
            "no run ",
        ]) {
            Code::NotFound
        } else if has(&["busy", "aren't reachable"]) {
            Code::Busy
        } else if has(&["absolute", "already used for another"]) {
            Code::InvalidArgument
        } else {
            Code::Failed
        };
        Self::new(code, message)
    }
}

impl From<&str> for ToolError {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl From<RemoteError> for ToolError {
    fn from(e: RemoteError) -> Self {
        let code = match &e {
            RemoteError::UnknownSource(_) | RemoteError::NotFound(_) => Code::NotFound,
            RemoteError::BadPath(_) | RemoteError::TooBig(_) => Code::InvalidArgument,
            RemoteError::InteractiveAuth(_)
            | RemoteError::AuthFailed(..)
            | RemoteError::HostKeyChanged(_)
            | RemoteError::HostKeyUnknown(_) => Code::AuthRequired,
            RemoteError::Unreachable(..)
            | RemoteError::Timeout(_)
            | RemoteError::Disconnected(_) => Code::Unreachable,
            RemoteError::NoSftp(_) | RemoteError::Unsupported(_) => Code::UnsupportedOnSource,
            RemoteError::Denied(_) => Code::PermissionDenied,
            RemoteError::Changed(_) => Code::ChangedSincePreview,
            RemoteError::Protocol(_) | RemoteError::Failed(_) | RemoteError::Cancelled => {
                Code::Failed
            }
        };
        Self::new(code, String::from(e))
    }
}

impl From<std::io::Error> for ToolError {
    fn from(e: std::io::Error) -> Self {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => Code::NotFound,
            std::io::ErrorKind::PermissionDenied => Code::PermissionDenied,
            std::io::ErrorKind::AlreadyExists => Code::NameTaken,
            _ => Code::Failed,
        };
        Self::new(code, e.to_string())
    }
}

/// A tool's answer: the terse text a model reads, and the same as typed
/// data for `structuredContent`.
pub struct Reply<T> {
    pub text: String,
    pub data: T,
    /// A JPEG to show the model, as MCP image content.
    pub image: Option<Vec<u8>>,
}

impl<T> Reply<T> {
    pub fn new(text: impl Into<String>, data: T) -> Self {
        Self {
            text: text.into(),
            data,
            image: None,
        }
    }
}

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Folder,
    Link,
    Other,
}

impl From<entries::Kind> for Kind {
    fn from(kind: entries::Kind) -> Self {
        match kind {
            entries::Kind::File => Kind::File,
            entries::Kind::Dir => Kind::Folder,
            entries::Kind::Link => Kind::Link,
            entries::Kind::Other => Kind::Other,
        }
    }
}

/// `ls -F` markers, so agents can tell folders and links from files.
pub fn suffix(kind: entries::Kind) -> &'static str {
    match kind {
        entries::Kind::Dir => "/",
        entries::Kind::Link => "@",
        entries::Kind::File | entries::Kind::Other => "",
    }
}

/// One item a change was asked about: where it is now, or why it didn't
/// change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Changed {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How a copy's data was written: clone when it shares blocks with
    /// its source on one APFS volume.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<Method>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Method {
    Clone,
    Copy,
}

/// A change the owner made as one step of the undo history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Change {
    /// "Rename", or "Undo Rename".
    pub title: String,
    pub items: Vec<Changed>,
    /// Items that didn't change; each says why.
    pub failed: usize,
    /// The owner stopped partway and read these results back from the disk.
    pub interrupted: bool,
}

impl Change {
    pub fn of(outcome: &ops::Outcome) -> Self {
        let items = outcome
            .items
            .iter()
            .map(|item| Changed {
                path: shown(&item.path),
                to: item.result.as_ref().ok().map(|to| shown(to)),
                error: item.result.as_ref().err().cloned(),
                method: None,
            })
            .collect();
        Self {
            title: outcome.title.clone(),
            items,
            failed: outcome.failures().count(),
            interrupted: outcome.interrupted,
        }
    }

    /// Each resulting path on its own line, or why an item didn't change.
    pub fn text(&self) -> String {
        let mut out = String::new();
        if self.title.starts_with("Undo ") || self.title.starts_with("Redo ") {
            out.push_str(&self.title);
            out.push('\n');
        }
        for item in &self.items {
            match (&item.to, &item.error) {
                (Some(to), _) => out.push_str(to),
                (None, Some(why)) => out.push_str(why),
                (None, None) => out.push_str(&item.path),
            }
            out.push('\n');
        }
        if self.interrupted {
            out.push_str(
                "(Monodrive stopped partway; these results were read back from the disk.)\n",
            );
        }
        out
    }
}

/// What a tool with a dry run replies: the plan and its token, or what
/// running it did.
#[derive(Serialize, JsonSchema)]
pub struct Planned<P, R = Change> {
    /// A dry run's plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<P>,
    /// Pass as confirm, within 10 minutes, to run exactly the plan once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(flatten)]
    pub done: Option<R>,
}

impl<P, R> Planned<P, R> {
    pub fn plan(plan: P, token: Option<String>) -> Self {
        Self {
            plan: Some(plan),
            token,
            done: None,
        }
    }

    pub fn done(done: R) -> Self {
        Self {
            plan: None,
            token: None,
            done: Some(done),
        }
    }
}

impl<P> Reply<P> {
    /// This reply as a dry run's.
    pub fn planned<R>(self, token: Option<String>) -> Reply<Planned<P, R>> {
        Reply::new(self.text, Planned::plan(self.data, token))
    }
}

impl<R> Reply<R> {
    pub fn ran<P>(self) -> Reply<Planned<P, R>> {
        Reply::new(self.text, Planned::done(self.data))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Copy,
    Move,
    Compress,
    Extract,
    Download,
    Upload,
    Command,
}

/// A job's state. New states may come; treat one you don't know as still
/// going.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    /// Stopped at a conflict; continue_job with another conflict goes on.
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// One item of a job: what it took, and where it went or why not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct JobItem {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<Method>,
    /// A command's invocation: how it ended, and its arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
}

/// Every long-running change replies with this, even one already done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct JobView {
    /// Opaque; pass it to job_status, cancel_job and continue_job.
    pub id: String,
    pub kind: JobKind,
    pub state: JobState,
    /// Where its items go.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub into: Option<String>,
    pub items: Vec<JobItem>,
    pub done: usize,
    pub total: usize,
    pub bytes: u64,
    pub total_bytes: u64,
    pub undoable: bool,
    /// Who asked: "window", or the agent's client name.
    pub by: String,
    /// What a command printed, its start and end when it printed more.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// A command run's id, apart from the owner's job ids.
const RUN: &str = "cmd-";

/// Which job an id names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobId {
    Job(u64),
    Run(u64),
}

impl JobId {
    pub fn parse(id: &str) -> Result<Self, ToolError> {
        let bad = || ToolError::new(Code::NotFound, format!("No job {id}."));
        match id.strip_prefix(RUN) {
            Some(run) => run.parse().map(JobId::Run).map_err(|_| bad()),
            None => id.parse().map(JobId::Job).map_err(|_| bad()),
        }
    }
}

fn by(origin: &Origin) -> String {
    match origin {
        Origin::Window => "window".into(),
        Origin::Agent { client: Some(name) } => name.clone(),
        Origin::Agent { client: None } => "agent".into(),
    }
}

impl JobView {
    pub fn of_job(job: &Job, method: impl Fn(&Path) -> Option<Method>) -> Self {
        let kind = match job.title.as_str() {
            "Move" => JobKind::Move,
            "Compress" => JobKind::Compress,
            "Extract" => JobKind::Extract,
            "Download" => JobKind::Download,
            "Upload" => JobKind::Upload,
            _ => JobKind::Copy,
        };
        let state = match job.state {
            ops::State::Queued => JobState::Queued,
            ops::State::Running => JobState::Running,
            ops::State::Waiting => JobState::Waiting,
            ops::State::Completed => JobState::Completed,
            ops::State::Failed => JobState::Failed,
            ops::State::Cancelled => JobState::Cancelled,
            ops::State::Interrupted => JobState::Interrupted,
        };
        let items: Vec<JobItem> = job
            .tasks
            .iter()
            .map(|task| {
                let mut item = JobItem {
                    source: shown(&task.source),
                    to: None,
                    skipped: None,
                    error: None,
                    method: None,
                    ended: None,
                };
                match &task.done {
                    Some(Done::To(to)) => {
                        item.method = method(&task.source);
                        item.to = Some(shown(to));
                    }
                    Some(Done::Skipped(why)) => item.skipped = Some(why.clone()),
                    Some(Done::Failed(why)) => item.error = Some(why.clone()),
                    None => {}
                }
                item
            })
            .collect();
        Self {
            id: job.id.to_string(),
            kind,
            state,
            into: Some(shown(&job.into)),
            done: job.tasks.iter().filter(|task| task.done.is_some()).count(),
            total: items.len(),
            items,
            bytes: job.bytes,
            total_bytes: job.total_bytes,
            undoable: job.undoable,
            by: by(&job.origin),
            output: None,
        }
    }

    pub fn of_run(run: &Run) -> Self {
        let state = match run.state {
            RunState::Running => JobState::Running,
            RunState::Completed => JobState::Completed,
            RunState::Failed => JobState::Failed,
            RunState::Cancelled => JobState::Cancelled,
        };
        let items: Vec<JobItem> = run
            .plan
            .invocations
            .iter()
            .enumerate()
            .map(|(i, invocation)| {
                let ended = run.progress.get(i).map(|progress| {
                    let how = match &progress.ended {
                        None => "running".to_string(),
                        Some(Ended::Exited(code)) => format!("exit {code}"),
                        Some(Ended::Signaled(signal)) => format!("killed by signal {signal}"),
                        Some(Ended::TimedOut) => "timed out".to_string(),
                        Some(Ended::Cancelled) => "cancelled".to_string(),
                        Some(Ended::Failed(why)) => why.clone(),
                    };
                    format!("{how} after {} ms", progress.millis)
                });
                JobItem {
                    source: argv(invocation.argv.iter().map(|a| a.as_os_str())),
                    to: None,
                    skipped: None,
                    error: None,
                    method: None,
                    ended: Some(ended.unwrap_or_else(|| "not run".into())),
                }
            })
            .collect();
        let output = &run.output;
        let printed = (output.total > 0).then(|| {
            let mut out = output.head.clone();
            if output.left_out() > 0 {
                out.push_str(&format!("\n[{} bytes left out]\n", output.left_out()));
            }
            out.push_str(&output.tail);
            out
        });
        Self {
            id: format!("{RUN}{}", run.id),
            kind: JobKind::Command,
            state,
            into: None,
            done: run.progress.iter().filter(|p| p.ended.is_some()).count(),
            total: items.len(),
            items,
            bytes: 0,
            total_bytes: 0,
            undoable: false,
            by: by(&run.origin),
            output: printed,
        }
    }

    /// One line: what it is, how far it's got, and who asked.
    pub fn summary(&self) -> String {
        let kind = serde_json::to_value(self.kind)
            .ok()
            .and_then(|kind| kind.as_str().map(str::to_owned))
            .unwrap_or_default();
        let state = serde_json::to_value(self.state)
            .ok()
            .and_then(|state| state.as_str().map(str::to_owned))
            .unwrap_or_default();
        let into = self
            .into
            .as_ref()
            .map_or(String::new(), |into| format!(" into {into}"));
        let mut line = format!(
            "job {} {kind}{into}: {state}, {} of {} items",
            self.id, self.done, self.total
        );
        if self.total_bytes > 0 {
            line.push_str(&format!(", {} of {} bytes", self.bytes, self.total_bytes));
        }
        line.push_str(&format!(", by {}", self.by));
        line
    }

    /// The summary, then each finished item, then any output.
    pub fn text(&self) -> String {
        let mut out = format!("{}\n", self.summary());
        for item in &self.items {
            let line = match (&item.to, &item.skipped, &item.error, &item.ended) {
                (Some(to), ..) => to.clone(),
                (_, Some(why), ..) | (_, _, Some(why), _) => why.clone(),
                (.., Some(ended)) => format!("{ended}: {}", item.source),
                _ => continue,
            };
            out.push_str(&line);
            out.push('\n');
        }
        if let Some(output) = &self.output {
            out.push_str("output:\n");
            out.push_str(output);
            if !out.ends_with('\n') {
                out.push('\n');
            }
        }
        out
    }
}

/// Arguments as a JSON array, so spaces, quotes and newlines read
/// unambiguously.
pub fn argv<'a>(args: impl IntoIterator<Item = &'a std::ffi::OsStr>) -> String {
    let args: Vec<_> = args.into_iter().map(|arg| arg.to_string_lossy()).collect();
    serde_json::to_string(&args).unwrap_or_default()
}

/// A page of a long list. Cursors are opaque to agents; here they're the
/// offset of the next item.
pub struct Page {
    pub offset: usize,
    pub limit: usize,
}

impl Page {
    pub fn new(cursor: Option<&str>, limit: usize) -> Result<Self, ToolError> {
        let offset = match cursor {
            None => 0,
            Some(cursor) => cursor.parse().map_err(|_| {
                ToolError::invalid(format!("{cursor} isn't a cursor from this tool."))
            })?,
        };
        Ok(Self {
            offset,
            limit: limit.max(1),
        })
    }

    /// How many to ask a service for, to know whether there's more.
    pub fn wanted(&self) -> usize {
        self.offset.saturating_add(self.limit).saturating_add(1)
    }

    /// This page of `all`, and the cursor for the next one.
    pub fn take<T>(&self, all: Vec<T>) -> (Vec<T>, Option<String>) {
        let more = all.len() > self.offset + self.limit;
        let items = all.into_iter().skip(self.offset).take(self.limit).collect();
        let next = more.then(|| (self.offset + self.limit).to_string());
        (items, next)
    }
}

/// Where a list goes on, said once at the end of its text.
pub fn more_line(next: Option<&String>) -> String {
    next.map_or(String::new(), |next| {
        format!("(more; pass cursor {next} for the next page)\n")
    })
}

pub fn shown(path: &Path) -> String {
    path.display().to_string()
}

/// ISO 8601 in UTC, to the second. A date too far out for a calendar year
/// prints as seconds from the epoch.
pub fn utc(time: SystemTime) -> String {
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

pub fn utc_seconds(seconds: u64) -> String {
    utc(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
}

/// A tool's arguments as JSON Schema: inlined, without the titles and
/// number formats schemars adds, which only cost a model tokens.
pub fn input_schema<T: JsonSchema>() -> Value {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    settings.meta_schema = None;
    let mut schema = settings
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    tidy(&mut schema);
    if let Some(root) = schema.as_object_mut() {
        root.remove("description");
        root.entry("properties").or_insert_with(|| json!({}));
    }
    schema
}

/// A tool's result as JSON Schema. Open, so new fields can come, and with
/// no required field at the top, since a failed call's result holds only
/// `error`.
pub fn output_schema<T: JsonSchema>() -> Value {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.inline_subschemas = true;
    settings.meta_schema = None;
    let mut generator = settings.into_generator();
    let mut schema = generator.root_schema_for::<T>().to_value();
    let mut error = generator.root_schema_for::<ToolError>().to_value();
    tidy(&mut schema);
    tidy(&mut error);
    if let Some(root) = schema.as_object_mut() {
        root.remove("description");
        root.remove("required");
        root.entry("properties")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .map(|properties| properties.insert("error".into(), error));
    }
    schema
}

/// Drops `title`, `format` and `$schema` from every schema in `value`,
/// leaving property names alone.
fn tidy(value: &mut Value) {
    // An optional field is left out, never null: `[T, "null"]` is T, and
    // `anyOf: [T, null]` is T.
    if let Some(one) = value
        .get("anyOf")
        .and_then(Value::as_array)
        .filter(|options| options.len() == 2)
        .and_then(|options| options.iter().find(|option| option["type"] != "null"))
        .filter(|_| value["anyOf"].to_string().contains(r#"{"type":"null"}"#))
        .cloned()
    {
        let mut one = one;
        if let (Some(one), Some(outer)) = (one.as_object_mut(), value.as_object()) {
            for (key, field) in outer.iter().filter(|(key, _)| *key != "anyOf") {
                one.entry(key.clone()).or_insert_with(|| field.clone());
            }
        }
        *value = one;
    }
    let Some(schema) = value.as_object_mut() else {
        return;
    };
    if let Some(Value::String(description)) = schema.get_mut("description") {
        *description = description.replace('\n', " ");
    }
    // An enum whose values have their own docs is a oneOf of strings: one
    // enum is the same to a model, and cheaper.
    let values: Option<Vec<Value>> =
        schema
            .get("oneOf")
            .and_then(Value::as_array)
            .and_then(|options| {
                options
                    .iter()
                    .map(|option| match (option.get("const"), option.get("enum")) {
                        (Some(one), None) => Some(vec![one.clone()]),
                        (None, Some(Value::Array(many))) => Some(many.clone()),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(|lists| lists.concat())
            });
    if let Some(values) = values.filter(|values| values.iter().all(Value::is_string)) {
        schema.remove("oneOf");
        schema.insert("type".into(), json!("string"));
        schema.insert("enum".into(), Value::Array(values));
    }
    if let Some(types) = schema.get_mut("type").and_then(Value::as_array_mut) {
        types.retain(|kind| kind != "null");
        if let [one] = types.as_slice() {
            let one = one.clone();
            schema.insert("type".into(), one);
        }
    }
    for key in ["title", "format", "$schema"] {
        schema.remove(key);
    }
    for (key, child) in schema.iter_mut() {
        match key.as_str() {
            "properties" => {
                if let Some(properties) = child.as_object_mut() {
                    properties.values_mut().for_each(tidy);
                }
            }
            "items" | "additionalProperties" | "not" => tidy(child),
            "oneOf" | "anyOf" | "allOf" | "prefixItems" => {
                if let Some(list) = child.as_array_mut() {
                    list.iter_mut().for_each(tidy);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_messages_become_codes_an_agent_can_act_on() {
        let code = |message: &str| ToolError::from(message).code;
        assert_eq!(
            code(
                "Agents can't change files: Let agents change files is off in Monodrive's Agents settings."
            ),
            Code::AgentsDisabled
        );
        assert_eq!(code("/a/b.txt already exists."), Code::NameTaken);
        assert_eq!(
            code("a.txt: changed since the preview"),
            Code::ChangedSincePreview
        );
        assert_eq!(
            code("/x/photo.jpg isn't downloaded. Download it first."),
            Code::NotDownloaded
        );
        assert_eq!(
            code("/x: No such file or directory (os error 2)"),
            Code::NotFound
        );
        assert_eq!(
            code("Monodrive's file operations aren't reachable: refused"),
            Code::Busy
        );
        assert!(ToolError::from("busy").retryable);
        assert!(!ToolError::from("/a already exists.").retryable);
    }
}
