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
