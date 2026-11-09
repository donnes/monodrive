//! Git repositories and worktrees: list_worktrees, worktree_status,
//! list_repos, register_repo and forget_repo. None fetches or takes a lock.

use super::{Call, Preview, local_one, short_timeout};
use crate::mcp::contract::{Planned, Reply, ToolError, shown};
use crate::mcp::server::Server;
use crate::ops::Request;
use crate::places::Availability;
use crate::worktrees::{self, Git, Worktree};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::time::Duration;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWorktrees {
    /// Any folder or worktree of the repository. Left out, every repository
    /// on the disk with a linked worktree, found through the index, and
    /// every registered one.
    path: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Tree {
    pub path: String,
    /// Absent when detached or bare.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    pub main: bool,
    pub bare: bool,
    /// Git's lock and its reason, maybe empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
    /// Why Git would prune it. Never a sign the folder is safe to delete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prunable: Option<String>,
    /// ok, missing, stale, replaced or unreadable.
    pub state: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Trees {
    pub worktrees: Vec<Tree>,
    /// Repositories, or the index, that couldn't be read, with why.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

impl Call for ListWorktrees {
    type Out = Trees;

    fn run(self, server: &mut Server) -> Result<Reply<Trees>, ToolError> {
        let git = server.git()?;
        let (repositories, problems) = match &self.path {
            Some(path) => {
                let inventory = worktrees::list(&git, &local_one(path, "list_worktrees")?)?;
                (vec![inventory], Vec::new())
            }
            None => {
                let places = server.places.clone();
                let found = match server.index() {
                    Ok(index) => {
                        worktrees::scan(&git, &places, |query, limit| index.find(query, limit))
                    }
                    Err(e) => worktrees::scan(&git, &places, |_, _| Err(e.message.clone())),
                };
                (found.repositories, found.problems)
            }
        };
        // Repositories a blank line apart, then what couldn't be read.
        let mut text = repositories
            .iter()
            .map(|repo| repo.worktrees.iter().map(line).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        if repositories.is_empty() {
            text.push_str("No worktrees found.\n");
        }
        for problem in &problems {
            text.push_str(&format!("Couldn't read: {problem}\n"));
        }
        let worktrees = repositories
            .iter()
            .flat_map(|repo| &repo.worktrees)
            .map(|worktree| Tree {
                path: shown(&worktree.path),
                branch: worktree.branch.clone(),
                head: worktree.head.clone(),
                main: worktree.main,
                bare: worktree.bare,
                locked: worktree.locked.clone(),
                prunable: worktree.prunable.clone(),
                state: match &worktree.state {
                    worktrees::State::Ok => "ok",
                    worktrees::State::Missing => "missing",
                    worktrees::State::Stale => "stale",
                    worktrees::State::Replaced => "replaced",
                    worktrees::State::Unreadable(_) => "unreadable",
                }
                .into(),
            })
            .collect();
        Ok(Reply::new(
            text,
            Trees {
                worktrees,
                problems,
            },
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveWorktree {
    /// The worktree, as list_worktrees gives it.
    path: Option<String>,
    /// Any folder of its repository, when the worktree's folder is gone.
    repo: Option<String>,
    #[serde(default)]
    dry_run: bool,
    /// A dry run's token: removes exactly what it showed.
    confirm: Option<String>,
    request_id: Option<String>,
}

/// What only the worktree holds, which would go to the Trash with it.
#[derive(Serialize, JsonSchema)]
pub struct AtRisk {
    pub path: String,
    /// Uncommitted changes, and the first few paths.
    pub uncommitted: usize,
    pub files: Vec<String>,
    /// Commits no other branch, tag or remote has, and the first few.
    pub unpushed: usize,
    pub commits: Vec<String>,
    /// HEAD is on no branch, so Git may drop those commits at its next
    /// cleanup, and undo can't bring them back after that.
    pub detached: bool,
    /// The folder is gone, so only Git's registration goes.
    pub folder_gone: bool,
    /// It stays.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl Call for RemoveWorktree {
    type Out = Planned<AtRisk>;

    fn run(self, server: &mut Server) -> Result<Reply<Self::Out>, ToolError> {
        let request_id = self.request_id.as_deref();
        if let Some(token) = &self.confirm {
            let Preview::Worktree(request) = server.kept(token, request_id)? else {
                return Err(ToolError::invalid("That token is another tool's plan."));
            };
            let done = server.change(request, request_id)?;
            server.spent(token, request_id);
            return Ok(done.ran());
        }
        let path = self.path.as_deref().ok_or_else(|| {
            ToolError::invalid("Give the worktree's path, or confirm a dry run's token.")
        })?;
        let path = local_one(path, "remove_worktree")?;
        let repo = match &self.repo {
            Some(repo) => local_one(repo, "remove_worktree")?,
            None => path.clone(),
        };
        let git = server.git()?;
        let removal = worktrees::removal(&git, &repo, &path)?;
        let request = Request::RemoveWorktree {
            repo,
            path: removal.worktree.path.clone(),
            confirmed: removal.risk.clone(),
            with_folder: removal.folder.is_some(),
        };
        // Nothing only it holds: it goes, and undo brings it back.
        if removal.risk.is_empty() && !self.dry_run {
            return server.change(request, request_id).map(Reply::ran);
        }
        let risk = &removal.risk;
        let plan = AtRisk {
            path: shown(&removal.worktree.path),
            uncommitted: risk.paths,
            files: risk.files.clone(),
            unpushed: risk.unpushed,
            commits: risk.commits.iter().map(ToString::to_string).collect(),
            detached: risk.detached,
            folder_gone: removal.folder.is_none(),
            branch: removal.worktree.branch.clone(),
        };
        let mut text = risk_text(&removal);
        let token = server.keep(Preview::Worktree(request));
        text.push_str(&format!(
            "Confirm with token {token} within 10 minutes to move it to the Trash. undo brings it back.\n"
        ));
        Ok(Reply::new(text, plan).planned(Some(token)))
    }
}

/// What only the worktree holds: its changed files, then the commits no
/// other branch, tag or remote has.
fn risk_text(removal: &worktrees::Removal) -> String {
    let risk = &removal.risk;
    let mut out = if risk.is_empty() {
        format!(
            "{} holds nothing that isn't elsewhere.\n",
            removal.worktree.path.display()
        )
    } else {
        format!(
            "{} holds work nothing else does.\n",
            removal.worktree.path.display()
        )
    };
    if risk.paths > 0 {
        let changes = if risk.paths == 1 { "change" } else { "changes" };
        out.push_str(&format!("{} uncommitted {changes}:\n", risk.paths));
        for file in &risk.files {
            out.push_str(&format!("  {file}\n"));
        }
        if risk.paths > risk.files.len() {
            out.push_str(&format!("  and {} more\n", risk.paths - risk.files.len()));
        }
    }
    if risk.unpushed > 0 {
        let on = match &removal.worktree.branch {
            Some(branch) => format!("on {branch} that no other branch, tag or remote has"),
            None => "on no branch, tag or remote".into(),
        };
        let commits = if risk.unpushed == 1 {
            "commit"
        } else {
            "commits"
        };
        out.push_str(&format!("{} {commits} {on}:\n", risk.unpushed));
        for commit in &risk.commits {
            out.push_str(&format!("  {commit}\n"));
        }
        if risk.detached {
            out.push_str(
                "Git may drop them at its next cleanup; undo can't bring them back after that.\n",
            );
        }
    }
    if removal.folder.is_none() {
        out.push_str("Its folder isn't where Git expects it, so only Git's registration goes.\n");
    }
    if let Some(branch) = &removal.worktree.branch {
        out.push_str(&format!("The branch {branch} stays.\n"));
    }
    out
}

/// A worktree's path, then its branch, short HEAD and what's worth knowing.
fn line(worktree: &Worktree) -> String {
    let mut about = vec![match (&worktree.branch, worktree.bare) {
        (Some(branch), _) => branch.clone(),
        (None, true) => "bare".into(),
        (None, false) => "detached".into(),
    }];
    match &worktree.head {
        Some(head) => about.push(short(head)),
        None if !worktree.bare => about.push("no commits".into()),
        None => {}
    }
    if worktree.main {
        about.push("main worktree".into());
    }
    let one_line = |text: &str| text.replace('\n', " ");
    match worktree.locked.as_deref() {
        Some("") => about.push("locked".into()),
        Some(why) => about.push(format!("locked: {}", one_line(why))),
        None => {}
    }
    match &worktree.state {
        worktrees::State::Ok => {}
        worktrees::State::Missing => about.push("missing".into()),
        worktrees::State::Stale => about.push("stale".into()),
        worktrees::State::Replaced => about.push("replaced".into()),
        worktrees::State::Unreadable(why) => about.push(format!("unreadable: {why}")),
    }
    if let Some(why) = &worktree.prunable {
        about.push(format!("prunable: {}", one_line(why)));
    }
    format!("{} ({})\n", worktree.path.display(), about.join(", "))
}

fn short(head: &str) -> String {
    head.chars().take(7).collect()
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorktreeStatus {
    path: String,
    #[serde(default = "short_timeout")]
    timeout_seconds: u64,
}

#[derive(Serialize, JsonSchema)]
pub struct TreeStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// Absent when the upstream isn't fetched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<u64>,
    pub staged: usize,
    pub unstaged: usize,
    pub conflicted: usize,
    pub untracked: usize,
}

impl Call for WorktreeStatus {
    type Out = TreeStatus;

    fn run(self, server: &mut Server) -> Result<Reply<TreeStatus>, ToolError> {
        let git = Git {
            timeout: Duration::from_secs(self.timeout_seconds),
            ..server.git()?
        };
        let status = worktrees::status(&git, &local_one(&self.path, "worktree_status")?)?;
        let text = status_text(&status);
        Ok(Reply::new(
            text,
            TreeStatus {
                branch: status.branch,
                head: status.head,
                upstream: status.upstream,
                ahead: status.ahead_behind.map(|(ahead, _)| ahead),
                behind: status.ahead_behind.map(|(_, behind)| behind),
                staged: status.staged,
                unstaged: status.unstaged,
                conflicted: status.conflicted,
                untracked: status.untracked,
            },
        ))
    }
}

/// The worktree's branch and HEAD, then its changes, or clean.
fn status_text(status: &worktrees::Status) -> String {
    let head = status
        .head
        .as_deref()
        .map_or_else(|| "no commits".into(), |head| format!("at {}", short(head)));
    let mut out = match &status.branch {
        Some(branch) => format!("{branch} {head}"),
        None => format!("detached {head}"),
    };
    let _ = match (&status.upstream, status.ahead_behind) {
        (Some(upstream), Some((ahead, behind))) => {
            writeln!(out, ", tracking {upstream}: {ahead} ahead, {behind} behind")
        }
        (Some(upstream), None) => writeln!(
            out,
            ", tracking {upstream}, which isn't fetched, so ahead and behind are unknown"
        ),
        (None, _) if status.branch.is_some() => writeln!(out, ", no upstream"),
        (None, _) => writeln!(out),
    };
    let counts: Vec<String> = [
        (status.staged, "staged"),
        (status.unstaged, "unstaged"),
        (status.conflicted, "conflicted"),
        (status.untracked, "untracked"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, label)| format!("{n} {label}"))
    .collect();
    if counts.is_empty() {
        out.push_str("clean\n");
    } else {
        let _ = writeln!(out, "{}", counts.join(", "));
    }
    out
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListRepos {}

#[derive(Serialize, JsonSchema)]
pub struct Repo {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

#[derive(Serialize, JsonSchema)]
pub struct Repos {
    pub repos: Vec<Repo>,
}

impl Call for ListRepos {
    type Out = Repos;

    fn run(self, server: &mut Server) -> Result<Reply<Repos>, ToolError> {
        let repos: Vec<Repo> = server
            .places
            .repositories()?
            .iter()
            .map(|repo| match repo.resolve() {
                Availability::Available(path) => Repo {
                    path: shown(&path),
                    unavailable: None,
                },
                Availability::Unavailable(why) => Repo {
                    path: shown(&repo.path),
                    unavailable: Some(why),
                },
            })
            .collect();
        let text: String = if repos.is_empty() {
            "No registered repositories.\n".into()
        } else {
            repos
                .iter()
                .map(|repo| match &repo.unavailable {
                    None => format!("{}\n", repo.path),
                    Some(why) => format!("{} (unavailable: {why})\n", repo.path),
                })
                .collect()
        };
        Ok(Reply::new(text, Repos { repos }))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterRepo {
    path: String,
}

#[derive(Serialize, JsonSchema)]
pub struct Registered {
    pub path: String,
}

impl Call for RegisterRepo {
    type Out = Registered;

    fn run(self, server: &mut Server) -> Result<Reply<Registered>, ToolError> {
        let git = server.git()?;
        let dir = local_one(&self.path, "register_repo")?;
        let repo = worktrees::register(&git, &server.places, &dir)?;
        Ok(Reply::new(
            format!("{}\n", repo.path.display()),
            Registered {
                path: shown(&repo.path),
            },
        ))
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgetRepo {
    path: String,
}

impl Call for ForgetRepo {
    type Out = Registered;

    fn run(self, server: &mut Server) -> Result<Reply<Registered>, ToolError> {
        let repo = server
            .places
            .forget_repository(&local_one(&self.path, "forget_repo")?)?;
        Ok(Reply::new(
            format!(
                "Forgot {}; the repository is untouched.\n",
                repo.path.display()
            ),
            Registered {
                path: shown(&repo.path),
            },
        ))
    }
}
