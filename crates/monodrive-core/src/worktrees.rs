//! Git worktrees: the repository a folder belongs to, its worktrees, and one
//! worktree's changes, read from Git itself. Git stays the authority; Monodrive
//! keeps only the repositories the user registered, in places.json.

use crate::places::{self, Availability};
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The most Monodrive reads of what git prints, and of what it says went
/// wrong.
const OUTPUT_LIMIT: u64 = 8 << 20;
const ERROR_LIMIT: u64 = 64 << 10;

/// The git Monodrive runs, and how long one command may take.
#[derive(Clone, Debug)]
pub struct Git {
    pub program: PathBuf,
    pub timeout: Duration,
}

/// A repository's worktrees as `git worktree list` gives them, the main
/// one first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    /// The repository's identity: its common Git folder, past any links.
    pub common_dir: PathBuf,
    pub worktrees: Vec<Worktree>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `None` for a bare repository and before the first commit.
    pub head: Option<String>,
    /// Without `refs/heads/`. `None` when detached or bare.
    pub branch: Option<String>,
    /// The repository's own checkout, or the bare repository itself.
    pub main: bool,
    pub bare: bool,
    /// Git's lock, with its reason, empty when none was given. It stops
    /// pruning; it doesn't say anyone is using the worktree.
    pub locked: Option<String>,
    /// Why Git would prune the registration. Information only: it never
    /// means the folder is safe to delete.
    pub prunable: Option<String>,
    pub state: State,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Ok,
    /// The folder is gone, maybe on a drive that isn't connected.
    Missing,
    /// The folder is there but is no longer this worktree's checkout.
    Stale,
    /// The folder couldn't be read.
    Unreadable(String),
}

/// One worktree's changes, from `git status`, against what's known
/// locally. Nothing is fetched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// `None` before the first commit.
    pub head: Option<String>,
    /// `None` when detached.
    pub branch: Option<String>,
    pub upstream: Option<String>,
    /// Commits ahead of and behind the upstream, when its ref is here.
    pub ahead_behind: Option<(u64, u64)>,
    pub staged: usize,
    pub unstaged: usize,
    pub conflicted: usize,
    pub untracked: usize,
}

/// A repository the user asked Monodrive to remember.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub common_dir: PathBuf,
    /// Its main worktree, or the bare repository, as shown.
    pub path: PathBuf,
}

impl Status {
    pub fn is_clean(&self) -> bool {
        self.staged + self.unstaged + self.conflicted + self.untracked == 0
    }
}

impl Registration {
    /// Whether the repository is still there, without running git.
    pub fn resolve(&self) -> Availability {
        let shown = self.path.display();
        match std::fs::metadata(&self.common_dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Availability::Unavailable(format!("{shown} no longer exists."))
            }
            Err(e) => Availability::Unavailable(format!("{shown}: {e}")),
            Ok(_) if self.common_dir.join("HEAD").is_file() => {
                Availability::Available(self.path.clone())
            }
            Ok(_) => Availability::Unavailable(format!("{shown} is no longer a Git repository.")),
        }
    }
}

impl Git {
    /// The first `git` on `PATH`, then Homebrew's, so the window, which
    /// gets launchd's `PATH`, finds the git a terminal does. `/usr/bin/git`
    /// comes last and only when developer tools are installed: without them
    /// it opens the Command Line Tools installer instead of running.
    pub fn find() -> Result<Self, String> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dirs = std::env::split_paths(&path)
            .filter(|dir| dir != Path::new("/usr/bin"))
            .chain(["/opt/homebrew/bin".into(), "/usr/local/bin".into()]);
        let found = dirs
            .map(|dir| dir.join("git"))
            .find(|git| executable(git))
            .or_else(|| developer_tools().then(|| "/usr/bin/git".into()));
        let program = found.ok_or(
            "Git isn't installed. Install the Command Line Tools with xcode-select --install, or Git from Homebrew.",
        )?;
        Ok(Self {
            program,
            timeout: Duration::from_secs(10),
        })
    }

    /// Runs git in `dir` with `args`, in an environment of Monodrive's own,
    /// and returns what it printed. Stops it when it runs past the timeout
    /// or prints more than Monodrive reads.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
        let passed = ["HOME", "XDG_CONFIG_HOME", "DEVELOPER_DIR"]
            .into_iter()
            .filter_map(|key| Some((key, std::env::var_os(key)?)));
        let mut child = Command::new(&self.program)
            .arg("-C")
            .arg(dir)
            .args(["--no-pager", "-c", "core.fsmonitor=false"])
            .args(args)
            .env_clear()
            .envs(passed)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("LANG", "C")
            .env("LC_ALL", "C")
            // Status otherwise refreshes the index, taking its lock.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Couldn't run {}: {e}", self.program.display()))?;

        enum Event {
            Printed(io::Result<Vec<u8>>),
            Exited,
        }
        let (tx, rx) = mpsc::channel();
        let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(stop(&mut child, "git's output wasn't piped.".into()));
        };
        let printed = tx.clone();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let read = stdout.take(OUTPUT_LIMIT + 1).read_to_end(&mut out);
            let _ = printed.send(Event::Printed(read.map(|_| out)));
        });
        let said = std::thread::spawn(move || {
            let mut said = Vec::new();
            let _ = (&mut stderr).take(ERROR_LIMIT).read_to_end(&mut said);
            let _ = io::copy(&mut stderr, &mut io::sink());
            said
        });
        let pid = child.id();
        std::thread::spawn(move || {
            exited(pid);
            let _ = tx.send(Event::Exited);
        });

        let deadline = Instant::now().checked_add(self.timeout);
        let (mut out, mut done) = (None, false);
        while out.is_none() || !done {
            let left = deadline.map_or(Duration::MAX, |d| {
                d.saturating_duration_since(Instant::now())
            });
            match rx.recv_timeout(left) {
                Ok(Event::Exited) => done = true,
                Ok(Event::Printed(Ok(bytes))) if bytes.len() as u64 > OUTPUT_LIMIT => {
                    let why = format!(
                        "git printed more than {} MB, so Monodrive stopped it.",
                        OUTPUT_LIMIT >> 20
                    );
                    return Err(stop(&mut child, why));
                }
                Ok(Event::Printed(Ok(bytes))) => out = Some(bytes),
                Ok(Event::Printed(Err(e))) => {
                    return Err(stop(&mut child, format!("Couldn't read git's output: {e}")));
                }
                Err(_) => {
                    let why = format!(
                        "git didn't finish within {:?}, so Monodrive stopped it.",
                        self.timeout
                    );
                    return Err(stop(&mut child, why));
                }
            }
        }
        let status = child.wait().map_err(|e| format!("git: {e}"))?;
        if status.success() {
            return Ok(out.unwrap_or_default());
        }
        let said = said.join().unwrap_or_default();
        let said = String::from_utf8_lossy(&said);
        let last = said.lines().map(str::trim).rfind(|line| !line.is_empty());
        if last.is_some_and(|line| line.contains("not a git repository")) {
            return Err(format!("{} isn't in a Git repository.", dir.display()));
        }
        Err(last.map_or_else(|| format!("git failed: {status}"), String::from))
    }
}

/// Kills the git this call started, and only it, and reaps it.
fn stop(child: &mut Child, why: String) -> String {
    let _ = child.kill();
    let _ = child.wait();
    why
}

/// Waits for the child `pid` to exit without reaping it, so its pid can't
/// be reused before `stop` or `wait` reaps it.
fn exited(pid: libc::id_t) {
    loop {
        // SAFETY: `info` is a valid siginfo_t for waitid to fill.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: waits on our own child; WNOWAIT leaves it to be reaped.
        let r = unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if r == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Whether Xcode or the Command Line Tools are installed, asked of
/// `xcode-select`, which never offers to install them.
fn developer_tools() -> bool {
    let Ok(out) = Command::new("/usr/bin/xcode-select")
        .arg("-p")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let dir = std::ffi::OsStr::from_bytes(out.stdout.trim_ascii());
    out.status.success() && executable(&Path::new(dir).join("usr/bin/git"))
}

/// The folder `dir` is checked to be one Git can be asked about.
fn folder(dir: &Path) -> Result<(), String> {
    match std::fs::metadata(dir) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} isn't a folder.", dir.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(format!("{} doesn't exist.", dir.display()))
        }
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// The common Git folder of the repository `dir` is in, past any links: the
/// same for the repository, any folder in it, and each of its worktrees.
pub fn common_dir(git: &Git, dir: &Path) -> Result<PathBuf, String> {
    folder(dir)?;
    let out = git.run(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let out = out.strip_suffix(b"\n").unwrap_or(&out);
    let common = Path::new(std::ffi::OsStr::from_bytes(out));
    std::fs::canonicalize(common).map_err(|e| format!("{}: {e}", common.display()))
}

/// The worktrees of the repository `dir` is in. Runs git twice and reads
/// only the worktrees' own folders; it fetches nothing and changes nothing.
pub fn list(git: &Git, dir: &Path) -> Result<Inventory, String> {
    let common_dir = common_dir(git, dir)?;
    let out = git.run(&common_dir, &["worktree", "list", "--porcelain", "-z"])?;
    let mut worktrees = parse_list(&out);
    for worktree in &mut worktrees {
        worktree.state = state(worktree);
    }
    if let Some(first) = worktrees.first_mut() {
        first.main = true;
    }
    Ok(Inventory {
        common_dir,
        worktrees,
    })
}

/// The changes in the worktree at `dir`, read without taking Git's locks.
pub fn status(git: &Git, dir: &Path) -> Result<Status, String> {
    folder(dir)?;
    let out = git.run(
        dir,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=normal",
        ],
    )?;
    Ok(parse_status(&out))
}

/// Remembers the repository `dir` is in, once however it's reached, and
/// returns it as registered. Changes nothing in the repository.
pub fn register(git: &Git, store: &places::Store, dir: &Path) -> Result<Registration, String> {
    let inventory = list(git, dir)?;
    let path = inventory
        .worktrees
        .first()
        .map_or_else(|| inventory.common_dir.clone(), |main| main.path.clone());
    let registration = Registration {
        common_dir: inventory.common_dir,
        path,
    };
    store.register_repository(registration.clone())?;
    Ok(registration)
}

/// `git worktree list --porcelain -z`: attributes end in NUL, and an empty
/// one ends each worktree, so reasons and paths may hold newlines.
fn parse_list(out: &[u8]) -> Vec<Worktree> {
    let mut worktrees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in out.split(|b| *b == 0) {
        if field.is_empty() {
            worktrees.extend(current.take());
            continue;
        }
        let (key, value) = match field.iter().position(|b| *b == b' ') {
            Some(at) => (&field[..at], Some(&field[at + 1..])),
            None => (field, None),
        };
        let text = || String::from_utf8_lossy(value.unwrap_or_default()).into_owned();
        if key == b"worktree" {
            worktrees.extend(current.take());
            current = Some(Worktree {
                path: PathBuf::from(std::ffi::OsStr::from_bytes(value.unwrap_or_default())),
                head: None,
                branch: None,
                main: false,
                bare: false,
                locked: None,
                prunable: None,
                state: State::Ok,
            });
            continue;
        }
        let Some(worktree) = current.as_mut() else {
            continue;
        };
        match key {
            b"HEAD" => worktree.head = Some(text()).filter(|oid| oid.bytes().any(|b| b != b'0')),
            b"branch" => {
                let branch = text();
                let short = branch.strip_prefix("refs/heads/").map(str::to_owned);
                worktree.branch = Some(short.unwrap_or(branch));
            }
            b"bare" => worktree.bare = true,
            b"locked" => worktree.locked = Some(text()),
            b"prunable" => worktree.prunable = Some(text()),
            _ => {}
        }
    }
    worktrees.extend(current);
    worktrees
}

/// Whether the worktree's folder is there and still its checkout.
fn state(worktree: &Worktree) -> State {
    if worktree.bare {
        return State::Ok;
    }
    let checkout = std::fs::metadata(&worktree.path)
        .and_then(|_| std::fs::symlink_metadata(worktree.path.join(".git")));
    match checkout {
        Ok(_) if worktree.prunable.is_none() => State::Ok,
        Ok(_) => State::Stale,
        Err(e) if e.kind() == io::ErrorKind::NotFound && !worktree.path.exists() => State::Missing,
        Err(e) if e.kind() == io::ErrorKind::NotFound => State::Stale,
        Err(e) => State::Unreadable(e.to_string()),
    }
}

/// `git status --porcelain=v2 -z --branch`: headers, then a record per
/// changed path. A rename's record is followed by its old path.
fn parse_status(out: &[u8]) -> Status {
    let mut status = Status::default();
    let mut fields = out.split(|b| *b == 0);
    while let Some(field) = fields.next() {
        let line = String::from_utf8_lossy(field);
        let mut words = line.split(' ');
        match words.next() {
            Some("#") => match (words.next(), words.next()) {
                (Some("branch.oid"), Some(oid)) if oid != "(initial)" => {
                    status.head = Some(oid.into());
                }
                (Some("branch.head"), Some(head)) if head != "(detached)" => {
                    status.branch = Some(line["# branch.head ".len()..].into());
                }
                (Some("branch.upstream"), Some(_)) => {
                    status.upstream = Some(line["# branch.upstream ".len()..].into());
                }
                (Some("branch.ab"), Some(ahead)) => {
                    let ahead = ahead.trim_start_matches('+').parse().ok();
                    let behind = words
                        .next()
                        .and_then(|b| b.trim_start_matches('-').parse().ok());
                    status.ahead_behind = ahead.zip(behind);
                }
                _ => {}
            },
            Some(kind @ ("1" | "2")) => {
                let xy = words.next().unwrap_or_default().as_bytes();
                status.staged += usize::from(xy.first().is_some_and(|x| *x != b'.'));
                status.unstaged += usize::from(xy.get(1).is_some_and(|y| *y != b'.'));
                if kind == "2" {
                    fields.next();
                }
            }
            Some("u") => status.conflicted += 1,
            Some("?") => status.untracked += 1,
            _ => {}
        }
    }
    status
}

#[cfg(test)]
pub(crate) mod tests;
