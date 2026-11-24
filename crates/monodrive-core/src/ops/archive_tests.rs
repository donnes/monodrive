use super::tests::*;
use super::*;

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
        .names()
        .map(String::from)
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
    // All of it holds two items, so they come out in one folder named for
    // the archive, which takes a conflict choice as any item does.
    let all = job(&shared, extract(&archive, &[], &dest, Conflict::Stop));
    assert_eq!(all.tasks.len(), 1);
    assert_eq!(all.tasks[0].name.as_deref(), Some("a"));
    assert_eq!(all.tasks[0].done, Some(Done::To(dest.join("a"))));
    assert_eq!(names(&dest.join("a")), ["a.zip", "proj"]);
    assert_eq!(read(&dest.join("a/proj/new.txt")), "new");
    let again = job(&shared, extract(&archive, &[], &dest, Conflict::Stop));
    assert_eq!(again.state, State::Waiting);
    let both = continued(&shared, again.id, Some(Conflict::KeepBoth));
    assert_eq!(both.tasks[0].done, Some(Done::To(dest.join("a 2"))));
    run(&mut shared.lock(), Request::Undo).unwrap();
    assert!(!dest.join("a 2").exists() && dest.join("a").exists());
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
