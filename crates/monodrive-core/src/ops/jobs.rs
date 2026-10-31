//! Copy, move, compress, extract and remote copy jobs. The owner queues them and runs at
//! most `RUNNING` at once, each on its own thread, one item at a time, so its
//! socket keeps answering while they work. Every change an item makes is a
//! `Step`, written to the journal before and after it happens, so undo
//! reverses a job and a crash leaves it settled from the disk rather than
//! repeated.

use super::{
    Engine, FileId, Origin, Outcome, Record, Request, Spot, Stamp, Step, Submission, disk, existing,
};
use crate::archives::{self, Archive};
use crate::cloud;
use crate::remote::{Connections, Connector, RemotePath};
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
    /// The owner stopped while it ran, and its items were settled from the
    /// disk, or it lost the connection to a remote source. Continuing it
    /// checks again what it's copying and where.
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

/// Which way a remote copy goes. A job with one has `source:/path` labels in
/// place of paths on the server's side: `into` for an upload and each task's
/// `source` and `Done::To` for what's remote. They print; they aren't absolute,
/// so local services refuse them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Download,
    Upload,
}

/// A copy, move, compression or extraction, as clients see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: u64,
    /// "Copy", "Move", "Compress", "Extract", "Download" or "Upload".
    pub title: String,
    pub origin: Origin,
    /// Where its items go: the archive's folder for Compress.
    pub into: PathBuf,
    pub state: State,
    pub tasks: Vec<Task>,
    /// Copied so far, of the bytes in the items' files.
    pub bytes: u64,
    pub total_bytes: u64,
    /// When its latest run was queued, in seconds since 1970.
    #[serde(default)]
    pub started: u64,
    /// Its latest run's id, unique among runs and operations: the job's id,
    /// then each continuation's. Undo knows the run by it.
    #[serde(default)]
    pub run: u64,
    /// The latest run has ended and changed something undo can reverse.
    #[serde(default)]
    pub undoable: bool,
}

impl Job {
    pub fn transfer(&self) -> Option<Transfer> {
        match self.title.as_str() {
            "Download" => Some(Transfer::Download),
            "Upload" => Some(Transfer::Upload),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// What it copies or moves. A compression has one task, the item or
    /// the folder of items it compresses; an extraction one for each entry
    /// or folder it extracts, as a path inside the archive.
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
    Compress,
    Extract,
    Download,
    Upload,
}

impl Kind {
    /// What an item is made as, before it's put in place.
    fn made(self) -> &'static str {
        match self {
            Kind::Copy | Kind::Move | Kind::Download | Kind::Upload => "copy",
            Kind::Compress => "archive",
            Kind::Extract => "extraction",
        }
    }
}

/// A job as accepted: what goes where.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Spec {
    pub(super) kind: Kind,
    pub(super) into: PathBuf,
    pub(super) conflict: Conflict,
    /// For an extraction, the entries it takes, as paths inside `archive`.
    sources: Vec<PathBuf>,
    /// Which item each source was when the job was accepted, so a source
    /// replaced while the job waits is refused rather than moved. For an
    /// extraction, each is the archive's.
    #[serde(default)]
    ids: Vec<FileId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive: Option<PathBuf>,
    /// The server's side of a remote copy: for a download each source, and
    /// for an upload the one folder they go into. `sources` and `into` hold
    /// their labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) remote: Vec<RemotePath>,
}

impl Spec {
    /// What a copy, move, compression or extraction asks for, checked now.
    fn new(request: &Request) -> Result<Self, String> {
        fn absolute<'a>(mut paths: impl Iterator<Item = &'a PathBuf>) -> Result<(), String> {
            match paths.find(|path| !path.is_absolute()) {
                Some(path) => Err(format!("{}: paths must be absolute.", path.display())),
                None => Ok(()),
            }
        }
        // Only the item, not a stamp: walking a big folder here would hold
        // the engine while the owner waits.
        let ids = |paths: &[PathBuf]| -> Result<Vec<FileId>, String> {
            if paths.is_empty() {
                return Err("Name at least one item.".into());
            }
            paths
                .iter()
                .map(|path| disk::lookup(path).map_err(|e| format!("{}: {e}", path.display())))
                .collect()
        };
        match request {
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
                absolute(paths.iter().chain([into]))?;
                Ok(Self {
                    kind: if matches!(request, Request::Copy { .. }) {
                        Kind::Copy
                    } else {
                        Kind::Move
                    },
                    into: into.clone(),
                    conflict: *conflict,
                    sources: paths.clone(),
                    ids: ids(paths)?,
                    archive: None,
                    remote: Vec::new(),
                })
            }
            Request::Compress { paths } => {
                absolute(paths.iter())?;
                let ids = ids(paths)?;
                let into = paths[0].parent().unwrap_or(&paths[0]);
                if paths.iter().any(|path| path.parent() != Some(into)) {
                    return Err(
                        "Compress items from one folder; the archive goes beside them.".into(),
                    );
                }
                Ok(Self {
                    kind: Kind::Compress,
                    into: into.to_path_buf(),
                    conflict: Conflict::KeepBoth,
                    sources: paths.clone(),
                    ids,
                    archive: None,
                    remote: Vec::new(),
                })
            }
            Request::Extract {
                archive,
                entries,
                into,
                conflict,
            } => {
                absolute([archive, into].into_iter())?;
                let id = ids(std::slice::from_ref(archive))?[0];
                let items = Archive::open(archive)?.plan(entries, disk::free(into).ok())?;
                Ok(Self {
                    kind: Kind::Extract,
                    into: into.clone(),
                    conflict: *conflict,
                    ids: vec![id; items.len()],
                    sources: items.iter().map(|item| archive.join(item)).collect(),
                    archive: Some(archive.clone()),
                    remote: Vec::new(),
                })
            }
            Request::Download {
                paths,
                into,
                conflict,
            } => {
                absolute([into].into_iter())?;
                if paths.is_empty() {
                    return Err("Name at least one item.".into());
                }
                Ok(Self {
                    kind: Kind::Download,
                    into: into.clone(),
                    conflict: *conflict,
                    sources: paths.iter().map(|path| path.to_string().into()).collect(),
                    ids: Vec::new(),
                    archive: None,
                    remote: paths.clone(),
                })
            }
            Request::Upload {
                paths,
                into,
                conflict,
            } => {
                absolute(paths.iter())?;
                Ok(Self {
                    kind: Kind::Upload,
                    into: into.to_string().into(),
                    conflict: *conflict,
                    sources: paths.clone(),
                    ids: ids(paths)?,
                    archive: None,
                    remote: vec![into.clone()],
                })
            }
            _ => unreachable!("only jobs have a spec"),
        }
    }

    /// The entry name an extraction's source stands for.
    fn entry<'a>(&self, source: &'a Path) -> Option<&'a str> {
        source.strip_prefix(self.archive.as_ref()?).ok()?.to_str()
    }
}

/// One item of a job, as the journal keeps it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Work {
    pub(super) source: PathBuf,
    /// The source as accepted; see `Spec::ids`.
    #[serde(default)]
    pub(super) id: Option<FileId>,
    /// Where it goes, once it starts.
    pub(super) to: Option<PathBuf>,
    /// The hidden copy made beside `to` and renamed into place once it's
    /// complete, so a partial copy never sits at `to`.
    pub(super) temp: Option<PathBuf>,
    /// Each change in the order it's made, `applied` once made. Each is
    /// written before it happens.
    pub(super) steps: Vec<Step>,
    /// In the source's files.
    pub(super) bytes: u64,
    pub(super) done: Option<Done>,
    /// A remote copy's source when it first started, as size and modification
    /// time, so continuing it refuses one that changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) seen: Option<(u64, u64)>,
    /// The name an upload's unfinished item takes on the server, written down
    /// before it's made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) left: Option<RemotePath>,
    /// The server confirmed this job made `left`, the only thing on a server a
    /// job ever removes. Until then what's there may not be the job's.
    #[serde(default)]
    pub(super) made: bool,
}

impl Work {
    /// What a stopped upload left on the server, and what becomes of it.
    pub(super) fn left_note(&self) -> Option<String> {
        let left = self.left.as_ref()?;
        Some(if self.made {
            format!(
                "An unfinished copy may be on the server at {left}; continuing the job removes it."
            )
        } else {
            format!(
                "Something may be on the server at {left}, and Monodrive can't tell whether this \
                job made it, so it left it there. Check it, and remove it if it's not wanted."
            )
        })
    }

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
    /// When this run was queued, in seconds since 1970.
    started: u64,
    cancel: Arc<AtomicBool>,
    /// The running item's bytes copied so far.
    copied: Arc<AtomicU64>,
    /// An extraction's archive, read once while the job runs.
    pub(super) archive: Option<Arc<Archive>>,
}

impl Entry {
    pub(super) fn new(id: u64, origin: Origin, spec: Spec, started: u64) -> Self {
        let work: Vec<Work> = match (spec.kind, spec.sources.as_slice()) {
            // One archive, so one item.
            (Kind::Compress, sources) => vec![Work {
                source: match sources {
                    [one] => one.clone(),
                    _ => spec.into.clone(),
                },
                ..Work::default()
            }],
            (_, sources) => sources
                .iter()
                .enumerate()
                .map(|(i, source)| Work {
                    source: source.clone(),
                    id: spec.ids.get(i).copied(),
                    ..Work::default()
                })
                .collect(),
        };
        Self {
            id,
            origin,
            spec,
            fresh: vec![true; work.len()],
            work,
            state: State::Queued,
            run: id,
            started,
            cancel: Arc::default(),
            copied: Arc::default(),
            archive: None,
        }
    }

    /// Queues the job again as run `run`, keeping what's finished: an item
    /// done, or one that stopped partway, stays as it is.
    pub(super) fn resume(&mut self, run: u64, conflict: Conflict, started: u64) {
        self.run = run;
        self.started = started;
        self.spec.conflict = conflict;
        self.state = State::Queued;
        self.cancel = Arc::default();
        self.archive = None;
        for (work, fresh) in self.work.iter_mut().zip(&mut self.fresh) {
            *fresh = work.untouched();
            if *fresh {
                *work = Work {
                    source: work.source.clone(),
                    id: work.id,
                    seen: work.seen,
                    left: work.left.take(),
                    made: work.made,
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
            Kind::Compress => "Compress",
            Kind::Extract => "Extract",
            Kind::Download => "Download",
            Kind::Upload => "Upload",
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
            started: self.started,
            run: self.run,
            undoable: !self.state.active() && !self.run_steps().is_empty(),
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
    /// Remote jobs' connections, kept only while a job uses them.
    pub(super) remote: Connections,
}

impl Shared {
    #[cfg(test)]
    pub(super) fn new(engine: Engine, wake: Option<UnixStream>) -> Arc<Self> {
        Self::with_connector(engine, wake, crate::remote::ssh::connector())
    }

    pub(super) fn with_connector(
        engine: Engine,
        wake: Option<UnixStream>,
        connector: Connector,
    ) -> Arc<Self> {
        Arc::new(Self {
            remote: Connections::new(engine.paths.places(), connector),
            engine: Mutex::new(engine),
            wake,
        })
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Engine> {
        self.engine.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Journals an item as it stands.
    pub(super) fn record(&self, op: u64, index: usize, work: &Work) -> Result<(), String> {
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
            shared.remote.release_idle();
            if let Some(mut wake) = shared.wake.as_ref() {
                let _ = std::io::Write::write_all(&mut wake, &[0]);
            }
        });
    }
}

/// Works through a job's items until it's done, stopped or cancelled.
pub(super) fn run(shared: &Shared, op: u64) {
    // The totals, for progress; reading the disk, so outside the lock.
    let Some((spec, sources)) = shared.lock().job_mut(op).map(|job| {
        let sources: Vec<(usize, PathBuf)> = job
            .work
            .iter()
            .enumerate()
            .filter(|(_, work)| work.done.is_none())
            .map(|(i, work)| (i, work.source.clone()))
            .collect();
        (job.spec.clone(), sources)
    }) else {
        return;
    };
    let tally = |path: &Path| disk::tally(path).map_or(0, |(_, bytes)| bytes);
    let archive = match spec.kind {
        Kind::Extract => opened(shared, op, &spec).ok(),
        _ => None,
    };
    let sizes: Vec<(usize, u64)> = sources
        .into_iter()
        .map(|(i, source)| {
            let bytes = match spec.kind {
                Kind::Copy | Kind::Move | Kind::Upload => tally(&source),
                // Only the server can say, so the item asks when it starts.
                Kind::Download => 0,
                Kind::Compress => spec.sources.iter().map(|source| tally(source)).sum(),
                Kind::Extract => archive
                    .as_ref()
                    .zip(spec.entry(&source))
                    .map_or(0, |(archive, entry)| archive.bytes(entry)),
            };
            (i, bytes)
        })
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
    let stop = match &done {
        Next::Done(_) => None,
        Next::Stop(_) => Some(State::Waiting),
        Next::Interrupt(_) => Some(State::Interrupted),
    };
    item.work.done = Some(match done {
        Next::Done(done) => done,
        Next::Stop(why) | Next::Interrupt(why) => Done::Failed(why),
    });
    let _ = shared.record(op, index, &item.work);
    if let Some(state) = stop {
        shared.finish(op, state);
        return false;
    }
    true
}

pub(super) enum Next {
    Done(Done),
    /// A conflict under `Conflict::Stop`.
    Stop(String),
    /// The connection to a remote source went, or couldn't be made. The items
    /// not done wait for the job to be continued.
    Interrupt(String),
}

/// One item on its way.
pub(super) struct Item<'a> {
    pub(super) shared: &'a Shared,
    pub(super) op: u64,
    pub(super) index: usize,
    pub(super) spec: &'a Spec,
    pub(super) work: Work,
}

/// An extraction's archive, read once and kept while the job runs.
fn opened(shared: &Shared, op: u64, spec: &Spec) -> Result<Arc<Archive>, String> {
    if let Some(open) = shared
        .lock()
        .job_mut(op)
        .and_then(|job| job.archive.clone())
    {
        return Ok(open);
    }
    let open = Arc::new(Archive::open(
        spec.archive.as_deref().unwrap_or(Path::new("")),
    )?);
    if let Some(job) = shared.lock().job_mut(op) {
        job.archive = Some(open.clone());
    }
    Ok(open)
}

/// The item at `path` and every folder holding it, by identity, so neither
/// a link to one nor its name in another case hides it. A link counts with
/// what it leads to, the folders holding that included. `None` when they
/// can't be found.
fn holders(path: &Path) -> Option<Vec<FileId>> {
    let folder = path.parent()?.canonicalize().ok()?;
    let mut held = vec![disk::id(path)?];
    held.extend(folder.ancestors().filter_map(disk::id));
    if let Ok(resolved) = path.canonicalize() {
        held.extend(resolved.ancestors().filter_map(disk::id));
    }
    Some(held)
}

/// "{path}: a different item...", for a source replaced since the job was
/// queued.
pub(super) fn replaced(path: &Path) -> String {
    format!(
        "{}: a different item is there now than when the job was queued, so it was left alone.",
        path.display()
    )
}

impl Item<'_> {
    fn transfer(&mut self, cancel: &AtomicBool, copied: &AtomicU64) -> Next {
        match self.spec.kind {
            Kind::Copy | Kind::Move => self.copy_or_move(cancel, copied),
            Kind::Compress => self.compress(cancel, copied),
            Kind::Extract => self.extract(cancel, copied),
            Kind::Download => self.download(cancel, copied),
            Kind::Upload => self.upload(cancel, copied),
        }
    }

    fn copy_or_move(&mut self, cancel: &AtomicBool, copied: &AtomicU64) -> Next {
        let source = self.work.source.clone();
        let fail = |why: String| Next::Done(Done::Failed(why));
        let (id, stamp) = match existing(&source) {
            Ok(found) => found,
            Err(e) => return fail(e),
        };
        if self.work.id.is_some_and(|accepted| accepted != id) {
            return fail(replaced(&source));
        }
        let into = self.spec.into.as_path();
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
        let to = into.join(name);
        if self.spec.kind == Kind::Move && to == source {
            return fail(format!(
                "{}: already in {}.",
                source.display(),
                into.display()
            ));
        }
        let (to, replacing) = match self.claim(to, &source, Some(&source)) {
            Ok(claimed) => claimed,
            Err(next) => return next,
        };
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

    /// Where an item meant for `to` goes under the job's conflict choice,
    /// and what it replaces. It comes from `source`, read from the item at
    /// `read`, which is never replaced, nor any folder holding it. A remote
    /// source has none here.
    pub(super) fn claim(
        &self,
        mut to: PathBuf,
        source: &Path,
        read: Option<&Path>,
    ) -> Result<(PathBuf, Option<Step>), Next> {
        let Ok((there, there_stamp)) = disk::stamp(&to) else {
            return Ok((to, None));
        };
        match self.spec.conflict {
            Conflict::Skip => Err(Next::Done(Done::Skipped(format!(
                "{} already exists, so {} was skipped.",
                to.display(),
                source.display()
            )))),
            Conflict::Stop => Err(Next::Stop(format!(
                "{} already exists. Continue the job with keep_both, replace or skip.",
                to.display()
            ))),
            Conflict::KeepBoth => {
                let Some(free) = numbered(&to) else {
                    return Err(Next::Done(Done::Failed(format!(
                        "{}: no free name for it.",
                        to.display()
                    ))));
                };
                to = free;
                Ok((to, None))
            }
            Conflict::Replace
                if read
                    .is_some_and(|read| holders(read).is_none_or(|held| held.contains(&there))) =>
            {
                Err(Next::Done(Done::Failed(format!(
                    "{}: can't be replaced by {}, which it is or holds.",
                    to.display(),
                    source.display()
                ))))
            }
            Conflict::Replace => {
                let replacing = Step {
                    id: there,
                    undone: Spot::Path(to.clone()),
                    done: Spot::Trash(None),
                    applied: false,
                    stamp: there_stamp,
                    keep: None,
                };
                Ok((to, Some(replacing)))
            }
        }
    }

    /// Compresses the job's items into a new archive beside them, named as
    /// Finder does and never replacing anything.
    fn compress(&mut self, cancel: &AtomicBool, copied: &AtomicU64) -> Next {
        let fail = |why: String| Next::Done(Done::Failed(why));
        let spec = self.spec;
        for (source, accepted) in spec.sources.iter().zip(&spec.ids) {
            match disk::lookup(source) {
                Ok(id) if id == *accepted => {}
                Ok(_) => return fail(replaced(source)),
                Err(e) => return fail(format!("{}: {e}", source.display())),
            }
        }
        if !spec.into.is_dir() {
            return fail(format!("{}: not a folder.", spec.into.display()));
        }
        if let Err(why) = spec
            .sources
            .iter()
            .try_for_each(|source| cloud::refuse_placeholders(source))
        {
            return fail(why);
        }
        let name = match spec.sources.as_slice() {
            [one] => format!(
                "{}.zip",
                one.file_name().unwrap_or_default().to_string_lossy()
            ),
            _ => "Archive.zip".into(),
        };
        let mut to = spec.into.join(name);
        if disk::id(&to).is_some() {
            let Some(free) = numbered(&to) else {
                return fail(format!("{}: no free name for it.", to.display()));
            };
            to = free;
        }
        self.work.to = Some(to.clone());
        self.work.bytes = spec
            .sources
            .iter()
            .map(|source| disk::tally(source).map_or(0, |(_, bytes)| bytes))
            .sum();
        let made = self.make_here(&to, None, cancel, |temp| {
            archives::create(&spec.sources, temp, cancel, copied)
        });
        Next::Done(match made {
            Ok(()) => Done::To(to),
            Err(why) => Done::Failed(why),
        })
    }

    /// Extracts one entry, or folder of them, into the job's folder.
    fn extract(&mut self, cancel: &AtomicBool, copied: &AtomicU64) -> Next {
        let fail = |why: String| Next::Done(Done::Failed(why));
        let spec = self.spec;
        let path = spec.archive.as_deref().unwrap_or(Path::new(""));
        let id = match disk::lookup(path) {
            Ok(id) => id,
            Err(e) => return fail(format!("{}: {e}", path.display())),
        };
        if self.work.id.is_some_and(|accepted| accepted != id) {
            return fail(replaced(path));
        }
        if let Err(why) = cloud::refuse_placeholders(path) {
            return fail(why);
        }
        if !spec.into.is_dir() {
            return fail(format!("{}: not a folder.", spec.into.display()));
        }
        let archive = match opened(self.shared, self.op, spec) {
            Ok(archive) => archive,
            Err(e) => return fail(e),
        };
        let source = self.work.source.clone();
        let (Some(entry), Some(name)) = (spec.entry(&source), source.file_name()) else {
            return fail(format!(
                "{}: isn't in {}.",
                source.display(),
                path.display()
            ));
        };
        let (to, replacing) = match self.claim(spec.into.join(name), &source, Some(path)) {
            Ok(claimed) => claimed,
            Err(next) => return next,
        };
        self.work.to = Some(to.clone());
        self.work.bytes = archive.bytes(entry);
        let free = disk::free(&spec.into).unwrap_or(u64::MAX);
        if self.work.bytes > free {
            return fail(format!(
                "{}: needs {} bytes once extracted, more than the {free} free in {}.",
                source.display(),
                self.work.bytes,
                spec.into.display()
            ));
        }
        let made = self.make_here(&to, replacing, cancel, |temp| {
            archive
                .extract(entry, temp, disk::ignores_case(&spec.into), cancel, copied)
                .map_err(|why| format!("{}: {why}", path.display()))
        });
        Next::Done(match made {
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
            keep: None,
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
        cloud::refuse_placeholders(&source)?;
        let meter = disk::Meter { copied, cancel };
        self.make_here(to, replacing, cancel, |temp| {
            disk::copy(&source, temp, Some(&meter))
                .map_err(|e| format!("{}: {e}", source.display()))?;
            let whole = disk::tally(&source).ok() == disk::tally(temp).ok()
                && disk::stamp(&source).ok() == Some((id, stamp));
            if whole {
                Ok(())
            } else {
                Err(format!("{} changed during the copy.", source.display()))
            }
        })?;
        if self.spec.kind == Kind::Move {
            self.apply(Step {
                id,
                undone: Spot::Path(source.clone()),
                done: Spot::Trash(None),
                applied: false,
                stamp,
                keep: None,
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

    /// Makes the item at a hidden name beside `to` with `make`, then puts
    /// it in place, sending what it replaces to the Trash first, so nothing
    /// partial ever sits at `to`.
    pub(super) fn make_here(
        &mut self,
        to: &Path,
        replacing: Option<Step>,
        cancel: &AtomicBool,
        make: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        let temp = self
            .spec
            .into
            .join(format!(".monodrive-{}-{}", self.op, self.index));
        if disk::id(&temp).is_some() {
            return Err(format!("{}: already exists.", temp.display()));
        }
        self.work.temp = Some(temp.clone());
        self.shared.record(self.op, self.index, &self.work)?;
        let made = match make(&temp) {
            Err(_) if cancel.load(Ordering::Relaxed) => Err(format!(
                "{}: cancelled partway.",
                self.work.source.display()
            )),
            made => made.and_then(|()| disk::stamp(&temp).map_err(|e| e.to_string())),
        };
        let (made, made_stamp) = match made {
            Ok(made) => made,
            Err(why) => return Err(self.without_temp(why)),
        };
        if let Some(old) = replacing
            && let Err(e) = self.apply(old)
        {
            return Err(self.without_temp(format!("{e} So {} wasn't replaced.", to.display())));
        }
        let mut placed = Step {
            id: made,
            undone: Spot::Path(temp),
            done: Spot::Path(to.to_path_buf()),
            applied: false,
            stamp: made_stamp,
            keep: None,
        };
        // Undoing it sends it to the Trash, not back to its hidden name.
        if let Err(e) = self.apply_then(&mut placed, |step| step.undone = Spot::Trash(None)) {
            let e = self.with_replaced(e);
            return Err(self.without_temp(e));
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
            Some(temp) => unfinished(why, temp, self.spec.kind),
            None => why,
        }
    }
}

/// `why`, after sending the unfinished item at `temp`, if it's there, to the
/// Trash.
fn unfinished(why: String, temp: &Path, kind: Kind) -> String {
    if disk::id(temp).is_none() {
        return why;
    }
    let made = kind.made();
    match disk::trash(temp) {
        Ok(landed) => format!(
            "{why} The unfinished {made} went to the Trash at {}.",
            landed.display()
        ),
        Err(e) => format!("{why} The unfinished {made} is still at {e}"),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `path` as "name 2.ext", "name 3.ext" and on: the first that's free.
pub(super) fn numbered(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    (2..).find_map(|n: u32| {
        let free = path.with_file_name(numbered_name(name, n));
        disk::id(&free).is_none().then_some(free)
    })
}

/// `name` as "name 2.ext".
pub(super) fn numbered_name(name: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    format!("{stem} {n}{ext}")
}

impl Engine {
    /// Accepts a copy, move, compression or extraction as a queued job, or
    /// queues a stopped one again.
    pub(super) fn queue(&mut self, submission: Submission) -> Result<Outcome, String> {
        let op = self.next;
        let record = match &submission.request {
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
                    at: now(),
                }
            }
            request => Record::Job {
                op,
                submission: submission.clone(),
                spec: Spec::new(request)?,
                at: now(),
            },
        };
        self.write(&record)?;
        self.fold(&record);
        Ok(self.finished[&submission.id].1.clone())
    }

    pub(super) fn cancel(&mut self, job: u64) -> Result<Outcome, String> {
        let entry = self.job_mut(job).ok_or_else(|| format!("No job {job}."))?;
        if !entry.state.active() && entry.state != State::Waiting {
            return Err(format!("Job {job} has already stopped."));
        }
        entry.cancel.store(true, Ordering::Relaxed);
        // A running job stops itself after its item; one queued or waiting
        // at a conflict has nothing on the way.
        if entry.state != State::Running {
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
    /// copy, archive or extraction goes to the Trash.
    pub(super) fn settle_jobs(&self) -> Vec<Record> {
        let mut records = Vec::new();
        for job in self.jobs.iter().filter(|job| job.state.active()) {
            for (index, work) in job.work.iter().enumerate() {
                if work.done.is_some()
                    || (work.to.is_none() && work.steps.is_empty() && work.left.is_none())
                {
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
                let whole = placed && (job.spec.kind != Kind::Move || left);
                let mut why = format!(
                    "Monodrive stopped partway through {}; undo reverses what it changed.",
                    work.source.display()
                );
                if let Some(temp) = work.temp.as_deref() {
                    why = unfinished(why, temp, job.spec.kind);
                }
                if let Some(note) = work.left_note() {
                    why = format!("{why} {note}");
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
