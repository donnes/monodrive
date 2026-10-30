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
            .begin(submission, None, vec![step], Vec::new())
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
    assert_eq!(listed, [trash.join("old folder"), trash.join("old.txt")]);
    // Something that arrives after the count isn't erased with it.
    std::fs::write(trash.join("new.txt"), "new").unwrap();
    let empty = |paths: Vec<PathBuf>| Request::EmptyTrash { paths };
    let agent = Origin::Agent { client: None };
    let refused = engine
        .submit(submission(agent, empty(listed.clone())))
        .unwrap_err();
    assert!(refused.starts_with("Only a person"), "{refused}");
    assert!(run(&mut engine, empty(vec![files.join("kept.txt")])).is_err());
    assert_eq!(moved(run(&mut engine, empty(listed.clone()))), listed);
    assert_eq!(trash_items(&trash).unwrap(), [trash.join("new.txt")]);
    assert!(trash.join(".DS_Store").exists() && files.join("kept.txt").exists());
    // It joins no history: undo still reaches the rename before it.
    assert!(run(&mut engine, Request::Undo).is_ok());
    assert!(keep.exists());
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
    std::fs::rename(trashed, into.join("untrashed")).unwrap();
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
    let retried = run(
        &mut shared.lock(),
        Request::Retry {
            job: stopped.id,
            conflict: Some(Conflict::Skip),
        },
    )
    .unwrap()
    .op;
    jobs::run(&shared, retried);
    let skipped = status(&shared, retried);
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
fn an_interrupted_job_is_settled_from_the_disk_and_retry_skips_what_finished() {
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

    let retried = run(
        &mut shared.lock(),
        Request::Retry {
            job: op,
            conflict: None,
        },
    )
    .unwrap()
    .op;
    jobs::run(&shared, retried);
    let retry = status(&shared, retried);
    assert_eq!(retry.tasks.len(), 1);
    assert_eq!(retry.tasks[0].done, Some(Done::To(dest.join("a.txt"))));
    // Undo takes back the retry, then all the interrupted job changed.
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
