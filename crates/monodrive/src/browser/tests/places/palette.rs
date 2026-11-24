use super::*;

#[gpui::test]
fn command_k_and_shift_command_p_open_the_palette_and_rebinding_moves_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    for keys in ["cmd-k", "shift-cmd-p"] {
        press(cx, keys);
        palette(&browser, cx);
        press(cx, "escape");
        assert!(browser.read_with(cx, |b, _| b.sheet.is_none()), "{keys}");
    }
    press(cx, "cmd-shift-k");
    assert!(browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::Connect(_)))));
    press(cx, "escape");
    cx.update(|_, cx| {
        preferences::update(cx, |p| {
            p.shortcuts
                .insert(OpenPalette.name().into(), Some("ctrl-j".into()));
        });
        crate::apply_keymap(cx);
    });
    press(cx, "cmd-k");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    press(cx, "ctrl-j");
    palette(&browser, cx);
}

#[gpui::test]
fn with_nothing_typed_the_palette_shows_its_sections_and_what_would_run(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    let shown = sections(&view, cx);
    let names: Vec<&str> = shown.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, ["Actions", "Go to", "View", "Settings"]);
    assert_eq!(
        shown[0].1,
        [
            "New Folder",
            "New File",
            "Paste",
            "Go to Folder…",
            "Connect to Server…",
            "Undo",
            "Empty Trash…"
        ]
    );
    assert_eq!(
        shown[1].1,
        [
            "Remote locations",
            "Cloud storage",
            "Drives",
            "Favorites",
            "Overview",
            "Recents",
            "Worktrees",
            "Trash"
        ]
    );
    assert_eq!(shown[2].1[..2], ["as List", "as Grid"]);
    assert_eq!(shown[3].1, ["Settings…"]);
    // Only what trashes or erases files is marked.
    let marked: Vec<String> = view.read_with(cx, |view, _| {
        let rows = view.rows.iter().filter(|row| row.destructive);
        rows.map(|row| row.title.to_string()).collect()
    });
    assert_eq!(marked, ["Empty Trash…"]);
    // The arrows go round the ends and skip what wouldn't run.
    assert_eq!(under_keys(&view, cx).as_deref(), Some("New Folder"));
    press(cx, "up");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("Settings…"));
    press(cx, "down");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("New Folder"));
    press(cx, "escape");

    // In the Trash nothing new can be made, so those rows are dimmed.
    press(cx, "cmd-k");
    type_in(cx, "trash");
    press(cx, "enter");
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    let dimmed: Vec<String> = view.read_with(cx, |view, _| {
        let rows = view.rows.iter().filter(|row| !row.enabled);
        rows.map(|row| row.title.to_string()).collect()
    });
    assert_eq!(dimmed, ["New Folder", "New File", "Paste"]);
    press(cx, "down");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("Connect to Server…"));
}

#[gpui::test]
fn a_selections_actions_are_what_its_context_menu_offers(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "down down down");
    let list = in_pane(&browser, cx, |b, _| b.list_focus.clone());
    let mut offered: Vec<&str> = cx.update(|window, cx| {
        commands::context_menu(&[Scope::Selection], cx.global::<Preferences>(), |action| {
            window.is_action_available_in(action, &list)
        })
        .iter()
        .map(|row| row.title)
        .collect()
    });
    offered.sort();
    assert!(offered.contains(&"Duplicate"));
    assert!(!offered.contains(&"Put Back"));
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    let actions = sections(&view, cx).remove(0).1;
    assert_eq!(actions[0], "Open");
    let fixed = actions.iter().position(|t| t == "New Folder").unwrap();
    assert_eq!(actions[fixed - 1], "Move to Trash");
    let mut chosen: Vec<&str> = actions[..fixed].iter().map(String::as_str).collect();
    chosen.sort();
    assert_eq!(chosen, offered);
}

#[gpui::test]
fn typing_ranks_commands_places_and_recents_together(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let project = project.canonicalize().unwrap();
    world.store.opened(&project.join("a.txt")).unwrap();
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "trash");
    let shown = titles(&view, cx);
    assert_eq!(shown[0], "Trash");
    assert!(shown.contains(&"Empty Trash…".to_string()));
    // Move to Trash is found but wouldn't run, so the keys skip it.
    assert!(shown.contains(&"Move to Trash".to_string()));
    press(cx, "cmd-a backspace");
    type_in(cx, "cloud");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("Cloud storage"));
    press(cx, "cmd-a backspace");
    // A recent, with where it is under it.
    type_in(cx, "a.txt");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("a.txt"));
    let about = view.read_with(cx, |v, _| v.rows[0].subtitle.clone());
    assert_eq!(
        about.as_deref(),
        Some(project.display().to_string().as_str())
    );
    press(cx, "enter");
    assert_eq!(*world.fake.opened.lock().unwrap(), [project.join("a.txt")]);
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    // A favorite the Go menu also reaches shows once, with its shortcut.
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "downloads");
    let downloads = titles(&view, cx);
    assert_eq!(
        downloads.iter().filter(|t| *t == "Downloads").count(),
        1,
        "{downloads:?}"
    );
    press(cx, "cmd-a backspace");
    type_in(cx, "zzzzqq");
    assert!(titles(&view, cx).is_empty());
}

#[gpui::test]
fn enter_runs_duplicate_on_the_selection_and_focus_comes_back(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down");
    press(cx, "cmd-k");
    cx.simulate_input("duplicate");
    cx.run_until_parked();
    let view = palette(&browser, cx);
    assert_eq!(under_keys(&view, cx).as_deref(), Some("Duplicate"));
    // Opening and typing leave the selection alone.
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    press(cx, "enter");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    let list = in_pane(&browser, cx, |b, _| b.list_focus.clone());
    let focused = cx.update(|window, _| list.is_focused(window));
    assert!(focused);
    assert!(project.join("a copy.txt").exists());
    release(owned, cx);
}

#[gpui::test]
fn a_paste_finishing_under_the_palette_stops_move_to_trash(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let owned = own(cx);
    let (browser, cx) = browse(&project, cx);
    press(cx, "down down down cmd-c cmd-k");
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    // It was asked for before the palette opened, and its job ends now.
    let pane = pane_of(&browser, cx);
    pane.update_in(cx, |pane, window, cx| pane.paste(false, window, cx));
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).1, ["a copy.txt"]);
    cx.simulate_input("move to trash");
    cx.run_until_parked();
    let view = palette(&browser, cx);
    assert_eq!(under_keys(&view, cx).as_deref(), Some("Move to Trash"));
    press(cx, "enter");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    assert_eq!(
        toast(&browser, cx),
        Some((
            "The selection changed while the palette was open.".into(),
            true,
            false
        ))
    );
    release(owned, cx);
    assert!(project.join("a.txt").exists() && project.join("a copy.txt").exists());
}

#[gpui::test]
fn a_drive_going_under_the_palette_stops_eject_reaching_the_next_one(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let drive = |name: &str| volume(name, &Path::new("/Volumes").join(name), name, 1000, 250);
    let world = world(vec![startup(), drive("A"), drive("B"), drive("C")], cx);
    let owned = own(cx);
    let (browser, cx) = open(&world, &project, cx);
    let favorites = browser.read_with(cx, |b, _| b.sidebar.places.len());
    // The Overview, Recents, the favorites, the startup disk, A, then B.
    browser.update_in(cx, |b, window, cx| {
        b.sidebar_cursor = favorites + 4;
        window.focus(&b.sidebar_focus, cx);
        cx.notify();
    });
    cx.run_until_parked();
    let eject_from_palette = |cx: &mut VisualTestContext| {
        cx.simulate_input("eject");
        cx.run_until_parked();
        let view = palette(&browser, cx);
        assert_eq!(under_keys(&view, cx).as_deref(), Some("Eject"));
        press(cx, "enter");
    };
    // B itself goes, so the keys fall to C, the next one.
    press(cx, "cmd-k");
    world.fake.volumes.lock().unwrap().remove(2);
    world.volumes_changed.try_send(()).unwrap();
    cx.run_until_parked();
    eject_from_palette(cx);
    assert!(world.fake.ejected.lock().unwrap().is_empty());
    assert_eq!(
        toast(&browser, cx),
        Some((
            "The selection changed while the palette was open.".into(),
            true,
            false
        ))
    );
    // Opened on the row as it is now, it runs.
    press(cx, "cmd-k");
    eject_from_palette(cx);
    assert_eq!(
        world.fake.ejected.lock().unwrap()[0],
        Path::new("/Volumes/C")
    );
    release(owned, cx);
}

#[gpui::test]
fn the_palette_over_another_sheet_still_knows_what_would_run(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "down down down");
    press(cx, "cmd-,");
    assert!(browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::Settings(_)))));
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    assert_eq!(under_keys(&view, cx).as_deref(), Some("Open"));
    assert!(view.read_with(cx, |v, _| v.rows[0].enabled));
}

#[gpui::test]
fn check_for_updates_from_settings_shows_the_updates(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-,");
    cx.dispatch_action(CheckForUpdates);
    assert!(browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::Updates))));
}

#[gpui::test]
fn a_page_opens_filters_and_goes_back(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let project = project.canonicalize().unwrap();
    *world.fake.cloud.lock().unwrap() = vec![
        CloudFolder {
            name: "iCloud Drive".into(),
            path: project.join("docs"),
        },
        CloudFolder {
            name: "Dropbox".into(),
            path: project.join("zeta"),
        },
    ];
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "cloud");
    // → opens it, and its filter starts empty.
    press(cx, "right");
    assert_eq!(view.read_with(cx, |v, _| v.page), Page::Cloud);
    assert_eq!(titles(&view, cx), ["iCloud Drive", "Dropbox"]);
    type_in(cx, "drop");
    assert_eq!(titles(&view, cx), ["Dropbox"]);
    // ← moves the caret while there's text, and Esc goes back regardless,
    // to what was typed and the row that opened it.
    press(cx, "left");
    assert_eq!(view.read_with(cx, |v, _| v.page), Page::Cloud);
    press(cx, "escape");
    assert_eq!(view.read_with(cx, |v, _| v.page), Page::Root);
    assert_eq!(under_keys(&view, cx).as_deref(), Some("Cloud storage"));
    // Return opens it too, and ← with nothing typed goes back.
    press(cx, "enter");
    assert_eq!(view.read_with(cx, |v, _| v.page), Page::Cloud);
    press(cx, "left");
    assert_eq!(view.read_with(cx, |v, _| v.page), Page::Root);
    // The chip goes back by mouse.
    press(cx, "enter");
    click(cx, "palette-back");
    assert_eq!(view.read_with(cx, |v, _| v.page), Page::Root);
    // A cloud folder opens in the window.
    press(cx, "enter down enter");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    assert_eq!(here(&browser, cx), project.join("zeta"));
}
