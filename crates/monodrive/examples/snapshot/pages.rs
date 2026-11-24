//! The pages: AirDrop, Worktrees, Find Duplicates and the Overview, each over a
//! made-up fixture.

use super::*;

/// The Worktrees page over made-up repositories: `worktrees.png` with a row
/// selected and its changes read, `worktrees-empty.png` with none, and the
/// card that asks before one goes: `worktrees-remove-clean.png`,
/// `-dirty`, `-unpushed`, `-detached`, `-forget` for a missing folder, and
/// `-locked` and `-main`, which it refuses. Nothing is removed.
/// AirDrop's page, `airdrop.png`.
pub(super) fn shoot_airdrop(cx: &mut Cx, out: &Path, folder: &Path) -> gpui::Result<()> {
    cx.update(|cx| {
        cx.set_global(AirDrop {
            me: "Sam Rivera".into(),
            discoverable: Some(Discoverable::ContactsOnly),
            live: false,
        })
    });
    let window = open(cx, folder)?;
    dispatch(cx, window, &GoAirDrop)?;
    save(cx, window, out, "airdrop")
}

pub(super) fn shoot_worktrees(
    cx: &mut Cx,
    out: &Path,
    folder: &Path,
    base: &Preferences,
) -> gpui::Result<()> {
    // With its sidebar row, which Settings turns on.
    cx.update(|cx| {
        cx.set_global(Preferences {
            show_worktrees: true,
            ..base.clone()
        })
    });
    let real_home = std::env::var_os("HOME");
    for empty in [false, true] {
        let dir = tempfile::tempdir()?;
        let home = dir.path().canonicalize()?;
        let places = worktrees_fixture(&home, empty)?;
        cx.update(|cx| cx.set_global(places));
        // SAFETY: the headless app is parked between shots, and nothing else
        // in this process reads the environment while it changes.
        unsafe { std::env::set_var("HOME", &home) };
        let window = open(cx, folder)?;
        dispatch(cx, window, &GoWorktrees)?;
        settle_git(cx);
        if empty {
            save(cx, window, out, "worktrees-empty")?;
        } else {
            // The rows in order: monocode, mc-popover-port, monodrive, then
            // monodrive's worktrees as Git lists them, by name.
            let shots = [
                (3, "worktrees", false),
                (3, "worktrees-remove-dirty", true),
                (4, "worktrees-remove-unpushed", true),
                (5, "worktrees-remove-locked", true),
                (6, "worktrees-remove-forget", true),
                (7, "worktrees-remove-detached", true),
                (2, "worktrees-remove-main", true),
                (1, "worktrees-remove-clean", true),
            ];
            let mut at: Option<usize> = None;
            for (row, name, ask) in shots {
                while at != Some(row) {
                    let down = at.is_none_or(|at| at < row);
                    let step: &dyn Action = if down { &SelectNext } else { &SelectPrevious };
                    dispatch(cx, window, step)?;
                    at = match at {
                        None => Some(0),
                        Some(at) if down => Some(at + 1),
                        Some(at) => Some(at - 1),
                    };
                }
                settle_git(cx);
                if ask {
                    dispatch(cx, window, &MoveToTrash)?;
                    settle_git(cx);
                }
                save(cx, window, out, name)?;
                if ask {
                    let cancel = named(cx, "remove_worktree::Cancel")?;
                    dispatch(cx, window, &*cancel)?;
                }
            }
        }
        cx.update_window(window.into(), |_, window, _| window.remove_window())?;
        cx.run_until_parked();
        if let Some(home) = &real_home {
            // SAFETY: as above.
            unsafe { std::env::set_var("HOME", home) };
        }
    }
    Ok(())
}

/// Find Duplicates over a made-up Downloads: `duplicates.png` with a group
/// open and a copy chosen, `duplicates-review.png` asking before it moves,
/// `duplicates-empty.png` with no copies of 1 MB and up, and
/// `duplicates-error.png` for a folder that went. Nothing is moved.
pub(super) fn shoot_duplicates(cx: &mut Cx, out: &Path, base: &Preferences) -> gpui::Result<()> {
    const MB: usize = 1_000_000;
    cx.update(|cx| cx.set_global(base.clone()));
    let dir = tempfile::tempdir()?;
    let home = dir.path().canonicalize()?;
    let downloads = home.join("Downloads");
    let copies: [(&str, &[&str], usize); 4] = [
        (
            "Sequoia trip.mov",
            &["", "Videos to edit", "Shared from Sam"],
            9 * MB,
        ),
        ("Xcode_16.2.xip", &["", "Old installers"], 12 * MB),
        ("Figma-126.4.dmg", &["", "Old installers"], 4 * MB),
        ("IMG_2041.HEIC", &["", "AirDrop"], 2 * MB),
    ];
    for (fill, (name, folders, size)) in copies.into_iter().enumerate() {
        let bytes = vec![fill as u8 + 1; size];
        for folder in folders {
            let at = downloads.join(folder);
            std::fs::create_dir_all(&at)?;
            std::fs::write(at.join(name), &bytes)?;
        }
    }
    std::fs::write(downloads.join("Notes.txt"), "a few words")?;
    let empty = home.join("Documents");
    std::fs::create_dir_all(&empty)?;
    std::fs::write(empty.join("Résumé.pdf"), vec![1u8; 2 * MB])?;
    std::fs::write(empty.join("Taxes 2025.pdf"), vec![2u8; 3 * MB])?;
    // Never made, as a folder that went would be.
    let gone = home.join("Gone");

    let real_home = std::env::var_os("HOME");
    // SAFETY: the headless app is parked between shots, and nothing else in
    // this process reads the environment while it changes.
    unsafe { std::env::set_var("HOME", &home) };
    let window = open(cx, &downloads)?;
    dispatch(cx, window, &FindDuplicates)?;
    settle_git(cx);
    for step in [
        &SelectNext as &dyn Action,
        &ExpandFolder,
        &SelectNext,
        &SelectNext,
        &QuickLook,
        &SelectNext,
    ] {
        dispatch(cx, window, step)?;
    }
    save(cx, window, out, "duplicates")?;
    dispatch(cx, window, &MoveToTrash)?;
    settle_git(cx);
    save(cx, window, out, "duplicates-review")?;
    let cancel = named(cx, "review_duplicates::Cancel")?;
    dispatch(cx, window, &*cancel)?;
    cx.update_window(window.into(), |_, window, _| window.remove_window())?;

    let window = open(cx, &empty)?;
    dispatch(cx, window, &FindDuplicates)?;
    settle_git(cx);
    save(cx, window, out, "duplicates-empty")?;
    cx.update_window(window.into(), |_, window, _| window.remove_window())?;

    let window = open(cx, &gone)?;
    dispatch(cx, window, &FindDuplicates)?;
    settle_git(cx);
    save(cx, window, out, "duplicates-error")?;
    cx.update_window(window.into(), |_, window, _| window.remove_window())?;
    cx.run_until_parked();
    if let Some(home) = &real_home {
        // SAFETY: as above.
        unsafe { std::env::set_var("HOME", home) };
    }
    Ok(())
}

/// Lets the git the page runs on the background threads answer, and a card
/// fade in. Headless, those threads run for real, so parking alone doesn't
/// wait for them.
pub(super) fn settle_git(cx: &mut Cx) {
    settle();
    cx.run_until_parked();
    settle();
    cx.run_until_parked();
}

/// Runs git with no configuration but the repository's, on a fixed date.
pub(super) fn git(dir: &Path, args: &[&str]) -> gpui::Result<()> {
    let date = "2026-11-01T12:00:00Z";
    let out = std::process::Command::new("/usr/bin/git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Monodrive")
        .env("GIT_AUTHOR_EMAIL", "snapshot@monodrive.invalid")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_NAME", "Monodrive")
        .env("GIT_COMMITTER_EMAIL", "snapshot@monodrive.invalid")
        .env("GIT_COMMITTER_DATE", date)
        .output()?;
    if !out.status.success() {
        let why = format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        return Err(std::io::Error::other(why).into());
    }
    Ok(())
}

pub(super) fn commit(dir: &Path, file: &str, message: &str) -> gpui::Result<()> {
    std::fs::write(dir.join(file), message)?;
    git(dir, &["add", file])?;
    git(dir, &["commit", "-q", "-m", message])
}

/// Two registered repositories with worktrees in every state the page shows.
pub(super) fn worktrees_fixture(home: &Path, empty: bool) -> gpui::Result<Places> {
    let support = home.join("Library/Application Support/Monodrive");
    std::fs::create_dir_all(&support)?;
    let store = Store {
        places: support.join("places.json"),
        recents: support.join("recents.json"),
    };
    let trash = home.join(".Trash");
    std::fs::create_dir_all(&trash)?;
    let places = Places {
        store: store.clone(),
        trash,
        applications: Vec::new(),
        drives: Arc::new(MadeUp),
        remote: Arc::new(Connections::new(store.clone(), local_sftp())),
        previews: support.join("previews"),
        root: support.join("disk"),
    };
    if empty {
        return Ok(places);
    }
    let projects = home.join("Projects");
    let linked = home.join(".t3/worktrees/monodrive");
    std::fs::create_dir_all(&linked)?;
    for name in ["monodrive", "monocode"] {
        let repo = projects.join(name);
        std::fs::create_dir_all(&repo)?;
        git(&repo, &["init", "-q", "-b", "main"])?;
        commit(&repo, "README.md", "first commit")?;
        let registration = monodrive_core::places::Registration {
            common_dir: repo.join(".git"),
            path: repo.clone(),
        };
        store
            .register_repository(registration)
            .map_err(std::io::Error::other)?;
    }
    let monodrive = projects.join("monodrive");
    let add = |args: &[&str]| git(&monodrive, &[&["worktree", "add", "-q"][..], args].concat());
    let at = |name: &str| linked.join(name).display().to_string();
    add(&["-b", "feat/archives-ui", &at("feat-archives-ui")])?;
    add(&["-b", "feat/column-view", &at("feat-column-view")])?;
    add(&["--detach", &at("t3-1c1e18ad")])?;
    add(&["-b", "fix/preview-trim", &at("t3-0f7b3c85")])?;
    add(&[
        "-b",
        "feat/worktree-management",
        &at("feat-worktree-management"),
    ])?;
    git(
        &monodrive,
        &[
            "worktree",
            "lock",
            "--reason",
            "t3 session",
            &at("feat-worktree-management"),
        ],
    )?;
    let archives = linked.join("feat-archives-ui");
    for (file, message) in [
        ("archives.rs", "feat(archives): browse a ZIP like a folder"),
        ("browser.rs", "feat(archives): add the Extract buttons"),
    ] {
        commit(&archives, file, message)?;
        std::fs::write(archives.join(file), "changed")?;
    }
    std::fs::write(archives.join("notes.md"), "")?;
    let columns = linked.join("feat-column-view");
    commit(&columns, "columns.rs", "feat(columns): first column view")?;
    commit(
        &columns,
        "preview.rs",
        "feat(columns): preview in the last column",
    )?;
    commit(
        &linked.join("t3-1c1e18ad"),
        "tips.rs",
        "wip: tip names for icon buttons",
    )?;
    std::fs::rename(linked.join("t3-0f7b3c85"), home.join("moved-away"))?;
    let monocode = projects.join("monocode");
    git(
        &monocode,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "mc/popover-port",
            "../mc-popover-port",
        ],
    )?;
    Ok(places)
}

/// The Overview, from saved totals on a made-up Mac: `overview-low.png` under
/// 20% free, with suggestions, a scanned drive, an unscanned share and a drive
/// that's gone; `overview-roomy.png` over it; `overview-locations.png` with the
/// drives on screen, `overview-one-drive.png` with nothing else to say, and
/// the low one again in a 900×600 window, the smallest, 720×440, and a wide
/// 1600×900. `overview-review.png` is the Worktrees page that the low one's
/// Review… opens, its clean worktrees marked.
pub(super) fn shoot_overview(
    cx: &mut Cx,
    out: &Path,
    folder: &Path,
    base: &Preferences,
) -> gpui::Result<()> {
    const GB: u64 = 1_000_000_000;
    cx.update(|cx| cx.set_global(base.clone()));
    let startup = |free: u64| Volume {
        mount: "/".into(),
        startup: true,
        system: Some(15 * GB),
        ..disk("Macintosh HD", 460 * GB, free * GB)
    };
    let backup = disk("Backup", 2000 * GB, 340 * GB);
    let media = Volume {
        network: true,
        id: Some(VolumeId::Url("smb://nas.local/media".into())),
        ..disk("media", 8000 * GB, 5100 * GB)
    };
    let cases = [
        (
            "overview-low",
            vec![startup(44), backup.clone(), media.clone()],
            true,
            true,
        ),
        (
            "overview-roomy",
            vec![startup(178), backup.clone(), media.clone()],
            true,
            true,
        ),
        (
            "overview-locations",
            vec![startup(178), backup.clone(), media.clone()],
            true,
            false,
        ),
        ("overview-one-drive", vec![startup(178)], false, true),
        // The smallest window macOS lets it shrink to, and one in between.
        (
            "overview-smallest",
            vec![startup(44), backup.clone(), media.clone()],
            true,
            true,
        ),
        (
            "overview-medium",
            vec![startup(44), backup.clone(), media.clone()],
            true,
            true,
        ),
        (
            "overview-wide",
            vec![startup(44), backup.clone(), media.clone()],
            true,
            true,
        ),
    ];
    // Paths under the made-up home read as they would on a Mac, from `~`.
    let real_home = std::env::var_os("HOME");
    for (name, volumes, more, folders) in cases {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        std::fs::create_dir(&home)?;
        let places = overview_fixture(&home, &dir.path().join("disk"), volumes, more, folders)?;
        cx.update(|cx| cx.set_global(places));
        let window = match name {
            "overview-smallest" => open_at(cx, folder, 720., 440.)?,
            "overview-medium" => open_at(cx, folder, 900., 600.)?,
            "overview-wide" => open_at(cx, folder, 1600., 900.)?,
            _ => open(cx, folder)?,
        };
        // SAFETY: the headless app is parked between shots, and nothing else
        // in this process reads the environment while it changes.
        unsafe { std::env::set_var("HOME", home.canonicalize()?) };
        dispatch(cx, window, &GoOverview)?;
        save(cx, window, out, name)?;
        if name == "overview-low" {
            // Review… on the worktrees' row, the third of Free up.
            let review = point(px(1038.), px(321.));
            let none = Modifiers::default();
            let away = point(px(120.), px(680.));
            let click = vec![
                press_at(review, none),
                release_at(review, none),
                move_to(away),
            ];
            mouse(cx, window, click)?;
            settle_git(cx);
            save(cx, window, out, "overview-review")?;
        }
        // Before its fixture goes, so nothing reads a folder that's gone.
        cx.update_window(window.into(), |_, window, _| window.remove_window())?;
        cx.run_until_parked();
        if let Some(home) = &real_home {
            // SAFETY: as above.
            unsafe { std::env::set_var("HOME", home) };
        }
    }
    Ok(())
}

pub(super) fn disk(name: &str, total: u64, free: u64) -> Volume {
    Volume {
        name: name.into(),
        mount: Path::new("/Volumes").join(name),
        disk: None,
        id: Some(VolumeId::Uuid(name.into())),
        total,
        free,
        read_only: false,
        network: false,
        ejectable: true,
        removable: true,
        startup: false,
        system: None,
    }
}

/// A made-up home and startup disk with what the Overview looks for, and saved
/// totals that are fresh, so the sizes drawn are these and not the fixture's
/// few bytes.
pub(super) fn overview_fixture(
    home: &Path,
    root: &Path,
    volumes: Vec<Volume>,
    more: bool,
    folders: bool,
) -> gpui::Result<Places> {
    const GB: u64 = 1_000_000_000;
    let home = home.canonicalize()?;
    std::fs::create_dir(root)?;
    let root = root.canonicalize()?;
    let trash = home.join(".Trash");
    std::fs::create_dir(&trash)?;
    let store = Store {
        places: home.join("places.json"),
        recents: home.join("recents.json"),
    };
    // The usual favorites, with the real home's folders this one lacks:
    // `HOME` points here while the shot is drawn.
    let real = std::env::var_os("HOME").map(PathBuf::from);
    let favorites: Vec<_> = std::iter::once(home.clone())
        .chain(
            real.iter()
                .flat_map(|real| ["Desktop", "Documents", "Downloads"].map(|name| real.join(name))),
        )
        .chain([PathBuf::from("/Applications")])
        .map(|path| serde_json::json!({ "path": path }))
        .collect();
    std::fs::write(
        &store.places,
        serde_json::to_vec(&serde_json::json!({ "favorites": favorites }))?,
    )?;
    let now = monodrive_core::overview::now();
    let size = |gb: u64, at: u64, complete: bool| {
        serde_json::json!({
            "on_disk": gb * GB, "unshared": gb * GB, "at": at, "complete": complete
        })
    };
    let mut totals = serde_json::Map::new();
    let mut total = |path: &Path, gb: u64| {
        totals.insert(path.display().to_string(), size(gb, now, true));
    };
    for (name, gb) in [
        ("Documents", 62),
        ("Downloads", 28),
        ("Pictures", 41),
        ("Movies", 17),
    ]
    .into_iter()
    .filter(|_| folders)
    {
        std::fs::create_dir(home.join(name))?;
        total(&home.join(name), gb);
    }
    let project = home.join("Work/monodrive");
    let target = project.join("target");
    std::fs::create_dir_all(&target)?;
    std::fs::write(project.join("Cargo.toml"), "")?;
    std::fs::write(
        target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )?;
    git(&project, &["init", "-q", "-b", "main"])?;
    git(
        &project,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:you/monodrive.git",
        ],
    )?;
    store
        .register_repository(monodrive_core::places::Registration {
            common_dir: project.join(".git"),
            path: project.clone(),
        })
        .map_err(std::io::Error::other)?;
    total(&project, 48);
    total(&target, 38);
    let cache = home.join("Library/Developer/Xcode/DerivedData");
    std::fs::create_dir_all(&cache)?;
    total(&cache, 21);
    let brew = home.join("Library/Caches/Homebrew");
    std::fs::create_dir_all(&brew)?;
    total(&brew, 6);
    total(&trash, 9);
    // Clean worktrees in two repositories, as Git confirms when the page asks.
    let mut clean = Vec::new();
    let repositories: [(&str, &[(&str, u64)]); 2] = [
        (
            "monocode",
            &[("mc-popover-port", 3), ("mc-settings", 3), ("mc-theme", 2)],
        ),
        ("t3code", &[("t3-1c1e18ad", 2), ("t3-0f7b3c85", 2)]),
    ];
    for (name, worktrees) in repositories {
        let repo = home.join("Work").join(name);
        std::fs::create_dir_all(&repo)?;
        git(&repo, &["init", "-q", "-b", "main"])?;
        commit(&repo, "README.md", "first commit")?;
        store
            .register_repository(monodrive_core::places::Registration {
                common_dir: repo.join(".git"),
                path: repo.clone(),
            })
            .map_err(std::io::Error::other)?;
        total(&repo, 1);
        for (worktree, gb) in worktrees {
            let at = home.join(".t3/worktrees").join(name).join(worktree);
            let shown = at.display().to_string();
            git(&repo, &["worktree", "add", "-q", "-b", worktree, &shown])?;
            total(&at, *gb);
            clean.push(serde_json::json!({
                "path": at, "common_dir": repo.join(".git"), "repository": name,
            }));
        }
    }
    // Where the rest of the disk goes.
    for (path, gb) in [
        (root.join("Applications"), 12),
        (root.join("Library"), 4),
        (home.join("Library/Application Support"), 6),
        (home.join("Library/Caches"), 8),
        (home.join("Library/Developer"), 24),
        (home.join(".t3"), 14),
        (home.join("Work"), 50),
    ] {
        std::fs::create_dir_all(&path)?;
        total(&path, gb);
    }
    let scans = if more {
        store
            .save_server("smb://studio.local/projects")
            .map_err(std::io::Error::other)?;
        store
            .add_remote("build-box", None, None)
            .map_err(std::io::Error::other)?;
        let row = |name: &str, gb: u64, remote: Option<&str>| {
            serde_json::json!({
                "name": name,
                "path": format!("/Volumes/Backup/{name}"),
                "size": size(gb, now - 2 * 86_400, true),
                "remote": remote,
            })
        };
        vec![
            serde_json::json!({
                "volume": {"id": {"uuid": "Backup"}},
                "name": "Backup",
                "total": size(1600, now - 2 * 86_400, true),
                "rows": [
                    row("Photos", 900, None),
                    row("Archive", 512, None),
                    row("monodrive-copy", 44, Some("github.com/you/monodrive")),
                ],
            }),
            serde_json::json!({
                "volume": {"id": {"uuid": "Old Drive"}},
                "name": "Old Drive",
                "total": size(240, now - 40 * 86_400, false),
                "rows": [{
                    "name": "Projects",
                    "path": "/Volumes/Old Drive/Projects",
                    "size": size(240, now - 40 * 86_400, false),
                }],
            }),
        ]
    } else {
        Vec::new()
    };
    let saved = serde_json::json!({
        "version": 1,
        "totals": totals,
        "scans": scans,
        "worktrees": clean,
    });
    std::fs::write(home.join("overview.json"), serde_json::to_vec(&saved)?)?;
    Ok(Places {
        remote: Arc::new(Connections::new(store.clone(), local_sftp())),
        previews: home.join("previews"),
        store,
        trash,
        applications: Vec::new(),
        drives: Arc::new(Disks(volumes)),
        root,
    })
}

/// `MadeUp`, with these volumes and no cloud folders.
pub(super) struct Disks(Vec<Volume>);

impl Drives for Disks {
    fn list(&self) -> Vec<Volume> {
        self.0.clone()
    }

    fn eject(&self, mount: &Path, ops: &mut ops::Client) -> Result<String, String> {
        MadeUp.eject(mount, ops)
    }

    fn mount(&self, address: &str, cancel: &Arc<Cancel>) -> Result<PathBuf, String> {
        MadeUp.mount(address, cancel)
    }

    fn open(&self, _: &Path, _: &gpui::App) {}

    fn needs_consent(&self, _: &Path) -> bool {
        false
    }

    fn cloud(&self) -> Vec<CloudFolder> {
        Vec::new()
    }

    fn cloud_free(&self, _: &CloudFolder) -> Option<Quota> {
        None
    }

    fn find_files(
        &self,
        _: &OnceLock<Result<Index, String>>,
        _: &str,
        _: usize,
    ) -> Result<Vec<Entry>, String> {
        Ok(Vec::new())
    }

    fn search<'a>(
        &self,
        started: &dyn Fn() -> Result<&'a Index, String>,
        search: &Search,
        limit: usize,
    ) -> Result<Found, String> {
        MadeUp.search(started, search, limit)
    }
}
