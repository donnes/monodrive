use super::*;

/// A startup disk with `free` of 1,000,000 bytes free, for a home folder in
/// `world`'s temp folder.
fn disk(free: u64) -> Volume {
    Volume {
        total: 1_000_000,
        free,
        ..startup()
    }
}

fn on_the_page(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> bool {
    in_pane(browser, cx, |pane, _| pane.showing_overview())
}

#[gpui::test]
fn the_overview_is_a_place_in_the_history_and_the_session(cx: &mut TestAppContext) {
    let world = world(vec![disk(900_000)], cx);
    let (_dir, project) = folder();
    let (browser, cx) = open(&world, &project, cx);

    press(cx, "cmd-alt-o");
    assert!(on_screen(cx, "overview-page"));
    assert_eq!(in_pane(&browser, cx, |pane, cx| pane.title(cx)), "Overview");
    let shows = in_pane(&browser, cx, |pane, _| pane.stop().shows);
    assert_eq!(shows, Shows::Overview);

    // Commands that act on files don't reach what's under it.
    assert!(!in_pane(&browser, cx, |pane, cx| pane.takes_files(cx)));

    press(cx, "cmd-[");
    assert!(!on_the_page(&browser, cx));
    assert!(!on_screen(cx, "overview-page"));
    assert_eq!(here(&browser, cx), project);
    press(cx, "cmd-]");
    assert!(on_screen(cx, "overview-page"));
}

#[gpui::test]
fn airdrop_is_in_locations_before_the_drives_are_listed(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let (browser, cx) = browse(&project, cx);
    let locations = browser.read_with(cx, |b, _| b.sidebar.locations.clone());
    assert_eq!(locations, [Row::AirDrop]);
}

#[gpui::test]
fn the_sidebars_keys_reach_the_overview_above_recents(cx: &mut TestAppContext) {
    let world = world(vec![disk(900_000)], cx);
    let (_dir, project) = folder();
    let (browser, cx) = open(&world, &project, cx);
    browser.read_with(cx, |browser, _| {
        assert_eq!(browser.row_at(0), Some(Row::Overview));
        assert_eq!(browser.row_at(1), Some(Row::Recents));
    });
    press(cx, "cmd-alt-o");
    let row = browser.read_with(cx, |browser, cx| browser.cursor_on_folder(cx));
    assert_eq!(row, Some(0));
}

#[gpui::test]
fn a_row_opens_its_folder_and_back_returns_to_the_page(cx: &mut TestAppContext) {
    let world = world(vec![disk(900_000)], cx);
    let home = world.dir.path().canonicalize().unwrap();
    let documents = home.join("Documents");
    std::fs::create_dir(&documents).unwrap();
    std::fs::write(documents.join("a.txt"), vec![1; 64 * 1024]).unwrap();
    std::fs::create_dir(home.join("Pictures")).unwrap();
    let (browser, cx) = open(&world, &home, cx);
    press(cx, "cmd-alt-o");
    // The left column picks which kind the list shows, by a press or ↑ and ↓.
    press_on(cx, "overview-pick-Pictures");
    assert!(on_screen(cx, "overview-spot-Pictures"));
    assert!(!on_screen(cx, "overview-spot-Documents"));
    press(cx, "up");
    assert!(!on_screen(cx, "overview-spot-Pictures"));
    assert!(on_screen(cx, "overview-spot-Documents"));
    press_on(cx, "overview-spot-Documents");
    assert_eq!(here(&browser, cx), documents);
    assert!(!on_the_page(&browser, cx));
    press(cx, "cmd-[");
    assert!(on_the_page(&browser, cx));
}

#[gpui::test]
fn suggestions_show_when_the_disk_is_low_and_their_button_can_be_undone(cx: &mut TestAppContext) {
    let world = world(vec![disk(100_000)], cx);
    let owned = own(cx);
    let home = world.dir.path().canonicalize().unwrap();
    let cache = home.join("Library/Caches/pip");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("wheel"), vec![1; 64 * 1024]).unwrap();
    let (browser, cx) = open(&world, &home, cx);
    press(cx, "cmd-alt-o");
    // The first read found it due, and measuring it brought the suggestion.
    assert!(on_screen(cx, "overview-clean"));

    // This goes to the user's own Trash, and comes back out before the test
    // ends.
    press_on(cx, "overview-clean");
    assert!(!cache.exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Moved \"pip\" to the Trash".into(), false, true))
    );
    assert!(!on_screen(cx, "overview-clean"), "its proof is gone");
    click(cx, "toast-offer");
    assert!(cache.join("wheel").exists());
    release(owned, cx);
}

#[gpui::test]
fn a_low_disk_leads_with_what_can_be_freed_while_nothing_can_yet(cx: &mut TestAppContext) {
    let world = world(vec![disk(100_000)], cx);
    let home = world.dir.path().canonicalize().unwrap();
    std::fs::create_dir(home.join("Documents")).unwrap();
    let (_browser, cx) = open(&world, &home, cx);
    press(cx, "cmd-alt-o");
    assert!(on_screen(cx, "overview-pick-Can be freed"));
    assert!(!on_screen(cx, "overview-spot-Documents"));
}

#[gpui::test]
fn a_drive_shows_nothing_until_someone_asks_for_a_scan(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mount = dir.path().canonicalize().unwrap();
    std::fs::create_dir(mount.join("Photos")).unwrap();
    std::fs::write(mount.join("Photos/a.jpg"), vec![1; 64 * 1024]).unwrap();
    let backup = Volume {
        name: "Backup".into(),
        mount: mount.clone(),
        id: Some(VolumeId::Uuid("backup".into())),
        startup: false,
        ..disk(900_000)
    };
    let world = world(vec![disk(900_000), backup], cx);
    let home = world.dir.path().canonicalize().unwrap();
    let (browser, cx) = open(&world, &home, cx);
    press(cx, "cmd-alt-o");
    assert!(!on_screen(cx, "overview-scan"));
    press_on(cx, "overview-pick-Backup");
    assert!(on_screen(cx, "overview-scan"));
    assert!(!on_screen(cx, "overview-row-Photos"));

    press_on(cx, "overview-scan");
    assert!(on_screen(cx, "overview-row-Photos"));
    // A row opens its folder on the drive.
    press_on(cx, "overview-row-Photos");
    assert_eq!(here(&browser, cx), mount.join("Photos"));
}

/// Runs git in `dir` with no configuration but the repository's.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new(monodrive_core::worktrees::Git::find().unwrap().program)
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Monodrive")
        .env("GIT_AUTHOR_EMAIL", "test@monodrive.invalid")
        .env("GIT_COMMITTER_NAME", "Monodrive")
        .env("GIT_COMMITTER_EMAIL", "test@monodrive.invalid")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[gpui::test]
fn the_worktrees_page_opens_a_worktree_and_moves_one_to_the_trash_once_asked(
    cx: &mut TestAppContext,
) {
    let world = world(vec![disk(900_000)], cx);
    let owned = own(cx);
    let home = world.dir.path().canonicalize().unwrap();
    let (app, feature) = (home.join("app"), home.join("app-feature"));
    std::fs::create_dir(&app).unwrap();
    git(&app, &["init", "-q", "-b", "main"]);
    std::fs::write(app.join("a"), "a\n").unwrap();
    git(&app, &["add", "a"]);
    git(&app, &["commit", "-q", "-m", "a"]);
    git(
        &app,
        &["worktree", "add", "-q", "-b", "feature", "../app-feature"],
    );
    world
        .store
        .register_repository(monodrive_core::places::Registration {
            common_dir: app.join(".git"),
            path: app.clone(),
        })
        .unwrap();
    let (browser, cx) = open(&world, &home, cx);
    let asking = |cx: &mut VisualTestContext| {
        browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::RemoveWorktree(_))))
    };

    press(cx, "cmd-alt-g");
    assert!(on_screen(cx, "worktrees-page"));
    assert_eq!(
        in_pane(&browser, cx, |pane, cx| pane.title(cx)),
        "Worktrees"
    );
    assert!(on_screen(cx, "worktree-app") && on_screen(cx, "worktree-app-feature"));

    // ⌘↓ opens the selected worktree's folder, and Back returns to the page.
    press(cx, "down cmd-down");
    assert_eq!(here(&browser, cx), app);
    press(cx, "cmd-[");
    assert!(on_screen(cx, "worktrees-page"));
    press(cx, "cmd-alt-c");
    assert_eq!(
        toast(&browser, cx),
        Some(("Copied the path of \"app\"".into(), false, false))
    );

    // The repository's own checkout stays, and the card says why.
    press(cx, "cmd-backspace");
    assert!(asking(cx));
    press(cx, "enter");
    assert!(!asking(cx) && app.join(".git").is_dir());

    // A clean linked worktree goes once confirmed: Esc keeps it, ↩ moves it.
    press(cx, "down cmd-backspace");
    assert!(asking(cx));
    press(cx, "escape");
    assert!(!asking(cx) && feature.exists());
    press(cx, "cmd-backspace enter");
    assert!(!feature.exists() && !app.join(".git/worktrees/app-feature").exists());
    assert_eq!(
        toast(&browser, cx),
        Some(("Moved \"app-feature\" to the Trash".into(), false, true))
    );
    assert!(!on_screen(cx, "worktree-app-feature"));
    release(owned, cx);
}

#[gpui::test]
fn clean_worktrees_are_one_suggestion_whose_review_opens_the_worktrees_page(
    cx: &mut TestAppContext,
) {
    let world = world(vec![disk(100_000)], cx);
    let home = world.dir.path().canonicalize().unwrap();
    let app = home.join("app");
    std::fs::create_dir(&app).unwrap();
    git(&app, &["init", "-q", "-b", "main"]);
    std::fs::write(app.join("a"), "a\n").unwrap();
    git(&app, &["add", "a"]);
    git(&app, &["commit", "-q", "-m", "a"]);
    for name in ["app-clean", "app-dirty"] {
        git(
            &app,
            &["worktree", "add", "-q", "-b", name, &format!("../{name}")],
        );
    }
    std::fs::write(home.join("app-dirty/notes.md"), "").unwrap();
    // Found clean through the index before, which isn't ready now, and in no
    // registered repository.
    let clean = home.join("app-clean");
    let saved = serde_json::json!({
        "version": 1,
        "worktrees": [{"path": clean, "common_dir": app.join(".git"), "repository": "app"}],
    });
    let file = world.store.places.with_file_name("overview.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, serde_json::to_vec(&saved).unwrap()).unwrap();
    let (browser, cx) = open(&world, &home, cx);
    press(cx, "cmd-alt-o");
    // Git said it's still clean, and measuring it brought the suggestion.
    assert!(on_screen(cx, "overview-review-worktrees"));

    // Review… removes nothing: the Worktrees page does, under its own card.
    press_on(cx, "overview-review-worktrees");
    assert!(on_screen(cx, "worktrees-page"));
    let (selected, suggested) = in_pane(&browser, cx, |pane, _| {
        let page = &pane.worktrees;
        (
            page.selected().map(Path::to_path_buf),
            page.suggested.clone(),
        )
    });
    assert_eq!(selected.as_ref(), Some(&clean));
    assert_eq!(suggested, std::slice::from_ref(&clean));
    assert!(clean.exists());

    // Back returns to the Overview, and opening the page another way drops
    // the marks.
    press(cx, "cmd-[");
    assert!(on_the_page(&browser, cx));
    press(cx, "cmd-alt-g");
    let suggested = in_pane(&browser, cx, |pane, _| pane.worktrees.suggested.clone());
    assert!(suggested.is_empty());
}

#[gpui::test]
fn removing_a_worktree_keeps_the_list_where_it_was_scrolled(cx: &mut TestAppContext) {
    let world = world(vec![disk(900_000)], cx);
    let owned = own(cx);
    let home = world.dir.path().canonicalize().unwrap();
    let app = home.join("app");
    std::fs::create_dir(&app).unwrap();
    git(&app, &["init", "-q", "-b", "main"]);
    std::fs::write(app.join("a"), "a\n").unwrap();
    git(&app, &["add", "a"]);
    git(&app, &["commit", "-q", "-m", "a"]);
    let names: Vec<String> = (0..12).map(|n| format!("w{n:02}")).collect();
    for name in &names {
        git(
            &app,
            &["worktree", "add", "-q", "-b", name, &format!("../{name}")],
        );
    }
    world
        .store
        .register_repository(monodrive_core::places::Registration {
            common_dir: app.join(".git"),
            path: app.clone(),
        })
        .unwrap();
    let (browser, cx) = open(&world, &home, cx);
    cx.simulate_resize(gpui::size(px(800.), px(300.)));
    press(cx, "cmd-alt-g");
    // Far enough down that the list has scrolled.
    press(cx, &["down"; 10].join(" "));
    let tops = |cx: &mut VisualTestContext| -> Vec<(String, Pixels)> {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
        let mut tops: Vec<_> = names
            .iter()
            .filter_map(|name| {
                let selector = format!("worktree-{name}").leak();
                Some((name.clone(), cx.debug_bounds(selector)?.top()))
            })
            .collect();
        tops.sort_by(|a, b| a.1.as_f32().total_cmp(&b.1.as_f32()));
        tops
    };
    let before = tops(cx);
    assert!(
        cx.debug_bounds("worktree-app").is_none(),
        "the list scrolled"
    );
    let selected = in_pane(&browser, cx, |pane, _| {
        pane.worktrees.selected().map(Path::to_path_buf)
    })
    .unwrap();
    let gone = selected.file_name().unwrap().to_string_lossy().into_owned();
    let at = before.iter().position(|(name, _)| *name == gone).unwrap();
    let (next, _) = &before[at + 1];

    press(cx, "cmd-backspace enter");
    assert!(!selected.exists());
    // The row below takes the removed one's place, where the eye was.
    let after = tops(cx);
    let moved = after
        .iter()
        .find(|(name, _)| name == next)
        .map(|(_, top)| *top);
    assert_eq!(moved, Some(before[at].1));
    release(owned, cx);
}

#[gpui::test]
fn worktrees_has_a_sidebar_row_only_once_settings_shows_it(cx: &mut TestAppContext) {
    let world = world(vec![disk(900_000)], cx);
    let (_dir, project) = folder();
    let (browser, cx) = open(&world, &project, cx);
    let row = |browser: &Entity<Browser>, cx: &mut VisualTestContext| {
        browser.read_with(cx, |browser, _| browser.row_at(2))
    };
    assert!(matches!(row(&browser, cx), Some(Row::Favorite(0))));
    // ⌥⌘G opens the page either way.
    press(cx, "cmd-alt-g");
    assert!(on_screen(cx, "worktrees-page"));
    let cursor = browser.read_with(cx, |browser, cx| browser.cursor_on_folder(cx));
    assert_eq!(cursor, None);

    cx.update(|_, cx| preferences::update(cx, |p| p.show_worktrees = true));
    cx.run_until_parked();
    assert_eq!(row(&browser, cx), Some(Row::Worktrees));
    let cursor = browser.read_with(cx, |browser, cx| browser.cursor_on_folder(cx));
    assert_eq!(cursor, Some(2));
    assert!(matches!(
        browser.read_with(cx, |browser, _| browser.row_at(3)),
        Some(Row::Favorite(0))
    ));
}

#[gpui::test]
fn column_view_lists_a_folder_behind_a_privacy_prompt_only_when_asked(cx: &mut TestAppContext) {
    let world = world(vec![startup()], cx);
    let project = world.dir.path().canonicalize().unwrap().join("project");
    let private = project.join("Private");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::write(private.join("secret.txt"), "").unwrap();
    std::fs::create_dir(project.join("Public")).unwrap();
    world.fake.gated.lock().unwrap().push(private.clone());
    let (browser, cx) = open(&world, &project, cx);
    let listed = |dir: &Path, cx: &mut VisualTestContext| {
        in_pane(&browser, cx, |pane, _| pane.children.contains_key(dir))
    };
    // Resting on it asks nothing of the disk; the last column offers Open.
    press(cx, "cmd-alt-3 down");
    cx.executor()
        .advance_clock(crate::browser::facts::READ_DELAY);
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).1, ["Private"]);
    assert!(!listed(&private, cx));
    let open = cx
        .debug_bounds("column-open")
        .expect("Open in the last column");
    cx.simulate_click(open.center(), Modifiers::none());
    cx.executor()
        .advance_clock(crate::browser::facts::READ_DELAY);
    cx.run_until_parked();
    assert!(listed(&private, cx));
    // Its neighbor lists as soon as the selection rests on it.
    press(cx, "down");
    cx.executor()
        .advance_clock(crate::browser::facts::READ_DELAY);
    cx.run_until_parked();
    assert!(listed(&project.join("Public"), cx));
}

#[gpui::test]
fn nothing_is_extracted_into_the_trash_and_pages_from_an_archive_start_on_the_disk(
    cx: &mut TestAppContext,
) {
    use std::io::Write;
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let trash = cx.update(|cx| cx.global::<Places>().trash.clone());
    for folder in [&trash, &project] {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(folder.join("a.zip")).unwrap());
        zip.start_file("a.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"a").unwrap();
        zip.finish().unwrap();
    }
    let (browser, cx) = open(&world, &project, cx);
    let can_extract =
        |cx: &mut VisualTestContext| in_pane(&browser, cx, |pane, cx| pane.can_extract(cx));
    // Recents in front of an archive starts at the folder holding it, so
    // its path never leaves for another app.
    press(cx, "down down down down");
    assert_eq!(shown(&browser, cx).1, ["a.zip"]);
    press(cx, "cmd-down");
    assert!(in_pane(&browser, cx, |pane, _| pane.showing_archive()));
    assert!(can_extract(cx));
    cx.dispatch_action(GoRecents);
    assert_eq!(here(&browser, cx), project);
    // In the Trash, an archive neither extracts beside itself nor from
    // inside.
    cx.dispatch_action(GoTrash);
    press(cx, "down");
    assert_eq!(shown(&browser, cx).1, ["a.zip"]);
    assert!(!can_extract(cx));
    press(cx, "cmd-down");
    assert!(in_pane(&browser, cx, |pane, _| pane.showing_archive()));
    assert!(!can_extract(cx));
}
