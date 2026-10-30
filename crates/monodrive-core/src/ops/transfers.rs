//! Copies between this Mac and a remote source, as jobs. A download makes its
//! copy the way any copy here does, so its conflicts, Trash and undo are the
//! same. An upload writes to a temp name of its own and renames it into place.
//! Nothing is resumed: continuing a job checks the source and destination again
//! and starts the item over.

use super::Conflict;
use super::disk::{self, Meter};
use super::jobs::{Done, Item, Next, numbered_name, replaced};
use crate::cloud;
use crate::entries::Kind;
use crate::remote::{Connection, RemoteEntry, RemoteError, RemotePath, Tree, walk_local};
use std::hash::{BuildHasher, RandomState};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How many numbered names Keep Both tries on a server, each a round trip.
const MOST_NUMBERED: u32 = 1000;

fn fail(why: impl Into<String>) -> Next {
    Next::Done(Done::Failed(why.into()))
}

/// How an error ends the item, or with a lost connection, the job for now.
fn ended(error: RemoteError, source: &Path) -> Next {
    let shown = source.display();
    match error {
        RemoteError::Cancelled => fail(format!("{shown}: cancelled partway.")),
        error if error.is_connection() => Next::Interrupt(format!("{shown}: {error}")),
        error => fail(error.to_string()),
    }
}

/// The note for items a folder held that a copy can't carry.
fn left_out(to: &impl std::fmt::Display, tree: &Tree) -> Option<String> {
    (tree.left_out > 0).then(|| {
        format!(
            "Copied to {to}, but left out {} items: only files, folders and links with UTF-8 \
            names copy.",
            tree.left_out
        )
    })
}

fn seen_remote(entry: &RemoteEntry) -> (u64, u64) {
    match entry.kind {
        // Every file in it is copied and checked again anyway.
        Kind::Dir => (0, 0),
        _ => (entry.size, entry.modified),
    }
}

fn changed(source: &Path) -> String {
    format!(
        "{}: changed since the job started, so nothing was copied. Copy it again to take it as \
        it is now.",
        source.display()
    )
}

impl Item<'_> {
    pub(super) fn download(&mut self, cancel: &AtomicBool, copied: &AtomicU64) -> Next {
        let label = self.work.source.clone();
        let Some(from) = self.spec.remote.get(self.index).cloned() else {
            return fail(format!("{}: isn't a remote item.", label.display()));
        };
        let into = self.spec.into.clone();
        if !into.is_dir() {
            return fail(format!("{}: not a folder.", into.display()));
        }
        let Some(name) = from.name().map(str::to_owned) else {
            return fail(format!("{from}: has no name to copy."));
        };
        let connection = match self.shared.remote.connect(from.source()) {
            Ok(connection) => connection,
            Err(e) => return ended(e, &label),
        };
        let entry = match connection.metadata(&from) {
            Ok(entry) => entry,
            Err(e) => return ended(e, &label),
        };
        let seen = seen_remote(&entry);
        if self.work.seen.is_some_and(|was| was != seen) {
            return fail(changed(&label));
        }
        self.work.seen = Some(seen);
        let (to, replacing) = match self.claim(into.join(name), &label, None) {
            Ok(claimed) => claimed,
            Err(next) => return next,
        };
        let tree = match connection.walk(&entry, cancel) {
            Ok(tree) => tree,
            Err(e) => return ended(e, &label),
        };
        self.work.to = Some(to.clone());
        self.work.bytes = tree.bytes;
        let free = disk::free(&into).unwrap_or(u64::MAX);
        if tree.bytes > free {
            return fail(format!(
                "{}: needs {} bytes, more than the {free} free in {}.",
                label.display(),
                tree.bytes,
                into.display()
            ));
        }
        let meter = Meter { copied, cancel };
        let mut lost = false;
        let made = self.make_here(&to, replacing, cancel, |temp| {
            connection.fetch(&entry, &tree, temp, &meter).map_err(|e| {
                lost = e.is_connection();
                format!("{}: {e}", label.display())
            })
        });
        match made {
            Err(why) if lost => Next::Interrupt(why),
            Err(why) => fail(why),
            Ok(()) => match left_out(&to.display(), &tree) {
                Some(why) => fail(why),
                None => Next::Done(Done::To(to)),
            },
        }
    }

    pub(super) fn upload(&mut self, cancel: &AtomicBool, copied: &AtomicU64) -> Next {
        let source = self.work.source.clone();
        let Some(into) = self.spec.remote.first().cloned() else {
            return fail(format!(
                "{}: no remote folder to copy to.",
                source.display()
            ));
        };
        if self.work.left.is_some() && !self.work.made {
            // Said once. The next attempt takes a name of its own.
            let why = format!(
                "{}: nothing was copied. {}",
                source.display(),
                self.work.left_note().unwrap_or_default()
            );
            self.work.left = None;
            return fail(why);
        }
        let connection = match self.shared.remote.connect(into.source()) {
            Ok(connection) => connection,
            Err(e) => return ended(e, &source),
        };
        if let Some(left) = self.work.left.clone() {
            if let Err(e) = connection.discard(&left) {
                let why = format!(
                    "{}: couldn't remove this job's unfinished copy at {left}: {e}",
                    source.display()
                );
                return if e.is_connection() {
                    Next::Interrupt(why)
                } else {
                    fail(why)
                };
            }
            self.work.left = None;
            self.work.made = false;
            if let Err(e) = self.shared.record(self.op, self.index, &self.work) {
                return fail(e);
            }
        }
        match disk::lookup(&source) {
            Ok(id) if self.work.id.is_none_or(|accepted| accepted == id) => {}
            Ok(_) => return fail(replaced(&source)),
            Err(e) => return fail(format!("{}: {e}", source.display())),
        }
        if let Err(why) = cloud::refuse_placeholders(&source) {
            return fail(why);
        }
        let meta = match std::fs::symlink_metadata(&source) {
            Ok(meta) => meta,
            Err(e) => return fail(format!("{}: {e}", source.display())),
        };
        let seen = if meta.is_dir() {
            (0, 0)
        } else {
            (meta.size(), meta.mtime().max(0) as u64)
        };
        if self.work.seen.is_some_and(|was| was != seen) {
            return fail(changed(&source));
        }
        self.work.seen = Some(seen);
        let Some(name) = source.file_name().and_then(|name| name.to_str()) else {
            return fail(format!(
                "{}: has no UTF-8 name to copy it under.",
                source.display()
            ));
        };
        match connection.follow(&into) {
            Ok(folder) if folder.kind == Kind::Dir => {}
            Ok(_) => return fail(format!("{into}: not a folder.")),
            Err(e) => return ended(e, &source),
        }
        let (to, replacing) = match self.claim_remote(&connection, &into, name, meta.is_file()) {
            Ok(claimed) => claimed,
            Err(next) => return next,
        };
        let tree = match walk_local(&source, cancel) {
            Ok(tree) => tree,
            Err(e) => return ended(e, &source),
        };
        self.work.bytes = tree.bytes;
        let tag = format!(
            "{}-{}-{:x}",
            self.op,
            self.index,
            RandomState::new().hash_one(self.op)
        );
        let temp = match connection.temp_in(&into, &tag) {
            Ok(temp) => temp,
            Err(e) => return fail(e.to_string()),
        };
        // On record before it exists, but it's not the job's until the server
        // says it made it: exclusive creation can fail on someone else's item.
        self.work.left = Some(temp.clone());
        self.work.made = false;
        if let Err(e) = self.shared.record(self.op, self.index, &self.work) {
            return fail(e);
        }
        let meter = Meter { copied, cancel };
        let mut created = false;
        let (shared, op, index, work) = (self.shared, self.op, self.index, &mut self.work);
        let made = connection
            .put(&source, &tree, &temp, &meter, || {
                created = true;
                work.made = true;
                shared.record(op, index, work).map_err(RemoteError::Failed)
            })
            .and_then(|()| {
                if cancel.load(Ordering::Relaxed) {
                    Err(RemoteError::Cancelled)
                } else {
                    connection.place(&temp, &to, replacing)
                }
            });
        match made {
            Ok(()) => {
                self.work.left = None;
                self.work.made = false;
                match left_out(&to, &tree) {
                    Some(why) => fail(why),
                    None => Next::Done(Done::To(to.to_string().into())),
                }
            }
            Err(e) if e.is_connection() => Next::Interrupt(format!(
                "{}: {e} {}",
                source.display(),
                self.work.left_note().unwrap_or_default()
            )),
            // It never made the temp, so what's there isn't the job's to touch.
            Err(e) if !created => {
                self.work.left = None;
                fail(format!(
                    "{}: {e} Nothing on the server changed.",
                    source.display()
                ))
            }
            Err(e) => {
                let mut why = match e {
                    RemoteError::Cancelled => format!("{}: cancelled partway.", source.display()),
                    e => e.to_string(),
                };
                match connection.discard(&temp) {
                    Ok(()) => {
                        self.work.left = None;
                        self.work.made = false;
                        why.push_str(" The unfinished copy was removed from the server.");
                    }
                    Err(e) => why.push_str(&format!(
                        " The unfinished copy is still on the server at {temp}: {e}"
                    )),
                }
                fail(why)
            }
        }
    }

    /// Where an item goes on the server under the job's conflict choice, and
    /// how the file it replaces was. A replace is for good, so only a file
    /// takes a file's place, and never a link's or a folder's.
    fn claim_remote(
        &self,
        connection: &Connection,
        into: &RemotePath,
        name: &str,
        is_file: bool,
    ) -> Result<(RemotePath, Option<(u64, u64)>), Next> {
        let source = &self.work.source;
        let to = into.join(name).map_err(|e| fail(e.to_string()))?;
        let there = match connection.metadata(&to) {
            Ok(there) => there,
            Err(RemoteError::NotFound(_)) => return Ok((to, None)),
            Err(e) => return Err(ended(e, source)),
        };
        match self.spec.conflict {
            Conflict::Skip => Err(Next::Done(Done::Skipped(format!(
                "{to} already exists, so {} was skipped.",
                source.display()
            )))),
            Conflict::Stop => Err(Next::Stop(format!(
                "{to} already exists. Continue the job with keep_both, replace or skip."
            ))),
            Conflict::KeepBoth => {
                for n in 2..MOST_NUMBERED {
                    let free = into
                        .join(&numbered_name(name, n))
                        .map_err(|e| fail(e.to_string()))?;
                    match connection.metadata(&free) {
                        Ok(_) => {}
                        Err(RemoteError::NotFound(_)) => return Ok((free, None)),
                        Err(e) => return Err(ended(e, source)),
                    }
                }
                Err(fail(format!("{to}: no free name for it.")))
            }
            Conflict::Replace if !is_file || there.kind != Kind::File => Err(fail(format!(
                "{to}: only a file can replace a file on a remote source, since what's replaced \
                can't be brought back."
            ))),
            Conflict::Replace if !connection.can_replace() => Err(fail(format!(
                "{to}: this server can't replace a file in one step, so it wasn't touched. Use \
                keep_both."
            ))),
            Conflict::Replace => Ok((to, Some((there.size, there.modified)))),
        }
    }
}
