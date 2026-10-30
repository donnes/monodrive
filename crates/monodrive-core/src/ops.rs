//! File operations: move to Trash, put back, rename, new folder and file,
//! duplicate, undo and redo. One owner process runs them for the window and
//! every `monodrive mcp` server, so they share one journal and one undo
//! history. docs/operations.md records why.

mod disk;
mod owner;

pub use owner::{Client, Owner, run_owner};

use disk::{FileId, Stamp};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Where the owner keeps its state, and the settings it obeys.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The journal, lock and socket, in a folder only this user can open.
    pub state: PathBuf,
    /// The app's settings.json, read for the Agents setting.
    pub settings: PathBuf,
}

impl Paths {
    /// Monodrive's own, in its support folder.
    pub fn user() -> Option<Self> {
        let support = crate::support_dir()?;
        Some(Self {
            state: support.join("ops"),
            settings: support.join("settings.json"),
        })
    }
}

/// What to do. Paths are absolute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Trash {
        paths: Vec<PathBuf>,
    },
    /// Restores items Monodrive moved to the Trash, named by their paths
    /// there.
    PutBack {
        paths: Vec<PathBuf>,
    },
    /// Renames an item within its folder.
    Rename {
        path: PathBuf,
        name: String,
    },
    NewFolder {
        path: PathBuf,
    },
    NewFile {
        path: PathBuf,
    },
    /// Copies each item beside itself, named as Finder does.
    Duplicate {
        paths: Vec<PathBuf>,
    },
    Undo,
    Redo,
}

impl Request {
    pub fn title(&self) -> &'static str {
        match self {
            Request::Trash { .. } => "Move to Trash",
            Request::PutBack { .. } => "Put Back",
            Request::Rename { .. } => "Rename",
            Request::NewFolder { .. } => "New Folder",
            Request::NewFile { .. } => "New File",
            Request::Duplicate { .. } => "Duplicate",
            Request::Undo => "Undo",
            Request::Redo => "Redo",
        }
    }
}

/// Who asked. An agent is known by the name its MCP client gives, not by
/// the person behind it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Origin {
    Window,
    Agent { client: Option<String> },
}

/// One request as sent to the owner. Sending the same `id` again returns
/// the first outcome instead of running the request twice.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Submission {
    pub id: String,
    pub origin: Origin,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub op: u64,
    /// "Rename", or "Undo Rename".
    pub title: String,
    pub items: Vec<Item>,
    /// The owner stopped partway; the results come from checking the disk.
    pub interrupted: bool,
}

impl Outcome {
    pub fn failures(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .filter_map(|item| item.result.as_ref().err())
            .map(String::as_str)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// The path asked about, or where undo or redo found the item.
    pub path: PathBuf,
    /// Where the item is now, or why it didn't change, path included.
    pub result: Result<PathBuf, String>,
}

/// Where an item is while its operation is done or undone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Spot {
    Path(PathBuf),
    /// In the Trash, where macOS put it; `None` until it first goes there.
    /// What an operation created waits in the Trash while it's undone.
    Trash(Option<PathBuf>),
}

/// One item an operation moved, and the two places it can be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Step {
    id: FileId,
    undone: Spot,
    done: Spot,
    /// At `done` rather than `undone`.
    applied: bool,
    /// For an item the operation created: how it was then, so undo refuses
    /// to trash it once it's been edited.
    created: Option<Stamp>,
}

impl Step {
    fn spot(&self, done: bool) -> &Spot {
        if done { &self.done } else { &self.undone }
    }

    /// The item's path now, if it's known.
    fn here(&self) -> Option<&Path> {
        match self.spot(self.applied) {
            Spot::Path(path) | Spot::Trash(Some(path)) => Some(path),
            Spot::Trash(None) => None,
        }
    }

    /// Checks that the item can move to its `done` (or `undone`) spot: it's
    /// still where and what it was, and nothing is in its way.
    fn ready(&self, done: bool) -> Result<PathBuf, String> {
        let here = self
            .here()
            .ok_or("Monodrive doesn't know where this item is.")?;
        if disk::id(here) != Some(self.id) {
            return Err(format!(
                "{} is no longer the item Monodrive moved: it was moved, replaced or deleted since.",
                here.display()
            ));
        }
        if let (Some(created), false) = (self.created, done)
            && disk::stamp(here).map(|(_, now)| now).ok() != Some(created)
        {
            return Err(format!("{} changed since it was created.", here.display()));
        }
        if let Spot::Path(to) = self.spot(done)
            && disk::id(to).is_some_and(|there| there != self.id)
        {
            return Err(format!("{} already exists.", to.display()));
        }
        Ok(here.to_path_buf())
    }

    /// Moves the item to its `done` (or `undone`) spot, checking first.
    fn shift(&mut self, done: bool) -> Result<PathBuf, String> {
        let from = self.ready(done)?;
        let to = match self.spot(done) {
            Spot::Path(to) => {
                disk::rename_new(&from, to).map_err(|e| format!("{}: {e}", from.display()))?;
                to.clone()
            }
            Spot::Trash(_) => {
                let landed = disk::trash(&from)?;
                *(if done {
                    &mut self.done
                } else {
                    &mut self.undone
                }) = Spot::Trash(Some(landed.clone()));
                landed
            }
        };
        self.applied = done;
        if self.created.is_some() && done {
            self.created = disk::stamp(&to).ok().map(|(_, stamp)| stamp);
        }
        Ok(to)
    }

    /// After a crash: finds where the item ended up, without moving it.
    fn locate(&mut self) -> Result<PathBuf, String> {
        if let Some(here) = self.here()
            && disk::id(here) == Some(self.id)
        {
            return Err(format!(
                "{}: Monodrive stopped before this changed.",
                here.display()
            ));
        }
        let original = match (&self.undone, &self.done) {
            (Spot::Path(path), _) | (_, Spot::Path(path)) => path.clone(),
            _ => PathBuf::new(),
        };
        let there = match self.spot(!self.applied) {
            Spot::Path(path) => Some(path.clone()).filter(|path| disk::id(path) == Some(self.id)),
            Spot::Trash(Some(path)) if disk::id(path) == Some(self.id) => Some(path.clone()),
            Spot::Trash(_) => disk::find_in_trash(self.id, &original),
        };
        let there = there.ok_or_else(|| {
            format!(
                "{}: Monodrive stopped partway and can't find where this went.",
                original.display()
            )
        })?;
        if let Spot::Trash(_) = self.spot(!self.applied) {
            *(if self.applied {
                &mut self.undone
            } else {
                &mut self.done
            }) = Spot::Trash(Some(there.clone()));
        }
        self.applied = !self.applied;
        Ok(there)
    }
}

/// One line of the journal.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum Record {
    /// Written and synced before anything on disk changes. `steps` are the
    /// items about to move (for undo and redo, all of `target`'s), and
    /// `creates` the paths about to be created or copied.
    Begin {
        op: u64,
        submission: Submission,
        target: Option<u64>,
        steps: Vec<Step>,
        creates: Vec<PathBuf>,
    },
    /// How it ended: the items it moved, or for undo and redo all of the
    /// target's items as they are now.
    End {
        op: u64,
        outcome: Outcome,
        steps: Vec<Step>,
    },
}

struct Begun {
    submission: Submission,
    target: Option<u64>,
    steps: Vec<Step>,
    creates: Vec<PathBuf>,
}

/// An operation undo can reach.
struct Undoable {
    op: u64,
    title: &'static str,
    steps: Vec<Step>,
}

const JOURNAL: &str = "journal.jsonl";
/// Past this many finished operations, opening the journal trims it to the
/// most recent `KEEP`. Undo reaches back that far at most.
const TRIM_AT: usize = 1000;
const KEEP: usize = 500;

/// Runs requests against the disk and the journal. Only the owner has one.
struct Engine {
    paths: Paths,
    journal: File,
    next: u64,
    /// Operations started but not known to have ended.
    begun: HashMap<u64, Begun>,
    /// Finished operations by request id, with what they asked.
    finished: HashMap<String, (Request, Outcome)>,
    /// Oldest first.
    history: Vec<Undoable>,
    /// How many at the end of `history` are undone, for redo.
    undone: usize,
}

impl Engine {
    /// Reads the journal, settles operations a crash left unfinished, and
    /// trims it once it's long.
    fn open(paths: Paths) -> Result<Self, String> {
        let at = paths.state.join(JOURNAL);
        let records: Vec<Record> = match File::open(&at) {
            Ok(file) => BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                // A crash mid-write can leave the last line torn.
                .filter_map(|line| serde_json::from_str(&line).ok())
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("{}: {e}", at.display())),
        };
        let mut engine = Self::load(paths, &records)?;
        let mut unfinished: Vec<u64> = engine.begun.keys().copied().collect();
        unfinished.sort_unstable();
        let mut records = records;
        for op in unfinished {
            let end = engine.settle(op);
            engine.write(&end)?;
            engine.fold(&end);
            records.push(end);
        }
        if engine.finished.len() > TRIM_AT {
            let mut ends: Vec<u64> = records
                .iter()
                .filter_map(|r| match r {
                    Record::End { op, .. } => Some(*op),
                    Record::Begin { .. } => None,
                })
                .collect();
            ends.sort_unstable();
            let first = ends[ends.len() - KEEP];
            records.retain(|r| match r {
                Record::Begin { op, .. } | Record::End { op, .. } => *op >= first,
            });
            let tmp = at.with_extension("jsonl.tmp");
            let mut file = File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            for record in &records {
                let line = serde_json::to_string(record).map_err(|e| e.to_string())?;
                writeln!(file, "{line}").map_err(|e| format!("{}: {e}", tmp.display()))?;
            }
            file.sync_all()
                .and_then(|()| std::fs::rename(&tmp, &at))
                .map_err(|e| format!("{}: {e}", at.display()))?;
            engine = Self::load(engine.paths, &records)?;
        }
        Ok(engine)
    }

    fn load(paths: Paths, records: &[Record]) -> Result<Self, String> {
        let at = paths.state.join(JOURNAL);
        let journal = File::options()
            .create(true)
            .append(true)
            .open(&at)
            .map_err(|e| format!("{}: {e}", at.display()))?;
        let mut engine = Self {
            paths,
            journal,
            next: 1,
            begun: HashMap::new(),
            finished: HashMap::new(),
            history: Vec::new(),
            undone: 0,
        };
        for record in records {
            engine.fold(record);
        }
        Ok(engine)
    }

    /// Applies one journal record to what's in memory.
    fn fold(&mut self, record: &Record) {
        match record.clone() {
            Record::Begin {
                op,
                submission,
                target,
                steps,
                creates,
            } => {
                self.next = self.next.max(op + 1);
                self.begun.insert(
                    op,
                    Begun {
                        submission,
                        target,
                        steps,
                        creates,
                    },
                );
            }
            Record::End { op, outcome, steps } => {
                let Some(begun) = self.begun.remove(&op) else {
                    return;
                };
                let request = begun.submission.request;
                let target = begun
                    .target
                    .and_then(|target| self.history.iter().position(|u| u.op == target));
                match (&request, target) {
                    (Request::Undo, Some(at)) => {
                        self.history[at].steps = steps;
                        self.undone = self.history.len() - at;
                    }
                    (Request::Redo, Some(at)) => {
                        self.history[at].steps = steps;
                        self.undone = self.history.len() - at - 1;
                    }
                    (Request::Undo | Request::Redo, None) => {}
                    _ if steps.is_empty() => {}
                    _ => {
                        // A new operation ends the redo trail.
                        self.history.truncate(self.history.len() - self.undone);
                        self.undone = 0;
                        self.history.push(Undoable {
                            op,
                            title: request.title(),
                            steps,
                        });
                    }
                }
                self.finished
                    .insert(begun.submission.id, (request, outcome));
            }
        }
    }

    /// The `End` for an operation the owner didn't finish, worked out from
    /// where its items are. Nothing is redone.
    fn settle(&self, op: u64) -> Record {
        let begun = &self.begun[&op];
        let reversal = begun.target.is_some();
        let mut items = Vec::new();
        let mut steps = Vec::new();
        for step in &begun.steps {
            let mut step = step.clone();
            let path = step.here().map(Path::to_path_buf).unwrap_or_default();
            let before = step.applied;
            let result = step.locate();
            if reversal || step.applied != before {
                steps.push(step);
            }
            items.push(Item { path, result });
        }
        items.extend(begun.creates.iter().map(|path| Item {
            path: path.clone(),
            result: Err(format!(
                "{}: Monodrive stopped partway, so check whether this was created or copied.",
                path.display()
            )),
        }));
        let title = match begun.target {
            Some(target) => {
                let of = self
                    .history
                    .iter()
                    .find(|u| u.op == target)
                    .map_or("", |u| u.title);
                format!("{} {of}", begun.submission.request.title())
            }
            None => begun.submission.request.title().to_string(),
        };
        Record::End {
            op,
            outcome: Outcome {
                op,
                title,
                items,
                interrupted: true,
            },
            steps,
        }
    }

    /// Appends a record and waits for it to reach the disk.
    fn write(&mut self, record: &Record) -> Result<(), String> {
        let mut line = serde_json::to_string(record).map_err(|e| e.to_string())?;
        line.push('\n');
        self.journal
            .write_all(line.as_bytes())
            .and_then(|()| self.journal.sync_data())
            .map_err(|e| format!("Monodrive's journal: {e}"))
    }

    fn submit(&mut self, submission: Submission) -> Result<Outcome, String> {
        if let Some((request, outcome)) = self.finished.get(&submission.id) {
            return if *request == submission.request {
                Ok(outcome.clone())
            } else {
                Err(format!(
                    "Request {} was already used for another operation.",
                    submission.id
                ))
            };
        }
        if matches!(submission.origin, Origin::Agent { .. }) && !agents_change_files(&self.paths) {
            return Err(
                "Agents can't change files: Let agents change files is off in Monodrive's Agents settings."
                    .into(),
            );
        }
        match submission.request {
            Request::Undo => self.reverse(submission, false),
            Request::Redo => self.reverse(submission, true),
            _ => self.run(submission),
        }
    }

    fn run(&mut self, submission: Submission) -> Result<Outcome, String> {
        let plans = self.plan(&submission.request);
        if plans.iter().all(|(_, plan)| plan.is_err()) {
            let refused: Vec<String> = plans.into_iter().filter_map(|(_, p)| p.err()).collect();
            return Err(refused.join("\n"));
        }
        let mut steps = Vec::new();
        let mut creates = Vec::new();
        for (path, plan) in &plans {
            match plan {
                Ok(Plan::Move(step)) => steps.push(step.clone()),
                Ok(Plan::Create(_)) => creates.push(path.clone()),
                Err(_) => {}
            }
        }
        let op = self.begin(&submission, None, steps, creates)?;
        let mut items = Vec::new();
        let mut steps = Vec::new();
        for (path, plan) in plans {
            let result = plan.and_then(|plan| match plan {
                Plan::Move(mut step) => {
                    let to = step.shift(true)?;
                    steps.push(step);
                    Ok(to)
                }
                Plan::Create(make) => {
                    let made = make.run(&path)?;
                    let (id, stamp) =
                        disk::stamp(&made).map_err(|e| format!("{}: {e}", made.display()))?;
                    steps.push(Step {
                        id,
                        undone: Spot::Trash(None),
                        done: Spot::Path(made.clone()),
                        applied: true,
                        created: Some(stamp),
                    });
                    Ok(made)
                }
            });
            items.push(Item { path, result });
        }
        let title = submission.request.title().to_string();
        Ok(self.end(op, title, items, steps))
    }

    /// Checks each item of a request against the disk now.
    fn plan(&self, request: &Request) -> Vec<(PathBuf, Result<Plan, String>)> {
        let each = |paths: &[PathBuf], plan: &dyn Fn(&Path) -> Result<Plan, String>| {
            paths
                .iter()
                .map(|path| (path.clone(), plan(path)))
                .collect()
        };
        match request {
            Request::Trash { paths } => each(paths, &plan_trash),
            Request::PutBack { paths } => each(paths, &|path| self.plan_put_back(path)),
            Request::Rename { path, name } => vec![(path.clone(), plan_rename(path, name))],
            Request::NewFolder { path } => vec![(path.clone(), plan_new(path, Make::Folder))],
            Request::NewFile { path } => vec![(path.clone(), plan_new(path, Make::File))],
            Request::Duplicate { paths } => each(paths, &|path| {
                existing(path).map(|_| Plan::Create(Make::Copy))
            }),
            Request::Undo | Request::Redo => Vec::new(),
        }
    }

    /// Puts back an item from where Monodrive's history last left it in
    /// the Trash.
    fn plan_put_back(&self, path: &Path) -> Result<Plan, String> {
        let id = existing(path)?;
        let trashed = Spot::Trash(Some(path.to_path_buf()));
        let original = self
            .history
            .iter()
            .rev()
            .flat_map(|u| &u.steps)
            .filter(|step| step.id == id && *step.spot(step.applied) == trashed)
            .find_map(|step| match step.spot(!step.applied) {
                Spot::Path(original) => Some(original.clone()),
                Spot::Trash(_) => None,
            })
            .ok_or_else(|| {
                format!(
                    "{}: Monodrive didn't move this to the Trash, so it doesn't know where it came from.",
                    path.display()
                )
            })?;
        movable(Step {
            id,
            undone: trashed,
            done: Spot::Path(original),
            applied: false,
            created: None,
        })
    }

    fn reverse(&mut self, submission: Submission, redo: bool) -> Result<Outcome, String> {
        let verb = if redo { "redo" } else { "undo" };
        let at = if redo {
            (self.undone > 0).then(|| self.history.len() - self.undone)
        } else {
            self.history.len().checked_sub(self.undone + 1)
        };
        let at = at.ok_or_else(|| format!("Nothing to {verb}."))?;
        let target = &self.history[at];
        let (of, target_op) = (target.title, target.op);
        // Undo moves back what the operation moved; redo what undo did.
        let moving: Vec<usize> = (0..target.steps.len())
            .filter(|&i| target.steps[i].applied != redo)
            .collect();
        let refused: Vec<String> = moving
            .iter()
            .filter_map(|&i| target.steps[i].ready(redo).err())
            .collect();
        if !refused.is_empty() {
            return Err(format!("Can't {verb} {of}: {}", refused.join(" ")));
        }
        let mut steps = target.steps.clone();
        let op = self.begin(&submission, Some(target_op), steps.clone(), Vec::new())?;
        let items = moving
            .into_iter()
            .map(|i| Item {
                path: steps[i].here().map(Path::to_path_buf).unwrap_or_default(),
                result: steps[i].shift(redo),
            })
            .collect();
        let title = format!("{} {of}", submission.request.title());
        Ok(self.end(op, title, items, steps))
    }

    /// Records what's about to change, before it does.
    fn begin(
        &mut self,
        submission: &Submission,
        target: Option<u64>,
        steps: Vec<Step>,
        creates: Vec<PathBuf>,
    ) -> Result<u64, String> {
        let op = self.next;
        let record = Record::Begin {
            op,
            submission: submission.clone(),
            target,
            steps,
            creates,
        };
        self.write(&record)?;
        self.fold(&record);
        Ok(op)
    }

    /// Records how an operation ended. The disk has changed by now, so a
    /// journal that can't be written still returns the outcome; the next
    /// owner settles the operation from the disk.
    fn end(&mut self, op: u64, title: String, items: Vec<Item>, steps: Vec<Step>) -> Outcome {
        let outcome = Outcome {
            op,
            title,
            items,
            interrupted: false,
        };
        let record = Record::End {
            op,
            outcome: outcome.clone(),
            steps,
        };
        let _ = self.write(&record);
        self.fold(&record);
        outcome
    }
}

enum Plan {
    Move(Step),
    /// Creates or copies the item's path.
    Create(Make),
}

fn absolute(path: &Path) -> Result<(), String> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(format!("{}: paths must be absolute.", path.display()))
    }
}

fn existing(path: &Path) -> Result<FileId, String> {
    absolute(path)?;
    disk::id(path).ok_or_else(|| format!("{}: No such file or folder.", path.display()))
}

fn plan_trash(path: &Path) -> Result<Plan, String> {
    let id = existing(path)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if path.parent().is_none() || home.as_deref() == Some(path) {
        return Err(format!(
            "{}: Monodrive won't move the disk or your home folder to the Trash.",
            path.display()
        ));
    }
    Ok(Plan::Move(Step {
        id,
        undone: Spot::Path(path.to_path_buf()),
        done: Spot::Trash(None),
        applied: false,
        created: None,
    }))
}

fn plan_rename(path: &Path, name: &str) -> Result<Plan, String> {
    let id = existing(path)?;
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(format!("{name:?} can't be a name."));
    }
    let to = path.with_file_name(name);
    if to == path {
        return Err(format!("{} already has that name.", path.display()));
    }
    movable(Step {
        id,
        undone: Spot::Path(path.to_path_buf()),
        done: Spot::Path(to),
        applied: false,
        created: None,
    })
}

/// A move whose destination is free now.
fn movable(step: Step) -> Result<Plan, String> {
    step.ready(true)?;
    Ok(Plan::Move(step))
}

fn plan_new(path: &Path, make: Make) -> Result<Plan, String> {
    absolute(path)?;
    if disk::id(path).is_some() {
        return Err(format!("{} already exists.", path.display()));
    }
    Ok(Plan::Create(make))
}

enum Make {
    Folder,
    File,
    /// A copy of the path, beside it.
    Copy,
}

impl Make {
    /// Makes it, never replacing anything, and returns its path.
    fn run(&self, path: &Path) -> Result<PathBuf, String> {
        let made = match self {
            Make::Folder => std::fs::create_dir(path).map(|()| path.to_path_buf()),
            Make::File => File::options()
                .write(true)
                .create_new(true)
                .open(path)
                .map(|_| path.to_path_buf()),
            Make::Copy => {
                let copy = copy_name(path)
                    .ok_or_else(|| format!("{}: can't name a copy of this.", path.display()))?;
                disk::duplicate(path, &copy).map(|()| copy)
            }
        };
        made.map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Finder's name for a copy: "name copy.ext", then "name copy 2.ext" and
/// on, never a name that's taken. A copy of a copy counts on from it.
fn copy_name(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    let base = stem
        .strip_suffix(" copy")
        .or_else(|| {
            let (base, n) = stem.rsplit_once(" copy ")?;
            n.bytes().all(|b| b.is_ascii_digit()).then_some(base)
        })
        .unwrap_or(stem);
    (1..).find_map(|n: u32| {
        let name = match n {
            1 => format!("{base} copy{ext}"),
            n => format!("{base} copy {n}{ext}"),
        };
        let copy = path.with_file_name(name);
        disk::id(&copy).is_none().then_some(copy)
    })
}

/// The Agents setting, read from the app's settings for each request so a
/// change applies at once. A missing or unreadable file means the default,
/// on, as the app reads it.
fn agents_change_files(paths: &Paths) -> bool {
    /// `Preferences::agents_change_files` in the app.
    #[derive(Deserialize)]
    struct Settings {
        #[serde(default = "on")]
        agents_change_files: bool,
    }
    fn on() -> bool {
        true
    }
    std::fs::read(&paths.settings)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .is_none_or(|settings| settings.agents_change_files)
}

#[cfg(test)]
mod tests;
