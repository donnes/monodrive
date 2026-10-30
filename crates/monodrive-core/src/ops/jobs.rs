//! Copy and move jobs. The owner queues them and runs at most `RUNNING` at
//! once, each on its own thread, one item at a time, so its socket keeps
//! answering while they work. Every change an item makes is a `Step`, written
//! to the journal before and after it happens, so undo reverses a job and a
//! crash leaves it settled from the disk rather than repeated.

use super::{
    Engine, FileId, Origin, Outcome, Record, Request, Spot, Stamp, Step, Submission, disk, existing,
};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// At most this many jobs copy at once; the rest wait their turn.
const RUNNING: usize = 2;
/// How many finished jobs clients are shown.
const SHOWN: usize = 50;

/// What to do when an item of the same name is already where one is going.
/// Monodrive never asks; the request says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conflict {
    /// Names the new one "name 2", "name 3" and on.
    KeepBoth,
    /// Moves the one there to the Trash first.
    Replace,
    Skip,
    /// Stops the job there, waiting to be continued with another choice.
    #[default]
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    Running,
    /// Stopped at a conflict; continuing it with another choice goes on.
    Waiting,
    Completed,
    Failed,
    Cancelled,
    /// The owner stopped while it ran; its items were settled from the disk.
    Interrupted,
}

impl State {
    pub fn active(self) -> bool {
        matches!(self, State::Queued | State::Running)
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Queued => "queued",
            State::Running => "running",
            State::Waiting => "waiting",
            State::Completed => "completed",
            State::Failed => "failed",
            State::Cancelled => "cancelled",
            State::Interrupted => "interrupted",
        }
    }
}

/// A copy or move, as clients see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    /// "Copy" or "Move".
    pub title: String,
    pub origin: Origin,
    pub into: PathBuf,
    pub state: State,
    pub tasks: Vec<Task>,
    /// Copied so far, of the bytes in the items' files.
    pub bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub source: PathBuf,
    /// `None` until the item is done, or for good if the job stopped first.
    pub done: Option<Done>,
}

/// How one item ended. Messages name the paths involved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Done {
    /// Where it is now.
    To(PathBuf),
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Copy,
    Move,
}

/// A job as accepted: what goes where.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Spec {
    kind: Kind,
    into: PathBuf,
    conflict: Conflict,
    sources: Vec<PathBuf>,
    /// Which item each source was when the job was accepted, so a source
    /// replaced while the job waits is refused rather than moved.
    #[serde(default)]
    ids: Vec<FileId>,
}

/// One item of a job, as the journal keeps it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Work {
    pub(super) source: PathBuf,
    /// The source as accepted; see `Spec::ids`.
    #[serde(default)]
    id: Option<FileId>,
    /// Where it goes, once it starts.
    pub(super) to: Option<PathBuf>,
    /// The hidden copy made beside `to` and renamed into place once it's
    /// complete, so a partial copy never sits at `to`.
    pub(super) temp: Option<PathBuf>,
    /// Each change in the order it's made, `applied` once made. Each is
    /// written before it happens.
    pub(super) steps: Vec<Step>,
    /// In the source's files.
    bytes: u64,
    done: Option<Done>,
}

impl Work {
    /// Not done, and nothing on disk changed for it.
    fn untouched(&self) -> bool {
        !matches!(self.done, Some(Done::To(_))) && self.steps.iter().all(|step| !step.applied)
    }
}

/// A job in the owner's memory.
pub(super) struct Entry {
    pub(super) id: u64,
    origin: Origin,
    spec: Spec,
    pub(super) work: Vec<Work>,
    pub(super) state: State,
    /// The operation this run's changes join the history as: the job's id,
    /// then each continuation's.
    pub(super) run: u64,
    /// Which items this run works on; the rest finished in an earlier one.
    fresh: Vec<bool>,
    cancel: Arc<AtomicBool>,
    /// The running item's bytes copied so far.
    copied: Arc<AtomicU64>,
}

impl Entry {
    pub(super) fn new(id: u64, origin: Origin, spec: Spec) -> Self {
        let work: Vec<Work> = spec
            .sources
            .iter()
            .enumerate()
            .map(|(i, source)| Work {
                source: source.clone(),
                id: spec.ids.get(i).copied(),
                ..Work::default()
            })
            .collect();
        Self {
            id,
            origin,
            spec,
            fresh: vec![true; work.len()],
            work,
            state: State::Queued,
            run: id,
            cancel: Arc::default(),
            copied: Arc::default(),
        }
    }

    /// Queues the job again as run `run`, keeping what's finished: an item
    /// done, or one that stopped partway, stays as it is.
    pub(super) fn resume(&mut self, run: u64, conflict: Conflict) {
        self.run = run;
        self.spec.conflict = conflict;
        self.state = State::Queued;
        self.cancel = Arc::default();
        for (work, fresh) in self.work.iter_mut().zip(&mut self.fresh) {
            *fresh = work.untouched();
            if *fresh {
                *work = Work {
                    source: work.source.clone(),
                    id: work.id,
                    ..Work::default()
                };
            }
        }
    }

    /// What this run changed, for the undo history.
    pub(super) fn run_steps(&self) -> Vec<Step> {
        self.work
            .iter()
            .zip(&self.fresh)
            .filter(|(_, fresh)| **fresh)
            .flat_map(|(work, _)| &work.steps)
            .filter(|step| step.applied)
            .cloned()
            .collect()
    }

    pub(super) fn title(&self) -> &'static str {
        match self.spec.kind {
            Kind::Copy => "Copy",
            Kind::Move => "Move",
        }
    }

    fn public(&self) -> Job {
        let finished: u64 = self
            .work
            .iter()
            .filter(|work| matches!(work.done, Some(Done::To(_))))
            .map(|work| work.bytes)
            .sum();
        let live = if self.state == State::Running {
            self.copied.load(Ordering::Relaxed)
        } else {
            0
        };
        Job {
            id: self.id,
            title: self.title().into(),
            origin: self.origin.clone(),
            into: self.spec.into.clone(),
            state: self.state,
            tasks: self
                .work
                .iter()
                .map(|work| Task {
                    source: work.source.clone(),
                    done: work.done.clone(),
                })
                .collect(),
            bytes: finished + live,
            total_bytes: self.work.iter().map(|work| work.bytes).sum(),
        }
    }
}

/// The engine as the owner's socket and its job threads share it. Disk work
/// happens without the lock; only the journal and job state need it.
pub(super) struct Shared {
    engine: Mutex<Engine>,
    /// Wakes the owner's poll loop when a job ends, so it can exit once
    /// idle.
    wake: Option<UnixStream>,
}

impl Shared {
    pub(super) fn new(engine: Engine, wake: Option<UnixStream>) -> Arc<Self> {
        Arc::new(Self {
            engine: Mutex::new(engine),
            wake,
        })
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Journals an item as it stands.
    fn record(&self, op: u64, index: usize, work: &Work) -> Result<(), String> {
        let record = Record::Work {
            op,
            index,
            work: work.clone(),
        };
        let mut engine = self.lock();
        engine.write(&record)?;
        engine.fold(&record);
        Ok(())
    }

    fn finish(&self, op: u64, state: State) {
        let record = Record::Finish { op, state };
        let mut engine = self.lock();
        let _ = engine.write(&record);
        engine.fold(&record);
    }
}

/// Starts queued jobs on their own threads while fewer than `RUNNING` run.
pub(super) fn start(shared: &Arc<Shared>) {
    let mut engine = shared.lock();
    while engine
        .jobs
        .iter()
        .filter(|job| job.state == State::Running)
        .count()
        < RUNNING
    {
        let Some(job) = engine
            .jobs
            .iter_mut()
            .find(|job| job.state == State::Queued)
        else {
            break;
        };
        job.state = State::Running;
        let (op, shared) = (job.id, shared.clone());
        std::thread::spawn(move || {
            run(&shared, op);
            start(&shared);
            if let Some(mut wake) = shared.wake.as_ref() {
                let _ = std::io::Write::write_all(&mut wake, &[0]);
            }
        });
    }
}

/// Works through a job's items until it's done, stopped or cancelled.
pub(super) fn run(shared: &Shared, op: u64) {
    // The totals, for progress; reading the disk, so outside the lock.
    let sources: Vec<(usize, PathBuf)> = shared.lock().job_mut(op).map_or_else(Vec::new, |job| {
        job.work
            .iter()
            .enumerate()
            .filter(|(_, work)| work.done.is_none())
            .map(|(i, work)| (i, work.source.clone()))
            .collect()
    });
    let sizes: Vec<(usize, u64)> = sources
        .into_iter()
        .map(|(i, source)| (i, disk::tally(&source).map_or(0, |(_, bytes)| bytes)))
        .collect();
    if let Some(job) = shared.lock().job_mut(op) {
        for (i, bytes) in sizes {
            job.work[i].bytes = bytes;
        }
    }
    while next(shared, op) {}
}

/// Runs the job's next item. False once the job has ended.
pub(super) fn next(shared: &Shared, op: u64) -> bool {
    let (index, spec, work, cancel, copied) = {
        let mut engine = shared.lock();
        let Some(job) = engine.job_mut(op) else {
            return false;
        };
        let pending = job.work.iter().position(|work| work.done.is_none());
        let cancelled = job.cancel.load(Ordering::Relaxed);
        match pending {
            Some(index) if !cancelled => {
                job.copied.store(0, Ordering::Relaxed);
                (
                    index,
                    job.spec.clone(),
                    job.work[index].clone(),
                    job.cancel.clone(),
                    job.copied.clone(),
                )
            }
            _ => {
                let failed = job
                    .work
                    .iter()
                    .any(|work| matches!(work.done, Some(Done::Failed(_))));
                drop(engine);
                let state = match (cancelled, failed) {
                    (true, _) => State::Cancelled,
                    (false, true) => State::Failed,
                    (false, false) => State::Completed,
                };
                shared.finish(op, state);
                return false;
            }
        }
    };
    let mut item = Item {
        shared,
        op,
        index,
        spec: &spec,
        work,
    };
    let done = item.transfer(&cancel, &copied);
    let stop = matches!(done, Next::Stop(_));
    item.work.done = Some(match done {
        Next::Done(done) => done,
        Next::Stop(why) => Done::Failed(why),
    });
    let _ = shared.record(op, index, &item.work);
    if stop {
        shared.finish(op, State::Waiting);
        return false;
    }
    true
}

enum Next {
    Done(Done),
    /// A conflict under `Conflict::Stop`.
    Stop(String),
}

/// One item on its way.
struct Item<'a> {
    shared: &'a Shared,
    op: u64,
    index: usize,
    spec: &'a Spec,
    work: Work,
}

impl Item<'_> {
    fn transfer(&mut self, cancel: &AtomicBool, copied: &AtomicU64) -> Next {
        let source = self.work.source.clone();
        let fail = |why: String| Next::Done(Done::Failed(why));
        let (id, stamp) = match existing(&source) {
            Ok(found) => found,
            Err(e) => return fail(e),
        };
        if self.work.id.is_some_and(|accepted| accepted != id) {
            return fail(format!(
                "{}: a different item is there now than when the job was queued, so it was left alone.",
                source.display()
            ));
        }
        let into = &self.spec.into;
        if !into.is_dir() {
            return fail(format!("{}: not a folder.", into.display()));
        }
        let Some(name) = source.file_name() else {
            return fail(format!("{}: has no name to copy.", source.display()));
        };
        if source.is_dir()
            && let (Ok(inside), Ok(folder)) = (into.canonicalize(), source.canonicalize())
            && inside.starts_with(&folder)
        {
            return fail(format!(
                "{}: can't go into itself or a folder inside it.",
                source.display()
            ));
        }
        let mut to = into.join(name);
        if self.spec.kind == Kind::Move && to == source {
            return fail(format!(
                "{}: already in {}.",
                source.display(),
                into.display()
            ));
        }
        let mut replacing = None;
        if let Ok((there, there_stamp)) = disk::stamp(&to) {
            match self.spec.conflict {
                Conflict::Skip => {
                    return Next::Done(Done::Skipped(format!(
                        "{} already exists, so {} was skipped.",
                        to.display(),
                        source.display()
                    )));
                }
                Conflict::Stop => {
                    return Next::Stop(format!(
                        "{} already exists. Continue the job with keep_both, replace or skip.",
                        to.display()
                    ));
                }
                Conflict::KeepBoth => {
                    let Some(free) = numbered(&to) else {
                        return fail(format!("{}: no free name for it.", to.display()));
                    };
                    to = free;
                }
                Conflict::Replace => {
                    if there == id || source.starts_with(&to) {
                        return fail(format!(
                            "{}: can't be replaced by {}, which it is or holds.",
                            to.display(),
                            source.display()
                        ));
                    }
                    replacing = Some(Step {
                        id: there,
                        undone: Spot::Path(to.clone()),
                        done: Spot::Trash(None),
                        applied: false,
                        stamp: there_stamp,
                    });
                }
            }
        }
        self.work.to = Some(to.clone());
        self.work.bytes = disk::tally(&source).map_or(self.work.bytes, |(_, bytes)| bytes);
        // The destination folder may be a link to another volume.
        let into_volume = std::fs::metadata(into).map(|meta| meta.dev());
        let same_volume = disk::volume(&source).ok() == into_volume.ok();
        let result = if self.spec.kind == Kind::Move && same_volume {
            self.move_here(id, stamp, &to, replacing)
        } else {
            self.copy_here(id, stamp, &to, replacing, cancel, copied)
        };
        Next::Done(match result {
            Ok(()) => Done::To(to),
            Err(why) => Done::Failed(why),
        })
    }

    /// A rename on one volume, which never replaces.
    fn move_here(
        &mut self,
        id: FileId,
        stamp: Stamp,
        to: &Path,
        replacing: Option<Step>,
    ) -> Result<(), String> {
        let source = self.work.source.clone();
        if let Some(old) = replacing {
            self.apply(old)
                .map_err(|e| format!("{e} So {} wasn't replaced.", to.display()))?;
        }
        self.apply(Step {
            id,
            undone: Spot::Path(source),
            done: Spot::Path(to.to_path_buf()),
            applied: false,
            stamp,
        })
        .map_err(|e| self.with_replaced(e))
        .map(|_| ())
    }

    /// Copies to a hidden name beside `to`, checks the copy is whole, then
    /// puts it in place. A move then sends the source to the Trash.
    fn copy_here(
        &mut self,
        id: FileId,
        stamp: Stamp,
        to: &Path,
        replacing: Option<Step>,
        cancel: &AtomicBool,
        copied: &AtomicU64,
    ) -> Result<(), String> {
        let source = self.work.source.clone();
        let temp = self
            .spec
            .into
            .join(format!(".monodrive-{}-{}", self.op, self.index));
        if disk::id(&temp).is_some() {
            return Err(format!("{}: already exists.", temp.display()));
        }
        self.work.temp = Some(temp.clone());
        self.shared.record(self.op, self.index, &self.work)?;
        let meter = disk::Meter { copied, cancel };
        let made = disk::copy(&source, &temp, Some(&meter));
        let whole = made.as_ref().is_ok_and(|()| {
            disk::tally(&source).ok() == disk::tally(&temp).ok()
                && disk::stamp(&source).ok() == Some((id, stamp))
        });
        let made = match (made, whole) {
            (Ok(()), true) => disk::stamp(&temp).map_err(|e| e.to_string()),
            (Ok(()), false) => Err(format!("{} changed during the copy.", source.display())),
            (Err(_), _) if cancel.load(Ordering::Relaxed) => {
                Err(format!("{}: cancelled partway.", source.display()))
            }
            (Err(e), _) => Err(format!("{}: {e}", source.display())),
        };
        let (copy, copy_stamp) = match made {
            Ok(made) => made,
            Err(why) => return Err(self.without_temp(why)),
        };
        if let Some(old) = replacing
            && let Err(e) = self.apply(old)
        {
            return Err(self.without_temp(format!("{e} So {} wasn't replaced.", to.display())));
        }
        let mut placed = Step {
            id: copy,
            undone: Spot::Path(temp),
            done: Spot::Path(to.to_path_buf()),
            applied: false,
            stamp: copy_stamp,
        };
        // Undoing a copy sends it to the Trash, not back to its hidden name.
        if let Err(e) = self.apply_then(&mut placed, |step| step.undone = Spot::Trash(None)) {
            let e = self.with_replaced(e);
            return Err(self.without_temp(e));
        }
        if self.spec.kind == Kind::Move {
            self.apply(Step {
                id,
                undone: Spot::Path(source.clone()),
                done: Spot::Trash(None),
                applied: false,
                stamp,
            })
            .map_err(|e| {
                format!(
                    "Copied to {}, but {} was left where it was: {e}",
                    to.display(),
                    source.display()
                )
            })?;
        }
        Ok(())
    }

    /// Journals `step`, makes the change, and journals it made. A step that
    /// fails is dropped, since nothing changed.
    fn apply(&mut self, mut step: Step) -> Result<PathBuf, String> {
        self.apply_then(&mut step, |_| {})
    }

    fn apply_then(
        &mut self,
        step: &mut Step,
        after: impl FnOnce(&mut Step),
    ) -> Result<PathBuf, String> {
        self.work.steps.push(step.clone());
        self.shared.record(self.op, self.index, &self.work)?;
        let moved = step.shift(true);
        match &moved {
            Ok(_) => {
                after(step);
                if let Some(last) = self.work.steps.last_mut() {
                    *last = step.clone();
                }
            }
            Err(_) => {
                self.work.steps.pop();
            }
        }
        self.shared.record(self.op, self.index, &self.work)?;
        moved
    }

    /// `why`, saying where the item a failed replace sent to the Trash is.
    fn with_replaced(&self, why: String) -> String {
        match self.work.steps.first() {
            Some(old) if old.applied => format!(
                "{why} What it was replacing is in the Trash at {}.",
                old.here().unwrap_or(Path::new("?")).display()
            ),
            _ => why,
        }
    }

    /// `why`, after sending this item's unfinished hidden copy to the Trash.
    fn without_temp(&self, why: String) -> String {
        match self.work.temp.as_deref() {
            Some(temp) if disk::id(temp).is_some() => match disk::trash(temp) {
                Ok(landed) => format!(
                    "{why} The unfinished copy went to the Trash at {}.",
                    landed.display()
                ),
                Err(e) => format!("{why} The unfinished copy is still at {e}"),
            },
            _ => why,
        }
    }
}

/// `path` as "name 2.ext", "name 3.ext" and on: the first that's free.
pub(super) fn numbered(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    (2..).find_map(|n: u32| {
        let free = path.with_file_name(format!("{stem} {n}{ext}"));
        disk::id(&free).is_none().then_some(free)
    })
}

impl Engine {
    /// Accepts a copy or move as a queued job, or queues a stopped one again.
    pub(super) fn queue(&mut self, submission: Submission) -> Result<Outcome, String> {
        let op = self.next;
        let record = match &submission.request {
            Request::Copy {
                paths,
                into,
                conflict,
            }
            | Request::Move {
                paths,
                into,
                conflict,
            } => {
                let kind = if matches!(submission.request, Request::Copy { .. }) {
                    Kind::Copy
                } else {
                    Kind::Move
                };
                if let Some(path) = paths.iter().chain([into]).find(|p| !p.is_absolute()) {
                    return Err(format!("{}: paths must be absolute.", path.display()));
                }
                if paths.is_empty() {
                    return Err("Name at least one item.".into());
                }
                let ids = paths
                    .iter()
                    // Only the item, not a stamp: walking a big folder here
                    // would hold the engine while the owner waits.
                    .map(|path| disk::lookup(path).map_err(|e| format!("{}: {e}", path.display())))
                    .collect::<Result<_, _>>()?;
                let spec = Spec {
                    kind,
                    into: into.clone(),
                    conflict: *conflict,
                    sources: paths.clone(),
                    ids,
                };
                Record::Job {
                    op,
                    submission: submission.clone(),
                    spec,
                }
            }
            Request::Retry { job, conflict } => {
                let old = self.job_mut(*job).ok_or_else(|| format!("No job {job}."))?;
                if old.state.active() {
                    return Err(format!("Job {job} is still going."));
                }
                if !old.work.iter().any(Work::untouched) {
                    return Err(format!("Job {job} has nothing left to do."));
                }
                Record::Continue {
                    op,
                    submission: submission.clone(),
                    job: *job,
                    conflict: conflict.unwrap_or(old.spec.conflict),
                }
            }
            _ => unreachable!("only jobs are queued"),
        };
        self.write(&record)?;
        self.fold(&record);
        Ok(self.finished[&submission.id].1.clone())
    }

    pub(super) fn cancel(&mut self, job: u64) -> Result<Outcome, String> {
        let entry = self.job_mut(job).ok_or_else(|| format!("No job {job}."))?;
        if !entry.state.active() {
            return Err(format!("Job {job} has already stopped."));
        }
        entry.cancel.store(true, Ordering::Relaxed);
        if entry.state == State::Queued {
            let record = Record::Finish {
                op: job,
                state: State::Cancelled,
            };
            self.write(&record)?;
            self.fold(&record);
        }
        Ok(Outcome {
            op: job,
            title: "Cancel".into(),
            items: Vec::new(),
            interrupted: false,
        })
    }

    /// The newest jobs first: every one still going, and the last finished.
    pub(super) fn jobs(&self) -> Vec<Job> {
        let mut shown = 0;
        self.jobs
            .iter()
            .rev()
            .filter(|job| {
                shown += usize::from(!job.state.active());
                job.state.active() || shown <= SHOWN
            })
            .map(Entry::public)
            .collect()
    }

    /// Jobs still going.
    pub(super) fn busy(&self) -> bool {
        self.jobs.iter().any(|job| job.state.active())
    }

    /// For each job the last owner didn't finish, where each of its items
    /// got to, from the disk. Nothing is repeated, and an unfinished hidden
    /// copy goes to the Trash.
    pub(super) fn settle_jobs(&self) -> Vec<Record> {
        let mut records = Vec::new();
        for job in self.jobs.iter().filter(|job| job.state.active()) {
            for (index, work) in job.work.iter().enumerate() {
                if work.done.is_some() || (work.to.is_none() && work.steps.is_empty()) {
                    continue;
                }
                let mut work = work.clone();
                for step in work.steps.iter_mut().filter(|step| !step.applied) {
                    let _ = step.locate();
                }
                work.steps.retain(|step| step.applied);
                // A copy found in place came from the hidden name; undoing it
                // sends it to the Trash, as when its placement was recorded.
                if let Some(temp) = &work.temp {
                    for step in &mut work.steps {
                        if step.undone == Spot::Path(temp.clone()) {
                            step.undone = Spot::Trash(None);
                        }
                    }
                }
                let to = work.to.clone().unwrap_or_default();
                let placed = work
                    .steps
                    .iter()
                    .any(|step| step.done == Spot::Path(to.clone()));
                let left = work
                    .steps
                    .iter()
                    .any(|step| step.undone == Spot::Path(work.source.clone()));
                let whole = placed && (job.spec.kind == Kind::Copy || left);
                let mut why = format!(
                    "Monodrive stopped partway through {}; undo reverses what it changed.",
                    work.source.display()
                );
                if let Some(temp) = work.temp.as_deref()
                    && disk::id(temp).is_some()
                {
                    why = match disk::trash(temp) {
                        Ok(landed) => format!(
                            "{why} The unfinished copy went to the Trash at {}.",
                            landed.display()
                        ),
                        Err(e) => format!("{why} The unfinished copy is still at {e}"),
                    };
                }
                work.done = Some(if whole {
                    Done::To(to)
                } else {
                    Done::Failed(why)
                });
                records.push(Record::Work {
                    op: job.id,
                    index,
                    work,
                });
            }
            records.push(Record::Finish {
                op: job.id,
                state: State::Interrupted,
            });
        }
        records
    }
}
