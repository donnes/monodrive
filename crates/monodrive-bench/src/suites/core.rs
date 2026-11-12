//! Monodrive's own services, called as the window and MCP call them:
//! `list_dir`, `sort`, Suggest Names, and whole copy, duplicate, move and
//! trash jobs through an owner. The owner runs in this process on its own
//! `ops::Paths` inside the fixture, so it never joins the user's owner or
//! journal. Trashing happens only on a disk image the runner attaches,
//! because trashing on the startup disk would fill the user's Trash.

use crate::format::{Cache, Measured};
use crate::measure::{self, Sample};
use crate::rng::Rng;
use crate::suites::{delete_fixture, walk_count};
use monodrive_core::entries::{Entry, Sort, SortBy, list_dir, sort};
use monodrive_core::ops::{Client, Origin, Owner, Paths, Request};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;

const LISTS: [usize; 3] = [1_000, 10_000, 100_000];
const SORTED: usize = 10_000;
/// Files selected at once for Move to Trash.
const TRASHED_FILES: usize = 1_000;
const IMAGE: &str = "trash.sparseimage";
const VOLUME: &str = "trash-volume";

fn list_folder(fixture: &Path, count: usize) -> PathBuf {
    fixture.join(format!("list-{count}"))
}

/// Names shaped like a real folder's: mixed case, numbered runs, a spread of
/// extensions, and one folder for every ten files.
fn name(i: usize) -> (String, bool) {
    const STEMS: [&str; 6] = [
        "IMG_",
        "report ",
        "Screenshot ",
        "notes-",
        "Invoice",
        "build",
    ];
    const EXTS: [&str; 6] = ["jpg", "pdf", "png", "md", "txt", "rs"];
    let stem = STEMS[i % STEMS.len()];
    if i.is_multiple_of(10) {
        (format!("{stem}{i}"), true)
    } else {
        (format!("{stem}{i}.{}", EXTS[i % EXTS.len()]), false)
    }
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

const TEXTS: [(&str, &str, usize); 5] = [
    ("notes.md", "# Trip notes\n\nWe left on Tuesday.\n", 200),
    ("page.html", "<html><title>Lease</title></html>", 200),
    ("scan.txt", "Invoice 4471\nAmount due.\n", 2000),
    ("main.rs", "// Parses the budget.\nfn main() {}\n", 200),
    ("data.csv", "name,email,total\nAda,a@b.c,3\n", 2000),
];

pub(super) fn hdiutil(args: &[&str]) -> Result<(), String> {
    let out = Command::new("hdiutil")
        .args(args)
        .output()
        .map_err(|e| format!("hdiutil: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "hdiutil {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn prepare(fixture: &Path, seed: u64, tree_entries: usize) -> Result<(), String> {
    for count in LISTS {
        let dir = list_folder(fixture, count);
        std::fs::create_dir(&dir).map_err(io(&dir))?;
        for i in 0..count {
            let (name, is_dir) = name(i);
            let path = dir.join(name);
            if is_dir {
                std::fs::create_dir(&path).map_err(io(&path))?;
            } else {
                std::fs::write(&path, i.to_le_bytes()).map_err(io(&path))?;
            }
        }
    }
    let texts = fixture.join("texts");
    std::fs::create_dir(&texts).map_err(io(&texts))?;
    for (name, text, times) in TEXTS {
        let path = texts.join(name);
        std::fs::write(&path, text.repeat(times)).map_err(io(&path))?;
    }
    let shape = crate::tree::load(&crate::suites::shapes(), "node_modules")?;
    let nodes = crate::tree::plan(&shape, seed, tree_entries);
    let tree = fixture.join("jobs/node_modules");
    std::fs::create_dir_all(fixture.join("jobs/into")).map_err(io(fixture))?;
    crate::tree::write(&tree, &nodes).map_err(io(&tree))?;

    // The Trash's own volume: a sparse APFS image, kept out of Finder's
    // sidebar and Spotlight.
    let image = fixture.join(IMAGE);
    let volume = fixture.join(VOLUME);
    let (image_text, volume_text) = (image.to_string_lossy(), volume.to_string_lossy());
    hdiutil(&[
        "create",
        "-quiet",
        "-size",
        "4g",
        "-fs",
        "APFS",
        "-type",
        "SPARSE",
        "-volname",
        "MonodriveBenchTrash",
        &image_text,
    ])?;
    std::fs::create_dir(&volume).map_err(io(&volume))?;
    hdiutil(&[
        "attach",
        "-quiet",
        "-nobrowse",
        "-noautoopen",
        "-mountpoint",
        &volume_text,
        &image_text,
    ])?;
    let masters = volume.join("masters");
    std::fs::create_dir(&masters).map_err(io(&masters))?;
    crate::tree::write(&masters.join("node_modules"), &nodes).map_err(io(&masters))?;
    let files = masters.join("files");
    std::fs::create_dir(&files).map_err(io(&files))?;
    for i in 0..TRASHED_FILES {
        let path = files.join(format!("file {i}.txt"));
        std::fs::write(&path, i.to_le_bytes()).map_err(io(&path))?;
    }
    Ok(())
}

pub fn finish(fixture: &Path) -> Result<(), String> {
    detach(&fixture.join(VOLUME))
}

/// Detaches the disk image a suite attached at `volume`, if one is there:
/// a mount point is on another device than the folder holding it.
pub(super) fn detach(volume: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let device = |path: &Path| std::fs::metadata(path).map(|meta| meta.dev()).ok();
    if device(volume).is_none() || device(volume) == volume.parent().and_then(device) {
        return Ok(());
    }
    let volume = volume.to_string_lossy();
    // Spotlight or Finder may still hold the volume for a moment.
    hdiutil(&["detach", "-quiet", &volume])
        .or_else(|_| hdiutil(&["detach", "-quiet", "-force", &volume]))
}

fn measured(
    bench: &str,
    title: &str,
    group: &str,
    items: usize,
    sample: Sample,
    check: String,
) -> Measured {
    Measured {
        bench: bench.into(),
        title: title.into(),
        group: group.into(),
        variant: String::new(),
        items: Some(items as u64),
        cache: Cache::Warm,
        sample,
        check,
        peak_bytes: None,
    }
}

fn listing(fixture: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    for count in LISTS {
        let dir = list_folder(fixture, count);
        let (sample, entries) = measure::here(|| list_dir(&dir, false, Sort::default()));
        let entries = entries.map_err(io(&dir))?;
        out.push(measured(
            &format!("list_dir/{count}"),
            &format!(
                "List {} entries, sorted by name",
                crate::report::thousands(count)
            ),
            "List a folder",
            count,
            sample,
            format!("{} entries", entries.len()),
        ));
    }
    Ok(())
}

fn sorting(fixture: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    let dir = list_folder(fixture, SORTED);
    let mut entries: Vec<Entry> = list_dir(&dir, false, Sort::default()).map_err(io(&dir))?;
    // `list_dir` hands them back sorted by name, with equal sizes and near
    // equal dates: shuffle them and spread both, so no column starts sorted.
    let mut rng = Rng::new(7);
    for entry in &mut entries {
        entry.size = rng.next() % 50_000_000;
        entry.modified = 1_700_000_000 + rng.next() % 100_000_000;
    }
    for i in (1..entries.len()).rev() {
        entries.swap(i, rng.below(i + 1));
    }
    for (by, id) in [
        (SortBy::Name, "name"),
        (SortBy::Kind, "kind"),
        (SortBy::Size, "size"),
        (SortBy::Modified, "modified"),
    ] {
        let mut shuffled = entries.clone();
        let order = Sort {
            by,
            ..Sort::default()
        };
        let (sample, ()) = measure::here(|| sort(&mut shuffled, order));
        out.push(measured(
            &format!("sort/{id}"),
            &format!("Sort 10,000 shuffled entries by {id}"),
            "Sort a listing",
            SORTED,
            sample,
            format!("{} entries, first {}", shuffled.len(), shuffled[0].name),
        ));
    }
    Ok(())
}

fn naming(fixture: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    use monodrive_core::naming::{Style, suggest};
    let texts = fixture.join("texts");
    let (sample, names) = measure::here(|| {
        TEXTS
            .iter()
            .map(|(name, _, _)| suggest(&texts.join(name), Style::Hyphens))
            .collect::<Result<Vec<String>, String>>()
    });
    out.push(measured(
        "naming/suggest",
        "Suggest names for five text files",
        "Suggest Names",
        TEXTS.len(),
        sample,
        names?.join(" "),
    ));
    Ok(())
}

/// Bytes in `tree`'s files that aren't shared with another file: 0 for a
/// whole clone. APFS reports it per file, as `ATTR_CMNEXT_PRIVATESIZE`.
fn private_bytes(tree: &Path) -> Result<u64, String> {
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        private: libc::off_t,
    }
    let mut total = 0;
    let mut folders = vec![tree.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for item in std::fs::read_dir(&folder).map_err(io(&folder))? {
            let item = item.map_err(io(&folder))?;
            let kind = item.file_type().map_err(io(&folder))?;
            if kind.is_dir() {
                folders.push(item.path());
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let path =
                CString::new(item.path().as_os_str().as_bytes()).map_err(|e| e.to_string())?;
            let mut list = libc::attrlist {
                bitmapcount: libc::ATTR_BIT_MAP_COUNT,
                reserved: 0,
                commonattr: 0,
                volattr: 0,
                dirattr: 0,
                fileattr: 0,
                // ATTR_CMNEXT_PRIVATESIZE
                forkattr: 0x0000_0008,
            };
            let mut reply = Reply {
                length: 0,
                private: 0,
            };
            // SAFETY: the list asks for one off_t, which `reply` has room for.
            let read = unsafe {
                libc::getattrlist(
                    path.as_ptr(),
                    (&raw mut list).cast(),
                    (&raw mut reply).cast(),
                    size_of::<Reply>(),
                    libc::FSOPT_ATTR_CMN_EXTENDED | libc::FSOPT_NOFOLLOW,
                )
            };
            if read != 0 {
                return Err(format!(
                    "{}: {}",
                    item.path().display(),
                    std::io::Error::last_os_error()
                ));
            }
            total += reply.private.max(0) as u64;
        }
    }
    Ok(total)
}

/// An owner of the fixture's own, served on a thread of this process.
pub(super) struct Jobs {
    client: Client,
    changed: mpsc::Receiver<()>,
    _watch: monodrive_core::ops::Watch,
    _short: crate::suites::ShortPath,
}

impl Jobs {
    pub(super) fn start(fixture: &Path) -> Result<Self, String> {
        let root = fixture.join(format!("owner-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Trash")).map_err(io(&root))?;
        let short = crate::suites::ShortPath::to(&root, "owner")?;
        let paths = Paths {
            state: short.path().join("state"),
            settings: short.path().join("settings.json"),
            trash: short.path().join("Trash"),
        };
        let owner = Owner::bind(paths.clone())?;
        std::thread::spawn(move || owner.serve());
        let client = Client::new(paths, None);
        let (tx, changed) = mpsc::channel();
        let watch = client.watch(move || {
            let _ = tx.send(());
        });
        let watch = watch.map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            changed,
            _watch: watch,
            _short: short,
        })
    }

    /// Runs `request` and, for a job, waits for it to end. Returns where the
    /// items went.
    pub(super) fn run(&mut self, request: Request, job: bool) -> Result<Vec<PathBuf>, String> {
        let outcome = self.client.submit(Origin::Window, request)?;
        if !job {
            return outcome.items.into_iter().map(|item| item.result).collect();
        }
        loop {
            let jobs = self.client.jobs()?;
            let found = jobs.into_iter().find(|j| j.id == outcome.op);
            let Some(found) = found else {
                return Err(format!("job {} is gone", outcome.op));
            };
            if !found.state.active() {
                return found
                    .tasks
                    .into_iter()
                    .map(|task| match task.done {
                        Some(monodrive_core::ops::Done::To(path)) => Ok(path),
                        other => Err(format!("{}: {other:?}", task.source.display())),
                    })
                    .collect();
            }
            // The journal changes when the job ends.
            self.changed.recv().map_err(|e| e.to_string())?;
        }
    }
}

fn one(paths: Vec<PathBuf>, what: &str) -> Result<PathBuf, String> {
    match <[PathBuf; 1]>::try_from(paths) {
        Ok([path]) => Ok(path),
        Err(paths) => Err(format!("{what}: {} results, not 1", paths.len())),
    }
}

fn jobs(fixture: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    let mut jobs = Jobs::start(fixture)?;
    let tree = fixture.join("jobs/node_modules");
    let into = fixture.join("jobs/into");
    let entries = walk_count(&tree).map_err(io(&tree))?;
    let group = "Jobs through the owner";

    let (sample, done) = measure::here(|| {
        jobs.run(
            Request::Copy {
                paths: vec![tree.clone()],
                into: into.clone(),
                conflict: Default::default(),
            },
            true,
        )
    });
    let copy = one(done?, "copy")?;
    let (copied, private) = (walk_count(&copy).map_err(io(&copy))?, private_bytes(&copy)?);
    delete_fixture(&copy)?;
    out.push(measured(
        "job/copy",
        "Copy a node_modules",
        group,
        entries,
        sample,
        format!("{copied} entries, {private} bytes not shared"),
    ));

    let (sample, done) = measure::here(|| {
        jobs.run(
            Request::Duplicate {
                paths: vec![tree.clone()],
            },
            false,
        )
    });
    let duplicate = one(done?, "duplicate")?;
    let (copied, private) = (
        walk_count(&duplicate).map_err(io(&duplicate))?,
        private_bytes(&duplicate)?,
    );
    delete_fixture(&duplicate)?;
    out.push(measured(
        "job/duplicate",
        "Duplicate it",
        group,
        entries,
        sample,
        format!("{copied} entries, {private} bytes not shared"),
    ));

    let (sample, done) = measure::here(|| {
        jobs.run(
            Request::Move {
                paths: vec![tree.clone()],
                into: into.clone(),
                conflict: Default::default(),
            },
            true,
        )
    });
    let moved = one(done?, "move")?;
    let count = walk_count(&moved).map_err(io(&moved))?;
    std::fs::rename(&moved, &tree).map_err(io(&moved))?;
    out.push(measured(
        "job/move",
        "Move it to another folder",
        group,
        entries,
        sample,
        format!("{count} entries moved"),
    ));

    trash(fixture, &mut jobs, out)
}

pub(super) fn clone_into(from: &Path, to: &Path) -> Result<(), String> {
    let (a, b) = (
        CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?,
        CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?,
    );
    // SAFETY: two NUL-terminated paths.
    if unsafe { libc::clonefile(a.as_ptr(), b.as_ptr(), crate::suites::CLONE_NOFOLLOW) } == 0 {
        Ok(())
    } else {
        Err(format!(
            "{}: {}",
            to.display(),
            std::io::Error::last_os_error()
        ))
    }
}

fn trash(fixture: &Path, jobs: &mut Jobs, out: &mut Vec<Measured>) -> Result<(), String> {
    let volume = fixture.join(VOLUME);
    let round = volume.join(format!("round-{}", std::process::id()));
    std::fs::create_dir(&round).map_err(io(&round))?;
    let tree = round.join("node_modules");
    let files = round.join("files");
    clone_into(&volume.join("masters/node_modules"), &tree)?;
    clone_into(&volume.join("masters/files"), &files)?;
    let entries = walk_count(&tree).map_err(io(&tree))?;
    let group = "Move to Trash, on a disk image";

    let (sample, done) = measure::here(|| {
        jobs.run(
            Request::Trash {
                paths: vec![tree.clone()],
            },
            false,
        )
    });
    let landed = one(done?, "trash")?;
    if !landed.starts_with(&volume) {
        return Err(format!(
            "{} went to {}, off the image",
            tree.display(),
            landed.display()
        ));
    }
    out.push(measured(
        "trash/folder",
        "Move a node_modules to the Trash",
        group,
        entries,
        sample,
        format!(
            "{} entries in the Trash",
            walk_count(&landed).map_err(io(&landed))?
        ),
    ));

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&files)
        .map_err(io(&files))?
        .map(|item| item.map(|item| item.path()))
        .collect::<Result<_, _>>()
        .map_err(io(&files))?;
    paths.sort();
    let (sample, done) = measure::here(|| jobs.run(Request::Trash { paths }, false));
    let trashed = done?;
    let left = std::fs::read_dir(&files).map_err(io(&files))?.count();
    out.push(measured(
        "trash/files",
        "Move 1,000 selected files to the Trash",
        group,
        TRASHED_FILES,
        sample,
        format!("{} trashed, {left} left", trashed.len()),
    ));
    // Out of the image's Trash, exactly what this round put there.
    delete_fixture(&landed)?;
    for file in trashed.iter().filter(|path| path.starts_with(&volume)) {
        #[expect(
            clippy::disallowed_methods,
            reason = "a fixture this round trashed on its own disk image"
        )]
        std::fs::remove_file(file).map_err(io(file))?;
    }
    delete_fixture(&round)
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let mut out = Vec::new();
    listing(fixture, &mut out)?;
    sorting(fixture, &mut out)?;
    naming(fixture, &mut out)?;
    jobs(fixture, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clone_shares_its_bytes_and_a_copy_doesnt() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        std::fs::create_dir(&from).unwrap();
        std::fs::write(from.join("a"), vec![1u8; 100_000]).unwrap();
        let cloned = dir.path().join("cloned");
        clone_into(&from, &cloned).unwrap();
        assert_eq!(private_bytes(&cloned).unwrap(), 0);
        let copied = dir.path().join("copied");
        std::fs::create_dir(&copied).unwrap();
        // Not std::fs::copy, which clones on APFS.
        std::fs::write(copied.join("a"), std::fs::read(from.join("a")).unwrap()).unwrap();
        assert!(private_bytes(&copied).unwrap() >= 100_000);
    }
}
