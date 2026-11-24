//! What a session's tools share: its connections, its index once started,
//! and the plans its dry runs kept.

use super::contract::{self, Code, JobView, Method, Reply, ToolError};
use crate::Origin;
use crate::index::{self, Index, Status};
use crate::localsend::{self, Device};
use crate::ops::{self, Job, Request, organize, rename};
use crate::places;
use crate::remote::{self, Connection, Connections, RemoteError, RemotePath};
use crate::storage::Cleanup;
use crate::user_commands::Run;
use crate::worktrees::Git;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a call may hold its reply for a job.
const MOST_WAIT: u64 = 60;

/// How many previews a session keeps, and for how long.
const PREVIEWS_KEPT: usize = 16;
const PREVIEW_LIFE: Duration = Duration::from_secs(600);

pub(super) struct Session {
    /// Started on the first tool that needs it, so connecting an agent
    /// never kicks off a disk crawl by itself.
    index: Option<Result<Index, String>>,
    /// Connects to the operation owner on the first change.
    ops: ops::Client,
    /// The name the agent's MCP client gave, to attribute its changes.
    pub(super) client: Option<String>,
    pub(super) places: places::Store,
    /// Where the startup disk's own folders are, which the Overview measures.
    pub(super) root: std::path::PathBuf,
    /// Found on the first Git tool, and again until one is found.
    git: Option<Git>,
    /// What dry runs planned, oldest first, so a confirm runs only that.
    previews: Vec<Kept>,
    /// Each opened on the first tool that needs it; each session its own.
    pub(super) remote: Connections,
    /// This Mac's LocalSend identity, read on the first LocalSend tool.
    localsend: Option<Arc<crate::localsend::Identity>>,
    /// The LocalSend devices this session listed, which it may send to.
    nearby: Vec<Device>,
}

struct Kept {
    token: String,
    made: Instant,
    preview: Preview,
    /// The request id that ran it, so a retry of that call reaches the
    /// owner, which answers with what it did the first time.
    ran: Option<String>,
}

/// A plan a dry run showed, kept for the confirm that runs exactly it.
#[derive(Clone)]
pub(super) enum Preview {
    Rename {
        paths: Vec<PathBuf>,
        rule: rename::Rule,
        fingerprint: rename::Fingerprint,
        skip_problems: bool,
    },
    Cleanup(Cleanup),
    Organize(Vec<organize::Group>),
    Command {
        name: String,
        paths: Vec<PathBuf>,
    },
    Worktree(crate::ops::Request),
}

impl Session {
    pub(super) fn new(ops: ops::Client, places: places::Store) -> Self {
        Self {
            index: None,
            ops,
            client: None,
            remote: Connections::new(places.clone(), remote::ssh::connector()),
            places,
            root: "/".into(),
            previews: Vec::new(),
            git: None,
            localsend: None,
            nearby: Vec::new(),
        }
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
        // Many items to the Trash run as a job; the reply waits for it.
        let change = if outcome.job {
            contract::Change::of_job(&self.wait(outcome.op, MOST_WAIT)?)
        } else {
            contract::Change::of(&outcome)
        };
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
            // A job writes the journal once per item; one look covers them all.
            while told.try_recv().is_ok() {}
        }
    }

    /// Refuses before anything is written when agents may not change files.
    pub(super) fn can_change(&self) -> Result<(), ToolError> {
        self.may("change files")
    }

    /// Refuses `what` when agents may not change files.
    pub(super) fn may(&self, what: &str) -> Result<(), ToolError> {
        if self.ops.agents_change_files() {
            Ok(())
        } else {
            Err(ToolError::new(
                Code::AgentsDisabled,
                format!(
                    "Agents can't {what}: Let agents change files is off in Monodrive's Agents settings."
                ),
            ))
        }
    }

    /// Ejects the volume mounted at `mount` for this session's agent.
    pub(super) fn eject(&mut self, mount: &Path) -> Result<String, ToolError> {
        let origin = self.origin();
        Ok(self.ops.eject(mount, &origin)?)
    }

    /// Starts the saved command `name` on `paths` for this session's agent.
    pub(super) fn run_command(
        &mut self,
        name: &str,
        paths: Vec<PathBuf>,
    ) -> Result<u64, ToolError> {
        let origin = self.origin();
        Ok(self.ops.run_command(origin, name, paths)?)
    }

    pub(super) fn jobs(&mut self) -> Result<Vec<Job>, ToolError> {
        Ok(self.ops.jobs()?)
    }

    pub(super) fn runs(&mut self) -> Result<Vec<Run>, ToolError> {
        Ok(self.ops.runs()?)
    }

    pub(super) fn cancel_run(&mut self, run: u64) -> Result<(), ToolError> {
        Ok(self.ops.cancel_run(run)?)
    }

    /// Adds what a look found to the devices this session may send to,
    /// each once.
    pub(super) fn heard(&mut self, devices: &[Device]) {
        for device in devices {
            match self
                .nearby
                .iter_mut()
                .find(|known| known.fingerprint == device.fingerprint)
            {
                Some(known) => {
                    if let Some(merged) = localsend::merged(known, device.clone()) {
                        *known = merged;
                    }
                }
                None => self.nearby.push(device.clone()),
            }
        }
    }

    /// A device this session listed, by its fingerprint in any case.
    pub(super) fn nearby(&self, fingerprint: &str) -> Option<Device> {
        self.nearby
            .iter()
            .find(|device| device.fingerprint.eq_ignore_ascii_case(fingerprint))
            .cloned()
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

    pub(super) fn localsend(&mut self) -> Result<Arc<crate::localsend::Identity>, ToolError> {
        if let Some(identity) = &self.localsend {
            return Ok(identity.clone());
        }
        let identity = Arc::new(crate::localsend::Identity::user().map_err(ToolError::from)?);
        self.localsend = Some(identity.clone());
        Ok(identity)
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
