use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

fn volume(name: &str, mount: &Path, uuid: Option<&str>) -> Volume {
    Volume {
        name: name.into(),
        mount: mount.into(),
        disk: None,
        id: uuid.map(|uuid| VolumeId::Uuid(uuid.into())),
        total: 0,
        free: 0,
        read_only: false,
        network: false,
        ejectable: true,
        removable: false,
        startup: mount == Path::new("/"),
    }
}

/// A temp folder and its real path, which catalogs record.
fn real_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap();
    (dir, real)
}

fn write(path: &Path, bytes: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

fn store(dir: &Path) -> Store {
    Store {
        dir: dir.join("state/catalogs"),
    }
}

/// A drive with photos, a report, notes and a link, mounted at
/// `dir/Backup`.
fn backup(dir: &Path) -> PathBuf {
    let mount = dir.join("Backup");
    write(&mount.join("Photos/beach.jpg"), 10);
    write(&mount.join("Photos/2024/report.pdf"), 2000);
    write(&mount.join("notes/todo.md"), 5);
    std::os::unix::fs::symlink("/nowhere", mount.join("link")).unwrap();
    mount
}

fn root() -> Volume {
    volume("Macintosh HD", "/".as_ref(), Some("R"))
}

fn create(store: &Store, path: &Path, volumes: &[Volume]) -> Catalog {
    store
        .create(path, volumes, &Progress::default(), None)
        .unwrap()
}

fn search(store: &Store, query: &str, volumes: &[Volume]) -> Found {
    store
        .search(&Query::parse(query).unwrap(), volumes, 50)
        .unwrap()
}

/// The catalogs, none unreadable.
fn listed(store: &Store) -> Vec<Catalog> {
    let listing = store.list().unwrap();
    assert_eq!(listing.unreadable, []);
    listing.catalogs
}

fn paths(found: &Found) -> Vec<PathBuf> {
    found
        .hits
        .iter()
        .map(|hit| hit.entry.path.clone())
        .collect()
}

/// What's in the catalogs folder, drafts and locks included.
fn files(store: &Store) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(&store.dir)
        .map(|listing| {
            listing
                .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn a_disconnected_source_searches_offline_and_resolves_by_identity_wherever_it_remounts() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    let catalog = create(&store, &mount, &mounted);
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(catalog.refreshed) < 60, "{catalog:?}");
    assert_eq!((catalog.entries, catalog.complete()), (7, true));
    assert_eq!(catalog.failed, None);
    assert_eq!(catalog.source.path, mount);
    assert!(catalog.bytes > PREFIX, "{catalog:?}");
    assert_eq!(listed(&store), std::slice::from_ref(&catalog));
    // The same source by another path is the same catalog.
    let link = dir.join("to-backup");
    std::os::unix::fs::symlink(&mount, &link).unwrap();
    assert_eq!(create(&store, &link, &mounted).id, catalog.id);
    assert_eq!(listed(&store).len(), 1);

    // Unplugged, and plugged in again where macOS mounts a second drive
    // of the same name.
    let moved = dir.join("Backup 1");
    std::fs::rename(&mount, &moved).unwrap();
    let report = mount.join("Photos/2024/report.pdf");
    for volumes in [
        vec![root()],
        vec![root(), volume("Backup", &moved, Some("U2"))],
    ] {
        let found = search(&store, "report", &volumes);
        assert_eq!(paths(&found), std::slice::from_ref(&report));
        let gone = Availability::Unavailable("Backup isn't connected.".into());
        assert_eq!(found.catalogs[0].source, gone);
        assert_eq!(found.catalogs[0].catalog.refreshed, catalog.refreshed);
        assert_eq!(
            found.catalogs[0].catalog.resolve(&found.hits[0], &volumes),
            Resolved::Offline("Backup isn't connected.".into())
        );
    }

    let back = [root(), volume("Backup", &moved, Some("U1"))];
    let found = search(&store, "report", &back);
    let now_at = moved.join("Photos/2024/report.pdf");
    assert_eq!(paths(&found), std::slice::from_ref(&now_at));
    assert_eq!(
        found.catalogs[0].source,
        Availability::Available(moved.clone())
    );
    assert_eq!(
        found.catalogs[0].catalog.resolve(&found.hits[0], &back),
        Resolved::Current(now_at)
    );
    let refreshed = store
        .refresh(&catalog.id, &back, &Progress::default(), None)
        .unwrap();
    assert_eq!(refreshed.source.path, moved);
    assert_eq!(refreshed.source.volume.mount, moved);
}

#[test]
fn offline_search_takes_find_files_filters_and_in_picks_the_catalogs() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let other = dir.join("Work");
    write(&other.join("todo-later.md"), 3000);
    write(&other.join("src/main.rs"), 1);
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    std::fs::File::options()
        .write(true)
        .open(mount.join("Photos/beach.jpg"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    let mounted = [
        root(),
        volume("Backup", &mount, Some("U1")),
        volume("Work", &other, Some("U2")),
    ];
    create(&store, &mount, &mounted);
    create(&store, &other, &mounted);
    let offline = [root()];
    let found = |query: &str| {
        let mut found = paths(&search(&store, query, &offline));
        found.sort();
        found
    };
    assert_eq!(
        found("todo"),
        [mount.join("notes/todo.md"), other.join("todo-later.md")]
    );
    assert_eq!(
        found("kind:folder"),
        [
            mount.join("Photos"),
            mount.join("Photos/2024"),
            mount.join("notes"),
            other.join("src")
        ]
    );
    assert_eq!(found("kind:link"), [mount.join("link")]);
    assert_eq!(found("ext:md size:>1kb"), [other.join("todo-later.md")]);
    assert_eq!(found("kind:image"), [mount.join("Photos/beach.jpg")]);
    assert_eq!(
        found("modified:<2021-01-01"),
        [mount.join("Photos/beach.jpg")]
    );
    assert_eq!(found("rpeort"), [mount.join("Photos/2024/report.pdf")]);
    let photos = mount.join("Photos");
    assert_eq!(
        found(&format!("in:\"{}\"", photos.display())),
        [
            photos.join("2024"),
            photos.join("2024/report.pdf"),
            photos.join("beach.jpg")
        ]
    );
    assert_eq!(
        found(&format!("todo in:\"{}\"", other.display())),
        [other.join("todo-later.md")]
    );
    assert_eq!(found("nothing-like-this"), Vec::<PathBuf>::new());
    let limited = store
        .search(&Query::parse("kind:file").unwrap(), &offline, 2)
        .unwrap();
    assert_eq!(limited.hits.len(), 2);
    assert!(
        store
            .search(&Query::parse("in:relative").unwrap(), &offline, 2)
            .is_err()
    );
}

#[test]
fn an_entry_changed_since_cataloging_resolves_as_changed() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    create(&store, &mount, &mounted);
    write(&mount.join("notes/todo.md"), 9);
    std::fs::rename(mount.join("Photos/beach.jpg"), dir.join("beach.jpg")).unwrap();
    let report = mount.join("Photos/2024/report.pdf");
    std::fs::rename(&report, dir.join("report.pdf")).unwrap();
    std::fs::create_dir(&report).unwrap();
    write(&mount.join("Photos/new.jpg"), 1);

    let resolved = |query: &str| {
        let found = search(&store, query, &mounted);
        assert_eq!(found.hits.len(), 1, "{query}: {:?}", paths(&found));
        found.catalogs[0].catalog.resolve(&found.hits[0], &mounted)
    };
    assert_eq!(
        resolved("todo"),
        Resolved::Changed(
            mount.join("notes/todo.md"),
            "changed since cataloged: was 5 bytes, now 9".into()
        )
    );
    assert_eq!(
        resolved("beach"),
        Resolved::Changed(
            mount.join("Photos/beach.jpg"),
            "gone since cataloged".into()
        )
    );
    assert_eq!(
        resolved("report"),
        Resolved::Changed(
            report,
            "changed since cataloged: was a file, now a folder".into()
        )
    );
    // A folder whose items changed is still that folder.
    assert_eq!(
        resolved("Photos kind:folder"),
        Resolved::Current(mount.join("Photos"))
    );
    // Not in the catalog until it's refreshed.
    assert!(search(&store, "new.jpg", &mounted).hits.is_empty());
}

#[test]
fn a_failed_cancelled_or_late_refresh_keeps_the_last_good_catalog() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    let good = create(&store, &mount, &mounted);
    let id = good.id.clone();
    write(&mount.join("fresh.txt"), 1);
    let cancelled = Progress::default();
    cancelled.cancel.store(true, Ordering::Relaxed);
    let kept = " The last good catalog is kept.";
    let refresh = |volumes: &[Volume], progress: &Progress, deadline| {
        store.refresh(&id, volumes, progress, deadline)
    };
    assert_eq!(
        refresh(&mounted, &cancelled, None),
        Err(format!("Cancelled before it finished.{kept}"))
    );
    assert_eq!(
        refresh(&mounted, &Progress::default(), Some(Instant::now())),
        Err(format!("Ran out of time before it finished.{kept}"))
    );
    std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o000)).unwrap();
    let locked = refresh(&mounted, &Progress::default(), None);
    std::fs::set_permissions(&mount, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        locked,
        Err(format!(
            "{}: Permission denied (os error 13).{kept}",
            mount.display()
        ))
    );
    assert_eq!(
        refresh(&[root()], &Progress::default(), None),
        Err(format!("Backup isn't connected.{kept}"))
    );

    let after = listed(&store).remove(0);
    let failed = after.failed.clone().unwrap();
    assert_eq!(failed.why, "Backup isn't connected.");
    assert_eq!(
        Catalog {
            failed: None,
            bytes: good.bytes,
            ..after
        },
        good
    );
    assert_eq!(search(&store, "report", &[root()]).hits.len(), 1);
    assert!(search(&store, "fresh", &[root()]).hits.is_empty());
    let (catalog, lock) = (format!("{id}.catalog"), format!("{id}.lock"));
    assert_eq!(files(&store), [catalog.clone(), lock.clone()]);

    // One refresh at a time, across processes.
    let held = ops::lock(&store.dir.join(&lock), libc::LOCK_EX).unwrap();
    assert_eq!(
        refresh(&mounted, &Progress::default(), None),
        Err(format!(
            "Catalog {id} is being refreshed. Wait for it to finish or cancel it."
        ))
    );
    drop(held);

    let fresh = refresh(&mounted, &Progress::default(), None).unwrap();
    assert_eq!((fresh.entries, fresh.failed), (good.entries + 1, None));
    assert_eq!(search(&store, "fresh", &[root()]).hits.len(), 1);
    assert_eq!(files(&store), [catalog, lock]);

    // A first walk that doesn't finish keeps nothing.
    let work = dir.join("Work");
    write(&work.join("a.txt"), 1);
    let volumes = [root(), volume("Work", &work, Some("U2"))];
    assert_eq!(
        store.create(&work, &volumes, &cancelled, None),
        Err("Cancelled before it finished.".into())
    );
    assert_eq!(listed(&store).len(), 1);
    assert_eq!(files(&store).len(), 3);
}

#[test]
fn a_refresh_stops_at_its_budget_and_keeps_what_it_met_as_incomplete() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let locked = mount.join("locked");
    write(&locked.join("secret.txt"), 1);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    let catalog = create(&store, &mount, &mounted);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!((catalog.entries, catalog.unreadable), (8, 1));
    assert!(!catalog.complete() && !catalog.truncated);

    let progress = Progress::default();
    let cut = store
        .refresh_within(&catalog.id, &mounted, &progress, None, 3)
        .unwrap();
    assert_eq!((cut.entries, cut.truncated, cut.failed), (3, true, None));
    assert_eq!(progress.items.load(Ordering::Relaxed), 3);
    let all = paths(&search(&store, "", &[root()]));
    assert_eq!(all.len(), 3, "{all:?}");
}

#[test]
fn forget_removes_only_monodrives_catalog_file() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    let catalog = create(&store, &mount, &mounted);
    assert_eq!(store.forget(&catalog.id), Ok(()));
    assert_eq!(listed(&store), Vec::<Catalog>::new());
    // The lock stays, so a refresh that opened it keeps excluding others.
    let lock = [format!("{}.lock", catalog.id)];
    assert_eq!(files(&store), lock);
    assert!(mount.join("Photos/2024/report.pdf").is_file());
    let missing = format!("No catalog {}.", catalog.id);
    assert_eq!(store.forget(&catalog.id), Err(missing.clone()));
    let progress = Progress::default();
    assert_eq!(
        store.refresh(&catalog.id, &mounted, &progress, None),
        Err(missing)
    );
    assert_eq!(
        store.forget("../places"),
        Err("No catalog ../places.".into())
    );
    assert_eq!(files(&store), lock);
}

#[test]
fn only_a_folder_on_a_volume_with_an_identity_other_than_the_startup_disk_is_cataloged() {
    let (_tmp, dir) = real_tempdir();
    let store = store(&dir);
    let (folder, file) = (dir.join("Loose"), dir.join("Loose/a.txt"));
    write(&file, 1);
    let progress = Progress::default();
    let refused =
        |path: &Path, volumes: &[Volume]| store.create(path, volumes, &progress, None).unwrap_err();
    assert_eq!(
        refused(&folder, &[root()]),
        format!(
            "{} is on the startup disk, which is always connected. Catalog a volume or a folder on one.",
            folder.display()
        )
    );
    assert_eq!(
        refused(&folder, &[root(), volume("FAT", &folder, None)]),
        "macOS gives FAT no identity, so Monodrive couldn't tell it from another drive once it's disconnected."
    );
    let usb = [root(), volume("USB", &folder, Some("U"))];
    assert_eq!(
        refused(&file, &usb),
        format!("{} isn't a folder.", file.display())
    );
    assert!(refused("Loose".as_ref(), &usb).contains("give an absolute path"));
    assert_eq!(listed(&store), Vec::<Catalog>::new());
}

#[test]
fn a_source_disconnected_during_a_refresh_keeps_the_last_good_catalog() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), dir.join("Backup"));
    for folder in ["a", "b", "c", "d"] {
        std::fs::create_dir_all(mount.join(folder)).unwrap();
        for n in 0..2000 {
            std::fs::write(mount.join(folder).join(format!("{n}.txt")), "").unwrap();
        }
    }
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    let good = create(&store, &mount, &mounted);
    // Unplugged partway: once the walk has met a few thousand entries, the
    // folders it hasn't listed yet are gone.
    let (from, to) = (mount.clone(), dir.join("unplugged"));
    let unplugged = AtomicBool::new(false);
    let progress = Progress::notifying(move || {
        if !unplugged.swap(true, Ordering::Relaxed) {
            std::fs::rename(&from, &to).unwrap();
        }
    });
    assert_eq!(
        store.refresh(&good.id, &mounted, &progress, None),
        Err("Backup was disconnected during the refresh. The last good catalog is kept.".into())
    );
    let kept = listed(&store).remove(0);
    assert_eq!(kept.entries, good.entries);
    assert_eq!(
        kept.failed.map(|failed| failed.why),
        Some("Backup was disconnected during the refresh.".into())
    );
}

#[test]
fn a_folder_replaced_by_a_link_is_neither_resolved_through_nor_walked() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    create(&store, &mount, &mounted);
    let photos = mount.join("Photos");
    let folder = create(&store, &photos, &mounted);
    // Photos now leads to another drive's folder with the same names and
    // sizes.
    let other = dir.join("Other/Photos");
    write(&other.join("2024/report.pdf"), 2000);
    write(&other.join("beach.jpg"), 10);
    std::fs::rename(&photos, dir.join("Photos before")).unwrap();
    std::os::unix::fs::symlink(&other, &photos).unwrap();

    let found = search(&store, "report", &mounted);
    let whole = found
        .catalogs
        .iter()
        .position(|searched| searched.catalog.source.path == mount)
        .unwrap();
    let hit = found.hits.iter().find(|hit| hit.catalog == whole).unwrap();
    assert_eq!(
        found.catalogs[whole].catalog.resolve(hit, &mounted),
        Resolved::Changed(
            photos.join("2024/report.pdf"),
            "changed since cataloged: a folder above it is now a link".into()
        )
    );
    let now_a_link = format!(
        "{} is now a link, not the cataloged folder.",
        photos.display()
    );
    assert_eq!(
        store.refresh(&folder.id, &mounted, &Progress::default(), None),
        Err(format!("{now_a_link} The last good catalog is kept."))
    );
    assert_eq!(
        folder.source.locate(&mounted),
        Availability::Unavailable(now_a_link)
    );
}

/// A catalog file with these records, this header and a body length, or
/// the body's own.
fn forge(records: &[u8], header: &[u8], len: Option<u64>) -> Vec<u8> {
    let mut body = DeflateEncoder::new(Vec::new(), Compression::default());
    body.write_all(records).unwrap();
    let body = body.finish().unwrap();
    let mut out = MAGIC.to_vec();
    out.extend(len.unwrap_or(body.len() as u64).to_le_bytes());
    out.extend(body);
    out.extend(header);
    out
}

/// A file's record: `shared` bytes of the last path, then `more` of `name`.
fn entry(shared: u64, more: u64, name: &[u8]) -> Vec<u8> {
    let mut record = Vec::new();
    put_varint(&mut record, shared);
    put_varint(&mut record, more);
    record.extend(name);
    record.extend([KIND_FILE, 0, 0]);
    record
}

#[test]
fn a_damaged_catalog_file_reads_as_unreadable_without_panicking() {
    let (_tmp, dir) = real_tempdir();
    let (store, mount) = (store(&dir), backup(&dir));
    let mounted = [root(), volume("Backup", &mount, Some("U1"))];
    let good = create(&store, &mount, &mounted);
    let file = store.file(&good.id).unwrap();
    let original = std::fs::read(&file).unwrap();
    let header = serde_json::to_vec(&good).unwrap();
    let seven: Vec<u8> = (0..7).flat_map(|_| entry(0, 1, b"a")).collect();
    let mut padded = header.clone();
    padded.extend(vec![b' '; 100_000]);
    let mut overflow = entry(0, 1, b"a");
    put_varint(&mut overflow, 1);
    put_varint(&mut overflow, u64::MAX);
    let header_damage = [
        (original[..original.len() / 2].to_vec(), "EOF while parsing"),
        (
            original[..PREFIX as usize + 1].to_vec(),
            "damaged catalog: its body runs past its end",
        ),
        (original[..10].to_vec(), "damaged catalog: it's too short"),
        (
            forge(&seven, &header, Some(u64::MAX)),
            "damaged catalog: its body runs past its end",
        ),
        (
            forge(&seven, &padded, None),
            "damaged catalog: its header is too large",
        ),
    ];
    for (bytes, why) in header_damage {
        std::fs::write(&file, bytes).unwrap();
        let listing = store.list().unwrap();
        assert_eq!(listing.catalogs, []);
        let [unreadable] = &listing.unreadable[..] else {
            panic!("{listing:?}");
        };
        assert_eq!(unreadable.id, good.id);
        let prefix = format!("{}: ", file.display());
        assert!(
            unreadable.why.starts_with(&prefix) && unreadable.why.contains(why),
            "{why}: {unreadable:?}"
        );
        let found = search(&store, "", &[root()]);
        assert_eq!(
            (found.hits.len(), found.unreadable),
            (0, listing.unreadable)
        );
    }
    let body_damage = [
        (
            forge(&entry(0, 5000, &[b'a'; 5000]), &header, None),
            "an entry's path is damaged",
        ),
        (
            forge(&overflow, &header, None),
            "an entry's path is damaged",
        ),
        (
            forge(&entry(0, 1, b"a"), &header, None),
            "it holds 1 entries, but its header says 7",
        ),
    ];
    for (bytes, why) in body_damage {
        std::fs::write(&file, bytes).unwrap();
        // The header reads, so it's listed; the body is checked as it's
        // searched.
        assert_eq!(listed(&store).len(), 1);
        let found = search(&store, "", &[root()]);
        let why = format!("{}: damaged catalog: {why}", file.display());
        assert_eq!(found.hits.len(), 0, "{why}");
        assert_eq!(
            found.unreadable,
            [Unreadable {
                id: good.id.clone(),
                why
            }]
        );
    }
    assert_eq!(store.forget(&good.id), Ok(()));
}

#[test]
fn a_damaged_catalogs_hits_never_push_out_a_healthy_ones() {
    let (_tmp, dir) = real_tempdir();
    let store = store(&dir);
    let (healthy, damaged) = (dir.join("Z"), dir.join("A"));
    write(&healthy.join("report"), 1);
    write(&damaged.join("report"), 1);
    let mounted = [
        root(),
        volume("Alpha", &healthy, Some("U1")),
        volume("Beta", &damaged, Some("U2")),
    ];
    let good = create(&store, &healthy, &mounted);
    let mut bad = create(&store, &damaged, &mounted);
    // Enough matches to fill the search's limit many times over, each
    // sorting ahead of the healthy one, in a body one entry short.
    let reports: Vec<u8> = (0..300).flat_map(|_| entry(0, 6, b"report")).collect();
    bad.entries = 301;
    let header = serde_json::to_vec(&bad).unwrap();
    std::fs::write(store.file(&bad.id).unwrap(), forge(&reports, &header, None)).unwrap();

    let found = store
        .search(&Query::parse("report").unwrap(), &[root()], 2)
        .unwrap();
    assert_eq!(paths(&found), [healthy.join("report")]);
    assert_eq!(found.catalogs[0].catalog.id, good.id);
    assert_eq!(found.unreadable.len(), 1);
    assert_eq!(found.unreadable[0].id, bad.id);
}
