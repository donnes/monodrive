use super::*;
use crate::ops::{self, Paths};
use tempfile::TempDir;

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";
const NOW: u64 = 1_000_000;

fn env(dir: &TempDir) -> Env {
    let at = dir.path().canonicalize().unwrap();
    let home = at.join("home");
    std::fs::create_dir(&home).unwrap();
    Env {
        // A made-up startup disk beside Home, so nothing reads the real one's
        // folders.
        root: at.join("disk"),
        ..Env::new(
            home.join(".Trash"),
            places::Store {
                places: at.join("support/places.json"),
                recents: at.join("support/recents.json"),
            },
        )
    }
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
        system: None,
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
    let paths: Vec<_> = low
        .suggestions
        .iter()
        .filter_map(Suggestion::trash)
        .collect();
    assert_eq!(paths, [pip.as_path(), &yarn, &pods]);
    assert_eq!(low.suggestions[0].title(), "Clear pip's cache");

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
fn the_rest_of_the_disk_is_measured_where_it_goes_and_each_folder_once() {
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let home = env.home.clone();
    let documents = home.join("Documents");
    let project = cargo_project(&documents, "app");
    env.store
        .register_repository(worktrees::Registration {
            common_dir: project.join(".git"),
            path: project.clone(),
        })
        .unwrap();
    let cloud = home.join("Library/CloudStorage/Drive");
    let folders = [
        home.join("Library/Application Support"),
        home.join("Library/Developer"),
        home.join(".tool"),
        home.join("Work"),
        env.root.join("Applications"),
        env.root.join("Library"),
        env.root.join("Library/Developer"),
    ];
    for folder in folders.iter().chain([&cloud, &env.trash]) {
        std::fs::create_dir_all(folder).unwrap();
    }
    std::os::unix::fs::symlink(&documents, home.join(".link")).unwrap();
    let brew = env.root.join("opt/homebrew");
    std::fs::create_dir_all(brew.join(".git")).unwrap();
    save(&env, |saved| saved.projects.push(brew.clone()));
    let disk = Volume {
        system: Some(50),
        ..startup(1_000, 250)
    };
    let about = read(&env, std::slice::from_ref(&disk));
    let groups: Vec<(&str, Kind, Vec<&Path>)> = about
        .stuff
        .iter()
        .map(|group| {
            let spots = group.spots.iter().map(|spot| spot.path.as_path()).collect();
            (group.name.as_str(), group.kind, spots)
        })
        .collect();
    let [support, xcode, tool, work, apps, system, system_xcode] = folders.each_ref();
    assert_eq!(
        groups,
        vec![
            ("App data", Kind::Apps, vec![support.as_path()]),
            ("Applications", Kind::Apps, vec![apps]),
            (
                "Developer tools",
                Kind::Developer,
                vec![xcode, system_xcode, &brew]
            ),
            ("Documents", Kind::Documents, vec![&documents]),
            ("Hidden folders", Kind::Developer, vec![tool]),
            ("System files", Kind::System, vec![system]),
            ("Work", Kind::Documents, vec![work]),
            ("app", Kind::Projects, vec![&project]),
        ],
        "Homebrew's repository is a tool, and the cloud folders' own folder, the Trash and \
         links aren't measured again"
    );
    for folder in folders.iter().chain([&documents, &project]) {
        assert!(about.due.contains(folder), "{}", folder.display());
    }

    save(&env, |saved| {
        for (folder, bytes) in folders.iter().zip([100, 80, 70, 10, 60, 40, 30]) {
            saved.totals.insert(folder.clone(), size(bytes, NOW));
        }
        saved.totals.insert(documents.clone(), size(300, NOW));
        saved.totals.insert(brew.clone(), size(20, NOW));
    });
    assert_eq!(
        read(&env, std::slice::from_ref(&disk)).everything_else(),
        None,
        "it waits for every total"
    );
    save(&env, |saved| {
        saved.totals.insert(project.clone(), size(200, NOW));
    });
    let about = read(&env, &[disk]);
    assert_eq!(
        about.parts(),
        vec![
            (Part::Kind(Kind::Projects), 200),
            (Part::Kind(Kind::Developer), 200),
            (Part::Kind(Kind::Apps), 160),
            (Part::Kind(Kind::Documents), 110),
            (Part::Kind(Kind::System), 60),
            (Part::Other, 20),
        ],
        "the project leaves Documents, and the system's Developer leaves its Library"
    );
    assert_eq!(about.everything_else(), Some(20));
}

#[test]
fn what_a_suggestion_frees_leaves_the_innermost_folder_holding_it() {
    let at = |path: &str, kind: Kind, bytes: u64| Group {
        name: path.into(),
        kind,
        spots: vec![Spot {
            location: "Mac".into(),
            path: path.into(),
            size: Some(size(bytes, NOW)),
            reachable: true,
        }],
    };
    let about = Overview {
        headline: Some(Headline {
            name: "Mac".into(),
            total: 100,
            free: 20,
            trash: Some(size(4, NOW)),
            system: None,
            low: true,
        }),
        suggestions: vec![Suggestion {
            evidence: Evidence::BuildOutput {
                project: "app".into(),
            },
            folders: vec![("/h/Documents/app/target".into(), size(5, NOW))],
            size: size(5, NOW),
        }],
        stuff: vec![
            at("/h/Documents", Kind::Documents, 30),
            at("/h/Documents/app", Kind::Projects, 15),
        ],
        ..Overview::default()
    };
    assert_eq!(
        about.parts(),
        vec![
            (Part::Kind(Kind::Documents), 15),
            (Part::Kind(Kind::Projects), 10),
            (Part::Freeable, 9),
            (Part::Other, 46),
        ]
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

#[test]
fn clean_worktrees_are_one_suggestion_ranked_by_what_they_free_together() {
    use crate::worktrees::tests::{commit, fixture, repo};
    let dir = TempDir::new().unwrap();
    let env = env(&dir);
    let git = Git::find().unwrap();
    let (app, lib) = (env.home.join("app"), env.home.join("lib"));
    for main in [&app, &lib] {
        repo(main);
        env.store
            .register_repository(worktrees::Registration {
                common_dir: main.join(".git"),
                path: main.clone(),
            })
            .unwrap();
    }
    // A build the next build makes again, which Git ignores.
    commit(&app, ".gitignore", "target/\n");
    commit(&app, "Cargo.toml", "");
    let add = |main: &Path, name: &str| {
        let at = env.home.join(name);
        fixture(
            main,
            &["worktree", "add", "-q", "-b", name, at.to_str().unwrap()],
        );
        at
    };
    let (clean, other, dirty, unpushed, locked) = (
        add(&app, "app-clean"),
        add(&lib, "lib-clean"),
        add(&app, "app-dirty"),
        add(&app, "app-unpushed"),
        add(&app, "app-locked"),
    );
    std::fs::write(dirty.join("notes.md"), "").unwrap();
    commit(&unpushed, "b", "b\n");
    fixture(&app, &["worktree", "lock", locked.to_str().unwrap()]);
    let built = clean.join("target");
    std::fs::create_dir(&built).unwrap();
    std::fs::write(built.join("CACHEDIR.TAG"), TAG).unwrap();

    // Without the index, the registered repositories are still asked about.
    check_worktrees(&env, &git, |_, _| Err("The index isn't running.".into())).unwrap();
    let saved: Vec<PathBuf> = Saved::read(&env.saved())
        .worktrees
        .into_iter()
        .map(|worktree| worktree.path)
        .collect();
    assert_eq!(saved, [clean.clone(), other.clone()]);

    let pip = env.home.join("Library/Caches/pip");
    let yarn = env.home.join("Library/Caches/Yarn");
    for cache in [&pip, &yarn] {
        std::fs::create_dir_all(cache).unwrap();
    }
    save(&env, |saved| {
        saved.tagged.push(built.clone());
        saved.totals.insert(built.clone(), size(450, NOW));
        saved.totals.insert(pip.clone(), size(500, NOW));
        saved.totals.insert(yarn.clone(), size(400, NOW));
        saved.totals.insert(clean.clone(), size(300, NOW));
    });

    // One measured of two: at least what it has, and it says how far it got.
    let low = read(&env, &[startup(10_000, 1_000)]);
    assert!(low.due.contains(&other));
    let worktrees = &low.suggestions[2];
    assert_eq!(worktrees.title(), "Remove 2 clean worktrees");
    assert_eq!(
        worktrees.why(),
        "Measured 1 of 2. No changes or commits only they hold. In app and lib."
    );
    assert_eq!(
        (worktrees.size.unshared, worktrees.size.complete),
        (300, false)
    );
    assert_eq!(worktrees.trash(), None);
    assert_eq!(worktrees.worktrees(), [clean.clone(), other.clone()]);

    // Together they beat any one cache, though neither does alone. The build
    // inside a clean worktree goes with it, so it isn't counted twice.
    save(&env, |saved| {
        saved.totals.insert(other.clone(), size(300, NOW - 7_200));
    });
    let low = read(&env, &[startup(10_000, 1_000)]);
    let titles: Vec<String> = low.suggestions.iter().map(Suggestion::title).collect();
    assert_eq!(
        titles,
        [
            "Remove 2 clean worktrees",
            "Clear pip's cache",
            "Clear Yarn's cache"
        ]
    );
    let freed = low.suggestions[0].size;
    assert_eq!(
        (freed.unshared, freed.complete, freed.at),
        (600, true, NOW - 7_200)
    );
    assert!(read(&env, &[startup(10_000, 2_000)]).suggestions.is_empty());

    // Asked again, one that changed since drops out, index or not.
    std::fs::write(other.join("new"), "").unwrap();
    check_worktrees(&env, &git, |_, _| Ok(Vec::new())).unwrap();
    let saved = Saved::read(&env.saved()).worktrees;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].path, clean);
}
