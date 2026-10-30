//! User commands: programs the user saved to run on a selection. Selected
//! paths reach the program as arguments of their own, never through a shell
//! string. Definitions live in places.json; the operation owner runs them, so
//! the window and every MCP server see and cancel the same runs.
//! docs/commands.md records why.

use crate::ops::Origin;
use crate::worktrees::{executable, exited};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::ops::RangeInclusive;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// An argument that stands for every selected path, each its own argument.
pub const PATHS: &str = "{paths}";
/// An argument that stands for one selected path; the command runs once per
/// path.
pub const PATH: &str = "{path}";
/// Where a program given by name is looked for, and the `PATH` it runs
/// with, unless the definition sets its own.
pub const SEARCH_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

const TIMEOUTS: RangeInclusive<u64> = 1..=86_400;
const OUTPUT_LIMITS: RangeInclusive<u64> = 1 << 10..=1 << 20;
/// How long a program has to exit after SIGTERM before its group gets
/// SIGKILL.
const GRACE: Duration = Duration::from_secs(2);
const MOST_PATHS: usize = 1000;
/// At most this many runs at once; another is refused.
const RUNNING: usize = 8;
/// How many ended runs clients are shown.
const SHOWN: usize = 20;
/// Names that suggest a secret, which a definition saved in plain text
/// mustn't hold.
const SECRETS: [&str; 7] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "API_KEY",
    "PRIVATE_KEY",
];

/// A command the user saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Definition {
    pub name: String,
    /// An absolute path, or a name looked up on `PATH` when it runs.
    pub program: PathBuf,
    /// The arguments after the program. `{paths}` or `{path}`, as a whole
    /// argument, stands for the selection.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Cwd,
    #[serde(default)]
    pub sources: Sources,
    /// Added to the environment. Saved in plain text, so never a secret.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// For each invocation.
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
    /// The most of a run's output kept: its start and its end.
    #[serde(default = "output_limit")]
    pub output_limit: u64,
}

fn timeout() -> u64 {
    60
}

fn output_limit() -> u64 {
    64 << 10
}

/// The folder a command runs in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cwd {
    /// The folder holding the selection, or with `{path}`, each item's.
    #[default]
    Selection,
    Home,
    Folder(PathBuf),
}

/// What a command runs on. Only local items: a remote source's paths mustn't
/// be taken for local ones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sources {
    Files,
    Folders,
    #[default]
    Both,
}

/// Exactly what a run starts: each invocation's arguments, the program
/// first, and its folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub name: String,
    pub invocations: Vec<Invocation>,
    /// The definition's additions to the environment.
    pub env: BTreeMap<String, String>,
    pub timeout_seconds: u64,
    pub output_limit: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invocation {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
}

/// A run as clients see it. Nothing a command changes can be undone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: u64,
    pub origin: Origin,
    pub state: RunState,
    /// In seconds since 1970.
    pub started: u64,
    pub plan: Plan,
    /// One for each invocation started so far, in order.
    pub progress: Vec<Progress>,
    /// What the invocations printed, standard output and errors together.
    pub output: Output,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    /// Every invocation exited with 0.
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// `None` while it runs.
    pub ended: Option<Ended>,
    /// So far, while it runs.
    pub millis: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    Exited(i32),
    /// By a signal Monodrive didn't send.
    Signaled(i32),
    TimedOut,
    Cancelled,
    /// It couldn't start.
    Failed(String),
}

/// The start and end of what a run printed, when it printed more than its
/// limit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    pub head: String,
    pub tail: String,
    /// Bytes printed.
    pub total: u64,
    /// Bytes kept in `head` and `tail`.
    pub kept: u64,
}

impl Output {
    pub fn left_out(&self) -> u64 {
        self.total - self.kept
    }
}

impl RunState {
    pub fn label(self) -> &'static str {
        match self {
            RunState::Running => "running",
            RunState::Completed => "completed",
            RunState::Failed => "failed",
            RunState::Cancelled => "cancelled",
        }
    }
}

impl Definition {
    /// The definition as it's saved, its name trimmed, or why it can't be.
    pub fn validated(mut self) -> Result<Self, String> {
        self.name = self.name.trim().into();
        if self.name.is_empty() || self.name.contains('\n') {
            return Err("Name the command, on one line.".into());
        }
        self.check()
            .map_err(|why| format!("{}: {why}", self.name))?;
        Ok(self)
    }

    fn check(&self) -> Result<(), String> {
        let program = self.program.as_os_str().as_bytes();
        if program.is_empty() {
            return Err("give a program to run.".into());
        }
        if !self.program.is_absolute() && program.contains(&b'/') {
            return Err("give the program's absolute path, or a name to look up on PATH.".into());
        }
        let texts = self.args.iter().chain(self.env.values());
        if program.contains(&0) || texts.into_iter().any(|text| text.contains('\0')) {
            return Err("the program, arguments and environment can't hold a NUL byte.".into());
        }
        let placeholder = |arg: &str| arg == PATH || arg == PATHS;
        if let Some(arg) = self
            .args
            .iter()
            .find(|arg| !placeholder(arg) && (arg.contains(PATH) || arg.contains(PATHS)))
        {
            return Err(format!(
                "{arg}: {PATH} and {PATHS} must be whole arguments, so a name is never split or joined."
            ));
        }
        let count = |of: &str| self.args.iter().filter(|arg| *arg == of).count();
        match (count(PATH), count(PATHS)) {
            (0, 0 | 1) | (_, 0) => {}
            (0, _) => return Err(format!("use {PATHS} once.")),
            _ => return Err(format!("use {PATH} or {PATHS}, not both.")),
        }
        for pair in self.args.windows(2) {
            if placeholder(&pair[1]) && runs_code(&self.program, &pair[0]) {
                return Err(format!(
                    "{} after {} would run a selected name as code. Pass the names after the script, as $1 and on.",
                    pair[1], pair[0]
                ));
            }
        }
        if let Cwd::Folder(dir) = &self.cwd
            && !dir.is_absolute()
        {
            return Err(format!(
                "{}: give the folder's absolute path.",
                dir.display()
            ));
        }
        for key in self.env.keys() {
            let named = key.bytes().enumerate().all(|(i, b)| {
                b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit())
            });
            if key.is_empty() || !named {
                return Err(format!("{key} isn't an environment variable name."));
            }
            let upper = key.to_ascii_uppercase();
            if SECRETS.iter().any(|word| upper.contains(word)) {
                return Err(format!(
                    "{key} looks like a secret. Commands are saved in plain text, so keep secrets where the program reads its own."
                ));
            }
        }
        if !TIMEOUTS.contains(&self.timeout_seconds) {
            return Err(format!(
                "give a timeout from {} to {} seconds.",
                TIMEOUTS.start(),
                TIMEOUTS.end()
            ));
        }
        if !OUTPUT_LIMITS.contains(&self.output_limit) {
            return Err(format!(
                "give an output limit from {} KB to {} MB.",
                OUTPUT_LIMITS.start() >> 10,
                OUTPUT_LIMITS.end() >> 20
            ));
        }
        Ok(())
    }
}

/// Whether `program` runs the argument after `flag` as code, as `sh -c`
/// does, so a selected name there would be run rather than read.
fn runs_code(program: &Path, flag: &str) -> bool {
    let name = program.file_name().and_then(OsStr::to_str).unwrap_or("");
    if matches!(
        name,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" | "csh" | "tcsh"
    ) {
        return flag.starts_with('-') && !flag.starts_with("--") && flag.contains('c');
    }
    let flags: &[&str] = match name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.') {
        "python" => &["-c"],
        "perl" | "ruby" => &["-e", "-E"],
        "node" => &["-e", "--eval", "-p", "--print"],
        "osascript" => &["-e"],
        _ => &[],
    };
    flags.contains(&flag)
}

/// What running `definition` on `paths` would start, read from the disk
/// now: the program found, each path checked, and each invocation's
/// arguments and folder. Paths must be absolute, so none can be taken for
/// an option, and they're passed as given.
pub fn preview(definition: &Definition, paths: &[PathBuf]) -> Result<Plan, String> {
    let definition = definition.clone().validated()?;
    let name = &definition.name;
    if paths.len() > MOST_PATHS {
        return Err(format!("Run {name} on at most {MOST_PATHS} items at once."));
    }
    for path in paths {
        source(path, definition.sources)?;
    }
    let each = definition.args.iter().any(|arg| arg == PATH);
    if paths.is_empty() && (each || definition.args.iter().any(|arg| arg == PATHS)) {
        return Err(format!("Select something for {name} to run on."));
    }
    let search = definition
        .env
        .get("PATH")
        .map_or(SEARCH_PATH, String::as_str);
    let program = resolve(&definition.program, search)?;
    let groups: Vec<&[PathBuf]> = if each {
        paths.chunks(1).collect()
    } else {
        vec![paths]
    };
    let invocations = groups
        .into_iter()
        .map(|group| {
            let mut argv = vec![program.clone().into_os_string()];
            for arg in &definition.args {
                if arg == PATH || arg == PATHS {
                    argv.extend(group.iter().map(|path| path.clone().into_os_string()));
                } else {
                    argv.push(arg.into());
                }
            }
            Ok(Invocation {
                argv,
                cwd: folder(&definition.cwd, group)?,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Plan {
        name: definition.name,
        invocations,
        env: definition.env,
        timeout_seconds: definition.timeout_seconds,
        output_limit: definition.output_limit,
    })
}

/// Checks that `path` is a local item of the kind the command takes.
fn source(path: &Path, sources: Sources) -> Result<(), String> {
    let shown = path.display();
    if !path.is_absolute() {
        return Err(format!(
            "{shown}: give the absolute path of an item on this Mac; commands run on local items only."
        ));
    }
    let is_dir = match std::fs::metadata(path) {
        Ok(meta) => meta.is_dir(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(format!("{shown} doesn't exist."));
        }
        Err(e) => return Err(format!("{shown}: {e}")),
    };
    match (sources, is_dir) {
        (Sources::Files, true) => Err(format!(
            "{shown} is a folder, and this command runs on files only."
        )),
        (Sources::Folders, false) => Err(format!(
            "{shown} isn't a folder, and this command runs on folders only."
        )),
        _ => Ok(()),
    }
}

/// The program at its absolute path, or the first of its name on `search`.
fn resolve(program: &Path, search: &str) -> Result<PathBuf, String> {
    if program.is_absolute() {
        return if executable(program) {
            Ok(program.into())
        } else {
            Err(format!("{} isn't an executable file.", program.display()))
        };
    }
    std::env::split_paths(search)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(program))
        .find(|path| executable(path))
        .ok_or_else(|| format!("{} isn't on PATH ({search}).", program.display()))
}

/// The folder an invocation on `paths` runs in.
fn folder(cwd: &Cwd, paths: &[PathBuf]) -> Result<PathBuf, String> {
    let dir = match cwd {
        Cwd::Home => std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or("No home folder.")?,
        Cwd::Folder(dir) => dir.clone(),
        Cwd::Selection => {
            let mut parents = paths.iter().map(|path| path.parent().unwrap_or(path));
            let first = parents
                .next()
                .ok_or("Select something; this command runs in the selection's folder.")?;
            if parents.any(|parent| parent != first) {
                return Err("The selection is in more than one folder, so there's no one folder to run in. Give the command a fixed folder or home.".into());
            }
            first.to_path_buf()
        }
    };
    match std::fs::metadata(&dir) {
        Ok(meta) if meta.is_dir() => Ok(dir),
        Ok(_) => Err(format!("{} isn't a folder.", dir.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(format!("{} doesn't exist.", dir.display()))
        }
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// An environment of Monodrive's own, the same whichever process started
/// the owner, then the definition's additions.
fn environment(plan: &Plan) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<OsString, OsString> = ["HOME", "USER", "LOGNAME", "TMPDIR"]
        .into_iter()
        .filter_map(|key| Some((key.into(), std::env::var_os(key)?)))
        .collect();
    env.insert("PATH".into(), SEARCH_PATH.into());
    env.insert("LANG".into(), "en_US.UTF-8".into());
    env.extend(
        plan.env
            .iter()
            .map(|(key, value)| (key.into(), value.into())),
    );
    env
}

/// The start of what a run printed and a ring of its end.
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
    limit: usize,
}

impl Capture {
    fn new(limit: u64) -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let room = (self.limit / 2).saturating_sub(self.head.len());
        let (head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(head);
        let ring = self.limit - self.limit / 2;
        let rest = &rest[rest.len().saturating_sub(ring)..];
        let over = (self.tail.len() + rest.len()).saturating_sub(ring);
        self.tail.drain(..over);
        self.tail.extend(rest);
    }

    fn public(&self) -> Output {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        Output {
            head: String::from_utf8_lossy(&self.head).into_owned(),
            tail: String::from_utf8_lossy(&tail).into_owned(),
            total: self.total,
            kept: (self.head.len() + tail.len()) as u64,
        }
    }
}

enum Event {
    Exited,
    /// Every holder of the output pipe has closed it.
    Closed,
    Cancel,
}

/// A run, shared by the thread running it and the clients asking about it.
struct Live {
    id: u64,
    /// The client's id for the request, so a resent request doesn't run
    /// twice.
    request: String,
    origin: Origin,
    started: u64,
    plan: Plan,
    state: Mutex<Going>,
}

struct Going {
    state: RunState,
    /// When each invocation started, and how it ended and how long it took.
    progress: Vec<(Instant, Option<(Ended, Duration)>)>,
    output: Capture,
    cancelled: bool,
    /// Tells the running invocation to stop.
    current: Option<mpsc::Sender<Event>>,
}

impl Live {
    fn new(id: u64, request: String, origin: Origin, plan: Plan) -> Self {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        Self {
            id,
            request,
            origin,
            started,
            state: Mutex::new(Going {
                state: RunState::Running,
                progress: Vec::new(),
                output: Capture::new(plan.output_limit),
                cancelled: false,
                current: None,
            }),
            plan,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Going> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs the invocations one at a time until they're done or the run is
    /// cancelled. One that fails doesn't stop the rest.
    fn execute(self: &Arc<Self>) {
        let env = environment(&self.plan);
        let timeout = Duration::from_secs(self.plan.timeout_seconds);
        for invocation in &self.plan.invocations {
            let (tx, rx) = mpsc::channel();
            {
                let mut going = self.lock();
                if going.cancelled {
                    break;
                }
                going.current = Some(tx.clone());
                going.progress.push((Instant::now(), None));
            }
            let ended = self.invoke(invocation, &env, timeout, tx, &rx);
            let mut going = self.lock();
            going.current = None;
            if let Some((began, slot)) = going.progress.last_mut() {
                *slot = Some((ended, began.elapsed()));
            }
        }
        let mut going = self.lock();
        let ended: Vec<&Ended> = going
            .progress
            .iter()
            .filter_map(|(_, ended)| ended.as_ref().map(|(ended, _)| ended))
            .collect();
        going.state =
            if ended.len() < self.plan.invocations.len() || ended.contains(&&Ended::Cancelled) {
                RunState::Cancelled
            } else if ended.iter().all(|ended| **ended == Ended::Exited(0)) {
                RunState::Completed
            } else {
                RunState::Failed
            };
    }

    /// Starts one invocation in a process group of its own and waits for
    /// it. Stopping it, on cancel, at the timeout, or when it has exited
    /// but something it started still holds its output, sends SIGTERM to
    /// the group, then SIGKILL after `GRACE`. Whatever is left in the group
    /// once it has exited gets SIGKILL too.
    fn invoke(
        self: &Arc<Self>,
        invocation: &Invocation,
        env: &BTreeMap<OsString, OsString>,
        timeout: Duration,
        tx: mpsc::Sender<Event>,
        rx: &mpsc::Receiver<Event>,
    ) -> Ended {
        let Some((program, args)) = invocation.argv.split_first() else {
            return Ended::Failed("Nothing to run.".into());
        };
        let shown = Path::new(program).display();
        let fail = |e: io::Error| Ended::Failed(format!("Couldn't run {shown}: {e}"));
        if !invocation.cwd.is_dir() {
            return Ended::Failed(format!("{} isn't a folder.", invocation.cwd.display()));
        }
        let (mut reader, writer) = match io::pipe() {
            Ok(pipe) => pipe,
            Err(e) => return fail(e),
        };
        let errors = match writer.try_clone() {
            Ok(errors) => errors,
            Err(e) => return fail(e),
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(&invocation.cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(errors)
            .process_group(0);
        let spawned = command.spawn();
        // It holds the pipe's write end, which must close for the output
        // to end.
        drop(command);
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => return fail(e),
        };
        let pid = child.id();
        let Ok(group) = libc::pid_t::try_from(pid) else {
            return Ended::Failed(format!("Couldn't run {shown}: its pid is out of range."));
        };

        let (live, closed) = (Arc::clone(self), tx.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0; 64 << 10];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => live.lock().output.push(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = closed.send(Event::Closed);
        });
        std::thread::spawn(move || {
            exited(pid);
            let _ = tx.send(Event::Exited);
        });

        let deadline = Instant::now().checked_add(timeout);
        let (mut done, mut closed) = (false, false);
        let mut why: Option<Ended> = None;
        let mut grace: Option<Instant> = None;
        let term = |grace: &mut Option<Instant>| {
            if grace.is_none() {
                signal(group, libc::SIGTERM);
                *grace = Instant::now().checked_add(GRACE);
            }
        };
        while !(done && closed) {
            let until = grace.or(deadline);
            let left = until.map_or(Duration::MAX, |until| {
                until.saturating_duration_since(Instant::now())
            });
            match rx.recv_timeout(left) {
                Ok(Event::Exited) => done = true,
                Ok(Event::Closed) => closed = true,
                Ok(Event::Cancel) => {
                    why.get_or_insert(Ended::Cancelled);
                    term(&mut grace);
                }
                Err(_) if grace.is_some() => break,
                Err(_) => {
                    why = Some(Ended::TimedOut);
                    term(&mut grace);
                }
            }
            if done && !closed {
                term(&mut grace);
            }
        }
        // The leader isn't reaped yet, so its group's id can't belong to
        // anyone else.
        signal(group, libc::SIGKILL);
        while !done {
            if let Ok(Event::Exited) = rx.recv() {
                done = true;
            }
        }
        let status = child.wait();
        let until = Instant::now().checked_add(GRACE);
        while !closed {
            let left = until.map_or(Duration::ZERO, |until| {
                until.saturating_duration_since(Instant::now())
            });
            match rx.recv_timeout(left) {
                Ok(Event::Closed) => closed = true,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        match (why, status) {
            (Some(why), _) => why,
            (None, Ok(status)) => match status.code() {
                Some(code) => Ended::Exited(code),
                None => Ended::Signaled(status.signal().unwrap_or(0)),
            },
            (None, Err(e)) => Ended::Failed(format!("{shown}: {e}")),
        }
    }

    fn cancel(&self) {
        let mut going = self.lock();
        going.cancelled = true;
        if let Some(tx) = &going.current {
            let _ = tx.send(Event::Cancel);
        }
    }

    fn public(&self) -> Run {
        let going = self.lock();
        let millis = |took: Duration| u64::try_from(took.as_millis()).unwrap_or(u64::MAX);
        Run {
            id: self.id,
            origin: self.origin.clone(),
            state: going.state,
            started: self.started,
            plan: self.plan.clone(),
            progress: going
                .progress
                .iter()
                .map(|(began, ended)| match ended {
                    Some((ended, took)) => Progress {
                        ended: Some(ended.clone()),
                        millis: millis(*took),
                    },
                    None => Progress {
                        ended: None,
                        millis: millis(began.elapsed()),
                    },
                })
                .collect(),
            output: going.output.public(),
        }
    }
}

/// Signals the process group Monodrive made for an invocation.
fn signal(group: libc::pid_t, sig: libc::c_int) {
    // SAFETY: killpg has no memory preconditions; `group` is the group of
    // a child this process hasn't reaped.
    unsafe { libc::killpg(group, sig) };
}

/// The owner's runs, newest first: those going and the latest ended.
pub(crate) struct Runs {
    runs: Mutex<(u64, Vec<Arc<Live>>)>,
    /// Wakes the owner's poll loop when a run ends, so it can exit once
    /// idle.
    wake: Option<UnixStream>,
}

impl Runs {
    pub(crate) fn new(wake: Option<UnixStream>) -> Arc<Self> {
        Arc::new(Self {
            runs: Mutex::default(),
            wake,
        })
    }

    fn lock(&self) -> MutexGuard<'_, (u64, Vec<Arc<Live>>)> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts `plan` on its own thread and returns its id, or the id of the
    /// run `request` already started.
    pub(crate) fn start(
        self: &Arc<Self>,
        request: String,
        origin: Origin,
        plan: Plan,
    ) -> Result<u64, String> {
        let mut runs = self.lock();
        let (next, list) = &mut *runs;
        if let Some(known) = list.iter().find(|run| run.request == request) {
            return Ok(known.id);
        }
        let running = |run: &Arc<Live>| run.lock().state == RunState::Running;
        if list.iter().filter(|run| running(run)).count() >= RUNNING {
            return Err(format!(
                "{RUNNING} commands are already running. Wait for one to end, or cancel one."
            ));
        }
        *next += 1;
        let live = Arc::new(Live::new(*next, request, origin, plan));
        list.insert(0, live.clone());
        let mut ended = 0;
        list.retain(|run| {
            running(run) || {
                ended += 1;
                ended <= SHOWN
            }
        });
        drop(runs);
        let id = live.id;
        let runs = Arc::clone(self);
        std::thread::spawn(move || {
            live.execute();
            if let Some(mut wake) = runs.wake.as_ref() {
                let _ = wake.write_all(&[0]);
            }
        });
        Ok(id)
    }

    pub(crate) fn list(&self) -> Vec<Run> {
        self.lock().1.iter().map(|run| run.public()).collect()
    }

    /// Stops run `id`: the invocation going gets SIGTERM, then SIGKILL,
    /// and the rest never start.
    pub(crate) fn cancel(&self, id: u64) -> Result<(), String> {
        let runs = self.lock();
        let run = runs
            .1
            .iter()
            .find(|run| run.id == id)
            .ok_or_else(|| format!("No run {id}."))?;
        if run.lock().state != RunState::Running {
            return Err(format!("Run {id} has already ended."));
        }
        run.cancel();
        Ok(())
    }

    pub(crate) fn busy(&self) -> bool {
        self.lock()
            .1
            .iter()
            .any(|run| run.lock().state == RunState::Running)
    }
}

#[cfg(test)]
pub(crate) mod tests;
