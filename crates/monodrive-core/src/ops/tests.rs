use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// An engine on a temp state folder, and a temp folder to change.
pub(super) fn engine() -> (tempfile::TempDir, PathBuf, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    let engine = Engine::open(paths(dir.path())).unwrap();
    (dir, files, engine)
}

pub(super) fn paths(dir: &Path) -> Paths {
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    Paths {
        state,
        settings: dir.join("settings.json"),
        trash: dir.join("Trash"),
    }
}

pub(super) fn submission(origin: Origin, request: Request) -> Submission {
    static SENT: AtomicU64 = AtomicU64::new(0);
    Submission {
        id: format!("test-{}", SENT.fetch_add(1, Ordering::Relaxed)),
        origin,
        request,
    }
}

pub(super) fn run(engine: &mut Engine, request: Request) -> Result<Outcome, String> {
    engine.submit(submission(Origin::Window, request))
}

/// Where each item went, failing the test if any didn't.
pub(super) fn moved(outcome: Result<Outcome, String>) -> Vec<PathBuf> {
    let outcome = outcome.unwrap();
    outcome
        .items
        .into_iter()
        .map(|item| item.result.unwrap())
        .collect()
}

pub(super) fn rename(path: &Path, name: &str) -> Request {
    Request::Rename {
        path: path.into(),
        name: name.into(),
    }
}

#[test]
fn rename_undoes_and_redoes_and_refuses_once_the_item_changed() {
    let (dir, files, mut engine) = engine();
    let (a, b) = (files.join("a.txt"), files.join("b.txt"));
    std::fs::write(&a, "a").unwrap();
    std::fs::write(files.join("c.txt"), "c").unwrap();
    assert!(
        run(&mut engine, rename(&a, "c.txt"))
            .unwrap_err()
            .contains("already exists")
    );
    assert!(run(&mut engine, rename(&a, "x/y")).is_err());
    assert_eq!(
        moved(run(&mut engine, rename(&a, "b.txt"))),
        vec![b.clone()]
    );
    assert_eq!(moved(run(&mut engine, Request::Undo)), vec![a.clone()]);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "a");
    assert_eq!(moved(run(&mut engine, Request::Redo)), vec![b.clone()]);
    assert_eq!(
        run(&mut engine, Request::Redo).unwrap_err(),
        "Nothing to redo."
    );
    // A change of case alone, on a case-insensitive volume.
    let upper = files.join("B.txt");
    assert_eq!(
        moved(run(&mut engine, rename(&b, "B.txt"))),
        vec![upper.clone()]
    );
    assert_eq!(moved(run(&mut engine, Request::Undo)), vec![b.clone()]);
    // Edited since, so redo leaves it as it is.
    std::fs::write(&b, "edited").unwrap();
    let refused = run(&mut engine, Request::Redo).unwrap_err();
    assert!(refused.contains("changed since"), "{refused}");
    let names: Vec<_> = std::fs::read_dir(&files)
        .unwrap()
        .map(|item| item.unwrap().file_name())
        .collect();
    assert!(names.contains(&"b.txt".into()), "{names:?}");

    // Something else now sits at b.txt: undo must not rename it to a.txt.
    let other = dir.path().join("other.txt");
    std::fs::write(&other, "other").unwrap();
    std::fs::rename(&other, &b).unwrap();
    let refused = run(&mut engine, Request::Undo).unwrap_err();
    assert!(refused.starts_with("Can't undo Rename"), "{refused}");
    assert!(!a.exists());
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "other");

    // Nor overwrite a new a.txt.
    std::fs::write(&a, "new").unwrap();
    assert!(run(&mut engine, Request::Undo).is_err());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "new");
}

#[test]
fn trash_puts_back_and_undoing_a_new_folder_trashes_it_until_redo() {
    let (_dir, files, mut engine) = engine();
    let notes = files.join("notes.txt");
    std::fs::write(&notes, "keep").unwrap();
    let [trashed] = moved(run(
        &mut engine,
        Request::Trash {
            paths: vec![notes.clone()],
        },
    ))
    .try_into()
    .unwrap();
    assert!(!notes.exists());
    assert!(trashed.components().any(|c| c.as_os_str() == ".Trash"));
    // Put Back's button asks first: this one's known, what's elsewhere isn't.
    assert_eq!(
        engine.origins(&[trashed.clone(), files.clone()]),
        [Some(notes.clone()), None]
    );
    assert_eq!(
        moved(run(
            &mut engine,
            Request::PutBack {
                paths: vec![trashed]
            }
        )),
        vec![notes.clone()]
    );
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "keep");
    // Edited once it was back, so undo won't send it to the Trash again.
    std::fs::write(&notes, "edited").unwrap();
    let refused = run(&mut engine, Request::Undo).unwrap_err();
    assert!(refused.contains("changed since"), "{refused}");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "edited");

    let folder = files.join("new");
    let made = run(
        &mut engine,
        Request::NewFolder {
            path: folder.clone(),
        },
    );
    assert_eq!(moved(made), vec![folder.clone()]);
    let [trashed] = moved(run(&mut engine, Request::Undo)).try_into().unwrap();
    assert!(!folder.exists() && trashed.exists());
    assert_eq!(moved(run(&mut engine, Request::Redo)), vec![folder.clone()]);
    assert!(folder.is_dir());
}

/// A storage scan's cleanup of `files`, trashing the copies `trash` names.
pub(super) fn cleanup(files: &Path, trash: &[&str]) -> Request {
    use crate::storage::{self, Progress};
    let report = storage::scan(files, &Progress::default(), None).unwrap();
    let found = report.duplicates(&|_| true, &Progress::default(), None);
    let trash: Vec<PathBuf> = trash.iter().map(|name| files.join(name)).collect();
    found.cleanup(&trash).unwrap().request()
}

/// Real paths, as a scan reports them, holding `names`, each with `text`.
pub(super) fn copies(names: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, Engine) {
    let (dir, files, engine) = engine();
    let files = files.canonicalize().unwrap();
    for (name, text) in names {
        std::fs::write(files.join(name), text).unwrap();
    }
    (dir, files, engine)
}

#[test]
fn a_duplicate_cleanup_refuses_what_changed_since_the_scan_and_undoes_in_one_step() {
    let (_dir, files, mut engine) = copies(&[
        ("a.txt", "same"),
        ("b.txt", "same"),
        ("c.txt", "same"),
        ("x.md", "other"),
        ("y.md", "other"),
    ]);
    let plan = cleanup(&files, &["b.txt", "c.txt", "y.md"]);
    // Edited after the scan: c.txt itself, and x.md, the only copy kept
    // beside y.md, so neither c.txt nor y.md may go.
    std::fs::write(files.join("c.txt"), "SAME").unwrap();
    std::fs::write(files.join("x.md"), "OTHER").unwrap();

    let outcome = run(&mut engine, plan).unwrap();
    assert_eq!(outcome.title, "Trash Duplicates");
    let results: Vec<(&Path, Result<(), &str>)> = outcome
        .items
        .iter()
        .map(|item| {
            (
                item.path.as_path(),
                item.result.as_ref().map(|_| ()).map_err(String::as_str),
            )
        })
        .collect();
    let (b, c, y) = (files.join("b.txt"), files.join("c.txt"), files.join("y.md"));
    let refusals = [
        format!(
            "{}: changed since the scan, so it stays. Scan again.",
            c.display()
        ),
        format!(
            "{}: stays, since no copy kept beside it still opens as the scan found it. Scan again.",
            y.display()
        ),
    ];
    assert_eq!(
        results,
        [
            (b.as_path(), Ok(())),
            (c.as_path(), Err(refusals[0].as_str())),
            (y.as_path(), Err(refusals[1].as_str())),
        ]
    );
    assert!(!b.exists() && c.exists() && y.exists());
    assert_eq!(moved(run(&mut engine, Request::Undo)), vec![b.clone()]);
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "same");
}

#[test]
fn a_duplicate_cleanup_keeps_every_copy_when_the_kept_one_can_no_longer_be_read() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, files, mut engine) = copies(&[("s1-keep.txt", "same"), ("s1-copy.txt", "same")]);
    let plan = cleanup(&files, &["s1-copy.txt"]);
    let keep = files.join("s1-keep.txt");
    let mode = |mode| std::fs::set_permissions(&keep, std::fs::Permissions::from_mode(mode));
    mode(0o000).unwrap();
    let refused = run(&mut engine, plan);
    mode(0o644).unwrap();
    let refused = refused.unwrap_err();
    assert!(
        refused.contains(": stays, since no copy kept beside it still opens"),
        "{refused}"
    );
    assert!(files.join("s1-copy.txt").exists());
}

#[test]
fn each_duplicate_waits_for_a_kept_copy_still_there_just_before_it_goes() {
    let (_dir, files, mut engine) = copies(&[
        ("s2-a.txt", "same"),
        ("s2-b.txt", "same"),
        ("s2-c.txt", "same"),
    ]);
    let seen = |name: &str| {
        let path = files.join(name);
        Seen {
            found: disk::stamp(&path).unwrap(),
            path,
        }
    };
    // The second group keeps what the first trashes, so by the time its
    // copy's turn comes, nothing it keeps is left.
    let request = Request::TrashDuplicates {
        groups: vec![
            Copies {
                keep: vec![seen("s2-a.txt")],
                trash: vec![seen("s2-b.txt")],
            },
            Copies {
                keep: vec![seen("s2-b.txt")],
                trash: vec![seen("s2-c.txt")],
            },
        ],
    };
    let outcome = run(&mut engine, request).unwrap();
    let (b, c) = (files.join("s2-b.txt"), files.join("s2-c.txt"));
    assert!(outcome.items[0].result.is_ok(), "{outcome:?}");
    let refused = outcome.items[1].result.as_ref().unwrap_err();
    assert!(
        refused.starts_with(&format!("{}: stays, since", c.display())),
        "{refused}"
    );
    assert!(c.exists());
    assert_eq!(moved(run(&mut engine, Request::Undo)), vec![b]);
}

#[test]
fn a_duplicate_group_that_keeps_nothing_trashes_nothing() {
    let (_dir, files, mut engine) = copies(&[("s6-a.txt", "same"), ("s6-b.txt", "same")]);
    let seen = |name: &str| {
        let path = files.join(name);
        Seen {
            found: disk::stamp(&path).unwrap(),
            path,
        }
    };
    let request = Request::TrashDuplicates {
        groups: vec![Copies {
            keep: Vec::new(),
            trash: vec![seen("s6-a.txt"), seen("s6-b.txt")],
        }],
    };
    let refused = run(&mut engine, request).unwrap_err();
    assert!(
        refused.contains("stays, since its group keeps no copy."),
        "{refused}"
    );
    assert!(files.join("s6-a.txt").exists() && files.join("s6-b.txt").exists());
    // Nor does a step of one that reaches the history some other way.
    let (id, stamp) = seen("s6-a.txt").found;
    let step = Step {
        id,
        undone: Spot::Path(files.join("s6-a.txt")),
        done: Spot::Trash(None),
        applied: false,
        stamp,
        keep: Some(Vec::new()),
    };
    assert!(step.ready(true, &[]).is_err());
}

#[test]
fn redo_of_a_duplicate_cleanup_refuses_once_the_kept_copy_changed() {
    let (_dir, files, mut engine) = copies(&[("s3-keep.txt", "same"), ("s3-copy.txt", "same")]);
    let copy = files.join("s3-copy.txt");
    moved(run(&mut engine, cleanup(&files, &["s3-copy.txt"])));
    assert_eq!(moved(run(&mut engine, Request::Undo)), vec![copy.clone()]);
    std::fs::write(files.join("s3-keep.txt"), "SAME").unwrap();
    let refused = run(&mut engine, Request::Redo).unwrap_err();
    assert!(
        refused.starts_with(&format!(
            "Can't redo Trash Duplicates: {}: stays, since no copy kept beside it",
            copy.display()
        )),
        "{refused}"
    );
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "same");
}

#[test]
fn undo_refuses_to_trash_a_new_file_or_copied_folder_once_it_was_edited() {
    let (_dir, files, mut engine) = engine();
    let file = files.join("draft.md");
    let new = || Request::NewFile { path: file.clone() };
    assert_eq!(moved(run(&mut engine, new())), vec![file.clone()]);
    assert!(
        run(&mut engine, new())
            .unwrap_err()
            .contains("already exists")
    );
    std::fs::write(&file, "work").unwrap();
    let refused = run(&mut engine, Request::Undo).unwrap_err();
    assert!(refused.contains("changed since"), "{refused}");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "work");

    // An edit deep inside a copied folder counts too, even one that keeps
    // the size and is older than another item's time.
    std::fs::create_dir_all(files.join("docs/deep")).unwrap();
    std::fs::write(files.join("docs/deep/inner.md"), "a").unwrap();
    let future = std::time::SystemTime::now() + std::time::Duration::from_secs(86_400 * 365);
    File::create(files.join("docs/later.md"))
        .unwrap()
        .set_modified(future)
        .unwrap();
    let [copy] = moved(run(
        &mut engine,
        Request::Duplicate {
            paths: vec![files.join("docs")],
        },
    ))
    .try_into()
    .unwrap();
    std::fs::write(copy.join("deep/inner.md"), "b").unwrap();
    let refused = run(&mut engine, Request::Undo).unwrap_err();
    assert!(refused.contains("changed since"), "{refused}");
    assert!(copy.join("deep/inner.md").exists());
}

#[test]
fn duplicate_names_copies_as_finder_does_and_keeps_metadata() {
    let (_dir, files, mut engine) = engine();
    let report = files.join("report.pdf");
    std::fs::write(&report, "pdf").unwrap();
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    File::options()
        .write(true)
        .open(&report)
        .unwrap()
        .set_modified(past)
        .unwrap();
    std::fs::create_dir(files.join("docs")).unwrap();
    std::fs::write(files.join("docs/inner.md"), "inner").unwrap();
    std::fs::write(files.join(".env"), "").unwrap();
    let mut duplicate = |name: &str| {
        let [copy] = moved(run(
            &mut engine,
            Request::Duplicate {
                paths: vec![files.join(name)],
            },
        ))
        .try_into()
        .unwrap();
        copy.file_name().unwrap().to_string_lossy().into_owned()
    };
    assert_eq!(duplicate("report.pdf"), "report copy.pdf");
    assert_eq!(duplicate("report.pdf"), "report copy 2.pdf");
    assert_eq!(duplicate("report copy.pdf"), "report copy 3.pdf");
    assert_eq!(duplicate("docs"), "docs copy");
    assert_eq!(duplicate(".env"), ".env copy");
    assert_eq!(
        std::fs::read_to_string(files.join("docs copy/inner.md")).unwrap(),
        "inner"
    );
    let copied = std::fs::metadata(files.join("report copy.pdf")).unwrap();
    assert_eq!(copied.modified().unwrap(), past);
}

#[test]
fn a_retried_request_gets_its_first_outcome_instead_of_running_again() {
    let (_dir, files, mut engine) = engine();
    let a = files.join("a");
    std::fs::write(&a, "").unwrap();
    let first = submission(Origin::Window, rename(&a, "b"));
    let outcome = engine.submit(first.clone()).unwrap();
    assert_eq!(engine.submit(first.clone()).unwrap(), outcome);
    assert!(files.join("b").exists());
    let reused = Submission {
        request: rename(&files.join("b"), "c"),
        ..first
    };
    assert!(engine.submit(reused).is_err());
}

#[test]
fn opening_settles_what_a_crash_left_unfinished_from_the_disk() {
    let (dir, files, mut engine) = engine();
    let (a, b, c) = (files.join("a"), files.join("b"), files.join("c"));
    std::fs::write(&a, "").unwrap();
    std::fs::write(&c, "").unwrap();
    // Recorded, then the owner dies: once after renaming, once before.
    let after = submission(Origin::Window, rename(&a, "b"));
    let before = submission(Origin::Window, rename(&c, "d"));
    for (submission, path, to) in [(&after, &a, &b), (&before, &c, &files.join("d"))] {
        let (id, stamp) = disk::stamp(path).unwrap();
        let step = Step {
            id,
            undone: Spot::Path(path.clone()),
            done: Spot::Path(to.clone()),
            applied: false,
            stamp,
            keep: None,
        };
        engine
            .begin(submission, None, vec![step], Vec::new(), Vec::new())
            .unwrap();
    }
    disk::rename_new(&a, &b).unwrap();
    drop(engine);

    let mut engine = Engine::open(paths(dir.path())).unwrap();
    let settled = engine.submit(after).unwrap();
    assert!(settled.interrupted);
    assert_eq!(settled.items[0].result, Ok(b.clone()));
    let untouched = engine.submit(before).unwrap();
    assert!(untouched.items[0].result.is_err());
    assert!(c.exists());
    // Only the rename that happened is in the history.
    assert_eq!(moved(run(&mut engine, Request::Undo)), vec![a.clone()]);
    assert_eq!(
        run(&mut engine, Request::Undo).unwrap_err(),
        "Nothing to undo."
    );
}

#[test]
fn the_agents_setting_gates_agents_but_not_the_window() {
    let (dir, files, mut engine) = engine();
    let agent = || Origin::Agent {
        client: Some("test".into()),
    };
    let folder = |name: &str| Request::NewFolder {
        path: files.join(name),
    };
    assert!(engine.submit(submission(agent(), folder("on"))).is_ok());
    std::fs::write(
        dir.path().join("settings.json"),
        r#"{"theme": "dark", "agents_change_files": false}"#,
    )
    .unwrap();
    let refused = engine
        .submit(submission(agent(), folder("off")))
        .unwrap_err();
    assert!(
        refused.starts_with("Agents can't change files"),
        "{refused}"
    );
    assert!(engine.submit(submission(agent(), Request::Undo)).is_err());
    assert!(!files.join("off").exists());
    assert!(run(&mut engine, folder("window")).is_ok());
}

#[test]
fn a_folder_made_with_items_undoes_and_redoes_in_one_step() {
    let (_dir, files, mut engine) = engine();
    let (notes, pics) = (files.join("notes.md"), files.join("pics"));
    std::fs::write(&notes, "notes").unwrap();
    std::fs::create_dir(&pics).unwrap();
    std::fs::write(pics.join("a.jpg"), "a").unwrap();
    let folder = files.join("New Folder With Items");
    let made = run(
        &mut engine,
        Request::NewFolderWith {
            path: folder.clone(),
            items: vec![notes.clone(), pics.clone()],
        },
    );
    assert_eq!(
        moved(made),
        [folder.clone(), folder.join("notes.md"), folder.join("pics")]
    );
    // One undo moves both back out and sends the folder to the Trash.
    let undone = moved(run(&mut engine, Request::Undo));
    assert_eq!(undone.len(), 3);
    assert!(!folder.exists() && undone[2].exists());
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "notes");
    assert!(pics.join("a.jpg").exists());
    // Redo brings the folder back from the Trash and fills it again.
    moved(run(&mut engine, Request::Redo));
    assert!(folder.join("notes.md").exists() && folder.join("pics/a.jpg").exists());
    assert!(!notes.exists() && !pics.exists());
    // An item already gone refuses the whole thing.
    let refused = run(
        &mut engine,
        Request::NewFolderWith {
            path: files.join("other"),
            items: vec![folder.join("notes.md"), notes.clone()],
        },
    );
    assert!(refused.is_err() && !files.join("other").exists());
    assert!(folder.join("notes.md").exists());
}

#[test]
fn empty_trash_erases_only_what_was_listed_in_the_trash_and_only_for_the_window() {
    let (dir, files, mut engine) = engine();
    let trash = dir.path().join("Trash");
    std::fs::create_dir_all(trash.join("old folder/inside")).unwrap();
    std::fs::write(trash.join("old folder/inside/x.txt"), "x").unwrap();
    std::fs::write(trash.join("old.txt"), "old").unwrap();
    std::fs::write(trash.join(".DS_Store"), "").unwrap();
    let keep = files.join("keep.txt");
    std::fs::write(&keep, "keep").unwrap();
    assert!(run(&mut engine, rename(&keep, "kept.txt")).is_ok());
    let listed = trash_items(&trash).unwrap();
    let paths =
        |items: &[TrashItem]| -> Vec<PathBuf> { items.iter().map(|i| i.path.clone()).collect() };
    assert_eq!(
        paths(&listed),
        [trash.join("old folder"), trash.join("old.txt")]
    );
    // Something that arrives after the count isn't erased with it.
    std::fs::write(trash.join("new.txt"), "new").unwrap();
    let empty = |items: Vec<TrashItem>| Request::EmptyTrash { items };
    let agent = || Origin::Agent { client: None };
    let refused = engine
        .submit(submission(agent(), empty(listed.clone())))
        .unwrap_err();
    assert!(refused.starts_with("Only a person"), "{refused}");
    let outside = TrashItem {
        path: files.join("kept.txt"),
        id: disk::id(&files.join("kept.txt")).unwrap(),
    };
    assert!(run(&mut engine, empty(vec![outside])).is_err());
    assert!(run(&mut engine, empty(Vec::new())).is_err());

    // It's a job: accepted at once, and nothing is erased until it runs.
    let shared = jobs::Shared::new(engine, None);
    let op = run(&mut shared.lock(), empty(listed.clone())).unwrap().op;
    assert!(trash.join("old.txt").exists());
    // One at a time, and an agent can't continue what only a person starts.
    let again = run(&mut shared.lock(), empty(listed.clone())).unwrap_err();
    assert_eq!(again, "The Trash is already being emptied.");
    let retry = Request::Retry {
        job: op,
        conflict: None,
    };
    let refused = shared
        .lock()
        .submit(submission(agent(), retry))
        .unwrap_err();
    assert!(refused.starts_with("Only a person"), "{refused}");
    jobs::run(&shared, op);
    let emptied = status(&shared, op);
    assert_eq!(
        (emptied.title.as_str(), emptied.state),
        ("Empty Trash", State::Completed)
    );
    assert_eq!(emptied.into, trash);
    assert!(
        emptied
            .tasks
            .iter()
            .all(|task| task.done == Some(Done::Erased))
    );
    assert!(!emptied.undoable);
    assert_eq!(
        paths(&trash_items(&trash).unwrap()),
        [trash.join("new.txt")]
    );
    assert!(trash.join(".DS_Store").exists() && files.join("kept.txt").exists());
    // It joins no history: undo still reaches the rename before it.
    assert!(run(&mut shared.lock(), Request::Undo).is_ok());
    assert!(keep.exists());
}

/// A temp Trash holding `old.txt` and `old folder/x.txt`, counted.
pub(super) fn counted_trash(dir: &Path) -> (PathBuf, Vec<TrashItem>) {
    let trash = dir.join("Trash");
    std::fs::create_dir_all(trash.join("old folder")).unwrap();
    std::fs::write(trash.join("old folder/x.txt"), "x").unwrap();
    std::fs::write(trash.join("old.txt"), "old").unwrap();
    let listed = trash_items(&trash).unwrap();
    (trash, listed)
}

#[test]
fn empty_trash_leaves_an_item_swapped_in_under_a_counted_name() {
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (trash, listed) = counted_trash(dir.path());
    // While the person reads the count, old.txt is put back and another
    // file is trashed under its name.
    std::fs::rename(trash.join("old.txt"), files.join("old.txt")).unwrap();
    std::fs::write(trash.join("old.txt"), "newer").unwrap();
    let emptied = job(&shared, Request::EmptyTrash { items: listed });
    assert_eq!(read(&trash.join("old.txt")), "newer");
    assert!(!trash.join("old folder").exists());
    assert_eq!(emptied.state, State::Completed);
    assert_eq!(emptied.tasks[0].done, Some(Done::Erased));
    assert!(
        matches!(&emptied.tasks[1].done, Some(Done::Skipped(why)) if why.contains("old.txt")),
        "{:?}",
        emptied.tasks[1].done
    );
}

#[test]
fn empty_trash_refuses_a_trash_that_became_a_link() {
    let (dir, _files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (trash, listed) = counted_trash(dir.path());
    let empty = Request::EmptyTrash {
        items: listed.clone(),
    };
    let op = run(&mut shared.lock(), empty.clone()).unwrap().op;
    // The same items, but the Trash now points somewhere else: the job
    // queued before that erases nothing, and another isn't accepted.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::rename(&trash, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &trash).unwrap();
    jobs::run(&shared, op);
    let stopped = status(&shared, op);
    assert_eq!(stopped.state, State::Failed);
    assert!(
        matches!(&stopped.tasks[0].done, Some(Done::Failed(why)) if why.contains("isn't the Trash folder")),
        "{:?}",
        stopped.tasks[0].done
    );
    let refused = run(&mut shared.lock(), empty).unwrap_err();
    assert!(refused.contains("isn't the Trash folder"), "{refused}");
    assert!(elsewhere.join("old.txt").exists() && elsewhere.join("old folder/x.txt").exists());
}

#[test]
fn a_cancelled_or_interrupted_empty_trash_keeps_the_rest_until_a_person_continues_it() {
    let (dir, _files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (trash, listed) = counted_trash(dir.path());
    let op = run(&mut shared.lock(), Request::EmptyTrash { items: listed })
        .unwrap()
        .op;
    assert!(jobs::next(&shared, op));
    run(&mut shared.lock(), Request::Cancel { job: op }).unwrap();
    assert!(!jobs::next(&shared, op));
    let cancelled = status(&shared, op);
    assert_eq!(cancelled.state, State::Cancelled);
    assert_eq!(cancelled.tasks[0].done, Some(Done::Erased));
    assert_eq!(cancelled.tasks[1].done, None);
    assert!(!trash.join("old folder").exists() && trash.join("old.txt").exists());

    // One Empty Trash at a time, continued ones included.
    let retry = Request::Retry {
        job: op,
        conflict: None,
    };
    let rest = trash_items(&trash).unwrap();
    let other = run(&mut shared.lock(), Request::EmptyTrash { items: rest })
        .unwrap()
        .op;
    let refused = run(&mut shared.lock(), retry.clone()).unwrap_err();
    assert_eq!(refused, "The Trash is already being emptied.");
    run(&mut shared.lock(), Request::Cancel { job: other }).unwrap();

    // Continued, it's cut short again: the owner goes once the item is
    // claimed under its hidden name, before it's erased.
    run(&mut shared.lock(), retry).unwrap();
    let claimed = trash.join(format!(".monodrive-{op}-1"));
    std::fs::rename(trash.join("old.txt"), &claimed).unwrap();
    drop(shared);
    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    let interrupted = status(&shared, op);
    assert_eq!(interrupted.state, State::Interrupted);
    assert_eq!(interrupted.tasks[0].done, Some(Done::Erased));
    assert!(claimed.exists());

    let emptied = continued(&shared, op, None);
    assert_eq!(emptied.state, State::Completed);
    assert_eq!(emptied.tasks[1].done, Some(Done::Erased));
    assert!(trash_items(&trash).unwrap().is_empty());
    assert!(run(&mut shared.lock(), Request::Undo).is_err());
}

/// `count` files in `files`, `000.txt` on, and a Trash of their own beside
/// them (see `disk::trash`), so nothing reaches the user's.
pub(super) fn trash_fixture(dir: &Path, files: &Path, count: usize) -> (PathBuf, Vec<PathBuf>) {
    // SAFETY: geteuid has no preconditions.
    let trash = dir
        .join(".Trashes")
        .join(unsafe { libc::geteuid() }.to_string());
    std::fs::create_dir_all(&trash).unwrap();
    let paths = (0..count)
        .map(|i| {
            let path = files.join(format!("{i:03}.txt"));
            std::fs::write(&path, i.to_string()).unwrap();
            path
        })
        .collect();
    (trash, paths)
}

pub(super) fn trash(paths: &[PathBuf]) -> Request {
    Request::Trash {
        paths: paths.to_vec(),
    }
}

#[test]
fn many_items_go_to_the_trash_as_a_job_that_reports_each_and_undoes_whole() {
    let (dir, files, mut engine) = engine();
    let (own_trash, paths) = trash_fixture(dir.path(), &files, TRASH_JOB_AT);
    // Fewer answer at once.
    let few = run(&mut engine, trash(&paths[..TRASH_JOB_AT - 1])).unwrap();
    assert!(!few.job);
    assert_eq!(few.items.len(), TRASH_JOB_AT - 1);
    assert!(run(&mut engine, Request::Undo).is_ok());

    let shared = jobs::Shared::new(engine, None);
    let gone = files.join("gone.txt");
    let mut asked = paths.clone();
    asked.push(gone.clone());
    let agent = Origin::Agent {
        client: Some("test".into()),
    };
    let queued = shared
        .lock()
        .submit(submission(agent, trash(&asked)))
        .unwrap();
    assert!(queued.job && queued.items.is_empty());
    assert!(paths[0].exists());
    // Swapped for another file while the job waits: that one stays.
    let other = dir.path().join("other.txt");
    std::fs::write(&other, "other").unwrap();
    std::fs::rename(&other, &paths[1]).unwrap();
    jobs::run(&shared, queued.op);
    let done = status(&shared, queued.op);
    assert_eq!(
        (done.title.as_str(), done.state),
        ("Move to Trash", State::Failed)
    );
    assert_eq!(done.into, dir.path().join("Trash"));
    let failed: Vec<(usize, &str)> = done
        .tasks
        .iter()
        .enumerate()
        .filter_map(|(i, task)| match &task.done {
            Some(Done::To(to)) => {
                assert!(to.starts_with(&own_trash), "{}", to.display());
                None
            }
            Some(Done::Failed(why)) => Some((i, why.as_str())),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(failed[0].0 == 1 && failed[0].1.contains("a different item"));
    assert!(failed[1].0 == TRASH_JOB_AT && failed[1].1.contains("gone.txt"));
    assert_eq!(read(&paths[1]), "other");
    assert!(done.undoable);
    let changes = agent_changes(&paths_of(&shared), 0, 1);
    assert_eq!(
        (changes[0].title.as_str(), changes[0].items),
        ("Move to Trash", TRASH_JOB_AT + 1)
    );

    // One undo puts back every item that went.
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    assert_eq!(undone.items.len(), TRASH_JOB_AT - 1);
    assert!(paths.iter().all(|path| path.exists()));
    assert_eq!(read(&paths[0]), "0");
    assert_eq!(std::fs::read_dir(&own_trash).unwrap().count(), 0);
}

pub(super) fn paths_of(shared: &jobs::Shared) -> Paths {
    shared.lock().paths.clone()
}

#[test]
fn a_cancelled_trash_job_keeps_what_went_and_undo_puts_it_back() {
    let (dir, files, engine) = engine();
    let (own_trash, paths) = trash_fixture(dir.path(), &files, TRASH_JOB_AT);
    let shared = jobs::Shared::new(engine, None);
    let op = run(&mut shared.lock(), trash(&paths)).unwrap().op;
    assert!(jobs::next(&shared, op) && jobs::next(&shared, op));
    run(&mut shared.lock(), Request::Cancel { job: op }).unwrap();
    assert!(!jobs::next(&shared, op));
    let cancelled = status(&shared, op);
    assert_eq!(cancelled.state, State::Cancelled);
    assert!(cancelled.undoable);
    let went = cancelled.tasks.iter().filter(|t| t.done.is_some()).count();
    assert_eq!(went, 2);
    assert!(!paths[1].exists() && paths[2].exists());
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    assert_eq!(undone.items.len(), 2);
    assert!(paths.iter().all(|path| path.exists()));
    assert_eq!(std::fs::read_dir(&own_trash).unwrap().count(), 0);
}

#[test]
fn a_trash_job_cut_short_by_a_crash_is_settled_from_the_disk_and_undoes_whole() {
    let (dir, files, engine) = engine();
    let (own_trash, items) = trash_fixture(dir.path(), &files, TRASH_JOB_AT);
    let shared = jobs::Shared::new(engine, None);
    // The same file named twice, of which only one went, and a hard link to
    // another, which share an identity.
    let linked = files.join("linked.txt");
    std::fs::hard_link(&items[2], &linked).unwrap();
    let mut asked = items.clone();
    asked.extend([items[1].clone(), linked.clone()]);
    let op = run(&mut shared.lock(), trash(&asked)).unwrap().op;
    assert!(jobs::next(&shared, op));
    // The owner goes once the next items are in the Trash, before their
    // records reach the journal.
    let landed = disk::trash(&items[1]).unwrap();
    let both = [
        disk::trash(&items[2]).unwrap(),
        disk::trash(&linked).unwrap(),
    ];
    drop(shared);
    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    let interrupted = status(&shared, op);
    assert_eq!(interrupted.state, State::Interrupted);
    assert!(matches!(&interrupted.tasks[0].done, Some(Done::To(to)) if to.starts_with(&own_trash)));
    assert_eq!(interrupted.tasks[1].done, Some(Done::To(landed)));
    let mut found: Vec<Option<Done>> = [2, TRASH_JOB_AT + 1]
        .map(|i| interrupted.tasks[i].done.clone())
        .into();
    found.sort_by_key(|done| format!("{done:?}"));
    assert_eq!(found, both.map(|to| Some(Done::To(to))));
    assert_eq!(interrupted.tasks[3].done, None);
    assert!(
        matches!(&interrupted.tasks[TRASH_JOB_AT].done, Some(Done::Failed(why)) if why.contains("can't find")),
        "{:?}",
        interrupted.tasks[TRASH_JOB_AT].done
    );

    let continued = continued(&shared, op, None);
    assert_eq!(continued.state, State::Failed);
    assert!(items.iter().all(|path| !path.exists()));
    // Each run is its own step back: the continued one, then the first.
    assert_eq!(
        run(&mut shared.lock(), Request::Undo).unwrap().items.len(),
        TRASH_JOB_AT - 3
    );
    assert_eq!(
        run(&mut shared.lock(), Request::Undo).unwrap().items.len(),
        4
    );
    assert!(items.iter().chain([&linked]).all(|path| path.exists()));
    assert_eq!(std::fs::read_dir(&own_trash).unwrap().count(), 0);
}

#[test]
fn one_owner_serves_clients_and_exits_once_the_last_one_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let owner = Owner::bind(paths.clone()).unwrap();
    assert!(
        Owner::bind(paths.clone())
            .err()
            .unwrap()
            .starts_with("Another Monodrive owner")
    );
    let serving = std::thread::spawn(move || owner.serve());
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "").unwrap();
    let mut client = Client::new(paths.clone(), None);
    assert_eq!(
        moved(client.submit(Origin::Window, rename(&a, "b"))),
        vec![b.clone()]
    );
    drop(client);
    serving.join().unwrap().unwrap();
    assert!(!paths.state.join("owner.sock").exists());
    // Without a binary to start an owner with, a client can't reach one.
    let mut client = Client::new(paths.clone(), None);
    assert!(client.submit(Origin::Window, Request::Undo).is_err());
    // The next owner takes over the same history.
    let owner = Owner::bind(paths.clone()).unwrap();
    let serving = std::thread::spawn(move || owner.serve());
    let mut client = Client::new(paths, None);
    assert_eq!(
        moved(client.submit(Origin::Window, Request::Undo)),
        vec![a.clone()]
    );
    assert_eq!(moved(client.submit(Origin::Window, Request::Redo)), [b]);
    drop(client);
    serving.join().unwrap().unwrap();
}

#[test]
fn a_torn_last_line_is_cut_before_the_journal_grows() {
    let dir = tempfile::tempdir().unwrap();
    let journal = paths(dir.path()).state.join(JOURNAL);
    // A crash partway through appending a record.
    std::fs::write(&journal, r#"{"record":"begin","op":1,"subm"#).unwrap();
    let a = dir.path().join("a");
    std::fs::write(&a, "").unwrap();
    let first = submission(Origin::Window, rename(&a, "b"));
    let mut engine = Engine::open(paths(dir.path())).unwrap();
    let outcome = engine.submit(first.clone()).unwrap();
    drop(engine);
    let mut engine = Engine::open(paths(dir.path())).unwrap();
    assert_eq!(engine.submit(first).unwrap(), outcome);
    drop(engine);

    // Anything else that doesn't read stops the owner instead.
    let mut damaged = b"not a record\n".to_vec();
    damaged.extend(std::fs::read(&journal).unwrap());
    std::fs::write(&journal, damaged).unwrap();
    let error = Engine::open(paths(dir.path())).err().unwrap();
    assert!(error.contains("line 1"), "{error}");
}

/// `src/` holding `a.txt`, `b/inner.md` and `c.txt`, and `dest/` holding
/// its own `a.txt` and `c.txt`.
pub(super) fn transfer_fixture(files: &Path) -> (PathBuf, PathBuf) {
    let (src, dest) = (files.join("src"), files.join("dest"));
    std::fs::create_dir_all(src.join("b")).unwrap();
    std::fs::create_dir(&dest).unwrap();
    for (path, text) in [
        (src.join("a.txt"), "new a"),
        (src.join("b/inner.md"), "inner"),
        (src.join("c.txt"), "new c"),
        (dest.join("a.txt"), "old a"),
        (dest.join("c.txt"), "old c"),
    ] {
        std::fs::write(path, text).unwrap();
    }
    (src, dest)
}

/// Queues a copy or move and runs it here to the end, as a job thread would.
pub(super) fn job(shared: &jobs::Shared, request: Request) -> Job {
    let op = run(&mut shared.lock(), request).unwrap().op;
    jobs::run(shared, op);
    status(shared, op)
}

pub(super) fn status(shared: &jobs::Shared, op: u64) -> Job {
    let engine = shared.lock();
    let jobs = engine.jobs();
    jobs.into_iter().find(|job| job.id == op).unwrap()
}

/// Continues a stopped job and runs it here to the end, checking it stays
/// the same job.
pub(super) fn continued(shared: &jobs::Shared, job: u64, conflict: Option<Conflict>) -> Job {
    let count = shared.lock().jobs().len();
    let op = run(&mut shared.lock(), Request::Retry { job, conflict })
        .unwrap()
        .op;
    assert_eq!(op, job);
    jobs::run(shared, op);
    assert_eq!(shared.lock().jobs().len(), count);
    status(shared, op)
}

pub(super) fn copy(paths: &[&Path], into: &Path, conflict: Conflict) -> Request {
    Request::Copy {
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
        into: into.into(),
        conflict,
    }
}

pub(super) fn transfer(paths: &[&Path], into: &Path, conflict: Conflict) -> Request {
    Request::Move {
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
        into: into.into(),
        conflict,
    }
}

pub(super) fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Takes what a test sent to the Trash back out, so the Trash ends as it was.
pub(super) fn untrash(trashed: &Path, into: &Path) {
    std::fs::rename(trashed, disk::free_name(&into.join("untrashed"))).unwrap();
}

#[test]
fn copies_and_moves_follow_each_conflict_choice() {
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let a = src.join("a.txt");

    let stopped = job(&shared, copy(&[&a], &dest, Conflict::Stop));
    assert_eq!(stopped.state, State::Waiting);
    assert!(
        matches!(&stopped.tasks[0].done, Some(Done::Failed(why)) if why.contains("already exists"))
    );
    let skipped = continued(&shared, stopped.id, Some(Conflict::Skip));
    assert_eq!(skipped.state, State::Completed);
    assert!(matches!(skipped.tasks[0].done, Some(Done::Skipped(_))));
    assert_eq!(read(&dest.join("a.txt")), "old a");

    let both = job(&shared, copy(&[&a], &dest, Conflict::KeepBoth));
    assert_eq!(both.tasks[0].done, Some(Done::To(dest.join("a 2.txt"))));
    assert_eq!(read(&dest.join("a 2.txt")), "new a");
    assert_eq!(read(&a), "new a");

    // Replace keeps what was there in the Trash, and undo brings it back.
    let replaced = job(&shared, copy(&[&a], &dest, Conflict::Replace));
    assert_eq!(replaced.state, State::Completed);
    assert_eq!(read(&dest.join("a.txt")), "new a");
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    assert_eq!(read(&dest.join("a.txt")), "old a");
    let copy_in_trash = undone.items[0].result.clone().unwrap();
    assert_eq!(read(&copy_in_trash), "new a");
    untrash(&copy_in_trash, &files);

    // On one volume a move is a rename; undo renames it back.
    let moved = job(&shared, transfer(&[&src.join("b")], &dest, Conflict::Stop));
    assert_eq!(moved.tasks[0].done, Some(Done::To(dest.join("b"))));
    assert!(!src.join("b").exists());
    assert_eq!(read(&dest.join("b/inner.md")), "inner");
    run(&mut shared.lock(), Request::Undo).unwrap();
    assert_eq!(read(&src.join("b/inner.md")), "inner");

    // A replacing move, undone: both items back where they were.
    let c = src.join("c.txt");
    job(&shared, transfer(&[&c], &dest, Conflict::Replace));
    assert_eq!(read(&dest.join("c.txt")), "new c");
    assert!(!c.exists());
    run(&mut shared.lock(), Request::Undo).unwrap();
    assert_eq!(
        (read(&c), read(&dest.join("c.txt"))),
        ("new c".into(), "old c".into())
    );

    let into_itself = job(&shared, copy(&[&src], &src.join("b"), Conflict::Stop));
    assert!(
        matches!(&into_itself.tasks[0].done, Some(Done::Failed(why)) if why.contains("into itself"))
    );
    assert_eq!(into_itself.state, State::Failed);
    let in_place = job(&shared, transfer(&[&a], &src, Conflict::Stop));
    assert!(
        matches!(&in_place.tasks[0].done, Some(Done::Failed(why)) if why.contains("already in"))
    );
}

#[test]
fn cancel_keeps_finished_items_and_a_queued_job_never_starts() {
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let (a, c) = (src.join("a.txt"), src.join("c.txt"));
    let request = copy(&[&src.join("b"), &a, &c], &dest, Conflict::KeepBoth);
    let op = run(&mut shared.lock(), request).unwrap().op;
    assert!(jobs::next(&shared, op));
    run(&mut shared.lock(), Request::Cancel { job: op }).unwrap();
    assert!(!jobs::next(&shared, op));
    let cancelled = status(&shared, op);
    assert_eq!(cancelled.state, State::Cancelled);
    assert_eq!(cancelled.tasks[0].done, Some(Done::To(dest.join("b"))));
    assert_eq!(cancelled.tasks[1].done, None);
    assert!(!dest.join("a 2.txt").exists());
    assert!(run(&mut shared.lock(), Request::Cancel { job: op }).is_err());

    let queued = run(&mut shared.lock(), copy(&[&a], &dest, Conflict::KeepBoth))
        .unwrap()
        .op;
    run(&mut shared.lock(), Request::Cancel { job: queued }).unwrap();
    assert!(!jobs::next(&shared, queued));
    assert_eq!(status(&shared, queued).state, State::Cancelled);
    assert!(!dest.join("a 2.txt").exists());
    // Undo reaches the cancelled job's finished copy, which goes to the
    // Trash; take it out again.
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    let trashed = undone.items[0].result.clone().unwrap();
    assert!(!dest.join("b").exists());
    untrash(&trashed, &files);
}

#[test]
fn answering_a_waiting_job_continues_it_in_place() {
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let (a, b, c) = (src.join("a.txt"), src.join("b"), src.join("c.txt"));
    let waiting = job(&shared, copy(&[&b, &a, &c], &dest, Conflict::Stop));
    assert_eq!(waiting.state, State::Waiting);
    assert_eq!(waiting.tasks[0].done, Some(Done::To(dest.join("b"))));
    assert_eq!(waiting.tasks[2].done, None);
    let done = continued(&shared, waiting.id, Some(Conflict::KeepBoth));
    assert_eq!(done.state, State::Completed);
    let landed: Vec<_> = done.tasks.into_iter().map(|task| task.done).collect();
    assert_eq!(
        landed,
        [dest.join("b"), dest.join("a 2.txt"), dest.join("c 2.txt")].map(|to| Some(Done::To(to)))
    );
    // A finished job has nothing left to continue.
    let again = Request::Retry {
        job: waiting.id,
        conflict: None,
    };
    let refused = run(&mut shared.lock(), again).unwrap_err();
    assert!(refused.contains("nothing left"), "{refused}");
    // Undo takes back what the answer did, apart from what came before it.
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    assert_eq!(undone.items.len(), 2);
    for item in undone.items {
        untrash(&item.result.unwrap(), &files);
    }
    assert!(dest.join("b/inner.md").exists() && !dest.join("a 2.txt").exists());
}

#[test]
fn a_failed_job_continues_in_place_once_the_reason_is_gone() {
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, _) = transfer_fixture(&files);
    let later = files.join("later");
    let failed = job(&shared, copy(&[&src.join("a.txt")], &later, Conflict::Stop));
    assert_eq!(failed.state, State::Failed);
    assert!(
        matches!(&failed.tasks[0].done, Some(Done::Failed(why)) if why.contains("not a folder"))
    );
    std::fs::create_dir(&later).unwrap();
    let done = continued(&shared, failed.id, None);
    assert_eq!(done.state, State::Completed);
    assert_eq!(done.tasks[0].done, Some(Done::To(later.join("a.txt"))));
    assert_eq!(read(&later.join("a.txt")), "new a");
}

#[test]
fn a_continued_job_cut_short_by_a_crash_keeps_both_runs() {
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let (a, b, c) = (src.join("a.txt"), src.join("b"), src.join("c.txt"));
    let waiting = job(&shared, copy(&[&b, &a, &c], &dest, Conflict::Stop));
    let answer = submission(
        Origin::Window,
        Request::Retry {
            job: waiting.id,
            conflict: Some(Conflict::KeepBoth),
        },
    );
    assert_eq!(shared.lock().submit(answer.clone()).unwrap().op, waiting.id);
    // The owner copies a.txt, then dies before c.txt.
    assert!(jobs::next(&shared, waiting.id));
    drop(shared);

    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    let settled = status(&shared, waiting.id);
    assert_eq!(settled.state, State::Interrupted);
    let landed: Vec<_> = settled.tasks.into_iter().map(|task| task.done).collect();
    assert_eq!(
        landed,
        [
            Some(Done::To(dest.join("b"))),
            Some(Done::To(dest.join("a 2.txt"))),
            None
        ]
    );
    // The answer sent again is the same continuation, not another.
    assert_eq!(shared.lock().submit(answer).unwrap().op, waiting.id);
    // Undo reaches the continuation's copy first, then the first run's.
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    let [trashed] = undone.items.try_into().unwrap();
    untrash(&trashed.result.unwrap(), &files);
    assert!(dest.join("b").exists() && !dest.join("a 2.txt").exists());
    let done = continued(&shared, waiting.id, None);
    assert_eq!(done.tasks[2].done, Some(Done::To(dest.join("c 2.txt"))));
}

#[test]
fn an_interrupted_job_is_settled_from_the_disk_and_continues_past_what_finished() {
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let (b, c, a) = (src.join("b"), src.join("c.txt"), src.join("a.txt"));
    std::fs::rename(dest.join("c.txt"), files.join("c elsewhere.txt")).unwrap();
    std::fs::rename(dest.join("a.txt"), files.join("a elsewhere.txt")).unwrap();
    let submitted = submission(
        Origin::Window,
        transfer(&[&b, &c, &a], &dest, Conflict::Stop),
    );
    let op = shared.lock().submit(submitted.clone()).unwrap().op;
    assert!(jobs::next(&shared, op));
    // The owner dies right after renaming c.txt, before recording it.
    let (id, stamp) = disk::stamp(&c).unwrap();
    let mut work = shared.lock().job_mut(op).unwrap().work[1].clone();
    work.to = Some(dest.join("c.txt"));
    work.steps.push(Step {
        id,
        undone: Spot::Path(c.clone()),
        done: Spot::Path(dest.join("c.txt")),
        applied: false,
        stamp,
        keep: None,
    });
    let intent = Record::Work { op, index: 1, work };
    shared.lock().write(&intent).unwrap();
    disk::rename_new(&c, &dest.join("c.txt")).unwrap();
    drop(shared);

    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    let settled = status(&shared, op);
    assert_eq!(settled.state, State::Interrupted);
    assert_eq!(settled.tasks[0].done, Some(Done::To(dest.join("b"))));
    assert_eq!(settled.tasks[1].done, Some(Done::To(dest.join("c.txt"))));
    assert_eq!(settled.tasks[2].done, None);
    assert!(a.exists());
    // The same submission again is the same job, not a new one.
    assert_eq!(shared.lock().submit(submitted).unwrap().op, op);

    let resumed = continued(&shared, op, None);
    assert_eq!(resumed.state, State::Completed);
    assert_eq!(resumed.tasks[1].done, Some(Done::To(dest.join("c.txt"))));
    assert_eq!(resumed.tasks[2].done, Some(Done::To(dest.join("a.txt"))));
    // Undo takes back the continuation, then all the interrupted run changed.
    run(&mut shared.lock(), Request::Undo).unwrap();
    run(&mut shared.lock(), Request::Undo).unwrap();
    for path in [&a, &b, &c] {
        assert!(path.exists(), "{}", path.display());
    }
}

#[test]
fn a_copy_placed_just_before_a_crash_still_undoes_to_the_trash() {
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let (b, placed) = (src.join("b"), dest.join("b"));
    let op = run(&mut shared.lock(), copy(&[&b], &dest, Conflict::Stop))
        .unwrap()
        .op;
    // The owner copies to the hidden name, records the placement, renames
    // the copy into place, and dies before recording that it did.
    let temp = dest.join(format!(".monodrive-{op}-0"));
    disk::copy(&b, &temp, None).unwrap();
    let (id, stamp) = disk::stamp(&temp).unwrap();
    let mut work = shared.lock().job_mut(op).unwrap().work[0].clone();
    work.to = Some(placed.clone());
    work.temp = Some(temp.clone());
    work.steps.push(Step {
        id,
        undone: Spot::Path(temp.clone()),
        done: Spot::Path(placed.clone()),
        applied: false,
        stamp,
        keep: None,
    });
    shared
        .lock()
        .write(&Record::Work { op, index: 0, work })
        .unwrap();
    disk::rename_new(&temp, &placed).unwrap();
    drop(shared);

    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    assert_eq!(
        status(&shared, op).tasks[0].done,
        Some(Done::To(placed.clone()))
    );
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    let trashed = undone.items[0].result.clone().unwrap();
    assert!(!placed.exists() && !temp.exists());
    assert!(
        trashed.components().any(|c| c.as_os_str() == ".Trash"),
        "{}",
        trashed.display()
    );
    untrash(&trashed, &files);
}

#[test]
fn a_source_replaced_while_its_job_waits_is_left_alone() {
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, _) = transfer_fixture(&files);
    let c = src.join("c.txt");
    let op = run(&mut shared.lock(), transfer(&[&c], &files, Conflict::Stop))
        .unwrap()
        .op;
    std::fs::write(files.join("other"), "other").unwrap();
    std::fs::rename(files.join("other"), &c).unwrap();
    jobs::run(&shared, op);
    let job = status(&shared, op);
    assert!(
        matches!(&job.tasks[0].done, Some(Done::Failed(why)) if why.contains("a different item")),
        "{job:?}"
    );
    assert_eq!(read(&c), "other");
    assert!(!files.join("c.txt").exists());
}

#[test]
fn a_watch_hears_another_client_queue_a_job() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let owner = Owner::bind(paths.clone()).unwrap();
    let serving = std::thread::spawn(move || owner.serve());
    let mut window = Client::new(paths.clone(), None);
    assert!(window.jobs().unwrap().is_empty());
    // Asking about jobs writes nothing; only the agent's copy will.
    let (heard, hear) = std::sync::mpsc::channel();
    let watch = window
        .watch(move || {
            let _ = heard.send(());
        })
        .unwrap();
    let (src, dest) = transfer_fixture(dir.path());
    let mut agent = Client::new(paths, None);
    let copy = copy(&[&src.join("b")], &dest, Conflict::Stop);
    agent.submit(Origin::Agent { client: None }, copy).unwrap();
    hear.recv().unwrap();
    drop((watch, window, agent));
    serving.join().unwrap().unwrap();
}

#[test]
fn cancelling_a_waiting_job_keeps_what_it_finished_in_the_history_once() {
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let waiting = job(
        &shared,
        copy(&[&src.join("b"), &src.join("a.txt")], &dest, Conflict::Stop),
    );
    assert_eq!(waiting.state, State::Waiting);
    run(&mut shared.lock(), Request::Cancel { job: waiting.id }).unwrap();
    let cancelled = status(&shared, waiting.id);
    assert_eq!(cancelled.state, State::Cancelled);
    assert_eq!(cancelled.tasks[0].done, Some(Done::To(dest.join("b"))));
    assert_eq!(read(&dest.join("a.txt")), "old a");
    // The copy it finished is in the history once: undone and redone,
    // there's nothing more to redo. Redo takes it back out of the Trash.
    run(&mut shared.lock(), Request::Undo).unwrap();
    assert!(!dest.join("b").exists());
    run(&mut shared.lock(), Request::Redo).unwrap();
    assert!(dest.join("b/inner.md").exists());
    assert_eq!(
        run(&mut shared.lock(), Request::Redo).unwrap_err(),
        "Nothing to redo."
    );
}

#[test]
fn a_job_retried_as_a_new_one_by_an_older_owner_no_longer_waits() {
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let a = src.join("a.txt");
    let waiting = job(&shared, copy(&[&a], &dest, Conflict::Stop));
    assert_eq!(waiting.state, State::Waiting);
    drop(shared);
    // What an owner before Continue wrote for Skip: a new job of the
    // remaining items, which finished.
    let retry = serde_json::json!({
        "record": "job",
        "op": 2,
        "submission": {
            "id": "old-retry",
            "origin": {"type": "window"},
            "request": {"type": "retry", "job": waiting.id, "conflict": "skip"}
        },
        "spec": {"kind": "copy", "into": dest, "conflict": "skip", "sources": [a], "ids": []}
    });
    let finish = serde_json::json!({"record": "finish", "op": 2, "state": "completed"});
    let journal = paths(dir.path()).state.join(JOURNAL);
    let mut lines = std::fs::read_to_string(&journal).unwrap();
    for record in [retry, finish] {
        lines.push_str(&format!("{record}\n"));
    }
    std::fs::write(&journal, lines).unwrap();

    let engine = Engine::open(paths(dir.path())).unwrap();
    let left: Vec<_> = engine
        .jobs()
        .iter()
        .map(|job| (job.id, job.state))
        .collect();
    assert_eq!(left, [(2, State::Completed)]);
}

#[test]
fn undoing_one_operation_is_refused_once_another_came_after_it() {
    let (_dir, files, mut engine) = engine();
    let (a, c) = (files.join("a"), files.join("c"));
    std::fs::write(&a, "").unwrap();
    std::fs::write(&c, "").unwrap();
    let mine = run(&mut engine, rename(&a, "b")).unwrap().op;
    let agent = Origin::Agent { client: None };
    engine.submit(submission(agent, rename(&c, "d"))).unwrap();
    let refused = run(&mut engine, Request::UndoOf { op: mine }).unwrap_err();
    assert_eq!(refused, "Can't undo: something else changed files since.");
    assert!(files.join("b").exists() && files.join("d").exists());
    // Once the agent's rename is undone, the window's is next again.
    run(&mut engine, Request::Undo).unwrap();
    assert_eq!(
        moved(run(&mut engine, Request::UndoOf { op: mine })),
        vec![a]
    );
}

#[test]
fn trimming_keeps_an_old_job_that_was_continued_lately() {
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, dest) = transfer_fixture(&files);
    let waiting = job(&shared, copy(&[&src.join("a.txt")], &dest, Conflict::Stop));
    drop(shared);
    // Enough finished requests since that once the job is continued, the
    // next opening trims the journal past the job's own records.
    let journal = paths(dir.path()).state.join(JOURNAL);
    let mut lines = std::fs::read_to_string(&journal).unwrap();
    for op in 2..TRIM_AT as u64 + 1 {
        let submission = submission(Origin::Window, Request::Undo);
        let begin = Record::Begin {
            op,
            submission,
            target: None,
            steps: Vec::new(),
            creates: Vec::new(),
            temps: Vec::new(),
            at: 0,
        };
        let outcome = Outcome {
            op,
            title: "Undo".into(),
            items: Vec::new(),
            interrupted: false,
            job: false,
        };
        let end = Record::End {
            op,
            outcome,
            steps: Vec::new(),
        };
        for record in [begin, end] {
            lines.push_str(&serde_json::to_string(&record).unwrap());
            lines.push('\n');
        }
    }
    std::fs::write(&journal, lines).unwrap();
    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    let done = continued(&shared, waiting.id, Some(Conflict::KeepBoth));
    assert_eq!(done.tasks[0].done, Some(Done::To(dest.join("a 2.txt"))));
    drop(shared);

    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    assert_eq!(status(&shared, waiting.id).state, State::Completed);
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    untrash(&undone.items[0].result.clone().unwrap(), &files);
    assert!(!dest.join("a 2.txt").exists());
}

#[test]
fn agent_changes_say_who_changed_what_lately_and_need_no_owner() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    std::fs::create_dir_all(&paths.state).unwrap();
    let agent = Origin::Agent {
        client: Some("claude-code".into()),
    };
    let begin = |op, origin, at| Record::Begin {
        op,
        submission: submission(origin, Request::Undo),
        target: None,
        steps: Vec::new(),
        creates: Vec::new(),
        temps: Vec::new(),
        at,
    };
    let end = |op, title: &str| Record::End {
        op,
        outcome: Outcome {
            op,
            title: title.into(),
            items: vec![
                Item {
                    path: "/a".into(),
                    result: Ok("/b".into()),
                },
                Item {
                    path: "/c".into(),
                    result: Err("no".into()),
                },
            ],
            interrupted: false,
            job: false,
        },
        steps: Vec::new(),
    };
    let records = [
        begin(1, agent.clone(), 50),
        end(1, "Too old"),
        begin(2, Origin::Window, 200),
        end(2, "The window's"),
        begin(3, agent.clone(), 300),
        end(3, "Rename"),
        begin(4, agent, 400),
    ];
    let mut journal = String::new();
    for record in &records {
        journal += &serde_json::to_string(record).unwrap();
        journal.push('\n');
    }
    journal += "{\"torn";
    std::fs::write(paths.state.join(JOURNAL), &journal).unwrap();

    let changes = agent_changes(&paths, 100, 10);
    assert_eq!(
        changes,
        [AgentChange {
            client: Some("claude-code".into()),
            title: "Rename".into(),
            items: 1,
            at: 300,
        }],
        "only what an agent finished since then, counting what changed"
    );
    assert_eq!(
        std::fs::read_to_string(paths.state.join(JOURNAL)).unwrap(),
        journal,
        "looking leaves the journal alone, torn line included"
    );
}

#[test]
fn removing_a_worktree_trashes_it_and_its_registration_and_undo_brings_it_back() {
    use crate::worktrees::{
        self, Git, Risk,
        tests::{fixture, real_tempdir, repo},
    };
    let (_state, _files, mut engine) = engine();
    let (_dir, root) = real_tempdir();
    let main = root.join("repo");
    repo(&main);
    fixture(
        &main,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    );
    let feature = root.join("feature");
    let registration = main.join(".git/worktrees/feature");
    let remove = |confirmed: Risk| Request::RemoveWorktree {
        repo: main.clone(),
        path: feature.clone(),
        confirmed,
        with_folder: true,
    };
    let listed = || {
        worktrees::list(&Git::find().unwrap(), &main)
            .unwrap()
            .worktrees
            .len()
    };

    // Changed since the person looked: nothing moves.
    std::fs::write(feature.join("new"), "").unwrap();
    let refused = run(&mut engine, remove(Risk::default())).unwrap_err();
    assert!(refused.contains("changed since"), "{refused}");
    assert!(feature.exists() && registration.exists());

    let seen = worktrees::risk(&Git::find().unwrap(), &feature).unwrap();
    // Asked to forget it while its folder was away: back now, nothing moves.
    let forget = Request::RemoveWorktree {
        repo: main.clone(),
        path: feature.clone(),
        confirmed: seen.clone(),
        with_folder: false,
    };
    let refused = run(&mut engine, forget).unwrap_err();
    assert!(refused.contains("changed since"), "{refused}");
    assert!(feature.exists() && registration.exists());

    let trashed = moved(run(&mut engine, remove(seen)));
    assert_eq!(trashed.len(), 2, "{trashed:?}");
    assert!(!feature.exists() && !registration.exists());
    assert_eq!(listed(), 1);
    assert_eq!(
        fixture(&main, &["branch", "--list", "feature"]).trim(),
        "feature",
        "the branch stays"
    );

    assert_eq!(
        moved(run(&mut engine, Request::Undo)),
        vec![registration.clone(), feature.clone()]
    );
    assert_eq!(listed(), 2);
    assert!(feature.join("new").exists());
    assert_eq!(
        fixture(&feature, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "feature"
    );

    let main_refused = run(
        &mut engine,
        Request::RemoveWorktree {
            repo: main.clone(),
            path: main.clone(),
            confirmed: Risk::default(),
            with_folder: true,
        },
    )
    .unwrap_err();
    assert!(main_refused.contains("own checkout"), "{main_refused}");
}

/// What a copy should keep of an item: everything but its name.
pub(super) fn about(path: &Path, with_time: bool) -> String {
    use std::os::darwin::fs::MetadataExt as _;
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).unwrap();
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let mut names = vec![0u8; 1024];
    // SAFETY: a NUL-terminated path and a buffer of its length.
    let len = unsafe {
        libc::listxattr(
            c.as_ptr(),
            names.as_mut_ptr().cast(),
            names.len(),
            libc::XATTR_NOFOLLOW,
        )
    };
    names.truncate(len.max(0) as usize);
    // The ACL as `ls -le` prints it, after the item's own line.
    let listed = std::process::Command::new("ls")
        .arg("-led")
        .arg(path)
        .output()
        .unwrap();
    let acl: Vec<String> = String::from_utf8_lossy(&listed.stdout)
        .lines()
        .skip(1)
        .map(String::from)
        .collect();
    let time = if with_time {
        format!("modified {}.{}, ", meta.mtime(), meta.mtime_nsec())
    } else {
        String::new()
    };
    format!(
        "mode {:o}, size {}, {time}created {}, flags {}, xattrs {:?}, acl {acl:?}, link {:?}",
        meta.mode(),
        if meta.is_dir() { 0 } else { meta.len() },
        meta.st_birthtime(),
        meta.st_flags(),
        String::from_utf8_lossy(&names),
        std::fs::read_link(path).ok(),
    )
}

/// What a copy should keep of the folder `root`, without its time, which
/// copyfile doesn't keep, and of each item inside it, by path.
pub(super) fn kept(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = vec![(PathBuf::from("."), about(root, false))];
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for item in std::fs::read_dir(&folder).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                folders.push(path.clone());
            }
            out.push((
                path.strip_prefix(root).unwrap().to_path_buf(),
                about(&path, true),
            ));
        }
    }
    out.sort();
    out
}

#[test]
fn a_whole_folder_clone_keeps_what_copyfile_keeps() {
    let dir = tempfile::tempdir().unwrap();
    let top = dir.path().canonicalize().unwrap().join("top");
    let nested = top.join("nested");
    std::fs::create_dir_all(nested.join("deeper")).unwrap();
    std::fs::create_dir(top.join("empty")).unwrap();
    std::fs::write(top.join("plain.txt"), "plain").unwrap();
    std::fs::write(top.join("noted.txt"), "noted").unwrap();
    // On a file, and on the folder itself, which gets its metadata last.
    for noted in [top.join("noted.txt"), top.clone()] {
        let noted = std::ffi::CString::new(noted.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: NUL-terminated name and path, and a value of its length.
        let set = unsafe {
            libc::setxattr(
                noted.as_ptr(),
                c"com.example.note".as_ptr(),
                b"kept".as_ptr().cast(),
                4,
                0,
                0,
            )
        };
        assert_eq!(set, 0);
    }
    std::os::unix::fs::symlink("missing", top.join("dangling")).unwrap();
    std::os::unix::fs::symlink("plain.txt", top.join("to plain")).unwrap();
    std::fs::write(top.join("tool"), "#!").unwrap();
    std::fs::set_permissions(
        top.join("tool"),
        std::os::unix::fs::PermissionsExt::from_mode(0o4755),
    )
    .unwrap();
    // An ACL inside the folder, which clonefile's CLONE_ACL doesn't reach.
    std::fs::write(nested.join("guarded.txt"), "guarded").unwrap();
    // And one on the folder itself, which goes on last. Not a deny write,
    // which copyfile puts on first and then can't fill the folder.
    for (guarded, rule) in [
        (nested.join("guarded.txt"), "everyone deny write"),
        (top.clone(), "everyone allow read"),
    ] {
        let acl = std::process::Command::new("chmod")
            .args(["+a", rule])
            .arg(guarded)
            .status()
            .unwrap();
        assert!(acl.success());
    }
    // Old times on a file and on folders, set last, since filling a folder
    // changes its own.
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    for path in [
        top.join("plain.txt"),
        nested.join("deeper"),
        nested.clone(),
        top.clone(),
    ] {
        File::open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(past)
                    .set_accessed(past),
            )
            .unwrap();
    }

    let (cloned, copied) = (dir.path().join("cloned"), dir.path().join("copied"));
    disk::clone_whole(&top, &cloned, None).unwrap();
    let c = |path: &Path| std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    disk::copy_each(&c(&top), &c(&copied), None).unwrap();
    assert_eq!(kept(&cloned), kept(&copied));
    assert_eq!(kept(&cloned).len(), 10);
    // Beyond copyfile, which gives the folder itself the time of the copy.
    assert_eq!(
        std::fs::metadata(&cloned).unwrap().modified().unwrap(),
        past
    );

    // A cancel before or between items stops it.
    let cancel = std::sync::atomic::AtomicBool::new(true);
    let stopped = disk::clone_whole(&top, &dir.path().join("stopped"), Some(&cancel));
    assert_eq!(stopped.unwrap_err().raw_os_error(), Some(libc::ECANCELED));
    assert!(!dir.path().join("stopped").exists());

    // A file that can't be read fails as a copy always has.
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::write(locked.join("secret"), "s").unwrap();
    std::fs::set_permissions(
        locked.join("secret"),
        std::os::unix::fs::PermissionsExt::from_mode(0o0),
    )
    .unwrap();
    let error = disk::copy(&locked, &dir.path().join("locked copy"), None).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    std::fs::set_permissions(
        locked.join("secret"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
}

/// Takes the unfinished item a failure names back out of the Trash.
pub(super) fn untrash_unfinished(why: &str, into: &Path) {
    let (_, rest) = why
        .split_once("went to the Trash at ")
        .unwrap_or_else(|| panic!("{why}"));
    untrash(Path::new(rest.trim_end_matches('.')), into);
}

/// Sets the modification time of the file or folder at `path`.
pub(super) fn modified(path: &Path, seconds: u64) {
    File::open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
        .unwrap();
}

/// What's in `folder`, by name, as the disk spells them.
pub(super) fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
