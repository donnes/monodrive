use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

/// An engine on a temp state folder, and a temp folder to change.
fn engine() -> (tempfile::TempDir, PathBuf, Engine) {
    let dir = tempfile::tempdir().unwrap();
    let files = dir.path().join("files");
    std::fs::create_dir(&files).unwrap();
    let engine = Engine::open(paths(dir.path())).unwrap();
    (dir, files, engine)
}

fn paths(dir: &Path) -> Paths {
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    Paths {
        state,
        settings: dir.join("settings.json"),
        trash: dir.join("Trash"),
    }
}

fn submission(origin: Origin, request: Request) -> Submission {
    static SENT: AtomicU64 = AtomicU64::new(0);
    Submission {
        id: format!("test-{}", SENT.fetch_add(1, Ordering::Relaxed)),
        origin,
        request,
    }
}

fn run(engine: &mut Engine, request: Request) -> Result<Outcome, String> {
    engine.submit(submission(Origin::Window, request))
}

/// Where each item went, failing the test if any didn't.
fn moved(outcome: Result<Outcome, String>) -> Vec<PathBuf> {
    let outcome = outcome.unwrap();
    outcome
        .items
        .into_iter()
        .map(|item| item.result.unwrap())
        .collect()
}

fn rename(path: &Path, name: &str) -> Request {
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
    let agent = Origin::Agent { client: None };
    let refused = engine
        .submit(submission(agent, empty(listed.clone())))
        .unwrap_err();
    assert!(refused.starts_with("Only a person"), "{refused}");
    let outside = TrashItem {
        path: files.join("kept.txt"),
        id: disk::id(&files.join("kept.txt")).unwrap(),
    };
    assert!(run(&mut engine, empty(vec![outside])).is_err());
    assert_eq!(
        moved(run(&mut engine, empty(listed.clone()))),
        paths(&listed)
    );
    assert_eq!(
        paths(&trash_items(&trash).unwrap()),
        [trash.join("new.txt")]
    );
    assert!(trash.join(".DS_Store").exists() && files.join("kept.txt").exists());
    // It joins no history: undo still reaches the rename before it.
    assert!(run(&mut engine, Request::Undo).is_ok());
    assert!(keep.exists());
}

/// A temp Trash holding `old.txt` and `old folder/x.txt`, counted.
fn counted_trash(dir: &Path) -> (PathBuf, Vec<TrashItem>) {
    let trash = dir.join("Trash");
    std::fs::create_dir_all(trash.join("old folder")).unwrap();
    std::fs::write(trash.join("old folder/x.txt"), "x").unwrap();
    std::fs::write(trash.join("old.txt"), "old").unwrap();
    let listed = trash_items(&trash).unwrap();
    (trash, listed)
}

#[test]
fn empty_trash_leaves_an_item_swapped_in_under_a_counted_name() {
    let (dir, files, mut engine) = engine();
    let (trash, listed) = counted_trash(dir.path());
    // While the person reads the count, old.txt is put back and another
    // file is trashed under its name.
    std::fs::rename(trash.join("old.txt"), files.join("old.txt")).unwrap();
    std::fs::write(trash.join("old.txt"), "newer").unwrap();
    let outcome = run(&mut engine, Request::EmptyTrash { items: listed }).unwrap();
    assert_eq!(read(&trash.join("old.txt")), "newer");
    assert!(!trash.join("old folder").exists());
    let skipped: Vec<&str> = outcome.failures().collect();
    assert!(
        skipped.len() == 1 && skipped[0].contains("old.txt"),
        "{skipped:?}"
    );
}

#[test]
fn empty_trash_refuses_a_trash_that_became_a_link() {
    let (dir, _files, mut engine) = engine();
    let (trash, listed) = counted_trash(dir.path());
    // The same items, but the Trash now points somewhere else.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::rename(&trash, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &trash).unwrap();
    let refused = run(&mut engine, Request::EmptyTrash { items: listed }).unwrap_err();
    assert!(refused.contains("isn't the Trash folder"), "{refused}");
    assert!(elsewhere.join("old.txt").exists() && elsewhere.join("old folder/x.txt").exists());
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
fn transfer_fixture(files: &Path) -> (PathBuf, PathBuf) {
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
fn job(shared: &jobs::Shared, request: Request) -> Job {
    let op = run(&mut shared.lock(), request).unwrap().op;
    jobs::run(shared, op);
    status(shared, op)
}

fn status(shared: &jobs::Shared, op: u64) -> Job {
    let engine = shared.lock();
    let jobs = engine.jobs();
    jobs.into_iter().find(|job| job.id == op).unwrap()
}

/// Continues a stopped job and runs it here to the end, checking it stays
/// the same job.
fn continued(shared: &jobs::Shared, job: u64, conflict: Option<Conflict>) -> Job {
    let count = shared.lock().jobs().len();
    let op = run(&mut shared.lock(), Request::Retry { job, conflict })
        .unwrap()
        .op;
    assert_eq!(op, job);
    jobs::run(shared, op);
    assert_eq!(shared.lock().jobs().len(), count);
    status(shared, op)
}

fn copy(paths: &[&Path], into: &Path, conflict: Conflict) -> Request {
    Request::Copy {
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
        into: into.into(),
        conflict,
    }
}

fn transfer(paths: &[&Path], into: &Path, conflict: Conflict) -> Request {
    Request::Move {
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
        into: into.into(),
        conflict,
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

/// Takes what a test sent to the Trash back out, so the Trash ends as it was.
fn untrash(trashed: &Path, into: &Path) {
    std::fs::rename(trashed, free_name(&into.join("untrashed"))).unwrap();
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
        };
        let outcome = Outcome {
            op,
            title: "Undo".into(),
            items: Vec::new(),
            interrupted: false,
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

fn rename_many(paths: &[&Path], rule: rename::Rule) -> Request {
    Request::RenameMany {
        paths: paths.iter().map(|path| path.to_path_buf()).collect(),
        rule,
        preview: None,
        skip_problems: false,
    }
}

fn numbered(name: &str, digits: usize) -> rename::Rule {
    rename::Rule::Format {
        name: name.into(),
        start: 1,
        digits,
        place: rename::Place::After,
    }
}

fn replacing(find: &str, with: &str, extension: bool) -> rename::Rule {
    rename::Rule::Replace {
        find: find.into(),
        with: with.into(),
        case_sensitive: false,
        extension,
    }
}

/// What's in `folder`, by name, as the disk spells them.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn touch(folder: &Path, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|name| {
            let path = folder.join(name);
            std::fs::write(&path, name).unwrap();
            path
        })
        .collect()
}

#[test]
fn a_batch_rename_numbers_in_the_order_given_keeps_extensions_and_undoes_whole() {
    let (_dir, files, mut engine) = engine();
    let [c, a, b, notes] = touch(&files, &["c.jpg", "a.png", "b.jpg", "txt notes.txt"])
        .try_into()
        .unwrap();
    let folder = files.join("v1.2");
    std::fs::create_dir(&folder).unwrap();
    let batch = [c.as_path(), &a, &folder, &b];
    let plan = rename::Plan::new(&batch.map(Path::to_path_buf), &numbered("Trip", 3));
    let shown: Vec<&str> = plan.rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(
        shown,
        ["Trip 001.jpg", "Trip 002.png", "Trip 003", "Trip 004.jpg"]
    );
    assert!(plan.rows.iter().all(|row| row.status == rename::Status::Ok));
    assert!(c.exists(), "a preview changes nothing");
    let renamed = moved(run(&mut engine, rename_many(&batch, numbered("Trip", 3))));
    assert_eq!(renamed[0], files.join("Trip 001.jpg"));
    assert_eq!(read(&files.join("Trip 002.png")), "a.png");

    let only_stem = replacing("TXT", "md", false);
    assert_eq!(
        moved(run(&mut engine, rename_many(&[&notes], only_stem))),
        [files.join("md notes.txt")]
    );
    let whole = replacing("txt", "md", true);
    let plan = rename::Plan::new(&[files.join("md notes.txt")], &whole);
    assert_eq!(plan.rows[0].name, "md notes.md");

    moved(run(&mut engine, Request::Undo));
    let undone = moved(run(&mut engine, Request::Undo));
    assert_eq!(undone.len(), 4);
    assert_eq!(
        names(&files),
        ["a.png", "b.jpg", "c.jpg", "txt notes.txt", "v1.2"]
    );
}

#[test]
fn a_batch_rename_shows_invalid_names_and_collisions_and_runs_only_when_asked_to_skip() {
    let (_dir, files, mut engine) = engine();
    let paths = touch(
        &files,
        &[
            "x.txt",
            "taken.txt",
            "x-1.txt",
            "x1-.txt",
            "ab-.txt",
            "a-B.txt",
            "k-.txt",
            "k.txt",
        ],
    );
    let plan = rename::Plan::new(&paths, &replacing("-", "", false));
    let status = |name: &str| {
        let row = plan.rows.iter().find(|row| row.path == files.join(name));
        row.unwrap().status.clone()
    };
    let collision = |why: &str| rename::Status::Collision(why.into());
    assert_eq!(status("x.txt"), rename::Status::Unchanged);
    assert_eq!(status("x-1.txt"), rename::Status::Ok);
    let first = files.join("x-1.txt").display().to_string();
    assert_eq!(
        status("x1-.txt"),
        collision(&format!("x1.txt is also the new name of {first}."))
    );
    // aB.txt and ab.txt are one name on a volume that ignores case.
    assert_eq!(status("ab-.txt"), rename::Status::Ok);
    assert!(matches!(status("a-B.txt"), rename::Status::Collision(_)));
    // k.txt is in the batch but keeps its name, so k-.txt can't take it.
    assert_eq!(status("k-.txt"), collision("k.txt already exists."));
    // One é composed and one decomposed: the same name to APFS.
    let accents = files.join("accents");
    std::fs::create_dir(&accents).unwrap();
    let pair = touch(&accents, &["\u{e9}-.txt", "e-\u{301}.txt"]);
    let plan = rename::Plan::new(&pair, &replacing("-", "", false));
    assert_eq!(plan.rows[0].status, rename::Status::Ok);
    assert!(
        matches!(plan.rows[1].status, rename::Status::Collision(_)),
        "{:?}",
        plan.rows[1]
    );
    let outside = rename::Plan::new(&paths[..1], &replacing("x", "taken", false));
    assert_eq!(
        outside.rows[0].status,
        collision("taken.txt already exists.")
    );

    let invalid = |rule: rename::Rule, path: &Path| {
        let plan = rename::Plan::new(&[path.to_path_buf()], &rule);
        match &plan.rows[0].status {
            rename::Status::Invalid(why) => why.clone(),
            other => panic!("{other:?}"),
        }
    };
    let x = files.join("x.txt");
    assert!(invalid(replacing("x", "a/b", false), &x).contains("can't be a name"));
    assert!(invalid(replacing("x.txt", "", true), &x).contains("can't be a name"));
    assert!(invalid(numbered("x", 300), &x).contains("at most 255"));
    assert!(invalid(numbered("x", 1), &files.join("gone")).contains("No such file"));

    let refused = run(
        &mut engine,
        rename_many(&[&paths[2], &paths[3]], replacing("-", "", false)),
    );
    assert!(refused.unwrap_err().starts_with("Nothing was renamed:\n"));
    assert!(files.join("x-1.txt").exists());
    let skipping = Request::RenameMany {
        paths: paths.clone(),
        rule: replacing("-", "", false),
        preview: None,
        skip_problems: true,
    };
    assert_eq!(
        moved(run(&mut engine, skipping)),
        [files.join("x1.txt"), files.join("ab.txt")]
    );
    assert!(files.join("x1-.txt").exists() && files.join("k-.txt").exists());
}

#[test]
fn a_batch_rename_untangles_cycles_and_case_changes_through_hidden_names() {
    let (_dir, files, mut engine) = engine();
    let [one, two] = touch(&files, &["1.txt", "2.txt"]).try_into().unwrap();
    let three = files.join("three");
    std::fs::create_dir(&three).unwrap();
    let [t1, t2, t3] = touch(&three, &["1", "2", "3"]).try_into().unwrap();
    let photo = touch(&files, &["photo.JPG"]).remove(0);
    let swap = || rename_many(&[&two, &one], numbered("", 0));
    let rotate = || rename_many(&[&t3, &t1, &t2], numbered("", 0));
    let case = || rename_many(&[&photo], replacing("jpg", "jpg", true));
    for request in [swap(), rotate(), case()] {
        moved(run(&mut engine, request));
    }
    assert_eq!((read(&one), read(&two)), ("2.txt".into(), "1.txt".into()));
    assert_eq!(
        [&t1, &t2, &t3].map(|path| read(path)),
        ["3", "1", "2"].map(String::from)
    );
    assert_eq!(names(&files), ["1.txt", "2.txt", "photo.jpg", "three"]);
    assert_eq!(names(&three), ["1", "2", "3"]);

    for _ in 0..3 {
        moved(run(&mut engine, Request::Undo));
    }
    assert_eq!((read(&one), read(&two)), ("1.txt".into(), "2.txt".into()));
    assert_eq!(
        [&t1, &t2, &t3].map(|path| read(path)),
        ["1", "2", "3"].map(String::from)
    );
    assert_eq!(names(&files), ["1.txt", "2.txt", "photo.JPG", "three"]);
    moved(run(&mut engine, Request::Redo));
    assert_eq!(read(&one), "2.txt");
    assert_eq!(names(&files), ["1.txt", "2.txt", "photo.JPG", "three"]);
}

#[test]
fn a_batch_rename_refuses_once_its_preview_is_out_of_date() {
    let (_dir, files, mut engine) = engine();
    let paths = touch(&files, &["a.txt", "b.txt"]);
    let rule = numbered("n", 0);
    let previewed = |paths: &[PathBuf], skip_problems: bool| Request::RenameMany {
        paths: paths.to_vec(),
        rule: rule.clone(),
        preview: Some(rename::Plan::new(paths, &rule).fingerprint()),
        skip_problems,
    };
    let edited = previewed(&paths, false);
    std::fs::write(&paths[0], "edited").unwrap();
    let refused = run(&mut engine, edited).unwrap_err();
    assert!(
        refused.contains("a.txt: changed since the preview"),
        "{refused}"
    );

    let collided = previewed(&paths, true);
    std::fs::write(files.join("n 2.txt"), "").unwrap();
    let refused = run(&mut engine, collided).unwrap_err();
    assert!(
        refused.contains("b.txt: n 2.txt already exists."),
        "{refused}"
    );

    let mut reordered = previewed(&paths, true);
    if let Request::RenameMany { paths, .. } = &mut reordered {
        paths.reverse();
    }
    let refused = run(&mut engine, reordered).unwrap_err();
    assert!(refused.contains("aren't the items previewed"), "{refused}");

    let mut renumbered = previewed(&paths[..1], false);
    if let Request::RenameMany { rule, .. } = &mut renumbered {
        *rule = numbered("other", 0);
    }
    let refused = run(&mut engine, renumbered).unwrap_err();
    assert!(
        refused.contains("a.txt: would be named other 1.txt, not n 1.txt"),
        "{refused}"
    );
    assert_eq!(names(&files), ["a.txt", "b.txt", "n 2.txt"]);

    assert_eq!(
        moved(run(&mut engine, previewed(&paths[..1], false))),
        [files.join("n 1.txt")]
    );
}

#[test]
fn a_batch_rename_reports_each_item_and_undoes_only_those_it_renamed() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, files, mut engine) = engine();
    let locked = files.join("locked");
    std::fs::create_dir(&locked).unwrap();
    let free = touch(&files, &["a-1.txt"]).remove(0);
    let stuck = touch(&locked, &["b-1.txt"]).remove(0);
    let mode = |mode| std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(mode));
    mode(0o555).unwrap();
    let outcome = run(
        &mut engine,
        rename_many(&[&free, &stuck], replacing("-1", "-2", false)),
    );
    mode(0o755).unwrap();
    let outcome = outcome.unwrap();
    assert_eq!(outcome.items[0].result, Ok(files.join("a-2.txt")));
    let why = outcome.items[1].result.as_ref().unwrap_err();
    assert!(
        why.contains("b-1.txt") && why.contains("ermission"),
        "{why}"
    );
    assert_eq!(
        moved(run(&mut engine, Request::Undo)),
        [files.join("a-1.txt")]
    );
    assert!(free.exists() && stuck.exists());
}

#[test]
fn a_batch_cut_short_by_a_crash_leaves_no_hidden_names_and_undoes() {
    // How far it got: parked the first of the cycle, then also moved the
    // second into its place.
    for far in [1, 2] {
        let (dir, files, mut engine) = engine();
        let [one, two, c] = touch(&files, &["1.txt", "2.txt", "c.txt"])
            .try_into()
            .unwrap();
        let request = rename_many(&[&two, &one, &c], numbered("", 0));
        let steps = vec![
            renaming(&two, "1.txt"),
            renaming(&one, "2.txt"),
            renaming(&c, "3.txt"),
        ];
        let (order, temps) = route(&steps, &[0, 1, 2], true, engine.next);
        assert_eq!(order, [1, 0, 2]);
        let [temp] = temps.try_into().unwrap();
        let submitted = submission(Origin::Window, request);
        engine
            .begin(&submitted, None, steps, Vec::new(), vec![temp.clone()])
            .unwrap();
        disk::rename_new(&two, &temp.path).unwrap();
        if far == 2 {
            disk::rename_new(&one, &two).unwrap();
        }
        drop(engine);

        let mut engine = Engine::open(paths(dir.path())).unwrap();
        let settled = engine.submit(submitted).unwrap();
        assert!(settled.interrupted);
        assert_eq!(names(&files), ["1.txt", "2.txt", "c.txt"]);
        assert!(settled.items[2].result.is_err());
        if far == 1 {
            assert_eq!(read(&two), "2.txt");
            assert!(run(&mut engine, Request::Undo).is_err());
        } else {
            assert_eq!(settled.items[0].result, Ok(one.clone()));
            assert_eq!(settled.items[1].result, Ok(two.clone()));
            assert_eq!((read(&one), read(&two)), ("2.txt".into(), "1.txt".into()));
            moved(run(&mut engine, Request::Undo));
            assert_eq!((read(&one), read(&two)), ("1.txt".into(), "2.txt".into()));
            assert_eq!(names(&files), ["1.txt", "2.txt", "c.txt"]);
        }
    }
}

/// A batch's step renaming `from` to `to` beside it, not yet made.
fn renaming(from: &Path, to: &str) -> Step {
    let (id, stamp) = disk::stamp(from).unwrap();
    Step {
        id,
        undone: Spot::Path(from.to_path_buf()),
        done: Spot::Path(from.with_file_name(to)),
        applied: false,
        stamp,
    }
}

#[test]
fn a_crash_partway_round_a_three_item_cycle_puts_every_item_back() {
    // Without and with something else taking 2's old name meanwhile.
    for outsider in [false, true] {
        let (dir, files, mut engine) = engine();
        let [one, two, three] = touch(&files, &["1", "2", "3"]).try_into().unwrap();
        // 3 to 1, 1 to 2, 2 to 3.
        let request = rename_many(&[&three, &one, &two], numbered("", 0));
        let steps = vec![
            renaming(&three, "1"),
            renaming(&one, "2"),
            renaming(&two, "3"),
        ];
        let (order, temps) = route(&steps, &[0, 1, 2], true, engine.next);
        assert_eq!(order, [2, 1, 0]);
        let [temp] = temps.try_into().unwrap();
        let submitted = submission(Origin::Window, request);
        engine
            .begin(&submitted, None, steps, Vec::new(), vec![temp.clone()])
            .unwrap();
        // Parked 3, then moved 2 into its place: 3's old and new names are
        // both taken now.
        disk::rename_new(&three, &temp.path).unwrap();
        disk::rename_new(&two, &three).unwrap();
        if outsider {
            std::fs::write(&two, "outsider").unwrap();
        }
        drop(engine);

        let mut engine = Engine::open(paths(dir.path())).unwrap();
        let settled = engine.submit(submitted).unwrap();
        assert!(settled.interrupted);
        if outsider {
            // It stays parked, but undo knows where, and finishes once 2 is
            // free again.
            let why = settled.items[0].result.as_ref().unwrap_err();
            assert!(why.contains("undo puts it back"), "{why}");
            assert!(run(&mut engine, Request::Undo).is_err());
            std::fs::rename(&two, dir.path().join("outsider")).unwrap();
            moved(run(&mut engine, Request::Undo));
        }
        assert_eq!(names(&files), ["1", "2", "3"]);
        assert_eq!([&one, &two, &three].map(|path| read(path)), ["1", "2", "3"]);
        assert_eq!(
            run(&mut engine, Request::Undo).unwrap_err(),
            "Nothing to undo."
        );
    }
}

/// Makes `path` immutable, so nothing can rename it, or lets it be again.
fn immutable(path: &Path, on: bool) {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let flags = if on { libc::UF_IMMUTABLE } else { 0 };
    // SAFETY: a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::chflags(path.as_ptr(), flags) }, 0);
}

#[test]
fn a_failure_partway_round_a_cycle_puts_its_items_back() {
    let (_dir, files, mut engine) = engine();
    let [one, two, three] = touch(&files, &["1", "2", "3"]).try_into().unwrap();
    // 3 parks and 2 takes its name, then 1 can't move, so 3 can't go on.
    immutable(&one, true);
    let outcome = run(
        &mut engine,
        rename_many(&[&three, &one, &two], numbered("", 0)),
    );
    immutable(&one, false);
    let outcome = outcome.unwrap();
    assert!(
        outcome.items.iter().all(|item| item.result.is_err()),
        "{outcome:?}"
    );
    assert_eq!(names(&files), ["1", "2", "3"]);
    assert_eq!([&one, &two, &three].map(|path| read(path)), ["1", "2", "3"]);
    assert_eq!(
        run(&mut engine, Request::Undo).unwrap_err(),
        "Nothing to undo."
    );
}

fn compress(paths: &[&Path]) -> Request {
    Request::Compress {
        paths: paths.iter().map(|path| path.to_path_buf()).collect(),
    }
}

fn extract(archive: &Path, entries: &[&str], into: &Path, conflict: Conflict) -> Request {
    Request::Extract {
        archive: archive.into(),
        entries: entries.iter().map(|entry| entry.to_string()).collect(),
        into: into.into(),
        conflict,
    }
}

/// Each item under `root` in name order: its path inside, what it is, its
/// permissions and modification time to the second, and what it holds or
/// points to.
fn tree(root: &Path) -> Vec<String> {
    use std::os::unix::fs::MetadataExt;
    let mut lines = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let inside = path.strip_prefix(root).unwrap().display().to_string();
        let mode = meta.mode() & 0o777;
        lines.push(if meta.is_symlink() {
            format!(
                "{inside} -> {}",
                std::fs::read_link(&path).unwrap().display()
            )
        } else if meta.is_dir() {
            let mut items: Vec<PathBuf> = std::fs::read_dir(&path)
                .unwrap()
                .map(|item| item.unwrap().path())
                .collect();
            items.sort_by(|a, b| b.cmp(a));
            pending.extend(items);
            format!("{inside}/ {mode:o} {}", meta.mtime())
        } else {
            format!("{inside} {mode:o} {} {}", meta.mtime(), read(&path))
        });
    }
    lines
}

/// Sets the modification time of the file or folder at `path`.
fn modified(path: &Path, seconds: u64) {
    File::open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
        .unwrap();
}

/// Takes the unfinished item a failure names back out of the Trash.
fn untrash_unfinished(why: &str, into: &Path) {
    let (_, rest) = why
        .split_once("went to the Trash at ")
        .unwrap_or_else(|| panic!("{why}"));
    untrash(Path::new(rest.trim_end_matches('.')), into);
}

#[test]
fn a_compressed_folder_extracts_as_it_was_and_undo_trashes_each_archive_and_extraction() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (src, out) = (files.join("src"), files.join("out"));
    let proj = src.join("proj");
    std::fs::create_dir_all(proj.join("sub")).unwrap();
    std::fs::create_dir_all(proj.join("empty")).unwrap();
    std::fs::create_dir(&out).unwrap();
    for (name, text, mode) in [
        ("a.txt", "alpha", 0o644),
        ("run.sh", "#!/bin/sh", 0o755),
        ("secret", "shh", 0o600),
        ("sub/deep.txt", "deep", 0o640),
    ] {
        let path = proj.join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        modified(&path, 1_000_000_000);
    }
    std::os::unix::fs::symlink("a.txt", proj.join("link")).unwrap();
    std::fs::set_permissions(proj.join("sub"), std::fs::Permissions::from_mode(0o750)).unwrap();
    for (folder, seconds) in [
        ("sub", 1_100_000_000),
        ("empty", 1_200_000_000),
        ("", 1_300_000_000),
    ] {
        modified(&proj.join(folder), seconds);
    }
    std::fs::write(src.join("notes.md"), "notes").unwrap();

    let zipped = job(&shared, compress(&[&proj]));
    assert_eq!(zipped.state, State::Completed, "{zipped:?}");
    let archive = src.join("proj.zip");
    assert_eq!(zipped.tasks[0].done, Some(Done::To(archive.clone())));
    let both = job(&shared, compress(&[&proj, &src.join("notes.md")]));
    assert_eq!(both.tasks[0].done, Some(Done::To(src.join("Archive.zip"))));
    let again = job(&shared, compress(&[&proj, &src.join("notes.md")]));
    assert_eq!(
        again.tasks[0].done,
        Some(Done::To(src.join("Archive 2.zip")))
    );
    let listed: Vec<String> = crate::archives::Archive::open(&archive)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(
        listed,
        [
            "proj/",
            "proj/a.txt",
            "proj/empty/",
            "proj/link",
            "proj/run.sh",
            "proj/secret",
            "proj/sub/",
            "proj/sub/deep.txt"
        ]
    );

    let unpacked = job(&shared, extract(&archive, &[], &out, Conflict::Stop));
    assert_eq!(unpacked.state, State::Completed, "{unpacked:?}");
    assert_eq!(unpacked.tasks[0].done, Some(Done::To(out.join("proj"))));
    assert_eq!(tree(&out.join("proj")), tree(&proj));
    // An entry inside comes out on its own.
    let one = job(
        &shared,
        extract(&archive, &["proj/sub/"], &out, Conflict::Stop),
    );
    assert_eq!(one.tasks[0].done, Some(Done::To(out.join("sub"))));
    assert_eq!(tree(&out.join("sub")), tree(&proj.join("sub")));
    assert!(!out.join("sub/../a.txt").exists());

    for made in [out.join("sub"), out.join("proj")] {
        let undone = run(&mut shared.lock(), Request::Undo).unwrap();
        assert!(!made.exists());
        untrash(&undone.items[0].result.clone().unwrap(), &files);
    }
    for _ in 0..3 {
        let undone = run(&mut shared.lock(), Request::Undo).unwrap();
        untrash(&undone.items[0].result.clone().unwrap(), &files);
    }
    assert!(!archive.exists() && !src.join("Archive.zip").exists());
    assert_eq!(names(&src), ["notes.md", "proj"]);
    assert_eq!(names(&out), Vec::<String>::new());
}

#[test]
fn extracting_follows_each_conflict_choice_and_never_replaces_the_archive() {
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (archive, dest) = (files.join("a.zip"), files.join("dest"));
    crate::archives::tests::zip(
        &archive,
        &[
            ("proj/", crate::archives::tests::Item::Dir),
            ("proj/new.txt", crate::archives::tests::Item::File(b"new")),
            ("a.zip", crate::archives::tests::Item::File(b"inner")),
        ],
    );
    std::fs::create_dir_all(dest.join("proj")).unwrap();
    std::fs::write(dest.join("proj/old.txt"), "old").unwrap();

    let waiting = job(&shared, extract(&archive, &["proj"], &dest, Conflict::Stop));
    assert_eq!(waiting.state, State::Waiting);
    let skipped = continued(&shared, waiting.id, Some(Conflict::Skip));
    assert!(matches!(skipped.tasks[0].done, Some(Done::Skipped(_))));
    let both = job(
        &shared,
        extract(&archive, &["proj"], &dest, Conflict::KeepBoth),
    );
    assert_eq!(both.tasks[0].done, Some(Done::To(dest.join("proj 2"))));
    assert_eq!(read(&dest.join("proj 2/new.txt")), "new");
    let replaced = job(
        &shared,
        extract(&archive, &["proj"], &dest, Conflict::Replace),
    );
    assert_eq!(replaced.state, State::Completed, "{replaced:?}");
    assert_eq!(names(&dest.join("proj")), ["new.txt"]);
    // Undo trashes what replaced it and puts the old one back.
    let undone = run(&mut shared.lock(), Request::Undo).unwrap();
    assert_eq!(read(&dest.join("proj/old.txt")), "old");
    untrash(&undone.items[0].result.clone().unwrap(), &files);
    // An item named as the archive never replaces it.
    let refused = job(
        &shared,
        extract(&archive, &["a.zip"], &files, Conflict::Replace),
    );
    assert!(
        matches!(&refused.tasks[0].done, Some(Done::Failed(why)) if why.contains("can't be replaced")),
        "{refused:?}"
    );
    assert!(crate::archives::Archive::open(&archive).is_ok());
}

#[test]
fn extracting_refuses_paths_out_of_the_destination_and_never_writes_through_a_link() {
    use crate::archives::tests::{Item, zip};
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let dest = files.join("dest");
    std::fs::create_dir(&dest).unwrap();
    for (name, why) in [("../up.txt", "climbs out"), ("/abs.txt", "absolute")] {
        let archive = files.join("bad.zip");
        zip(
            &archive,
            &[("ok.txt", Item::File(b"ok")), (name, Item::File(b"bad"))],
        );
        let refused = run(
            &mut shared.lock(),
            extract(&archive, &[], &dest, Conflict::Stop),
        )
        .unwrap_err();
        assert!(refused.contains(why), "{refused}");
        // Leaving that entry out, the rest extracts.
        let ok = job(
            &shared,
            extract(&archive, &["ok.txt"], &dest, Conflict::Replace),
        );
        assert_eq!(ok.state, State::Completed, "{ok:?}");
        let undone = run(&mut shared.lock(), Request::Undo).unwrap();
        untrash(&undone.items[0].result.clone().unwrap(), &files);
    }
    assert!(!files.join("up.txt").exists());

    // A link already in the destination leads outside; the archive makes
    // a link to it, then writes through that.
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("x")).unwrap();
    let through = files.join("through.zip");
    zip(
        &through,
        &[
            ("item/sub/l", Item::Link("../../x")),
            ("item/sub/l/evil.txt", Item::File(b"evil")),
        ],
    );
    let failed = job(&shared, extract(&through, &[], &dest, Conflict::Stop));
    let Some(Done::Failed(why)) = &failed.tasks[0].done else {
        panic!("{failed:?}")
    };
    assert!(why.contains("item/sub/l is a link that leads out"), "{why}");
    untrash_unfinished(why, &files);
    assert_eq!(names(&outside), Vec::<String>::new());
    assert_eq!(names(&dest), ["x"]);
    // A link that stays inside is made, but nothing is written through it.
    let inside = files.join("inside.zip");
    zip(
        &inside,
        &[
            ("item/sub/", Item::Dir),
            ("item/l", Item::Link("sub")),
            ("item/l/evil.txt", Item::File(b"evil")),
        ],
    );
    let failed = job(&shared, extract(&inside, &[], &dest, Conflict::Stop));
    let Some(Done::Failed(why)) = &failed.tasks[0].done else {
        panic!("{failed:?}")
    };
    assert!(why.contains("item/l/evil.txt goes through a link"), "{why}");
    untrash_unfinished(why, &files);
    assert_eq!(names(&dest), ["x"]);

    let away = files.join("away.zip");
    zip(&away, &[("item/etc", Item::Link("/etc"))]);
    let failed = job(&shared, extract(&away, &[], &dest, Conflict::Stop));
    let Some(Done::Failed(why)) = &failed.tasks[0].done else {
        panic!("{failed:?}")
    };
    assert!(why.contains("item/etc is a link that leads out"), "{why}");
    untrash_unfinished(why, &files);
    assert_eq!(names(&dest), ["x"]);
}

#[test]
fn extracting_refuses_zip_bombs_entries_it_cant_read_and_more_than_fits() {
    use crate::archives::tests::{Item, patch, set, zip};
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let dest = files.join("dest");
    std::fs::create_dir(&dest).unwrap();
    let refused = |archive: &Path, entries: &[&str]| {
        run(
            &mut shared.lock(),
            extract(archive, entries, &dest, Conflict::Stop),
        )
        .unwrap_err()
    };

    let bomb = files.join("bomb.zip");
    zip(&bomb, &[("zeros", Item::File(&[0; 100]))]);
    patch(&bomb, "zeros", |fixed, _| set(fixed, 24, 50_000_000, 4));
    assert!(refused(&bomb, &[]).contains("as zip bombs do"));

    // An entry that holds more than it says stops once it's written that.
    let liar = files.join("liar.zip");
    zip(&liar, &[("text", Item::File(b"hello hello hello hello"))]);
    patch(&liar, "text", |fixed, _| set(fixed, 24, 5, 4));
    let failed = job(&shared, extract(&liar, &[], &dest, Conflict::Stop));
    let Some(Done::Failed(why)) = &failed.tasks[0].done else {
        panic!("{failed:?}")
    };
    assert!(
        why.contains("text holds more than the archive says"),
        "{why}"
    );
    untrash_unfinished(why, &files);

    let huge = files.join("huge.zip");
    let mut writer = zip::ZipWriter::new(File::create(&huge).unwrap());
    let options = zip::write::SimpleFileOptions::default().large_file(true);
    writer.start_file("huge", options).unwrap();
    writer.write_all(b"small").unwrap();
    writer.finish().unwrap();
    patch(&huge, "huge", |_, extra| {
        assert_eq!(&extra[..2], &[1, 0], "the ZIP64 field comes first");
        set(extra, 4, 1 << 60, 8);
    });
    let why = refused(&huge, &[]);
    assert!(
        why.contains("more than the") && why.contains("free there"),
        "{why}"
    );

    let locked = files.join("locked.zip");
    zip(
        &locked,
        &[
            ("ok.txt", Item::File(b"ok")),
            ("secret.txt", Item::File(b"secret")),
            ("packed.txt", Item::File(b"packed")),
        ],
    );
    patch(&locked, "secret.txt", |fixed, _| fixed[8] |= 1);
    patch(&locked, "packed.txt", |fixed, _| set(fixed, 10, 14, 2));
    let why = refused(&locked, &[]);
    assert!(why.starts_with("Nothing was extracted from"), "{why}");
    assert!(why.contains("secret.txt is encrypted"), "{why}");
    assert!(why.contains("packed.txt is compressed with LZMA"), "{why}");
    assert!(refused(&locked, &["secret.txt"]).contains("encrypted"));
    assert!(refused(&locked, &["missing"]).contains("has no entry missing"));
    let ok = job(
        &shared,
        extract(&locked, &["ok.txt"], &dest, Conflict::Stop),
    );
    assert_eq!(ok.tasks[0].done, Some(Done::To(dest.join("ok.txt"))));
    assert_eq!(names(&dest), ["ok.txt"]);
}

#[test]
fn cancelling_an_extraction_keeps_the_items_it_finished() {
    use crate::archives::tests::{Item, zip};
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (archive, dest) = (files.join("a.zip"), files.join("dest"));
    std::fs::create_dir(&dest).unwrap();
    zip(
        &archive,
        &[("a.txt", Item::File(b"a")), ("b.txt", Item::File(b"b"))],
    );
    let op = run(
        &mut shared.lock(),
        extract(&archive, &[], &dest, Conflict::Stop),
    )
    .unwrap()
    .op;
    assert!(jobs::next(&shared, op));
    run(&mut shared.lock(), Request::Cancel { job: op }).unwrap();
    assert!(!jobs::next(&shared, op));
    let cancelled = status(&shared, op);
    assert_eq!(cancelled.state, State::Cancelled);
    assert_eq!(cancelled.tasks[0].done, Some(Done::To(dest.join("a.txt"))));
    assert_eq!(cancelled.tasks[1].done, None);
    assert_eq!(names(&dest), ["a.txt"]);
}

#[test]
fn an_extraction_cut_short_by_a_crash_trashes_what_it_left_unfinished() {
    use crate::archives::tests::{Item, zip};
    let (dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let (archive, dest) = (files.join("a.zip"), files.join("dest"));
    std::fs::create_dir(&dest).unwrap();
    zip(
        &archive,
        &[
            ("proj/a.txt", Item::File(b"a")),
            ("proj/b.txt", Item::File(b"b")),
        ],
    );
    let op = run(
        &mut shared.lock(),
        extract(&archive, &[], &dest, Conflict::Stop),
    )
    .unwrap()
    .op;
    // The owner records the hidden folder, writes one entry into it, and
    // dies.
    let temp = dest.join(format!(".monodrive-{op}-0"));
    let mut work = shared.lock().job_mut(op).unwrap().work[0].clone();
    work.to = Some(dest.join("proj"));
    work.temp = Some(temp.clone());
    shared
        .lock()
        .write(&Record::Work { op, index: 0, work })
        .unwrap();
    std::fs::create_dir(&temp).unwrap();
    std::fs::write(temp.join("a.txt"), "a").unwrap();
    drop(shared);

    let shared = jobs::Shared::new(Engine::open(paths(dir.path())).unwrap(), None);
    let settled = status(&shared, op);
    assert_eq!(settled.state, State::Interrupted);
    let Some(Done::Failed(why)) = &settled.tasks[0].done else {
        panic!("{settled:?}")
    };
    assert!(
        why.contains("The unfinished extraction went to the Trash"),
        "{why}"
    );
    untrash_unfinished(why, &files);
    assert_eq!(names(&dest), Vec::<String>::new());
    // Continued, it extracts again from the start.
    let done = continued(&shared, op, None);
    assert_eq!(done.tasks[0].done, Some(Done::To(dest.join("proj"))));
    assert_eq!(names(&dest.join("proj")), ["a.txt", "b.txt"]);
}

#[test]
fn replace_never_trashes_a_folder_holding_the_source_through_an_alias_or_a_change_of_case() {
    use crate::archives::tests::{Item, zip};
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let real = files.join("real");
    std::fs::create_dir_all(real.join("sub")).unwrap();
    std::fs::create_dir_all(real.join("a")).unwrap();
    let archive = real.join("sub/input.zip");
    zip(
        &archive,
        &[("sub/", Item::Dir), ("sub/x.txt", Item::File(b"x"))],
    );
    let source = real.join("a/a");
    std::fs::write(&source, "a").unwrap();
    std::os::unix::fs::symlink(&real, files.join("alias")).unwrap();
    // An alias of real, and real spelled in another case on a volume that
    // ignores case: sub there holds the archive, and a holds a/a.
    for into in [files.join("alias"), files.join("REAL")] {
        for request in [
            extract(&archive, &["sub"], &into, Conflict::Replace),
            copy(&[&source], &into, Conflict::Replace),
            transfer(&[&source], &into, Conflict::Replace),
        ] {
            let refused = job(&shared, request);
            assert!(
                matches!(&refused.tasks[0].done, Some(Done::Failed(why)) if why.contains("can't be replaced")),
                "{refused:?}"
            );
        }
        assert_eq!(read(&source), "a");
        assert!(crate::archives::Archive::open(&archive).is_ok());
    }
    // The archive named through a link of its own: its real folder is still
    // spared.
    let link = files.join("alias.zip");
    std::os::unix::fs::symlink(&archive, &link).unwrap();
    let refused = job(&shared, extract(&link, &["sub"], &real, Conflict::Replace));
    assert!(
        matches!(&refused.tasks[0].done, Some(Done::Failed(why)) if why.contains("can't be replaced")),
        "{refused:?}"
    );
    assert!(crate::archives::Archive::open(&archive).is_ok());
}

#[test]
fn an_extracted_link_never_climbs_so_no_chain_of_links_leads_out() {
    use crate::archives::tests::{Item, zip};
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let dest = files.join("dest");
    std::fs::create_dir(&dest).unwrap();
    for (links, refused) in [
        // root/b goes through root/a, which leads to dest.
        (&[("root/a", ".."), ("root/b", "a/..")][..], "root/a"),
        (
            &[("root/sub/a", ".."), ("root/sub/b", "a/../..")][..],
            "root/sub/a",
        ),
        (
            &[("root/sub/A", ".."), ("root/sub/b", "a/../..")][..],
            "root/sub/A",
        ),
        // σ and ς are one name where case is ignored, which lowercasing
        // can't tell: root/l climbs through root/sub/σ.
        (
            &[("root/sub/σ", ".."), ("root/l", "sub/ς/../..")][..],
            "root/sub/σ",
        ),
        (
            &[("root/l", "sub/ς/../.."), ("root/sub/σ", "..")][..],
            "root/l",
        ),
        (
            &[("root/c", "sub/a/x"), ("root/sub/a", "..")][..],
            "root/sub/a",
        ),
        // Stays inside as written, but climbs all the same.
        (&[("root/x", "sub/../y")][..], "root/x"),
        // Beside dest's other items, where the archive doesn't decide what's there.
        (&[("top", "x")][..], "top"),
    ] {
        let archive = files.join("links.zip");
        let items: Vec<(&str, Item)> = links
            .iter()
            .map(|(name, to)| (*name, Item::Link(to)))
            .collect();
        zip(&archive, &items);
        let failed = job(&shared, extract(&archive, &[], &dest, Conflict::Stop));
        let Some(Done::Failed(why)) = &failed.tasks[0].done else {
            panic!("{links:?}: {failed:?}")
        };
        assert!(
            why.contains(&format!("{refused} is a link that leads out")),
            "{why}"
        );
        if why.contains("went to the Trash") {
            untrash_unfinished(why, &files);
        }
        assert_eq!(names(&dest), Vec::<String>::new());
    }
    // Pointing down into the link's own folder is fine, through another
    // link too.
    let archive = files.join("fine.zip");
    zip(
        &archive,
        &[
            ("root/sub/f.txt", Item::File(b"f")),
            ("root/l", Item::Link("sub/f.txt")),
            ("root/m", Item::Link("./l")),
        ],
    );
    let done = job(&shared, extract(&archive, &[], &dest, Conflict::Stop));
    assert_eq!(done.state, State::Completed, "{done:?}");
    assert_eq!(read(&dest.join("root/m")), "f");
}

#[test]
fn entries_that_are_one_path_once_extracted_refuse_the_item_before_anything_is_written() {
    use crate::archives::tests::{Item, zip};
    let (_dir, files, engine) = engine();
    let shared = jobs::Shared::new(engine, None);
    let dest = files.join("dest");
    std::fs::create_dir(&dest).unwrap();
    for (entries, named) in [
        (&[("a", "one"), ("./a", "two")][..], "a and ./a"),
        (
            &[("d/B.txt", "one"), ("d//b.txt", "two")][..],
            "d/B.txt and d//b.txt",
        ),
        // One name to a volume that ignores case: lowercasing misses σ and ς.
        (
            &[("d/σ.txt", "one"), ("d/ς.txt", "two")][..],
            "d/σ.txt and d/ς.txt",
        ),
        (
            &[("d/\u{c4}.txt", "one"), ("d/a\u{308}.txt", "two")][..],
            "d/\u{c4}.txt and d/a\u{308}.txt",
        ),
        (
            &[("d/\u{212a}.txt", "one"), ("d/k.txt", "two")][..],
            "d/\u{212a}.txt and d/k.txt",
        ),
        (
            &[("d/\u{1e9e}.txt", "one"), ("d/\u{df}.txt", "two")][..],
            "d/\u{1e9e}.txt and d/\u{df}.txt",
        ),
    ] {
        let archive = files.join("twice.zip");
        let items: Vec<(&str, Item)> = entries
            .iter()
            .map(|(name, text)| (*name, Item::File(text.as_bytes())))
            .collect();
        zip(&archive, &items);
        let failed = job(&shared, extract(&archive, &[], &dest, Conflict::Stop));
        let Some(Done::Failed(why)) = &failed.tasks[0].done else {
            panic!("{entries:?}: {failed:?}")
        };
        if why.contains("went to the Trash") {
            untrash_unfinished(why, &files);
        }
        assert!(why.contains(named), "{why}");
        assert!(!why.contains("Trash"), "nothing was written: {why}");
        assert_eq!(names(&dest), Vec::<String>::new());
    }
}
