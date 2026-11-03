use super::*;
use crate::ops::{self, Paths};
use tempfile::TempDir;

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";
const NOW: u64 = 1_000_000;

fn env(dir: &TempDir) -> Env {
    let home = dir.path().canonicalize().unwrap();
    Env::new(
        home.join(".Trash"),
        places::Store {
            places: home.join("support/places.json"),
            recents: home.join("support/recents.json"),
        },
    )
}

fn startup(total: u64, free: u64) -> Volume {
    Volume {
        name: "Mac".into(),
        mount: "/".into(),
        disk: None,
        id: Some(VolumeId::Uuid("startup".into())),
        total,
        free,
        read_only: false,
        network: false,
        ejectable: false,
        removable: false,
        startup: true,
    }
}

fn drive(id: &str, name: &str) -> Volume {
    Volume {
        name: name.into(),
        mount: format!("/Volumes/{name}").into(),
        id: Some(VolumeId::Uuid(id.into())),
        startup: false,
        ..startup(1_000, 400)
    }
}

fn no_sftp(_: &str) -> bool {
    false
}

fn read(env: &Env, volumes: &[Volume]) -> Overview {
    super::read(env, volumes, &[], &no_sftp, NOW)
}

fn size(on_disk: u64, at: u64) -> Measured {
    Measured {
        on_disk,
        unshared: on_disk,
        at,
        complete: true,
    }
}

fn save(env: &Env, change: impl FnOnce(&mut Saved)) {
    Saved::change(&env.saved(), change).unwrap();
}

/// A Cargo project at `home/name`, with a tagged `target` of its own.
fn cargo_project(home: &Path, name: &str) -> PathBuf {
    let project = home.join(name);
    std::fs::create_dir_all(project.join("target/debug")).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::write(project.join("Cargo.toml"), "").unwrap();
    std::fs::write(project.join("target/CACHEDIR.TAG"), TAG).unwrap();
    project
}

#[test]
fn a_low_disk_ranks_at_most_three_suggestions_by_what_they_free() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let brew = env.home.join("Library/Caches/Homebrew");
    let pip = env.home.join("Library/Caches/pip");
    let yarn = env.home.join("Library/Caches/Yarn");
    let pods = env.home.join("Library/Caches/CocoaPods");
    for dir in [&brew, &pip, &yarn, &pods] {
        std::fs::create_dir_all(dir).unwrap();
    }
    save(&env, |saved| {
        // More on disk but shared with clones, so it frees the least.
        let shared = Measured {
            unshared: 1,
            ..size(900, NOW)
        };
        saved.totals.insert(brew.clone(), shared);
        saved.totals.insert(pip.clone(), size(300, NOW));
        saved.totals.insert(yarn.clone(), size(200, NOW));
        saved.totals.insert(pods.clone(), size(100, NOW));
    });

    let low = read(&env, &[startup(1_000, 199)]);
    let paths: Vec<_> = low.suggestions.iter().map(|s| s.path.clone()).collect();
    assert_eq!(paths, [pip.clone(), yarn, pods]);
    assert_eq!(low.suggestions[0].evidence.title(), "Clear pip's cache");

    let roomy = read(&env, &[startup(1_000, 200)]);
    assert!(
        roomy.suggestions.is_empty(),
        "exactly 20% free isn't under it"
    );
    assert!(
        !roomy.due.contains(&pip),
        "nothing measures what won't show"
    );
}

#[test]
fn a_build_output_needs_its_tag_and_its_manifest() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let project = cargo_project(&env.home, "app");
    let target = project.join("target");
    assert_eq!(
        check(&env, &target),
        Ok(Evidence::BuildOutput {
            project: "app".into()
        })
    );

    std::fs::rename(target.join("CACHEDIR.TAG"), target.join("moved")).unwrap();
    assert!(check(&env, &target).is_err(), "no tag");
    std::fs::write(target.join("CACHEDIR.TAG"), "not the signature").unwrap();
    assert!(check(&env, &target).is_err(), "not a real tag");

    std::fs::write(target.join("CACHEDIR.TAG"), TAG).unwrap();
    std::fs::rename(project.join("Cargo.toml"), project.join("moved")).unwrap();
    assert!(check(&env, &target).is_err(), "no manifest");

    let elsewhere = cargo_project(&env.home, "lib");
    std::fs::rename(elsewhere.join("target"), elsewhere.join("build")).unwrap();
    assert!(
        check(&env, &elsewhere.join("build")).is_err(),
        "not `target`"
    );
}

#[test]
fn cleaning_refuses_while_cargo_holds_a_profile_lock() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let project = cargo_project(&env.home, "monodrive");
    let target = project.join("target");
    let lock = target.join("debug/.cargo-lock");
    std::fs::write(&lock, "").unwrap();
    assert!(check(&env, &target).is_ok(), "a lock nobody holds");

    let held = std::fs::File::open(&lock).unwrap();
    // SAFETY: the descriptor stays open for the call.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    assert_eq!(
        check(&env, &target),
        Err("Cargo is building in monodrive. Try again when it finishes.".to_string())
    );
    drop(held);
    assert!(check(&env, &target).is_ok(), "the build ended");

    // Cross-compiling keeps its profiles one level down.
    let nested = target.join("aarch64-apple-darwin/debug");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join(".cargo-lock"), "").unwrap();
    let held = std::fs::File::open(nested.join(".cargo-lock")).unwrap();
    // SAFETY: the descriptor stays open for the call.
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    assert!(check(&env, &target).is_err());
}

#[test]
fn cleaning_goes_through_the_owner_only_after_the_recheck() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let paths = Paths {
        state: env.home.join("nowhere"),
        settings: env.home.join("settings.json"),
        trash: env.trash.clone(),
    };
    let mut client = ops::Client::new(paths, None);
    let gone = env.home.join("gone/target");
    let refused = clean(&env, &gone, Origin::Window, &mut client);
    assert!(
        refused.is_err_and(|why| why.contains("no longer safe")),
        "the proof is what refuses, before any owner is asked"
    );
}

#[test]
fn remotes_compare_however_they_are_written() {
    let same = "github.com/donnes/monodrive";
    for url in [
        "git@github.com:donnes/monodrive.git",
        "https://github.com/donnes/monodrive",
        "https://token@GitHub.com/donnes/monodrive.git/",
        "ssh://git@github.com/donnes/monodrive.git",
    ] {
        assert_eq!(normalized_remote(url).as_deref(), Some(same), "{url}");
    }
    assert_eq!(normalized_remote("/srv/git/monodrive.git"), None);
    assert_eq!(normalized_remote("../monodrive"), None);
}

#[test]
fn a_project_on_a_scanned_drive_joins_its_copy_as_one_row() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let project = cargo_project(&env.home, "app");
    std::fs::write(
        project.join(".git/config"),
        "[remote \"upstream\"]\n\turl = git@github.com:other/app.git\n\
         [remote \"origin\"]\n\turl = git@github.com:me/app.git\n",
    )
    .unwrap();
    env.store
        .register_repository(worktrees::Registration {
            common_dir: project.join(".git"),
            path: project.clone(),
        })
        .unwrap();
    let backup = drive("backup", "Backup");
    save(&env, |saved| {
        saved.scans.push(Scan {
            volume: VolumeKey::of(&backup),
            name: backup.name.clone(),
            total: size(500, NOW),
            rows: vec![
                ScanRow {
                    name: "app-copy".into(),
                    path: "/Volumes/Backup/app-copy".into(),
                    size: size(300, NOW),
                    remote: Some("github.com/me/app".into()),
                },
                ScanRow {
                    name: "Photos".into(),
                    path: "/Volumes/Backup/Photos".into(),
                    size: size(200, NOW),
                    remote: None,
                },
            ],
        });
    });

    let about = read(&env, &[startup(1_000, 900), backup.clone()]);
    let group = about.stuff.iter().find(|g| g.name == "app").unwrap();
    assert_eq!(
        group.on_disk(),
        Some(300),
        "the unmeasured copy adds nothing yet"
    );
    let places: Vec<_> = group.spots.iter().map(|s| s.location.as_str()).collect();
    assert_eq!(places, ["Mac", "Backup"]);

    let rows = &about.locations[0].scan.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 1, "the copy moved to the project's row");
    assert_eq!(rows[0].name, "Photos");
}

#[test]
fn a_gone_drive_keeps_its_dated_totals_and_nothing_can_act_on_them() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let backup = drive("backup", "Backup");
    save(&env, |saved| {
        saved.scans.push(Scan {
            volume: VolumeKey::of(&backup),
            name: "Backup".into(),
            total: size(500, NOW - 3 * 86_400),
            rows: Vec::new(),
        });
    });

    let mounted = read(&env, &[startup(1_000, 900), backup.clone()]);
    assert_eq!(mounted.locations.len(), 1);
    assert_eq!(mounted.locations[0].kind, LocationKind::Drive);
    assert!(mounted.locations[0].takes_trash);

    let gone = read(&env, &[startup(1_000, 900)]);
    let location = &gone.locations[0];
    assert_eq!(location.kind, LocationKind::Disconnected);
    assert_eq!(location.mount, None);
    assert_eq!(location.capacity, None);
    assert!(!location.takes_trash);
    assert_eq!(
        location.scan.as_ref().unwrap().total.age(NOW).as_deref(),
        Some("3 days ago")
    );
}

#[test]
fn opening_the_page_scans_no_drive_and_connects_to_no_server() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    env.store.save_server("smb://nas/photos").unwrap();
    let share = Volume {
        network: true,
        ..drive("share", "Share")
    };
    let about = read(&env, &[startup(1_000, 900), drive("d", "Drive"), share]);
    assert!(about.due.iter().all(|path| !path.starts_with("/Volumes")));
    let nas = about.locations.iter().find(|l| l.name.contains("nas"));
    assert!(nas.is_some_and(|nas| !nas.connected && nas.mount.is_none()));
    assert!(
        about.locations.iter().all(|l| l.scan.is_none()),
        "not scanned stays not scanned"
    );
}

#[test]
fn what_is_due_is_what_has_no_total_or_an_old_one() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let (fresh, old, none) = (
        env.home.join("Documents"),
        env.home.join("Movies"),
        env.home.join("Music"),
    );
    for dir in [&fresh, &old, &none] {
        std::fs::create_dir(dir).unwrap();
    }
    save(&env, |saved| {
        saved.totals.insert(fresh.clone(), size(1, NOW - 60));
        saved.totals.insert(old.clone(), size(1, NOW - REMEASURE));
    });
    let due = read(&env, &[startup(1_000, 900)]).due;
    assert!(!due.contains(&fresh));
    assert!(due.contains(&old));
    assert!(due.contains(&none));
}

#[test]
fn everything_else_counts_nested_folders_once_and_waits_for_every_total() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let documents = env.home.join("Documents");
    let project = cargo_project(&documents, "app");
    env.store
        .register_repository(worktrees::Registration {
            common_dir: project.join(".git"),
            path: project.clone(),
        })
        .unwrap();
    let disk = startup(1_000, 100);
    save(&env, |saved| {
        saved.totals.insert(documents.clone(), size(300, NOW));
    });
    assert_eq!(
        read(&env, std::slice::from_ref(&disk)).everything_else,
        None
    );

    save(&env, |saved| {
        saved.totals.insert(project.clone(), size(200, NOW));
    });
    assert_eq!(
        read(&env, &[disk]).everything_else,
        Some(600),
        "900 used, less Documents, which already holds the project"
    );
}

#[test]
fn a_size_says_its_age_only_once_it_is_not_current() {
    assert_eq!(size(1, NOW).age(NOW), None);
    assert_eq!(size(1, NOW - CURRENT + 1).age(NOW), None);
    assert_eq!(size(1, NOW - CURRENT).age(NOW).unwrap(), "5 minutes ago");
    assert_eq!(size(1, NOW - 3_600).age(NOW).unwrap(), "1 hour ago");
    assert_eq!(size(1, NOW - 2 * 86_400).age(NOW).unwrap(), "2 days ago");
}

#[test]
fn measuring_saves_the_total_and_a_cancelled_walk_saves_nothing() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let folder = env.home.join("Documents");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("a"), vec![1; 8192]).unwrap();

    let cancelled = storage::Progress::default();
    cancelled
        .cancel
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(measure(&env, &folder, &cancelled, None, NOW), None);
    assert!(Saved::read(&env.saved()).totals.is_empty());

    let size = measure(&env, &folder, &storage::Progress::default(), None, NOW).unwrap();
    assert!(size.complete);
    assert!(size.on_disk >= 8192);
    assert_eq!(Saved::read(&env.saved()).totals.get(&folder), Some(&size));
}

#[test]
fn a_scan_keeps_totals_and_top_folders_and_no_file_list() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let root = env.home.join("Drive");
    for name in ["big", "small"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
    }
    std::fs::write(root.join("big/secret-name.bin"), vec![1; 64 * 1024]).unwrap();
    std::fs::write(root.join("small/a"), vec![1; 8 * 1024]).unwrap();
    let report = storage::scan(&root, &storage::Progress::default(), None).unwrap();
    let scan = record_scan(&env, &drive("d", "Drive"), &report, NOW).unwrap();

    assert_eq!(scan.rows[0].name, "big");
    assert_eq!(scan.rows.len(), 2);
    let file = std::fs::read_to_string(env.saved()).unwrap();
    assert!(!file.contains("secret-name"), "it's not a catalog");

    // Scanning again replaces the last scan, and the mount shows it.
    record_scan(&env, &drive("d", "Drive"), &report, NOW + 1).unwrap();
    assert_eq!(Saved::read(&env.saved()).scans.len(), 1);
}

#[test]
fn discovery_keeps_only_what_git_or_the_tag_confirms() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let project = cargo_project(&env.home, "app");
    let tag = project.join("target/CACHEDIR.TAG");
    let stray = env.home.join("cache/CACHEDIR.TAG");
    std::fs::create_dir_all(stray.parent().unwrap()).unwrap();
    std::fs::write(&stray, TAG).unwrap();
    let entry = |path: PathBuf, name: &str| Entry {
        name: name.into(),
        path,
        kind: crate::entries::Kind::File,
        size: 0,
        modified: 0,
        availability: crate::cloud::Availability::Unknown,
    };
    // Without a Git to ask, no repository counts.
    discover(&env, None, |name, _| {
        Ok(match name {
            ".git" => vec![entry(project.join(".git"), ".git")],
            _ => vec![
                entry(tag.clone(), "CACHEDIR.TAG"),
                entry(stray.clone(), "CACHEDIR.TAG"),
                // The index's fuzzy answers aren't that name.
                entry(env.home.join("CACHEDIR.TAG.bak"), "CACHEDIR.TAG.bak"),
            ],
        })
    })
    .unwrap();
    let saved = Saved::read(&env.saved());
    assert!(saved.projects.is_empty());
    assert_eq!(saved.tagged, [project.join("target")]);
    assert!(hidden_or_tool_folder(Path::new("/h/.cargo/git/x")));
    assert!(hidden_or_tool_folder(Path::new("/h/app/node_modules/x")));
    assert!(!hidden_or_tool_folder(Path::new("/h/Work/app")));
}

#[test]
fn a_trash_total_from_before_the_trash_changed_says_nothing() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    std::fs::create_dir(&env.trash).unwrap();
    let now = now();
    // The folder's own time is whole seconds, and it changed before this.
    save(&env, |saved| {
        saved.totals.insert(env.trash.clone(), size(500, now + 1));
    });
    let disk = [startup(1_000, 100)];
    let read = |at| super::read(&env, &disk, &[], &no_sftp, at);
    assert_eq!(read(now + 1).headline.unwrap().free_after_trash(), 600);

    save(&env, |saved| {
        saved.totals.insert(env.trash.clone(), size(500, now - 100));
    });
    let about = read(now);
    assert_eq!(about.headline.unwrap().free_after_trash(), 100);
    assert!(about.due.contains(&env.trash), "measured again");
}

#[test]
fn a_repository_on_another_drive_waits_for_that_drives_scan() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let mount = env.home.join("Drive");
    let project = cargo_project(&mount, "far");
    env.store
        .register_repository(worktrees::Registration {
            common_dir: project.join(".git"),
            path: project.clone(),
        })
        .unwrap();
    let volumes = [
        startup(1_000, 100),
        Volume {
            mount,
            ..drive("d", "Drive")
        },
    ];
    let about = read(&env, &volumes);
    assert!(about.stuff.iter().all(|group| group.name != "far"));
    assert!(about.suggestions.is_empty());
    assert!(about.due.iter().all(|path| !path.starts_with(&project)));
}

#[test]
fn a_scans_rows_follow_the_drive_to_where_it_mounts_now() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let backup = drive("backup", "Backup");
    save(&env, |saved| {
        saved.scans.push(Scan {
            volume: VolumeKey::of(&backup),
            name: "Backup".into(),
            total: size(500, NOW),
            rows: vec![ScanRow {
                name: "Photos".into(),
                path: "/Volumes/Backup/Photos".into(),
                size: size(500, NOW),
                remote: None,
            }],
        });
    });
    let moved = Volume {
        mount: "/Volumes/Backup 1".into(),
        ..backup
    };
    let about = read(&env, &[startup(1_000, 900), moved]);
    let rows = &about.locations[0].scan.as_ref().unwrap().rows;
    assert_eq!(rows[0].path, Path::new("/Volumes/Backup 1/Photos"));
}

#[test]
fn changes_from_two_threads_both_land() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    std::thread::scope(|threads| {
        for n in 0..8u64 {
            let env = &env;
            threads.spawn(move || {
                save(env, |saved| {
                    saved.totals.insert(format!("/f{n}").into(), size(n, NOW));
                });
            });
        }
    });
    assert_eq!(Saved::read(&env.saved()).totals.len(), 8);
}
