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
/// The most read of a `.git` file or a worktree's `gitdir`, which hold a
/// path.
const LINK_LIMIT: u64 = 64 << 10;

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
    /// The folder is there, but its link to the repository is gone.
    Stale,
    /// The folder holds another checkout than this worktree's.
    Replaced,
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
    /// The first `SHOWN` changed paths, each after one letter as `git
    /// status --short` gives it: M, A, D, R, U or ?. An untracked folder is
    /// one path, ending in `/`.
    pub files: Vec<String>,
    /// Every changed path, of which `files` is the start.
    pub paths: usize,
    /// Every changed path and its letter, hashed, so a list that differs past
    /// what `files` shows still differs.
    pub changed: u64,
}

/// How many changed files or commits a confirmation lists.
pub const SHOWN: usize = 10;

/// What removing a worktree's folder would put at risk, beyond what Git keeps
/// in the repository: changes no commit holds, and commits that no other
/// branch, tag or remote does. Nothing is fetched, so a remote is as last
/// known here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Risk {
    /// The first `SHOWN` changed paths, as `Status::files`.
    pub files: Vec<String>,
    pub paths: usize,
    /// As `Status::changed`.
    pub changed: u64,
    /// The first `SHOWN` of those commits, newest first.
    pub commits: Vec<Commit>,
    pub unpushed: usize,
    /// HEAD is on no branch, so nothing keeps its commits once the
    /// registration goes: Git may drop them at its next cleanup.
    pub detached: bool,
}

/// A commit, by its full hash, which shows short.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
}

impl std::fmt::Display for Commit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let short: String = self.hash.chars().take(7).collect();
        write!(f, "{short} {}", self.subject)
    }
}

/// A worktree Monodrive may remove, and what removing it means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removal {
    pub worktree: Worktree,
    /// The folder Git keeps for it, `worktrees/<name>` in the common Git
    /// folder. Without it Git no longer lists the worktree, and its branch
    /// may be checked out elsewhere; the branch itself stays.
    pub registration: PathBuf,
    /// The checkout, when it's there to remove. `None` when only Git's
    /// registration is left: the folder is gone or no longer linked.
    pub folder: Option<PathBuf>,
    pub risk: Risk,
}

/// Every repository a scan found with a worktree besides its own checkout,
/// and each registered one, with what Git couldn't answer for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub repositories: Vec<Inventory>,
    /// A repository, or the index, and why it couldn't be read.
    pub problems: Vec<String>,
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

impl Risk {
    /// Nothing that only this worktree holds.
    pub fn is_empty(&self) -> bool {
        self.paths == 0 && self.unpushed == 0
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
            Said(Vec<u8>),
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
        let said = tx.clone();
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = (&mut stderr).take(ERROR_LIMIT).read_to_end(&mut bytes);
            let _ = io::copy(&mut stderr, &mut io::sink());
            let _ = said.send(Event::Said(bytes));
        });
        let pid = child.id();
        std::thread::spawn(move || {
            exited(pid);
            let _ = tx.send(Event::Exited);
        });

        let deadline = Instant::now().checked_add(self.timeout);
        let next = || {
            let left = deadline.map_or(Duration::MAX, |d| {
                d.saturating_duration_since(Instant::now())
            });
            rx.recv_timeout(left).ok()
        };
        let (mut out, mut done, mut said) = (None, false, None);
        while out.is_none() || !done {
            match next() {
                Some(Event::Exited) => done = true,
                Some(Event::Printed(Ok(bytes))) if bytes.len() as u64 > OUTPUT_LIMIT => {
                    let why = format!(
                        "git printed more than {} MB, so Monodrive stopped it.",
                        OUTPUT_LIMIT >> 20
                    );
                    return Err(stop(&mut child, why));
                }
                Some(Event::Printed(Ok(bytes))) => out = Some(bytes),
                Some(Event::Printed(Err(e))) => {
                    return Err(stop(&mut child, format!("Couldn't read git's output: {e}")));
                }
                Some(Event::Said(bytes)) => said = Some(bytes),
                None => {
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
        // Something git started may keep its errors open past its exit.
        while said.is_none() {
            match next() {
                Some(Event::Said(bytes)) => said = Some(bytes),
                Some(_) => {}
                None => return Err(format!("git failed: {status}")),
            }
        }
        let said = String::from_utf8_lossy(said.as_deref().unwrap_or_default());
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
pub(crate) fn exited(pid: libc::id_t) {
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

pub(crate) fn executable(path: &Path) -> bool {
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
    if let Some(first) = worktrees.first_mut() {
        first.main = true;
    }
    for worktree in &mut worktrees {
        worktree.state = state(worktree, &common_dir);
    }
    Ok(Inventory {
        common_dir,
        worktrees,
    })
}

/// The changes in the worktree at `dir`, read without taking Git's locks.
/// Refuses a repository whose own config gives a filter a command, since
/// status runs it on any file whose times changed. Submodules are compared
/// by commit only, so status never runs git, and their filters, inside them.
pub fn status(git: &Git, dir: &Path) -> Result<Status, String> {
    folder(dir)?;
    let config = git.run(dir, &["config", "--list", "--show-scope", "-z"])?;
    let filters = own_filters(&config);
    if !filters.is_empty() {
        return Err(format!(
            "This repository's own config sets filters that run commands ({}), so Monodrive skipped status.",
            filters.join(", ")
        ));
    }
    let out = git.run(
        dir,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=normal",
            "--ignore-submodules=dirty",
        ],
    )?;
    Ok(parse_status(&out))
}

/// The filters the repository's config, not the user's own, gives a clean
/// or process command, from `git config --list --show-scope -z`: a scope,
/// then `key` and value split by a newline.
fn own_filters(config: &[u8]) -> Vec<String> {
    let mut fields = config.split(|b| *b == 0);
    let mut names: Vec<String> = Vec::new();
    while let (Some(scope), Some(entry)) = (fields.next(), fields.next()) {
        let entry = String::from_utf8_lossy(entry);
        let (key, value) = entry.split_once('\n').unwrap_or((&entry, ""));
        let Some((name, var)) = key.strip_prefix("filter.").and_then(|k| k.rsplit_once('.')) else {
            continue;
        };
        if matches!(scope, b"local" | b"worktree")
            && matches!(var, "clean" | "process")
            && !value.is_empty()
            && !names.iter().any(|known| known == name)
        {
            names.push(name.into());
        }
    }
    names
}

/// What removing the worktree at `dir` would put at risk. Runs status, so it
/// refuses what status refuses, then reads the commits only HEAD reaches.
pub fn risk(git: &Git, dir: &Path) -> Result<Risk, String> {
    let status = status(git, dir)?;
    let (unpushed, commits) = match &status.head {
        Some(head) => lone_commits(git, dir, head, status.branch.as_deref())?,
        None => (0, Vec::new()),
    };
    Ok(Risk {
        files: status.files,
        paths: status.paths,
        changed: status.changed,
        commits,
        unpushed,
        detached: status.branch.is_none(),
    })
}

/// How many commits `head` reaches that no branch but `branch`, no tag and
/// no remote does, and the newest `SHOWN` of them. `dir` is any folder of
/// the repository, so a missing worktree's commits count too.
fn lone_commits(
    git: &Git,
    dir: &Path,
    head: &str,
    branch: Option<&str>,
) -> Result<(usize, Vec<Commit>), String> {
    // `--exclude` takes the branch's name as `--branches` sees it.
    let own = branch.map(|branch| format!("--exclude={branch}"));
    let mut only = vec![head, "--not"];
    only.extend(own.as_deref());
    only.extend(["--branches", "--tags", "--remotes"]);
    let count = git.run(dir, &[&["rev-list", "--count"][..], &only].concat())?;
    let count: usize = String::from_utf8_lossy(&count)
        .trim()
        .parse()
        .map_err(|_| "git rev-list didn't print a count.".to_owned())?;
    if count == 0 {
        return Ok((0, Vec::new()));
    }
    let shown = SHOWN.to_string();
    let log = [&["log", "--format=%H %s", "-n", shown.as_str()][..], &only].concat();
    let out = git.run(dir, &log)?;
    let commits = String::from_utf8_lossy(&out)
        .lines()
        .map(|line| {
            let (hash, subject) = line.split_once(' ').unwrap_or((line, ""));
            Commit {
                hash: hash.into(),
                subject: subject.into(),
            }
        })
        .collect();
    Ok((count, commits))
}

/// What removing the worktree at `path`, of the repository `repo` is in,
/// would do, or why Monodrive won't: the repository's own checkout, a bare
/// repository and a worktree Git has locked stay. `repo` may be the worktree
/// itself while its folder is there. Changes nothing.
pub fn removal(git: &Git, repo: &Path, path: &Path) -> Result<Removal, String> {
    let inventory = list(git, repo)?;
    let real = std::fs::canonicalize(path).ok();
    let worktree = inventory
        .worktrees
        .iter()
        .find(|w| w.path == path || real.is_some() && std::fs::canonicalize(&w.path).ok() == real)
        .cloned()
        .ok_or_else(|| format!("{} isn't a worktree of {}.", path.display(), repo.display()))?;
    let shown = worktree.path.display();
    if worktree.main || worktree.bare {
        return Err(format!(
            "{shown} is the repository's own checkout. Monodrive removes only linked worktrees."
        ));
    }
    if let Some(reason) = &worktree.locked {
        let why = if reason.is_empty() {
            String::new()
        } else {
            format!(": {reason}")
        };
        return Err(format!(
            "{shown} is locked by Git{why}. Unlock it with git worktree unlock once nothing uses it."
        ));
    }
    // The Trash would take the link and leave the checkout it leads to.
    if std::fs::symlink_metadata(&worktree.path).is_ok_and(|meta| meta.is_symlink()) {
        return Err(format!(
            "{shown} is a link to another folder, so Monodrive leaves it alone."
        ));
    }
    if let Some(inside) = inventory
        .worktrees
        .iter()
        .find(|other| other.path != worktree.path && other.path.starts_with(&worktree.path))
    {
        return Err(format!(
            "{shown} holds another worktree, {}, so it stays.",
            inside.path.display()
        ));
    }
    let registration = registration(&inventory.common_dir, &worktree.path)?;
    // A linked worktree's submodules keep their repositories in its
    // registration, and status doesn't look inside them.
    if registration.join("modules").exists() {
        return Err(format!(
            "{shown} has submodules checked out, which Monodrive can't look into, so it stays. Git's worktree remove refuses it too."
        ));
    }
    let (folder, risk) = match &worktree.state {
        State::Ok => (Some(worktree.path.clone()), risk(git, &worktree.path)?),
        // Git would prune it, yet the checkout still leads to it: forgetting
        // it would leave a checkout whose `.git` leads nowhere.
        State::Stale if is_checkout(&worktree, &inventory.common_dir).unwrap_or(false) => {
            return Err(format!(
                "{shown} is still linked to its repository, though Git would prune it, so Monodrive leaves it alone. git worktree repair can fix the link."
            ));
        }
        // The folder's changes can't be read, but its commits can.
        State::Missing | State::Stale => {
            let (unpushed, commits) = match &worktree.head {
                Some(head) => {
                    lone_commits(git, &inventory.common_dir, head, worktree.branch.as_deref())?
                }
                None => (0, Vec::new()),
            };
            let risk = Risk {
                commits,
                unpushed,
                detached: worktree.branch.is_none(),
                ..Risk::default()
            };
            (None, risk)
        }
        State::Replaced => {
            return Err(format!(
                "{shown} holds another checkout now, so Monodrive leaves it alone."
            ));
        }
        State::Unreadable(why) => return Err(format!("{shown}: {why}")),
    };
    Ok(Removal {
        worktree,
        registration,
        folder,
        risk,
    })
}

/// The folder in `common_dir/worktrees` that registers the worktree at
/// `path`: the one its `.git` names, or, when that's gone, the one whose
/// `gitdir` names `path`'s `.git`, which Git may have written relative to it.
fn registration(common_dir: &Path, path: &Path) -> Result<PathBuf, String> {
    let admin = common_dir.join("worktrees");
    let named = link(&path.join(".git"), b"gitdir: ").ok().flatten();
    if let Some(dir) = named.filter(|dir| dir.parent() == Some(admin.as_path())) {
        return Ok(dir);
    }
    let dirs = std::fs::read_dir(&admin).map_err(|e| format!("{}: {e}", admin.display()))?;
    dirs.flatten()
        .map(|dir| dir.path())
        .find(|dir| {
            linked(&dir.join("gitdir"), b"")
                .ok()
                .flatten()
                .is_some_and(|dot_git| normalized(&dot_git).parent() == Some(path))
        })
        .ok_or_else(|| format!("Git keeps no registration for {}.", path.display()))
}

/// `path` with `.` and `..` worked out by name, for a path that may not
/// exist to be followed.
fn normalized(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Every repository with a linked worktree that `search`, the index's
/// answers for a query, finds a `.git` file of, and every registered
/// repository, each listed once by Git. A linked worktree's `.git` is a file
/// that names its repository, so Git runs once per repository. Fetches and
/// changes nothing. Runs Git, so call it off the UI thread.
pub fn scan(
    git: &Git,
    store: &places::Store,
    search: impl Fn(&str, usize) -> Result<Vec<crate::entries::Entry>, String>,
) -> Found {
    let mut found = Found::default();
    let mut commons: Vec<PathBuf> = Vec::new();
    match search(GIT_FILES, SCANNED) {
        Ok(entries) => {
            for entry in entries {
                let in_trash = entry
                    .path
                    .components()
                    .any(|part| matches!(part.as_os_str().to_str(), Some(".Trash" | ".Trashes")));
                if entry.name != ".git" || in_trash {
                    continue;
                }
                if let Some(common) = common_of_link(&entry.path)
                    && !commons.contains(&common)
                {
                    commons.push(common);
                }
            }
        }
        Err(e) => found.problems.push(e),
    }
    for repo in store.repositories().unwrap_or_default() {
        if !commons.contains(&repo.common_dir) {
            commons.push(repo.common_dir);
        }
    }
    for common in commons {
        match list(git, &common) {
            Ok(inventory) => found.repositories.push(inventory),
            Err(e) => found.problems.push(format!("{}: {e}", common.display())),
        }
    }
    found.repositories.sort_by(|a, b| {
        let main = |i: &Inventory| i.worktrees.first().map(|w| w.path.clone());
        main(a).cmp(&main(b))
    });
    found
}

/// What the index is asked for: files named exactly `.git`.
const GIT_FILES: &str = "^.git .git$ kind:file";
/// The most `.git` files a scan reads.
const SCANNED: usize = 20_000;

/// The common Git folder a linked worktree's `.git` file leads to: its
/// `gitdir` names `worktrees/<name>`, whose `commondir` names the common
/// folder, relative to it. A submodule's `.git` names a folder under
/// `modules` instead, which isn't a worktree.
fn common_of_link(dot_git: &Path) -> Option<PathBuf> {
    let admin = link(dot_git, b"gitdir: ").ok()??;
    if admin.parent()?.file_name()? != "worktrees" {
        return None;
    }
    let common = linked(&admin.join("commondir"), b"").ok()??;
    std::fs::canonicalize(common).ok()
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
fn state(worktree: &Worktree, common_dir: &Path) -> State {
    if worktree.bare {
        return State::Ok;
    }
    match is_checkout(worktree, common_dir) {
        Ok(true) if worktree.prunable.is_none() => State::Ok,
        Ok(true) => State::Stale,
        Ok(false) => State::Replaced,
        Err(e) if e.kind() == io::ErrorKind::NotFound && !worktree.path.exists() => State::Missing,
        Err(e) if e.kind() == io::ErrorKind::NotFound => State::Stale,
        Err(e) => State::Unreadable(e.to_string()),
    }
}

/// Whether the folder's `.git` leads to this worktree's Git folder: the
/// common one for the main worktree, or the one Git keeps for a linked
/// worktree under `worktrees/`, whose `gitdir` file leads back.
fn is_checkout(worktree: &Worktree, common_dir: &Path) -> io::Result<bool> {
    std::fs::metadata(&worktree.path)?;
    let dot_git = worktree.path.join(".git");
    let git_dir = if std::fs::metadata(&dot_git)?.is_dir() {
        std::fs::canonicalize(&dot_git)?
    } else {
        match link(&dot_git, b"gitdir: ")? {
            Some(git_dir) => git_dir,
            None => return Ok(false),
        }
    };
    if worktree.main {
        return Ok(git_dir == common_dir);
    }
    if git_dir.parent() != Some(&common_dir.join("worktrees")) {
        return Ok(false);
    }
    let back = link(&git_dir.join("gitdir"), b"")?;
    Ok(back == Some(std::fs::canonicalize(&dot_git)?))
}

/// The real path a one-line link file such as `.git` names after `prefix`,
/// relative to the file's folder unless it's absolute. Anything in a
/// worktree's folder can stand in its place, so it's opened without waiting
/// for a writer, as a FIFO would make it, and read only as far as a link
/// could go.
fn link(file: &Path, prefix: &[u8]) -> io::Result<Option<PathBuf>> {
    linked(file, prefix)?.map(std::fs::canonicalize).transpose()
}

/// What `link` reads, before following it, so it names a path that may be
/// gone.
fn linked(file: &Path, prefix: &[u8]) -> io::Result<Option<PathBuf>> {
    use std::os::unix::fs::OpenOptionsExt;
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(file)?;
    let shown = file.display();
    if !opened.metadata()?.is_file() {
        let why = format!("{shown} isn't a regular file.");
        return Err(io::Error::new(io::ErrorKind::InvalidData, why));
    }
    let mut text = Vec::new();
    opened.take(LINK_LIMIT + 1).read_to_end(&mut text)?;
    if text.len() as u64 > LINK_LIMIT {
        let why = format!("{shown} is over {} KB.", LINK_LIMIT >> 10);
        return Err(io::Error::new(io::ErrorKind::InvalidData, why));
    }
    let text = text.strip_suffix(b"\n").unwrap_or(&text);
    let Some(to) = text.strip_prefix(prefix) else {
        return Ok(None);
    };
    let base = file.parent().unwrap_or(Path::new("/"));
    Ok(Some(base.join(std::ffi::OsStr::from_bytes(to))))
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
                let letter = match xy {
                    [_, y, ..] if *y != b'.' => *y,
                    [x, ..] => *x,
                    [] => b'M',
                };
                // The path follows 8 fields, and a rename's score.
                let path = line.splitn(if kind == "2" { 10 } else { 9 }, ' ').last();
                listed(&mut status, char::from(letter), path);
                if kind == "2" {
                    fields.next();
                }
            }
            Some("u") => {
                status.conflicted += 1;
                listed(&mut status, 'U', line.splitn(11, ' ').last());
            }
            Some("?") => {
                status.untracked += 1;
                listed(&mut status, '?', line.get(2..));
            }
            _ => {}
        }
    }
    status
}

/// Counts a changed path, and keeps it while fewer than `SHOWN` are.
fn listed(status: &mut Status, letter: char, path: Option<&str>) {
    let line = format!("{letter} {}", path.unwrap_or_default());
    status.paths += 1;
    // FNV-1a, which every Monodrive process works out the same.
    if status.changed == 0 {
        status.changed = 0xcbf2_9ce4_8422_2325;
    }
    for byte in line.bytes().chain([0]) {
        status.changed = (status.changed ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    if status.files.len() < SHOWN {
        status.files.push(line);
    }
}

#[cfg(test)]
pub(crate) mod tests;
