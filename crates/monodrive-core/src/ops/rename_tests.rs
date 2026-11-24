use super::tests::*;
use super::*;

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

fn prepared_edit(folder: &Path, text: &str) -> PathBuf {
    let prepared = crate::edits::prepared_path(folder, "txt");
    std::fs::write(&prepared, text).unwrap();
    prepared
}

#[test]
fn a_saved_edit_numbers_around_a_taken_name_and_undoes_to_the_trash() {
    let (_dir, files, mut engine) = engine();
    let [photo, taken] = touch(&files, &["photo.txt", "photo edited.txt"])
        .try_into()
        .unwrap();
    let source = Opened::read(&photo).unwrap();
    let save = |prepared: PathBuf, save: SaveTo| Request::SaveEdit {
        source: source.clone(),
        prepared,
        save,
    };
    let made = moved(run(
        &mut engine,
        save(prepared_edit(&files, "new"), SaveTo::Copy),
    ));
    let numbered = files.join("photo edited 2.txt");
    assert_eq!(made, std::slice::from_ref(&numbered));
    assert_eq!(read(&numbered), "new");
    assert_eq!(
        read(&taken),
        "photo edited.txt",
        "a taken name stays as it was"
    );
    assert_eq!(read(&photo), "photo.txt");
    moved(run(&mut engine, Request::Undo));
    assert!(!numbered.exists());
    // Nothing's left beside the file, and Redo finds it in the Trash.
    assert_eq!(names(&files), ["photo edited.txt", "photo.txt"]);
    moved(run(&mut engine, Request::Redo));
    assert_eq!(read(&numbered), "new");
    moved(run(&mut engine, Request::Undo));

    // Save As only takes a free name.
    let refused = run(
        &mut engine,
        save(
            prepared_edit(&files, "x"),
            SaveTo::As {
                path: taken.clone(),
            },
        ),
    );
    assert!(refused.is_err());
    assert_eq!(read(&taken), "photo edited.txt");
}

#[test]
fn replacing_keeps_the_original_in_the_trash_and_undo_restores_that_same_file() {
    let (_dir, files, mut engine) = engine();
    let [photo] = touch(&files, &["photo.txt"]).try_into().unwrap();
    let inode = disk::id(&photo).unwrap();
    let request = Request::SaveEdit {
        source: Opened::read(&photo).unwrap(),
        prepared: prepared_edit(&files, "edited"),
        save: SaveTo::Replace,
    };
    moved(run(&mut engine, request));
    assert_eq!(read(&photo), "edited");
    assert_ne!(disk::id(&photo), Some(inode));
    moved(run(&mut engine, Request::Undo));
    assert_eq!(read(&photo), "photo.txt");
    assert_eq!(disk::id(&photo), Some(inode), "the same file, not a copy");
    moved(run(&mut engine, Request::Redo));
    assert_eq!(read(&photo), "edited");
}

#[test]
fn a_replace_cut_short_by_a_crash_can_still_be_undone() {
    let (dir, files, mut engine) = engine();
    let [photo] = touch(&files, &["photo.txt"]).try_into().unwrap();
    let source = Opened::read(&photo).unwrap();
    let prepared = prepared_edit(&files, "edited");
    let steps: Vec<Step> = plan_save_edit(&source, &prepared, &SaveTo::Replace)
        .into_iter()
        .map(|(_, plan)| match plan.unwrap() {
            Plan::Move(step) => step,
            _ => panic!("every part of a replace is a move"),
        })
        .collect();
    let submitted = submission(
        Origin::Window,
        Request::SaveEdit {
            source,
            prepared: prepared.clone(),
            save: SaveTo::Replace,
        },
    );
    engine
        .begin(&submitted, None, steps, Vec::new(), Vec::new())
        .unwrap();
    // Both happened, and the owner died before recording the end.
    disk::trash(&photo).unwrap();
    disk::rename_new(&prepared, &photo).unwrap();
    drop(engine);

    let mut engine = Engine::open(paths(dir.path())).unwrap();
    assert!(engine.submit(submitted).unwrap().interrupted);
    assert_eq!(read(&photo), "edited");
    moved(run(&mut engine, Request::Undo));
    assert_eq!(read(&photo), "photo.txt");
}

#[test]
fn a_replace_that_would_change_the_format_is_refused() {
    let (_dir, files, mut engine) = engine();
    let [photo] = touch(&files, &["photo.webp"]).try_into().unwrap();
    let prepared = crate::edits::prepared_path(&files, "png");
    std::fs::write(&prepared, "x").unwrap();
    let refused = run(
        &mut engine,
        Request::SaveEdit {
            source: Opened::read(&photo).unwrap(),
            prepared: prepared.clone(),
            save: SaveTo::Replace,
        },
    );
    assert!(refused.unwrap_err().contains("own format"));
    assert_eq!(read(&photo), "photo.webp");
    // A copy takes the format it was written in.
    let made = moved(run(
        &mut engine,
        Request::SaveEdit {
            source: Opened::read(&photo).unwrap(),
            prepared,
            save: SaveTo::Copy,
        },
    ));
    assert_eq!(made, [files.join("photo edited.png")]);
}

#[test]
fn a_file_changed_since_it_opened_refuses_the_save_and_only_prepared_files_move() {
    let (_dir, files, mut engine) = engine();
    let [photo, other] = touch(&files, &["photo.txt", "other.txt"])
        .try_into()
        .unwrap();
    let source = Opened::read(&photo).unwrap();
    std::fs::write(&photo, "changed underneath").unwrap();
    let prepared = prepared_edit(&files, "edited");
    let refused = run(
        &mut engine,
        Request::SaveEdit {
            source,
            prepared: prepared.clone(),
            save: SaveTo::Replace,
        },
    );
    assert!(refused.unwrap_err().contains("changed since"));
    assert_eq!(read(&photo), "changed underneath");
    // A file that isn't a prepared edit can't be moved this way.
    let refused = run(
        &mut engine,
        Request::SaveEdit {
            source: Opened::read(&photo).unwrap(),
            prepared: other.clone(),
            save: SaveTo::Copy,
        },
    );
    assert!(refused.unwrap_err().contains("isn't a prepared edit"));
    assert!(other.exists());
}

#[test]
fn organize_moves_as_one_undo_never_overwrites_and_refuses_a_stale_plan() {
    let (_dir, files, mut engine) = engine();
    let folder = files.canonicalize().unwrap();
    touch(&folder, &["a.png", "b.png", "c.pdf"]);
    std::fs::create_dir(folder.join("Documents")).unwrap();
    let plan = organize::propose(&folder).unwrap();
    let groups = plan.groups.clone();

    // A file changed since the review refuses it all.
    std::fs::write(folder.join("b.png"), "changed").unwrap();
    let refused = run(
        &mut engine,
        Request::Organize {
            groups: groups.clone(),
        },
    );
    assert!(refused.unwrap_err().contains("changed since"));
    assert!(folder.join("a.png").exists() && !folder.join("Images").exists());

    let groups = organize::propose(&folder).unwrap().groups;
    // A name taken at the destination refuses it all too.
    std::fs::write(folder.join("Documents/c.pdf"), "other").unwrap();
    let taken = run(
        &mut engine,
        Request::Organize {
            groups: groups.clone(),
        },
    );
    assert!(taken.is_err());
    assert!(folder.join("c.pdf").exists());
    assert_eq!(read(&folder.join("Documents/c.pdf")), "other");
    std::fs::rename(
        folder.join("Documents/c.pdf"),
        folder.join("Documents/d.pdf"),
    )
    .unwrap();

    let groups = organize::propose(&folder).unwrap().groups;
    moved(run(&mut engine, Request::Organize { groups }));
    assert!(folder.join("Images/a.png").exists() && folder.join("Documents/c.pdf").exists());
    moved(run(&mut engine, Request::Undo));
    assert!(folder.join("a.png").exists() && folder.join("c.pdf").exists());
    assert!(!folder.join("Images").exists(), "the folder it made goes");
    assert!(
        folder.join("Documents/d.pdf").exists(),
        "one already there stays"
    );
    moved(run(&mut engine, Request::Redo));
    assert!(folder.join("Images/b.png").exists());
}

#[test]
fn a_group_cannot_reach_outside_its_folder() {
    let (_dir, files, mut engine) = engine();
    let folder = files.canonicalize().unwrap();
    touch(&folder, &["a.png"]);
    let mut groups = organize::propose(&folder).unwrap().groups;
    groups[0].folder = folder.parent().unwrap().join("Elsewhere");
    let refused = run(&mut engine, Request::Organize { groups });
    assert!(refused.unwrap_err().contains("isn't beside"));
    assert!(folder.join("a.png").exists());
}

#[test]
fn a_names_rule_collides_swaps_refuses_stale_previews_and_undoes_whole() {
    let (_dir, files, mut engine) = engine();
    let [a, b, c] = touch(&files, &["a.txt", "b.txt", "c.txt"])
        .try_into()
        .unwrap();
    let names = |names: [&str; 3]| rename::Rule::Names {
        names: names.map(String::from).to_vec(),
    };
    let batch = [a.as_path(), &b, &c];
    let paths = batch.map(Path::to_path_buf);
    let plan = rename::Plan::new(&paths, &names(["same.txt", "same.txt", ""]));
    assert!(matches!(plan.rows[1].status, rename::Status::Collision(_)));
    assert_eq!(plan.rows[2].status, rename::Status::Unchanged);

    let stale = rename::Plan::new(&paths, &names(["x.txt", "y.txt", ""])).fingerprint();
    std::fs::write(&b, "changed since").unwrap();
    let refused = run(
        &mut engine,
        Request::RenameMany {
            paths: paths.to_vec(),
            rule: names(["x.txt", "y.txt", ""]),
            preview: Some(stale),
            skip_problems: false,
        },
    );
    assert!(refused.is_err());

    // A swap, which needs a hidden name on the way, and one Undo reverses all.
    let swapped = names(["b.txt", "a.txt", "d.txt"]);
    assert_eq!(
        moved(run(&mut engine, rename_many(&batch, swapped))).len(),
        3
    );
    assert_eq!(read(&files.join("a.txt")), "changed since");
    moved(run(&mut engine, Request::Undo));
    assert_eq!(read(&files.join("b.txt")), "changed since");
    assert!(files.join("c.txt").exists() && !files.join("d.txt").exists());
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
        keep: None,
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
