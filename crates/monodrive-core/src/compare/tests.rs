use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::UNIX_EPOCH;

/// A fixed time, so two files can share one exactly.
fn when(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_700_000_000 + secs)
}

fn write(path: &Path, text: &str, modified: SystemTime) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
}

/// Two folders with one of each kind of difference, real paths.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let (left, right) = (root.join("left"), root.join("right"));
    for (path, text, at) in [
        ("same.txt", "hello", 0),
        ("shared/nested.txt", "deep", 0),
        ("size.txt", "a", 0),
        ("only/inner.txt", "inner", 0),
        ("dated.txt", "new", 3600),
        ("clash/inside.txt", "x", 0),
    ] {
        write(&left.join(path), text, when(at));
    }
    for (path, text, at) in [
        ("same.txt", "hello", 0),
        ("shared/nested.txt", "deep", 0),
        ("size.txt", "abc", 0),
        ("dated.txt", "old", 0),
        ("clash", "a file", 0),
        ("extra.txt", "extra", 0),
    ] {
        write(&right.join(path), text, when(at));
    }
    // Same size and time, other bytes: what metadata alone gets wrong.
    write(&left.join("twin.txt"), "aaaa", when(0));
    write(&right.join("twin.txt"), "bbbb", when(0));
    // Folder links on both sides, to the same place, and one to its own
    // folder: neither is walked.
    for side in [&left, &right] {
        std::os::unix::fs::symlink(&root, side.join("up")).unwrap();
    }
    std::os::unix::fs::symlink(".", left.join("loop")).unwrap();
    (dir, left, right)
}

fn statuses(comparison: &Comparison) -> Vec<(&str, &Status)> {
    comparison
        .entries
        .iter()
        .map(|entry| (entry.path.to_str().unwrap(), &entry.status))
        .collect()
}

#[test]
fn metadata_only_picks_candidates_and_reading_settles_them() {
    let (_dir, left, right) = fixture();
    let progress = Progress::default();
    let mut comparison = compare(&left, &right, &progress, None).unwrap();
    assert!(comparison.complete);
    assert_eq!(
        statuses(&comparison),
        [
            ("clash", &Status::TypeConflict),
            ("dated.txt", &Status::MetadataDiffers),
            ("extra.txt", &Status::OnlyRight),
            ("loop", &Status::OnlyLeft),
            ("only", &Status::OnlyLeft),
            ("same.txt", &Status::SameMetadata),
            ("shared/nested.txt", &Status::SameMetadata),
            ("size.txt", &Status::Differs),
            ("twin.txt", &Status::SameMetadata),
            ("up", &Status::Equal),
        ]
    );
    assert_eq!(comparison.entries[1].newer(), Some(Newer::Left));
    assert_eq!(
        comparison.entries[3].left.as_ref().unwrap().kind,
        Kind::Link
    );

    // Selected: only what's under shared/ is read.
    assert!(comparison.verify(&["shared".into()], &progress, None));
    assert_eq!(comparison.entries[5].status, Status::SameMetadata);
    assert_eq!(comparison.entries[6].status, Status::Equal);
    assert_eq!(progress.bytes.load(Ordering::Relaxed), 8);

    assert!(comparison.verify(&[PathBuf::new()], &progress, None));
    let read: Vec<_> = statuses(&comparison)
        .into_iter()
        .filter(|(path, _)| ["dated.txt", "same.txt", "twin.txt"].contains(path))
        .collect();
    assert_eq!(
        read,
        [
            ("dated.txt", &Status::Differs),
            ("same.txt", &Status::Equal),
            ("twin.txt", &Status::Differs),
        ]
    );
}

#[test]
fn two_files_are_one_entry_and_a_file_and_a_folder_conflict() {
    let (_dir, left, right) = fixture();
    let progress = Progress::default();
    let files = compare(
        &left.join("twin.txt"),
        &right.join("twin.txt"),
        &progress,
        None,
    )
    .unwrap();
    assert_eq!(statuses(&files), [("", &Status::SameMetadata)]);
    let mut files = files;
    files.verify(&[PathBuf::new()], &progress, None);
    assert_eq!(statuses(&files), [("", &Status::Differs)]);
    let mixed = compare(&left.join("clash"), &right.join("clash"), &progress, None).unwrap();
    assert_eq!(statuses(&mixed), [("", &Status::TypeConflict)]);
    let missing = compare(&left.join("nothing"), &right, &progress, None).unwrap_err();
    assert!(missing.contains("No such file"), "{missing}");
}

#[test]
fn what_cant_be_read_is_unknown_with_the_reason() {
    let (_dir, left, right) = fixture();
    let locked = left.join("shared");
    let file = right.join("same.txt");
    let set = |path: &Path, mode| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    set(&locked, 0o000);
    set(&file, 0o000);
    let progress = Progress::default();
    let mut comparison = compare(&left, &right, &progress, None);
    let root_locked = compare(&locked, &right.join("shared"), &progress, None);
    if let Ok(comparison) = &mut comparison {
        comparison.verify(&[PathBuf::new()], &progress, None);
    }
    set(&locked, 0o755);
    set(&file, 0o644);
    let comparison = comparison.unwrap();
    let unknown: Vec<_> = statuses(&comparison)
        .into_iter()
        .filter_map(|(path, status)| match status {
            Status::Unknown(why) => Some((path, why.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        unknown,
        [
            (
                "same.txt",
                format!("{}: Permission denied (os error 13)", file.display())
            ),
            (
                "shared",
                format!("{}: Permission denied (os error 13)", locked.display())
            ),
        ]
    );
    // A root that can't be listed fails the comparison.
    assert!(root_locked.unwrap_err().contains("Permission denied"));
}

#[test]
fn cancelling_stops_the_walk_and_verification_without_settling_anything() {
    let (_dir, left, right) = fixture();
    let cancelled = Progress::default();
    cancelled.cancel.store(true, Ordering::Relaxed);
    let partial = compare(&left, &right, &cancelled, None).unwrap();
    assert!(!partial.complete);
    assert!(partial.entries.is_empty());

    let mut comparison = compare(&left, &right, &Progress::default(), None).unwrap();
    let before = comparison.clone();
    assert!(!comparison.verify(&[PathBuf::new()], &cancelled, None));
    assert_eq!(comparison, before);
    assert_eq!(cancelled.bytes.load(Ordering::Relaxed), 0);
}

#[test]
fn a_plan_copies_whats_only_left_replaces_whats_older_and_skips_the_rest() {
    let (_dir, left, right) = fixture();
    let comparison = compare(&left, &right, &Progress::default(), None).unwrap();
    let plan = comparison.plan(&[PathBuf::new()]);
    let rows: Vec<_> = plan
        .rows
        .iter()
        .map(|row| (row.path.to_str().unwrap(), &row.action))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "clash",
                &Action::Skip("A folder on the left and a file on the right.".into())
            ),
            ("dated.txt", &Action::Replace),
            (
                "extra.txt",
                &Action::Skip("Only on the right; Monodrive never deletes it.".into())
            ),
            ("loop", &Action::Copy),
            ("only", &Action::Copy),
            (
                "size.txt",
                &Action::Skip("Neither is newer, so neither replaces the other.".into())
            ),
        ]
    );
    assert_eq!(plan.rows[4].source, left.join("only"));
    assert_eq!(plan.rows[4].destination, right.join("only"));
    let Some(Request::CopyPlanned { into, items }) = plan.request() else {
        panic!("a plan with copies has a request");
    };
    assert_eq!(into, right);
    let sources: Vec<_> = items.iter().map(|item| item.source.clone()).collect();
    assert_eq!(
        sources,
        [left.join("dated.txt"), left.join("loop"), left.join("only")]
    );
    assert!(items[0].replaces.is_some() && items[2].replaces.is_none());

    // Selected only, and checked against the disk: a destination that
    // changed or appeared since the comparison is skipped.
    write(&right.join("dated.txt"), "edited", when(0));
    std::fs::create_dir(right.join("only")).unwrap();
    let plan = comparison.plan(&["dated.txt".into(), "only".into()]);
    let skipped: Vec<_> = plan.rows.iter().map(|row| &row.action).collect();
    assert_eq!(
        skipped,
        [
            &Action::Skip(format!(
                "{} changed since the comparison. Compare again.",
                right.join("dated.txt").display()
            )),
            &Action::Skip(format!(
                "{} appeared since the comparison. Compare again.",
                right.join("only").display()
            )),
        ]
    );
    assert_eq!(plan.request(), None);
}

#[test]
fn a_text_diff_keeps_context_around_changes_and_folds_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let (old, new) = (dir.path().join("old.txt"), dir.path().join("new.txt"));
    let numbered = |lines: &[&str]| lines.join("\n") + "\n";
    std::fs::write(&old, numbered(&["1", "2", "3", "4", "5", "6", "7", "8"])).unwrap();
    std::fs::write(
        &new,
        numbered(&["1", "2", "3", "4", "5", "6", "seven", "8"]),
    )
    .unwrap();
    let diff = diff(&old, &new, 1).unwrap();
    assert_eq!((diff.additions, diff.deletions), (1, 1));
    assert_eq!(diff.blocks, [Block::Fold(0..5), Block::Hunk(5..9)]);
    let hunk: Vec<_> = diff.lines[5..]
        .iter()
        .map(|l| (l.kind, l.text.as_str(), l.old_number, l.new_number))
        .collect();
    assert_eq!(
        hunk,
        [
            (LineKind::Context, "6", Some(6), Some(6)),
            (LineKind::Del, "7", Some(7), None),
            (LineKind::Add, "seven", None, Some(7)),
            (LineKind::Context, "8", Some(8), Some(8)),
        ]
    );
}

#[test]
fn binary_and_oversized_files_have_no_text_diff() {
    let dir = tempfile::tempdir().unwrap();
    let (text, binary, big) = (
        dir.path().join("a.txt"),
        dir.path().join("a.bin"),
        dir.path().join("big.txt"),
    );
    std::fs::write(&text, "a\n").unwrap();
    std::fs::write(&binary, b"a\0b").unwrap();
    File::create(&big).unwrap().set_len(DIFF_BYTES + 1).unwrap();
    assert_eq!(
        diff(&text, &binary, 3).unwrap_err(),
        format!("{} is binary, so it has no text diff.", binary.display())
    );
    assert_eq!(
        diff(&big, &text, 3).unwrap_err(),
        format!(
            "{} is {} bytes, more than the {DIFF_BYTES} a text diff reads.",
            big.display(),
            DIFF_BYTES + 1
        )
    );
}
