//! Batch rename. A `Plan` works out every item's new name from the disk
//! without changing it, so the window and agents preview the same table;
//! the owner works it out again when it runs, as one operation.

use super::{
    Engine, FileId, Item, Outcome, Request, Spot, Stamp, Step, Submission, check_name, disk, route,
    shift_all,
};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// How each new name is made from the old one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Rule {
    /// Replaces every `find` with `with`, before the extension unless
    /// `extension` says in the whole name.
    Replace {
        find: String,
        #[serde(default)]
        with: String,
        #[serde(default)]
        case_sensitive: bool,
        #[serde(default)]
        extension: bool,
    },
    /// `name` and a number counting up from `start` in the items' order,
    /// padded to `digits`: "Trip 001". Keeps extensions.
    Format {
        #[serde(default)]
        name: String,
        #[serde(default = "one")]
        start: u64,
        #[serde(default)]
        digits: usize,
        #[serde(default)]
        place: Place,
    },
}

fn one() -> u64 {
    1
}

/// Which side of the name Format puts the number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Place {
    Before,
    #[default]
    After,
}

impl Rule {
    /// The new name for `old`, the item at `n` in the batch.
    fn apply(&self, old: &str, n: usize, has_extension: bool) -> String {
        let (stem, ext) = match old.rfind('.') {
            Some(dot) if dot > 0 && has_extension => old.split_at(dot),
            _ => (old, ""),
        };
        match self {
            Rule::Replace {
                find,
                with,
                case_sensitive,
                extension: true,
            } => replace(old, find, with, *case_sensitive),
            Rule::Replace {
                find,
                with,
                case_sensitive,
                extension: false,
            } => replace(stem, find, with, *case_sensitive) + ext,
            Rule::Format {
                name,
                start,
                digits,
                place,
            } => {
                // Any longer is too long a name anyway.
                let width = (*digits).min(255);
                let number = format!("{:0width$}", start.saturating_add(n as u64));
                let named = match (name.is_empty(), place) {
                    (true, _) => number,
                    (false, Place::After) => format!("{name} {number}"),
                    (false, Place::Before) => format!("{number} {name}"),
                };
                named + ext
            }
        }
    }
}

fn replace(text: &str, find: &str, with: &str, case_sensitive: bool) -> String {
    if find.is_empty() {
        return text.to_string();
    }
    if case_sensitive {
        return text.replace(find, with);
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        match matched(rest, find) {
            Some(len) => {
                out.push_str(with);
                rest = &rest[len..];
            }
            None => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

/// How many bytes of `text` match `find` at its start, whatever their case.
fn matched(text: &str, find: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    for f in find.chars() {
        let (_, c) = chars.next()?;
        if !c.to_lowercase().eq(f.to_lowercase()) {
            return None;
        }
    }
    Some(chars.next().map_or(text.len(), |(at, _)| at))
}

/// Every item's new name and whether it can take it, in the batch's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub path: PathBuf,
    /// The new name, or the old one when the rule makes none.
    pub name: String,
    pub status: Status,
    found: Option<(FileId, Stamp)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Unchanged,
    /// The new name can't be a name, or the item can't be read: why.
    Invalid(String),
    /// Something else has or gets the new name: what.
    Collision(String),
}

/// What a plan was worked out from and came to: each path's item and stamp,
/// its new name, and whether it would be renamed. A batch only runs if it
/// works out the same.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint(Vec<Seen>);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    path: PathBuf,
    found: Option<(FileId, Stamp)>,
    name: String,
    ok: bool,
}

impl Plan {
    /// Reads the disk and changes nothing. Numbers count in the order of
    /// `paths`. New names collide as APFS compares them: whatever their
    /// Unicode normalization, and their case where the volume ignores it.
    pub fn new(paths: &[PathBuf], rule: &Rule) -> Self {
        let mut rows: Vec<Row> = paths
            .iter()
            .enumerate()
            .map(|(n, path)| Row::new(path, rule, n))
            .collect();
        let renamed: HashSet<PathBuf> = rows
            .iter()
            .filter(|row| row.status == Status::Ok)
            .map(|row| row.path.clone())
            .collect();
        let mut first: HashMap<FileId, usize> = HashMap::new();
        for (i, row) in rows.iter_mut().enumerate() {
            let Some((id, _)) = row.found else { continue };
            let earlier = *first.entry(id).or_insert(i);
            if row.status != Status::Ok {
                continue;
            }
            if earlier != i {
                row.status = Status::Invalid("It's listed twice.".into());
            } else if let Some(outer) = row.path.ancestors().skip(1).find(|a| renamed.contains(*a))
            {
                row.status = Status::Invalid(format!(
                    "It's inside {}, which is renamed too.",
                    outer.display()
                ));
            }
        }

        let mut ignores_case: HashMap<PathBuf, bool> = HashMap::new();
        let mut taken: HashMap<(PathBuf, String), PathBuf> = HashMap::new();
        for row in rows.iter_mut().filter(|row| row.status == Status::Ok) {
            let folder = row.path.parent().unwrap_or(&row.path).to_path_buf();
            let folded = *ignores_case
                .entry(folder.clone())
                .or_insert_with(|| disk::ignores_case(&folder));
            let key = if folded {
                row.name.to_lowercase().nfd().collect()
            } else {
                row.name.nfd().collect()
            };
            match taken.entry((folder, key)) {
                Entry::Occupied(first) => {
                    row.status = Status::Collision(format!(
                        "{} is also the new name of {}.",
                        row.name,
                        first.get().display()
                    ));
                }
                Entry::Vacant(free) => {
                    free.insert(row.path.clone());
                }
            }
        }

        // Taking a batch item's name is fine only if that item leaves it,
        // and one that can't leaves another stuck in turn.
        let there: Vec<Option<FileId>> = rows
            .iter()
            .map(|row| match row.status {
                Status::Ok => disk::id(&row.to()),
                _ => None,
            })
            .collect();
        loop {
            let mut stuck = Vec::new();
            for (i, row) in rows.iter().enumerate() {
                let Some(there) = there[i] else { continue };
                let leaving = first
                    .get(&there)
                    .is_some_and(|&j| rows[j].status == Status::Ok);
                if row.status == Status::Ok && Some(there) != row.id() && !leaving {
                    stuck.push(i);
                }
            }
            if stuck.is_empty() {
                break;
            }
            for i in stuck {
                let why = format!("{} already exists.", rows[i].name);
                rows[i].status = Status::Collision(why);
            }
        }
        Self { rows }
    }

    pub fn fingerprint(&self) -> Fingerprint {
        Fingerprint(
            self.rows
                .iter()
                .map(|row| Seen {
                    path: row.path.clone(),
                    found: row.found,
                    name: row.name.clone(),
                    ok: row.status == Status::Ok,
                })
                .collect(),
        )
    }

    /// The rows that stop the batch unless problems are skipped.
    pub fn problems(&self) -> impl Iterator<Item = &Row> {
        self.rows
            .iter()
            .filter(|row| matches!(row.status, Status::Invalid(_) | Status::Collision(_)))
    }

    /// Refuses unless the plan works out as it did for `preview`.
    fn matches(&self, preview: &Fingerprint) -> Result<(), String> {
        let now = self.fingerprint();
        if now == *preview {
            return Ok(());
        }
        let same_paths = now.0.len() == preview.0.len()
            && now.0.iter().zip(&preview.0).all(|(a, b)| a.path == b.path);
        if !same_paths {
            return Err("Nothing was renamed: these aren't the items previewed.".into());
        }
        let changed: Vec<String> = self
            .rows
            .iter()
            .zip(now.0.iter().zip(&preview.0))
            .filter(|(_, (now, then))| now != then)
            .map(|(row, (now, then))| match &row.status {
                _ if now.found != then.found => {
                    format!("{}: changed since the preview.", row.path.display())
                }
                _ if now.name != then.name => format!(
                    "{}: would be named {}, not {} as previewed.",
                    row.path.display(),
                    now.name,
                    then.name
                ),
                Status::Ok => format!("{}: can be renamed now.", row.path.display()),
                _ => row.problem(),
            })
            .collect();
        Err(format!(
            "Nothing was renamed, since the preview is out of date:\n{}",
            changed.join("\n")
        ))
    }
}

impl Row {
    fn new(path: &Path, rule: &Rule, n: usize) -> Self {
        let old = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let mut row = Self {
            path: path.to_path_buf(),
            name: old.to_string(),
            status: Status::Ok,
            found: None,
        };
        let found = if path.is_absolute() {
            disk::stamp(path).map_err(|e| e.to_string())
        } else {
            Err("Paths must be absolute.".into())
        };
        match found {
            Ok(found) => row.found = Some(found),
            Err(why) => {
                row.status = Status::Invalid(why);
                return row;
            }
        }
        if old.is_empty() {
            row.status = Status::Invalid("Its name can't be read as text.".into());
            return row;
        }
        let folder = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
        row.name = rule.apply(old, n, !folder || crate::info::is_package(path));
        row.status = if row.name == old {
            Status::Unchanged
        } else {
            check_name(&row.name).map_or_else(Status::Invalid, |()| Status::Ok)
        };
        row
    }

    fn id(&self) -> Option<FileId> {
        self.found.map(|(id, _)| id)
    }

    fn to(&self) -> PathBuf {
        self.path.with_file_name(&self.name)
    }

    /// The path and why it won't be renamed.
    fn problem(&self) -> String {
        let why = match &self.status {
            Status::Invalid(why) | Status::Collision(why) => why.as_str(),
            Status::Ok | Status::Unchanged => "",
        };
        format!("{}: {why}", self.path.display())
    }

    fn step(&self) -> Option<Step> {
        let (id, stamp) = self.found?;
        Some(Step {
            id,
            undone: Spot::Path(self.path.clone()),
            done: Spot::Path(self.to()),
            applied: false,
            stamp,
            keep: Vec::new(),
        })
    }
}

impl Engine {
    /// Works the plan out again and renames what it can, as one operation.
    /// Items are renamed one by one, so a failure partway keeps the rest;
    /// undo reverses those that were.
    pub(super) fn rename_many(&mut self, submission: Submission) -> Result<Outcome, String> {
        let Request::RenameMany {
            paths,
            rule,
            preview,
            skip_problems,
        } = &submission.request
        else {
            return Err("Not a batch rename.".into());
        };
        let plan = Plan::new(paths, rule);
        if let Some(preview) = preview {
            plan.matches(preview)?;
        }
        let problems: Vec<String> = plan.problems().map(Row::problem).collect();
        if !problems.is_empty() && !skip_problems {
            return Err(format!("Nothing was renamed:\n{}", problems.join("\n")));
        }
        let mut steps: Vec<Step> = plan
            .rows
            .iter()
            .filter(|row| row.status == Status::Ok)
            .filter_map(Row::step)
            .collect();
        if steps.is_empty() {
            return Err("Nothing to rename: no item gets a new name.".into());
        }
        let moving: Vec<usize> = (0..steps.len()).collect();
        let (order, temps) = route(&steps, &moving, true, self.next);
        let op = self.begin(&submission, None, steps.clone(), Vec::new(), temps.clone())?;
        let mut done = shift_all(&mut steps, &order, &temps, true);
        done.sort_by_key(|(i, _)| *i);
        let items: Vec<Item> = done.into_iter().map(|(_, item)| item).collect();
        steps.retain(|step| step.applied);
        let title = submission.request.title().to_string();
        Ok(self.end(op, title, items, steps))
    }
}
