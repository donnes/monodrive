use super::*;

/// A pane in a saved session, on these places, the last on screen.
fn saved_pane(places: Vec<Favorite>) -> PaneState {
    PaneState {
        at: places.len() - 1,
        history: places
            .into_iter()
            .map(|place| StopState {
                place,
                shows: Shows::Folder,
                remote: None,
            })
            .collect(),
        view: View::List,
        sort: Sort::default(),
    }
}

fn plain(path: &Path) -> Favorite {
    Favorite {
        path: path.into(),
        volume: None,
    }
}

/// A window on a saved `pane`, hearing `world`'s watches.
fn reopen<'a>(
    world: &World,
    pane: PaneState,
    cx: &'a mut TestAppContext,
) -> (Entity<Browser>, &'a mut VisualTestContext) {
    let state = WindowState {
        tabs: vec![TabState {
            panes: vec![pane],
            second_focused: false,
            ratio: 0.5,
        }],
        active: 0,
        frame: None,
    };
    open_state(world, state, cx)
}

fn open_state<'a>(
    world: &World,
    state: WindowState,
    cx: &'a mut TestAppContext,
) -> (Entity<Browser>, &'a mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(Preferences::default());
        crate::apply_keymap(cx);
    });
    let (browser, cx) = cx.add_window_view(|_, cx| Browser::restored(state, cx));
    cx.update(|window, cx| {
        window.activate_window();
        let list = browser.read(cx).focus_handle(cx);
        window.focus(&list, cx);
    });
    browser.update(cx, |browser, cx| {
        browser.hear_places(
            (),
            world.volume_changes.clone(),
            world.list_changes.clone(),
            cx,
        );
    });
    cx.run_until_parked();
    (browser, cx)
}

#[gpui::test]
fn tabs_and_panes_come_back_as_they_were(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    // The first tab went into docs; the second is on the project, in the grid,
    // split, with its right pane up a level and the keys there.
    press(cx, "down cmd-down cmd-t cmd-up cmd-alt-2 cmd-alt-s cmd-up");
    let state = saved_state(&browser, cx);
    let bytes = serde_json::to_vec(&Session::of(vec![state])).unwrap();
    let state = session::decode(&bytes).unwrap().windows.remove(0);
    assert_eq!((state.tabs.len(), state.active), (2, 1));

    let cx: &mut TestAppContext = cx;
    let (browser, cx) = open_state(&world, state, cx);
    let parent = project.parent().unwrap().to_path_buf();
    assert_eq!(here(&browser, cx), parent);
    assert_eq!(in_pane(&browser, cx, |pane, _| pane.view), View::Grid);
    assert_eq!(in_pane(&browser, cx, |pane, _| pane.history.len()), 2);
    let left = browser.read_with(cx, |b, _| b.tabs[1].pane.clone());
    assert_eq!(
        left.read_with(cx, |pane, _| pane.folder().to_path_buf()),
        project
    );
    assert!(left.read_with(cx, |pane, _| !pane.entries.is_empty()));
    // The hidden tab knows where it was and has read nothing yet.
    let hidden = browser.read_with(cx, |b, _| b.tabs[0].pane.clone());
    let read = |cx: &mut VisualTestContext| {
        hidden.read_with(cx, |pane, _| (pane.entries.len(), pane.listing))
    };
    assert_eq!(read(cx), (0, false));
    press(cx, "ctrl-tab");
    assert_eq!(read(cx), (1, false));
    assert_eq!(here(&browser, cx), project.join("docs"));
    press(cx, "cmd-[");
    assert_eq!(here(&browser, cx), project);
}

#[gpui::test]
fn a_missing_drive_is_unavailable_and_never_another_drive_at_its_path(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let drive = tempfile::tempdir().unwrap();
    let mount = drive.path().canonicalize().unwrap();
    std::fs::create_dir(mount.join("work")).unwrap();
    std::fs::write(mount.join("work/theirs.txt"), "").unwrap();
    // Another drive is mounted where Backup was.
    let world = world(
        vec![startup(), volume("Impostor", &mount, "I", 1000, 500)],
        cx,
    );
    let pinned = Favorite {
        path: mount.join("work"),
        volume: Some(Pinned {
            id: VolumeId::Uuid("B".into()),
            name: "Backup".into(),
            mount: mount.clone(),
        }),
    };
    let (browser, cx) = reopen(&world, saved_pane(vec![plain(&project), pinned]), cx);
    let why = in_pane(&browser, cx, |pane, _| pane.unavailable.clone());
    assert_eq!(
        why,
        Some(Unavailable::Gone("Backup isn't connected.".into()))
    );
    assert!(shown(&browser, cx).0.is_empty());
    assert!(on_screen(cx, "unavailable"));
    // Nothing can land there, and Go to Folder doesn't start from it.
    let makes = cx.update(|window, cx| window.is_action_available(&NewFolder, cx));
    assert!(!makes);
    // Back leaves, and Forward finds it still unavailable.
    press(cx, "cmd-[");
    assert_eq!(here(&browser, cx), project);
    assert_eq!(
        in_pane(&browser, cx, |pane, _| pane.unavailable.clone()),
        None
    );
    press(cx, "cmd-]");
    assert!(in_pane(&browser, cx, |pane, _| pane.unavailable.is_some()));
    // Backup comes back mounted somewhere else, and that's where it opens.
    let elsewhere = tempfile::tempdir().unwrap();
    let elsewhere = elsewhere.path().canonicalize().unwrap();
    std::fs::create_dir(elsewhere.join("work")).unwrap();
    std::fs::write(elsewhere.join("work/ours.txt"), "").unwrap();
    *world.fake.volumes.lock().unwrap() =
        vec![startup(), volume("Backup", &elsewhere, "B", 1000, 500)];
    world.volumes_changed.try_send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(
        in_pane(&browser, cx, |pane, _| pane.unavailable.clone()),
        None
    );
    assert_eq!(shown(&browser, cx).0, ["ours.txt"]);
    assert_eq!(here(&browser, cx), elsewhere.join("work"));
}

#[gpui::test]
fn a_restored_folder_that_would_ask_for_permission_waits_for_a_person(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let docs = project.join("docs");
    let world = world(vec![startup()], cx);
    let real = docs.canonicalize().unwrap();
    world.fake.gated.lock().unwrap().push(real);
    let (browser, cx) = reopen(&world, saved_pane(vec![plain(&docs)]), cx);
    let why = in_pane(&browser, cx, |pane, _| pane.unavailable.clone());
    assert_eq!(why, Some(Unavailable::NeedsConsent));
    assert!(shown(&browser, cx).0.is_empty());
    click(cx, "unavailable-open");
    assert_eq!(shown(&browser, cx).0, ["inner.md"]);
    assert!(on_screen(cx, "path-bar"));
}

#[gpui::test]
fn a_folder_gone_from_the_startup_disk_says_so(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let gone = project.join("gone");
    let (browser, cx) = reopen(&world, saved_pane(vec![plain(&gone)]), cx);
    let why = in_pane(&browser, cx, |pane, _| pane.unavailable.clone());
    assert_eq!(
        why,
        Some(Unavailable::Gone(format!(
            "{} no longer exists.",
            gone.display()
        )))
    );
    click(cx, "unavailable-choose");
    assert!(browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::Goto(_)))));
}

#[gpui::test]
fn windows_share_one_index(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let shared: Arc<OnceLock<Result<Index, String>>> = Arc::default();
    cx.update(|cx| cx.set_global(SharedIndex(shared.clone())));
    let (first, cx) = browse(&project, cx);
    let cx: &mut TestAppContext = cx;
    let (second, cx) = browse(&project, cx);
    for browser in [&first, &second] {
        assert!(browser.read_with(cx, |b, _| Arc::ptr_eq(&b.index, &shared)));
    }
}

/// What the window would save now.
fn saved_state(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> WindowState {
    let kept: Rc<RefCell<Option<WindowState>>> = Rc::default();
    let into = kept.clone();
    browser.update(cx, |b, cx| {
        b.capture_state(cx, move |state, _| *into.borrow_mut() = Some(state))
            .detach()
    });
    cx.run_until_parked();
    kept.take().unwrap()
}

#[gpui::test]
fn a_link_into_a_drive_is_saved_with_that_drive(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let drive = tempfile::tempdir().unwrap();
    let mount = drive.path().canonicalize().unwrap();
    std::fs::create_dir(mount.join("work")).unwrap();
    let link = project.join("to-drive");
    std::os::unix::fs::symlink(mount.join("work"), &link).unwrap();
    let world = world(
        vec![startup(), volume("Backup", &mount, "B", 1000, 500)],
        cx,
    );
    let (browser, cx) = open(&world, &project, cx);
    on_pane(&browser, cx, |pane, cx| pane.navigate(link.clone(), cx));
    cx.run_until_parked();
    let state = saved_state(&browser, cx);
    let place = state.tabs[0].panes[0].history[1].place.clone();
    assert_eq!(place.path, mount.join("work"));
    let drive_name = place.volume.as_ref().map(|volume| volume.name.as_str());
    assert_eq!(drive_name, Some("Backup"));
    // Another drive at that mount isn't what comes back.
    *world.fake.volumes.lock().unwrap() =
        vec![startup(), volume("Impostor", &mount, "I", 1000, 500)];
    let cx: &mut TestAppContext = cx;
    let (browser, cx) = open_state(&world, state, cx);
    assert!(in_pane(&browser, cx, |pane, _| pane.unavailable.is_some()));
    assert!(shown(&browser, cx).0.is_empty());
}

#[gpui::test]
fn going_to_a_dotfile_does_not_open_the_trash_or_applications_to_hidden_files(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let root = world.dir.path().canonicalize().unwrap();
    let (trash, applications) = (root.join("Trash"), root.join("Applications"));
    with_a_dotfile(&trash);
    with_a_dotfile(&applications);
    std::fs::write(root.join(".named"), "").unwrap();
    let (browser, cx) = open(&world, &project, cx);
    let go_to = |folder: &Path, file: &str, cx: &mut VisualTestContext| {
        let pane = pane_of(&browser, cx);
        let (folder, file) = (folder.to_path_buf(), folder.join(file));
        pane.update_in(cx, |pane, window, cx| {
            pane.go_to(folder, vec![file], window, cx)
        });
        cx.run_until_parked();
    };
    // Applications and the Trash open on what they always show.
    go_to(&applications, ".DS_Store", cx);
    assert_eq!(shown(&browser, cx).0, ["app.app"]);
    go_to(&trash, ".DS_Store", cx);
    assert_eq!(shown(&browser, cx).0, ["app.app"]);
    // Elsewhere the file asked for by name shows, as before.
    go_to(&project, ".env", cx);
    assert_eq!(shown(&browser, cx).1, [".env"]);
    // A folder opened in place under one with a named dotfile keeps the rule.
    go_to(&root, ".named", cx);
    assert_eq!(shown(&browser, cx).1, [".named"]);
    on_pane(&browser, cx, |pane, cx| {
        pane.toggle_folder(applications.clone(), cx)
    });
    cx.run_until_parked();
    let rows = shown(&browser, cx).0;
    assert!(rows.contains(&"  app.app".to_string()), "{rows:?}");
    assert!(!rows.contains(&"  .DS_Store".to_string()), "{rows:?}");
}

#[test]
fn every_applications_folder_is_tidy_through_links_too() {
    let roots = crate::application_roots(Path::new("/Users/someone"));
    let system = Path::new("/System/Applications");
    assert!(roots.iter().any(|root| root == system));
    assert!(!crate::browser::pane::lists_hidden(system, true, &roots));
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("to-apps");
    std::os::unix::fs::symlink(system, &link).unwrap();
    assert!(!crate::browser::pane::lists_hidden(&link, true, &roots));
    assert!(crate::browser::pane::lists_hidden(dir.path(), true, &roots));
}

#[gpui::test]
fn close_window_closes_from_the_keys_and_the_menu_and_forgets_that_window(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let saved = world.dir.path().join("windows.json");
    let path = saved.clone();
    cx.update(|cx| {
        session::init(path, cx);
        crate::window_commands(cx);
    });
    // Three windows that keep their session, the last keeping these keys.
    let mut handles = Vec::new();
    let mut cx = cx;
    for _ in 0..3 {
        let (browser, opened) = open(&world, &project, cx);
        let handle = opened.update(|window, _| window.window_handle());
        browser.update(opened, |b, cx| {
            b.keep_session(handle.window_id().as_u64(), cx)
        });
        opened.run_until_parked();
        handles.push(handle);
        cx = opened;
    }
    assert_eq!(cx.windows().len(), 3);
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(session::load(&saved).unwrap().windows.len(), 3);

    // ⇧⌘W, as a key.
    let last = handles[2];
    last.update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.update(|cx| {
        let keys = gpui::Keystroke::parse("cmd-shift-w").unwrap();
        let _ = last.update(cx, |_, window, cx| window.dispatch_keystroke(keys, cx));
    });
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2);

    // Window ▸ Close Window, as the menu bar sends it.
    let second = handles[1];
    second
        .update(cx, |_, window, _| window.activate_window())
        .unwrap();
    cx.update(|cx| cx.dispatch_action(&commands::CloseWindow));
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);

    // What's saved is the window left, as after the red button.
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    assert_eq!(session::load(&saved).unwrap().windows.len(), 1);
}

#[gpui::test]
fn a_restored_place_on_a_drive_is_checked_for_consent_before_it_is_touched(
    cx: &mut TestAppContext,
) {
    use std::os::unix::fs::PermissionsExt;
    // A temp home whose Documents is gated, and locked so a stat inside it
    // fails another way than asking.
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let documents = home.join("Documents");
    std::fs::create_dir_all(documents.join("deep")).unwrap();
    std::fs::write(documents.join("deep/mine.txt"), "").unwrap();
    // The drive now mounts somewhere other than where it was saved.
    let drive = tempfile::tempdir().unwrap();
    let mount = drive.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(documents.join("deep"), mount.join("link")).unwrap();
    let world = world(
        vec![startup(), volume("Backup", &mount, "B", 1000, 500)],
        cx,
    );
    world.fake.gated.lock().unwrap().push(documents.clone());
    let saved = |inside: &str| Favorite {
        path: Path::new("/Volumes/Backup").join(inside),
        volume: Some(Pinned {
            id: VolumeId::Uuid("B".into()),
            name: "Backup".into(),
            mount: "/Volumes/Backup".into(),
        }),
    };
    std::fs::set_permissions(&documents, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (browser, cx) = reopen(&world, saved_pane(vec![saved("link")]), cx);
    let why = in_pane(&browser, cx, |pane, _| pane.unavailable.clone());
    std::fs::set_permissions(&documents, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(why, Some(Unavailable::NeedsConsent));
    // Asked, it opens through the link, where the drive is now.
    click(cx, "unavailable-open");
    assert_eq!(shown(&browser, cx).0, ["mine.txt"]);
    assert_eq!(here(&browser, cx), mount.join("link"));
}
