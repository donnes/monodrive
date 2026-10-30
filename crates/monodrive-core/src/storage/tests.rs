use super::*;
use std::os::unix::fs::PermissionsExt;

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A temp folder by its real path, as a scan reports paths.
fn folder() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

fn scanned(root: &Path) -> Report {
    scan(root, &Progress::default(), None).unwrap()
}

fn duplicates(report: &Report) -> Duplicates {
    report.duplicates(&|_| true, &Progress::default(), None)
}

/// Each group as its match and its members' names.
fn named(duplicates: &Duplicates) -> Vec<(Match, Vec<String>)> {
    duplicates
        .groups
        .iter()
        .map(|group| {
            let names = group
                .members
                .iter()
                .map(|m| m.path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            (group.matched, names)
        })
        .collect()
}

#[test]
fn largest_files_and_folder_totals_count_hard_links_once_and_never_as_duplicates() {
    let (_dir, root) = folder();
    write(&root.join("big.bin"), &[1; 300_000]);
    write(&root.join("sub/mid.bin"), &[2; 200_000]);
    write(&root.join("sub/deeper/small.txt"), b"small");
    write(&root.join("other/tiny.txt"), b"t");
    std::fs::hard_link(root.join("big.bin"), root.join("sub/big-again.bin")).unwrap();

    let report = scanned(&root);
    assert_eq!(report.stopped, None);
    let total = report.root().usage;
    assert_eq!((total.bytes, total.files), (500_006, 4));
    assert!(total.on_disk >= total.bytes && total.unshared <= total.on_disk);
    let largest: Vec<PathBuf> = report.largest(2).into_iter().map(|l| l.path).collect();
    assert_eq!(largest, [root.join("big.bin"), root.join("sub/mid.bin")]);
    assert_eq!(report.largest(10).len(), 4);
    let children: Vec<&Path> = report
        .root()
        .children
        .iter()
        .map(|&i| report.folders[i].path.as_path())
        .collect();
    assert_eq!(children, [root.join("sub"), root.join("other")]);
    let sub = &report.folders[report.root().children[0]];
    assert_eq!((sub.usage.bytes, sub.usage.files), (200_005, 2));
    assert!(duplicates(&report).groups.is_empty());
}

#[test]
fn only_identical_contents_verify_even_when_the_samples_match() {
    let (_dir, root) = folder();
    let big: Vec<u8> = (0..1_000_000u32).map(|n| (n % 251) as u8).collect();
    // Same start, middle and end, but a byte apart between them.
    let mut almost = big.clone();
    almost[300_000] ^= 1;
    write(&root.join("a/big.bin"), &big);
    write(&root.join("b/big copy.bin"), &big);
    write(&root.join("c/almost.bin"), &almost);
    write(&root.join("one.txt"), b"same");
    write(&root.join("two.txt"), b"same");
    // The same size, other contents.
    write(&root.join("three.txt"), b"diff");

    let found = duplicates(&scanned(&root));
    assert_eq!(found.stopped, None);
    assert_eq!(
        named(&found),
        [
            (
                Match::Content,
                vec!["big.bin".into(), "big copy.bin".into()]
            ),
            (Match::Content, vec!["one.txt".into(), "two.txt".into()]),
        ]
    );
    assert_eq!(found.groups[0].wasted(), 1_000_000);
    assert!(found.unreadable.is_empty());
}

#[test]
fn clones_verify_as_duplicates_whose_trashing_frees_nothing() {
    let (_dir, root) = folder();
    let (original, clone, plain) = (root.join("a.bin"), root.join("b.bin"), root.join("c.bin"));
    write(&original, &[7; 1 << 20]);
    write(&plain, &[9; 1 << 20]);
    let path = |p: &Path| CString::new(p.as_os_str().as_bytes()).unwrap();
    // SAFETY: both are NUL-terminated paths that outlive the call.
    let cloned = unsafe { libc::clonefile(path(&original).as_ptr(), path(&clone).as_ptr(), 0) };
    assert_eq!(cloned, 0, "{}", std::io::Error::last_os_error());

    let report = scanned(&root);
    let found = duplicates(&report);
    assert_eq!(
        named(&found),
        [(Match::Content, vec!["a.bin".into(), "b.bin".into()])]
    );
    assert!(found.groups[0].members.iter().all(|m| m.unshared == 0));
    let total = report.root().usage;
    assert_eq!((total.on_disk, total.unshared), (3 << 20, 1 << 20));
    assert_eq!(found.cleanup(&[clone]).unwrap().frees, 0);
}

#[test]
fn unreadable_folders_and_files_are_reported_and_left_out() {
    let (_dir, root) = folder();
    let (locked, secret) = (root.join("locked"), root.join("secret.txt"));
    write(&locked.join("inside.txt"), &[0; 5_000]);
    write(&root.join("open.txt"), b"same");
    write(&secret, b"same");
    write(&root.join("third.txt"), b"same");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();

    let report = scanned(&root);
    let found = duplicates(&report);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(report.stopped, None);
    assert_eq!(
        report.unreadable,
        [format!(
            "{}: Permission denied (os error 13)",
            locked.display()
        )]
    );
    let total = report.root();
    assert!(total.unreadable && total.usage.bytes == 12);
    assert_eq!(
        named(&found),
        [(Match::Content, vec!["open.txt".into(), "third.txt".into()])]
    );
    assert_eq!(
        found.unreadable,
        [format!(
            "{}: Permission denied (os error 13)",
            secret.display()
        )]
    );
}

#[test]
fn a_cancelled_scan_or_search_says_so_and_keeps_its_candidates_unverified() {
    let (_dir, root) = folder();
    write(&root.join("a.txt"), b"same");
    write(&root.join("b.txt"), b"same");
    let cancelled = Progress::default();
    cancelled.cancel.store(true, Ordering::Relaxed);
    let report = scan(&root, &cancelled, None).unwrap();
    assert_eq!(report.stopped, Some(Stop::Cancelled));
    assert_eq!(report.root().usage, Usage::default());

    let report = scanned(&root);
    let found = report.duplicates(&|_| true, &cancelled, None);
    assert_eq!(found.stopped, Some(Stop::Cancelled));
    assert_eq!(
        named(&found),
        [(Match::Size, vec!["a.txt".into(), "b.txt".into()])]
    );
    let refused = found.cleanup(&[root.join("a.txt")]).unwrap_err();
    assert!(
        refused.ends_with("not a verified duplicate in this scan."),
        "{refused}"
    );
}

#[test]
fn a_cleanup_trashes_only_named_verified_copies_and_keeps_one_of_each() {
    let (_dir, root) = folder();
    for name in ["a.txt", "b.txt", "c.txt"] {
        write(&root.join(name), b"same");
    }
    write(&root.join("solo.txt"), b"solo");
    let found = duplicates(&scanned(&root));
    let all = ["a.txt", "b.txt", "c.txt"].map(|name| root.join(name));
    let refused = found.cleanup(&all).unwrap_err();
    assert!(refused.starts_with("Keep at least one of "), "{refused}");
    let refused = found.cleanup(&[root.join("solo.txt")]).unwrap_err();
    assert!(refused.contains("not a verified duplicate"), "{refused}");
    assert!(found.cleanup(&[]).is_err());

    let plan = found.cleanup(&all[1..]).unwrap();
    let [copies] = plan.groups.as_slice() else {
        panic!("{plan:?}")
    };
    let paths = |seen: &[Seen]| seen.iter().map(|s| s.path.clone()).collect::<Vec<_>>();
    assert_eq!(paths(&copies.keep), all[..1]);
    assert_eq!(paths(&copies.trash), all[1..]);
    assert!(matches!(plan.request(), Request::TrashDuplicates { groups } if groups == plan.groups));
}
