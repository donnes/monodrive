//! Saved SFTP servers in the window: the sidebar's rows, a server folder in
//! a pane, Connect to Server, and copies through the owner. Every server is
//! macOS's own `sftp-server` over pipes on a temp folder (`local_sftp`), so
//! nothing here reaches ssh, the network or a real machine.

use super::places_tests::*;
use super::remote::{Link, Listing};
use super::sidebar::Row;
use super::tests::*;
use super::*;
use gpui::{TestAppContext, VisualTestContext};
use monodrive_core::remote::RemoteError;

/// The folder a server starts in: `docs/` holding `inner.md`, `a.txt`, and
/// the hidden `.profile`.
fn far(world: &World) -> PathBuf {
    let far = world.dir.path().canonicalize().unwrap().join("far");
    std::fs::create_dir_all(far.join("docs")).unwrap();
    std::fs::write(far.join("docs/inner.md"), "inner").unwrap();
    std::fs::write(far.join("a.txt"), "remote text").unwrap();
    std::fs::write(far.join(".profile"), "").unwrap();
    far
}

/// The sidebar's selector for saved server `ix`.
fn server_row(browser: &Entity<Browser>, ix: usize, cx: &mut VisualTestContext) -> String {
    let row = browser.read_with(cx, |b, _| {
        (0..100)
            .find(|row| b.row_at(*row) == Some(Row::Server(ix)))
            .expect("the server has a row")
    });
    format!("place-{row}")
}

/// The server folder's rows, and the selected ones.
fn listed(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> (Vec<String>, Vec<String>) {
    in_pane(browser, cx, |pane, _| {
        let rows = pane.remote.entries.iter().map(|e| e.name.clone()).collect();
        let selected = pane
            .remote_chosen()
            .iter()
            .filter_map(|path| path.name().map(str::to_owned))
            .collect();
        (rows, selected)
    })
}

fn listing(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Listing {
    in_pane(browser, cx, |pane, _| pane.remote.listing.clone())
}

fn link(id: &str, cx: &mut VisualTestContext) -> Option<Link> {
    cx.update(|_, cx| remote::link(id, cx).cloned())
}

#[gpui::test]
fn a_saved_server_opens_in_the_pane_and_back_leaves_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    let far = far(&world);
    let start = far.display().to_string();
    world
        .store
        .add_remote("box.example", Some("box"), Some(&start))
        .unwrap();
    let (browser, cx) = open(&world, &project, cx);
    let row = server_row(&browser, 0, cx);
    press_on(cx, &row);
    // It connected and read its start folder alone, folders first, without
    // the hidden file.
    assert_eq!(link("box", cx), Some(Link::Connected));
    assert_eq!(listed(&browser, cx).0, ["docs", "a.txt"]);
    assert_eq!(in_pane(&browser, cx, |pane, cx| pane.title(cx)), "far");
    assert!(in_pane(&browser, cx, |pane, _| pane.showing_remote()));
    // Nothing local is on screen, so nothing local can be chosen.
    assert!(shown(&browser, cx).0.is_empty());
    // Into a folder, up again with the folder selected, and Back through it.
    press(cx, "down cmd-down");
    assert_eq!(listed(&browser, cx).0, ["inner.md"]);
    press(cx, "cmd-up");
    assert_eq!(
        listed(&browser, cx),
        (vec!["docs".into(), "a.txt".into()], vec!["docs".into()])
    );
    press(cx, "cmd-[");
    assert_eq!(listed(&browser, cx).0, ["inner.md"]);
    press(cx, "cmd-[ cmd-[");
    assert!(!in_pane(&browser, cx, |pane, _| pane.showing_remote()));
    assert_eq!(shown(&browser, cx).0, ["docs", "zeta", "a.txt"]);
    // Forward comes back to the server, where it was.
    press(cx, "cmd-]");
    assert_eq!(listed(&browser, cx).0, ["docs", "a.txt"]);
    // Show Hidden Files lists the dotfile.
    press(cx, "cmd->");
    assert_eq!(listed(&browser, cx).0, ["docs", ".profile", "a.txt"]);
}

#[gpui::test]
fn a_server_that_wont_connect_says_why_in_the_pane_and_the_sidebar(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    world
        .store
        .add_remote("changed.example", Some("moved"), None)
        .unwrap();
    let (browser, cx) = open(&world, &project, cx);
    let row = server_row(&browser, 0, cx);
    press_on(cx, &row);
    let changed = RemoteError::HostKeyChanged("changed.example".into());
    assert_eq!(listing(&browser, cx), Listing::Failed(changed.clone()));
    assert_eq!(link("moved", cx), Some(Link::Failed(changed)));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert!(cx.debug_bounds("remote-failed").is_some());
    assert!(cx.debug_bounds("server-failed-0").is_some());
    // Back leaves it for the folder it was opened from.
    press(cx, "cmd-[");
    assert_eq!(shown(&browser, cx).0, ["docs", "zeta", "a.txt"]);
}

#[gpui::test]
fn disconnecting_waits_to_be_asked_and_forgetting_leaves_the_server_alone(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    let far = far(&world);
    let start = far.display().to_string();
    world
        .store
        .add_remote("box.example", Some("box"), Some(&start))
        .unwrap();
    let (browser, cx) = open(&world, &project, cx);
    let row = server_row(&browser, 0, cx);
    press_on(cx, &row);
    assert_eq!(listed(&browser, cx).0, ["docs", "a.txt"]);
    // The row's Eject disconnects; the pane says so and doesn't reconnect.
    press_on(cx, "server-disconnect-0");
    assert_eq!(link("box", cx), None);
    assert_eq!(listing(&browser, cx), Listing::Disconnected);
    let connected = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| cx.global::<Places>().remote.is_connected("box"))
    };
    assert!(!connected(cx));
    press_on(cx, "remote-retry");
    assert_eq!(listed(&browser, cx).0, ["docs", "a.txt"]);
    assert!(connected(cx));
    // Remove from Sidebar on its row forgets it, and only that.
    let ix: usize = row.trim_start_matches("place-").parse().unwrap();
    browser.update_in(cx, |b, window, cx| {
        b.sidebar_cursor = ix;
        window.focus(&b.sidebar_focus, cx);
        cx.notify();
    });
    cx.run_until_parked();
    press(cx, "cmd-backspace");
    assert_eq!(world.store.remotes(), Ok(Vec::new()));
    assert!(!connected(cx));
    assert!(far.join("a.txt").exists() && far.join("docs/inner.md").exists());
    assert_eq!(browser.read_with(cx, |b, _| b.sidebar.remotes.len()), 0);
}

#[gpui::test]
fn the_palette_opens_a_server_and_disconnects_a_connected_one(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    let far = far(&world);
    let start = far.display().to_string();
    world
        .store
        .add_remote("box.example", Some("box"), Some(&start))
        .unwrap();
    let (browser, cx) = open(&world, &project, cx);
    let about = |view: &Entity<crate::palette::PaletteView>, cx: &mut VisualTestContext| {
        view.read_with(cx, |v, _| {
            v.rows[v.active.unwrap()]
                .subtitle
                .as_ref()
                .map(ToString::to_string)
        })
    };
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "remote");
    press(cx, "enter");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("box"));
    assert_eq!(
        about(&view, cx).as_deref(),
        Some("Not connected · box.example")
    );
    press(cx, "enter");
    assert_eq!(listed(&browser, cx).0, ["docs", "a.txt"]);
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "remote");
    press(cx, "enter");
    assert_eq!(about(&view, cx).as_deref(), Some("Connected · box.example"));
    press(cx, "cmd-e");
    assert_eq!(link("box", cx), None);
    assert_eq!(listing(&browser, cx), Listing::Disconnected);
    assert_eq!(
        about(&view, cx).as_deref(),
        Some("Not connected · box.example")
    );
}

#[gpui::test]
fn connect_to_server_saves_an_sftp_server_only_once_it_connects(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    let far = far(&world);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-shift-k");
    type_in(cx, "sftp://changed.example/srv");
    press(cx, "enter");
    let error = browser.read_with(cx, |b, cx| match &b.sheet {
        Some(Sheet::Connect(view)) => view.read(cx).error.clone(),
        _ => None,
    });
    assert!(error.unwrap().contains("host key changed"));
    assert_eq!(world.store.remotes(), Ok(Vec::new()));
    press(cx, "cmd-a backspace");
    type_in(cx, &format!("sftp://me@box{}", far.display()));
    press(cx, "enter");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    let saved = world.store.remotes().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (saved[0].host.as_str(), saved[0].start.as_deref()),
        ("me@box", Some(far.to_str().unwrap()))
    );
    assert_eq!(listed(&browser, cx).0, ["docs", "a.txt"]);
}

#[gpui::test]
fn new_folder_and_rename_change_the_server_and_never_replace(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    let far = far(&world);
    let start = far.display().to_string();
    world
        .store
        .add_remote("box.example", Some("box"), Some(&start))
        .unwrap();
    let (browser, cx) = open(&world, &project, cx);
    let row = server_row(&browser, 0, cx);
    press_on(cx, &row);
    press(cx, "cmd-shift-n");
    assert!(far.join("untitled folder").is_dir());
    // Its name is being edited; a name that's taken is refused in place.
    press(cx, "cmd-a");
    type_in(cx, "docs");
    press(cx, "enter");
    assert!(far.join("untitled folder").is_dir());
    let said = in_pane(&browser, cx, |pane, _| {
        pane.remote.renaming.as_ref().and_then(|r| r.error.clone())
    });
    assert!(said.unwrap().contains("already exists"));
    press(cx, "cmd-a");
    type_in(cx, "made");
    press(cx, "enter");
    assert!(far.join("made").is_dir() && !far.join("untitled folder").exists());
    assert_eq!(listed(&browser, cx).1, ["made"]);
    // Move to Trash has nothing to run on a server.
    let trash = cx.update(|window, cx| window.is_action_available(&MoveToTrash, cx));
    assert!(!trash);
}

#[gpui::test]
fn copy_to_other_pane_downloads_from_a_server_and_uploads_to_one(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let world = world(vec![startup()], cx);
    let far = far(&world);
    let start = far.display().to_string();
    world
        .store
        .add_remote("box.example", Some("box"), Some(&start))
        .unwrap();
    // An owner whose saved list is the window's, reaching the same servers.
    let real = world.dir.path().canonicalize().unwrap();
    let paths = ops::Paths {
        state: real.join("state"),
        settings: real.join("support/settings.json"),
        trash: real.join("Trash"),
    };
    let serve = |cx: &mut VisualTestContext| {
        let owner = ops::Owner::bind_with(paths.clone(), local_sftp(&[])).unwrap();
        let serving = std::thread::spawn(move || owner.serve());
        let client = ops::Client::new(paths.clone(), None);
        cx.update(|_, cx| cx.set_global(Operations(Arc::new(Mutex::new(client)))));
        serving
    };
    // The window lets go, and the owner ends once its jobs are done.
    let finish = |serving: std::thread::JoinHandle<std::io::Result<()>>,
                  cx: &mut VisualTestContext| {
        cx.update(|_, cx| drop(cx.remove_global::<Operations>()));
        serving.join().unwrap().unwrap();
    };
    std::fs::write(far.join("notes.md"), "from the server").unwrap();
    std::fs::write(project.join("up.txt"), "from this Mac").unwrap();
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-alt-s");
    let row = server_row(&browser, 0, cx);
    press_on(cx, &row);
    assert_eq!(listed(&browser, cx).0, ["docs", "a.txt", "notes.md"]);

    let serving = serve(cx);
    press(cx, "down down down");
    assert_eq!(listed(&browser, cx).1, ["notes.md"]);
    press(cx, "alt-f5");
    finish(serving, cx);
    assert_eq!(
        std::fs::read_to_string(project.join("notes.md"))
            .ok()
            .as_deref(),
        Some("from the server")
    );
    // Moving off a server isn't offered: there's no Trash to recover from.
    press(cx, "alt-f6");
    let said = toast(&browser, cx).unwrap();
    assert!(said.0.starts_with("Servers have no Trash"), "{said:?}");

    // From the local pane, a file goes up into the server's folder.
    let local = browser.read_with(cx, |b, _| b.tabs[b.active].pane.clone());
    local.update_in(cx, |pane, window, cx| pane.focus_list(window, cx));
    local.update(cx, |pane, cx| pane.reload(cx));
    cx.run_until_parked();
    let serving = serve(cx);
    press(cx, "down down down down down");
    assert_eq!(shown(&browser, cx).1, ["up.txt"]);
    press(cx, "alt-f5");
    finish(serving, cx);
    assert_eq!(
        std::fs::read_to_string(far.join("up.txt")).ok().as_deref(),
        Some("from this Mac")
    );
}
