//! The sidebar, the Trash, Recents, Connect to Server and the command palette,
//! driven through the real `Browser` over temp folders. `Fake` stands in for the system, so
//! nothing here mounts or ejects anything, and `Store` is a temp folder's.

use super::pane::Unavailable;
use super::sidebar::Row;
use super::tests::*;
use super::*;
use crate::palette::{Page, PaletteView};
use crate::preferences::View;
use crate::session::{self, PaneState, Session, Shows, StopState};
use gpui::{Action, Keystroke, Modifiers, Pixels, TestAppContext, VisualTestContext, point};
use monodrive_core::cloud::CloudFolder;
use monodrive_core::entries::Sort;
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::index::Found;
use monodrive_core::ops::Origin;
use monodrive_core::places::{Favorite, Pinned, Store};
use monodrive_core::search::Search;
use monodrive_core::servers::Cancel;
use monodrive_core::volumes::{Volume, VolumeId};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

#[derive(Default)]
pub(super) struct Fake {
    /// Folders that would raise a privacy prompt.
    gated: Mutex<Vec<PathBuf>>,
    volumes: Mutex<Vec<Volume>>,
    /// Why the next eject is refused.
    refusal: Mutex<Option<String>>,
    ejected: Mutex<Vec<PathBuf>>,
    /// What the next mount answers.
    mounted: Mutex<Option<Result<PathBuf, String>>>,
    pub(super) opened: Mutex<Vec<PathBuf>>,
    pub(super) cloud: Mutex<Vec<CloudFolder>>,
    /// What the index finds, whatever's asked.
    files: Mutex<Vec<Entry>>,
    /// The paths whose device was asked for, in order.
    devices_asked: Mutex<Vec<PathBuf>>,
    /// What a search in the window finds, by what's typed.
    pub(super) found: Mutex<HashMap<String, Found>>,
    /// The searches that ran, in order.
    pub(super) searched: Mutex<Vec<Search>>,
}

impl Drives for Fake {
    fn list(&self) -> Vec<Volume> {
        self.volumes.lock().unwrap().clone()
    }

    fn eject(&self, mount: &Path, _: &mut ops::Client) -> Result<String, String> {
        if let Some(why) = self.refusal.lock().unwrap().take() {
            return Err(why);
        }
        self.ejected.lock().unwrap().push(mount.into());
        self.volumes.lock().unwrap().retain(|v| v.mount != mount);
        Ok("Ejected it.".into())
    }

    fn mount(&self, _: &str, _: &Arc<Cancel>) -> Result<PathBuf, String> {
        self.mounted.lock().unwrap().take().expect("a mount answer")
    }

    fn open(&self, path: &Path, _: &App) {
        self.opened.lock().unwrap().push(path.into());
    }

    fn needs_consent(&self, path: &Path) -> bool {
        monodrive_core::sizes::leads_into(path, &self.gated.lock().unwrap())
    }

    /// Which made-up volume holds where `path` really leads.
    fn device(&self, path: &Path) -> Option<u64> {
        self.devices_asked.lock().unwrap().push(path.into());
        let real = path.canonicalize().ok()?;
        let volumes = self.volumes.lock().unwrap();
        let holder = monodrive_core::volumes::holding(&volumes, &real)?;
        let ix = volumes.iter().position(|v| v.mount == holder.mount)?;
        Some(ix as u64)
    }

    fn cloud(&self) -> Vec<CloudFolder> {
        self.cloud.lock().unwrap().clone()
    }

    fn find_files(
        &self,
        _: &OnceLock<Result<Index, String>>,
        _: &str,
        _: usize,
    ) -> Result<Vec<Entry>, String> {
        Ok(self.files.lock().unwrap().clone())
    }

    fn search<'a>(
        &self,
        _: &dyn Fn() -> Result<&'a Index, String>,
        search: &Search,
        _: usize,
    ) -> Result<Found, String> {
        self.searched.lock().unwrap().push(search.clone());
        let found = self.found.lock().unwrap().get(&search.text).cloned();
        found.ok_or_else(|| format!("Nothing made up for {}.", search.text))
    }
}

fn volume(name: &str, mount: &Path, id: &str, total: u64, free: u64) -> Volume {
    Volume {
        name: name.into(),
        mount: mount.into(),
        disk: None,
        id: Some(VolumeId::Uuid(id.into())),
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

pub(super) fn startup() -> Volume {
    Volume {
        ejectable: false,
        removable: false,
        startup: true,
        ..volume("Macintosh HD", "/".as_ref(), "root", 1000, 400)
    }
}

pub(super) struct World {
    /// Keeps the store's and the Trash's folder.
    pub(super) dir: tempfile::TempDir,
    pub(super) store: Store,
    pub(super) fake: Arc<Fake>,
    volumes_changed: async_channel::Sender<()>,
    lists_changed: async_channel::Sender<()>,
    volume_changes: async_channel::Receiver<()>,
    list_changes: async_channel::Receiver<()>,
}

/// Isolated places with `volumes` mounted, set as the app's. Call it before
/// `browse`, which is when the sidebar first reads them.
pub(super) fn world(volumes: Vec<Volume>, cx: &mut TestAppContext) -> World {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().canonicalize().unwrap();
    let store = Store {
        places: real.join("support/places.json"),
        recents: real.join("support/recents.json"),
    };
    let trash = real.join("Trash");
    std::fs::create_dir(&trash).unwrap();
    let fake = Arc::new(Fake {
        volumes: Mutex::new(volumes),
        ..Fake::default()
    });
    let places = Places {
        previews: real.join("previews"),
        remote: Arc::new(monodrive_core::remote::Connections::new(
            store.clone(),
            local_sftp(&["changed.example"]),
        )),
        store: store.clone(),
        trash: trash.clone(),
        drives: fake.clone(),
        applications: vec![real.join("Applications")],
        root: real.join("disk"),
    };
    cx.update(|cx| cx.set_global(places));
    let (volumes_changed, volume_changes) = async_channel::unbounded();
    let (lists_changed, list_changes) = async_channel::unbounded();
    World {
        dir,
        store,
        fake,
        volumes_changed,
        lists_changed,
        volume_changes,
        list_changes,
    }
}

/// A browser on `folder` that hears `world`'s watches.
pub(super) fn open<'a>(
    world: &World,
    folder: &Path,
    cx: &'a mut TestAppContext,
) -> (Entity<Browser>, &'a mut VisualTestContext) {
    let (browser, cx) = browse(folder, cx);
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

/// The favorites as the sidebar last read them.
fn favorites(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> Vec<PathBuf> {
    browser.read_with(cx, |browser, _| {
        let places = &browser.sidebar.places;
        places.iter().map(|p| p.favorite.path.clone()).collect()
    })
}

fn saved(store: &Store) -> Vec<PathBuf> {
    let places = store.places(&[startup()]).unwrap();
    places.into_iter().map(|p| p.favorite.path).collect()
}

fn on_screen(cx: &mut VisualTestContext, selector: &str) -> bool {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector.to_string().leak()).is_some()
}

pub(super) fn press_on(cx: &mut VisualTestContext, selector: &str) {
    click(cx, selector.to_string().leak());
}

pub(super) fn center(cx: &mut VisualTestContext, selector: &str) -> Point<Pixels> {
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.debug_bounds(selector.to_string().leak())
        .unwrap()
        .center()
}

fn here(browser: &Entity<Browser>, cx: &mut VisualTestContext) -> PathBuf {
    in_pane(browser, cx, |browser, _| browser.folder().to_path_buf())
}

/// Pins two folders ahead of the default favorites and returns them.
fn two_favorites(world: &World) -> (PathBuf, PathBuf) {
    let root = world.dir.path().canonicalize().unwrap();
    let (one, two) = (root.join("One"), root.join("Two"));
    for folder in [&one, &two] {
        std::fs::create_dir(folder).unwrap();
    }
    let volumes = [startup()];
    world.store.pin(&one, Some(0), &volumes).unwrap();
    world.store.pin(&two, Some(1), &volumes).unwrap();
    (one, two)
}

#[gpui::test]
fn drives_show_how_full_each_is_with_marks_and_follow_the_watch(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let locked = Volume {
        read_only: true,
        ..volume("Installer", "/Volumes/Installer".as_ref(), "I", 100, 0)
    };
    let world = world(
        vec![
            startup(),
            volume("Backup", "/Volumes/Backup".as_ref(), "B", 1000, 250),
            locked,
        ],
        cx,
    );
    let (browser, cx) = open(&world, &project, cx);
    let names = |browser: &Entity<Browser>, cx: &mut VisualTestContext| {
        browser.read_with(cx, |browser, _| {
            let volumes = &browser.sidebar.volumes;
            volumes.iter().map(|v| v.name.clone()).collect::<Vec<_>>()
        })
    };
    assert_eq!(names(&browser, cx), ["Macintosh HD", "Backup", "Installer"]);
    assert!(on_screen(cx, "volume-bar-1"));
    let bar = cx.debug_bounds("volume-bar-1").unwrap().size.width.as_f32();
    let used = cx
        .debug_bounds("volume-used-1")
        .unwrap()
        .size
        .width
        .as_f32();
    assert!((used / bar - 0.75).abs() < 0.01, "{used} of {bar}");
    // Only the read-only one is marked; the startup disk has no Eject.
    assert!(on_screen(cx, "read-only-2"));
    assert!(!on_screen(cx, "read-only-1"));
    assert!(!on_screen(cx, "volume-eject-0"));
    assert!(on_screen(cx, "volume-eject-1"));
    // The status line has the space of the volume the folder is on.
    let space = in_pane(&browser, cx, |pane, _| pane.chrome.space.clone());
    assert_eq!(space, Some(format!("{} free", format_bytes(400),)));

    // Nothing new is listed until the watch speaks: no polling.
    world.fake.volumes.lock().unwrap().truncate(2);
    cx.run_until_parked();
    assert_eq!(names(&browser, cx).len(), 3);
    world.volumes_changed.try_send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(names(&browser, cx), ["Macintosh HD", "Backup"]);
    assert!(!on_screen(cx, "volume-bar-2"));
}

#[gpui::test]
fn the_startup_disk_leads_locations_with_cloud_folders_then_drives_after_it(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let ext = tempfile::tempdir().unwrap();
    let drive = ext.path().canonicalize().unwrap();
    let world = world(vec![startup(), volume("Backup", &drive, "B", 100, 10)], cx);
    *world.fake.cloud.lock().unwrap() = vec![
        CloudFolder {
            name: "iCloud Drive".into(),
            path: project.join("docs"),
        },
        CloudFolder {
            name: "ProtonDrive".into(),
            path: project.join("zeta"),
        },
    ];
    let owned = own(cx);
    let (browser, cx) = open(&world, &project, cx);
    let first = favorites(&browser, cx).len() + 2;
    let rows: Vec<_> = (first..first + 4)
        .map(|ix| browser.read_with(cx, |b, _| b.row_at(ix)))
        .collect();
    let expected = [Row::Volume(0), Row::Cloud(0), Row::Cloud(1), Row::Volume(1)];
    assert_eq!(rows, expected.map(Some));
    press_on(cx, &format!("place-{}", first + 2));
    assert_eq!(here(&browser, cx), project.join("zeta"));
    // From the cloud folder that's open, the keys go on to the drives.
    let focus_sidebar = |cx: &mut VisualTestContext| {
        browser.update_in(cx, |b, window, cx| window.focus(&b.sidebar_focus, cx));
        cx.run_until_parked();
    };
    focus_sidebar(cx);
    press(cx, "up enter");
    assert_eq!(here(&browser, cx), project.join("docs"));
    focus_sidebar(cx);
    press(cx, "down down cmd-e");
    assert_eq!(*world.fake.ejected.lock().unwrap(), vec![drive]);
    release(owned, cx);
}

#[gpui::test]
fn eject_shows_macos_refusal_and_leaves_a_drive_that_went(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mount = dir.path().canonicalize().unwrap();
    let world = world(
        vec![startup(), volume("Backup", &mount, "B", 1000, 250)],
        cx,
    );
    let owned = own(cx);
    let (browser, cx) = open(&world, &mount, cx);
    *world.fake.refusal.lock().unwrap() = Some("Backup: Spotify (pid 7) is using it.".into());
    press_on(cx, "volume-eject-1");
    assert_eq!(
        toast(&browser, cx),
        Some(("Backup: Spotify (pid 7) is using it.".into(), true, false))
    );
    assert!(world.fake.ejected.lock().unwrap().is_empty());
    // Clicking Eject doesn't also open the drive's row.
    assert_eq!(in_pane(&browser, cx, |b, _| b.history.len()), 1);

    press_on(cx, "volume-eject-1");
    assert_eq!(*world.fake.ejected.lock().unwrap(), vec![mount.clone()]);
    assert_eq!(
        toast(&browser, cx),
        Some(("Ejected it.".into(), false, false))
    );
    assert_eq!(browser.read_with(cx, |b, _| b.sidebar.volumes.len()), 1);
    assert_eq!(here(&browser, cx), home());
    release(owned, cx);
}

#[gpui::test]
fn edit_mode_shows_handles_and_removes_and_reorders_for_good(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (one, two) = two_favorites(&world);
    let (browser, cx) = open(&world, &project, cx);
    assert_eq!(favorites(&browser, cx)[..2], [one.clone(), two.clone()]);
    // Outside Edit mode: no handles or ✕, and a click opens the row.
    assert!(!on_screen(cx, "favorite-handle-0"));
    assert!(!on_screen(cx, "favorite-remove-0"));
    press_on(cx, "place-2");
    assert_eq!(here(&browser, cx), one);
    press(cx, "cmd-[");

    click(cx, "favorites-edit");
    assert!(on_screen(cx, "favorite-handle-0"));
    assert!(on_screen(cx, "favorite-remove-0"));
    // In Edit mode a click on the row goes nowhere, and Recents has no ✕.
    press_on(cx, "place-2");
    assert_eq!(here(&browser, cx), project);
    let rows = favorites(&browser, cx).len();
    assert!(!on_screen(cx, &format!("favorite-remove-{rows}")));

    // Dragging the first handle onto the third row moves it there.
    let from = center(cx, "favorite-handle-0");
    let to = center(cx, "place-4");
    cx.simulate_mouse_down(from, MouseButton::Left, with(""));
    cx.simulate_mouse_move(to, MouseButton::Left, with(""));
    cx.run_until_parked();
    cx.simulate_mouse_up(to, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert_eq!(saved(&world.store)[2], one);
    assert_eq!(saved(&world.store)[0], two);
    assert_eq!(favorites(&browser, cx)[2], one);

    // A drop in the gap under a row counts as a drop on that row.
    let from = center(cx, "favorite-handle-2");
    let row = cx.debug_bounds("place-2").unwrap();
    let to = point(row.center().x, row.bottom() + px(1.));
    cx.simulate_mouse_down(from, MouseButton::Left, with(""));
    cx.simulate_mouse_move(to, MouseButton::Left, with(""));
    cx.run_until_parked();
    cx.simulate_mouse_up(to, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert_eq!(saved(&world.store)[0], one);
    browser.update(cx, |b, cx| b.move_favorite(0, 2, cx));
    cx.run_until_parked();

    // The ✕ forgets only the bookmark.
    press_on(cx, "favorite-remove-2");
    assert!(!saved(&world.store).contains(&one));
    assert!(one.is_dir());
    assert!(!favorites(&browser, cx).contains(&one));

    // Done, or Esc, is the way out.
    click(cx, "favorites-edit");
    assert!(!on_screen(cx, "favorite-handle-0"));
    click(cx, "favorites-edit");
    press(cx, "escape");
    assert!(!on_screen(cx, "favorite-handle-0"));
}

#[gpui::test]
fn command_and_a_number_opens_that_favorite(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (one, two) = two_favorites(&world);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-2");
    assert_eq!(here(&browser, cx), two);
    press(cx, "cmd-1");
    assert_eq!(here(&browser, cx), one);
    // Past the last favorite, nothing moves.
    assert!(favorites(&browser, cx).len() < 10);
    press(cx, "cmd-0");
    assert_eq!(here(&browser, cx), one);
}

#[gpui::test]
fn holding_command_alone_shows_the_favorites_keys(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    two_favorites(&world);
    let (_browser, cx) = open(&world, &project, cx);
    let hold = |cx: &mut VisualTestContext, keys: Modifiers| {
        cx.simulate_modifiers_change(keys);
        cx.run_until_parked();
    };
    let shown = |cx: &mut VisualTestContext| cx.debug_bounds("favorite-key-0").is_some();
    assert!(!shown(cx));
    hold(cx, Modifiers::command());
    assert!(shown(cx));
    assert!(cx.debug_bounds("favorite-key-1").is_some());
    // ⇧⌘1 is another shortcut, so the hint would be wrong.
    hold(cx, Modifiers::command_shift());
    assert!(!shown(cx));
    hold(cx, Modifiers::command());
    assert!(shown(cx));
    hold(cx, Modifiers::none());
    assert!(!shown(cx));
    // Edit mode has its own controls at the row's end.
    click(cx, "favorites-edit");
    hold(cx, Modifiers::command());
    assert!(!shown(cx));
}

#[gpui::test]
fn an_unavailable_favorite_says_why_and_never_opens_another_drive(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let dir = tempfile::tempdir().unwrap();
    let mount = dir.path().canonicalize().unwrap().join("Backup");
    std::fs::create_dir_all(mount.join("Photos")).unwrap();
    // Pinned on drive A; drive B of the same name now mounts where A did.
    let world = world(vec![startup(), volume("Backup", &mount, "B", 10, 5)], cx);
    let drive_a = [startup(), volume("Backup", &mount, "A", 10, 5)];
    let photos = mount.join("Photos");
    world.store.pin(&photos, Some(0), &drive_a).unwrap();
    let (browser, cx) = open(&world, &project, cx);
    assert_eq!(favorites(&browser, cx)[0], photos);
    press_on(cx, "place-2");
    assert_eq!(
        toast(&browser, cx),
        Some(("Backup isn't connected.".into(), true, false))
    );
    assert_eq!(here(&browser, cx), project);
    // It can still be moved and forgotten while it's away.
    click(cx, "favorites-edit");
    browser.update(cx, |b, cx| b.move_favorite(0, 1, cx));
    cx.run_until_parked();
    let held = world.store.places(&drive_a).unwrap();
    assert_eq!(held[1].favorite.path, photos);
    assert_eq!(held[1].favorite.volume.as_ref().unwrap().name, "Backup");
    press_on(cx, "favorite-remove-1");
    assert!(!saved(&world.store).contains(&photos));
    assert!(photos.is_dir());
}

#[gpui::test]
fn add_to_sidebar_takes_a_folder_and_remove_forgets_it(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    // Rows: docs, zeta, a.txt. ⌃⌘T does nothing on a file.
    press(cx, "down down down ctrl-cmd-t");
    assert!(!saved(&world.store).iter().any(|p| p.ends_with("a.txt")));
    press(cx, "up up ctrl-cmd-t");
    let real = project.canonicalize().unwrap();
    let (docs, zeta) = (real.join("docs"), real.join("zeta"));
    assert_eq!(favorites(&browser, cx).last(), Some(&docs));
    assert_eq!(saved(&world.store).last(), Some(&docs));

    // An agent's change shows when the watch speaks.
    world.store.pin(&zeta, None, &[startup()]).unwrap();
    assert!(!favorites(&browser, cx).contains(&zeta));
    world.lists_changed.try_send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(favorites(&browser, cx).last(), Some(&zeta));

    // The menu on each kind of row offers what applies.
    let rows = favorites(&browser, cx).len();
    let menu_on = |row: usize, cx: &mut VisualTestContext| {
        let at = center(cx, &format!("place-{row}"));
        right_click(cx, at);
        cx.run_until_parked();
        let titles = menu(&browser, cx).unwrap().0;
        press(cx, "escape");
        titles
    };
    assert_eq!(
        menu_on(rows + 1, cx),
        ["Open", "Open in New Tab", "Remove from Sidebar"]
    );
    assert_eq!(menu_on(1, cx), ["Open", "Open in New Tab", "Clear Recents"]);
    assert_eq!(menu_on(0, cx), ["Open", "Open in New Tab"]);
    assert_eq!(menu_on(rows + 2, cx), ["Open", "Open in New Tab"]);

    // The gap under a row answers as the row does.
    let row = cx.debug_bounds("place-1").unwrap();
    let gap = point(row.center().x, row.bottom() + px(1.));
    right_click(cx, gap);
    cx.run_until_parked();
    assert_eq!(
        menu(&browser, cx).unwrap().0,
        ["Open", "Open in New Tab", "Clear Recents"]
    );
    press(cx, "escape");
    cx.simulate_click(gap, with(""));
    cx.run_until_parked();
    assert!(in_pane(&browser, cx, |b, _| b.showing_recents()));
    press(cx, "cmd-[");

    // Remove from Sidebar through that menu.
    let at = center(cx, &format!("place-{}", rows + 1));
    right_click(cx, at);
    cx.run_until_parked();
    press(cx, "up enter");
    assert!(!saved(&world.store).contains(&zeta));
    assert!(zeta.is_dir());
}

#[gpui::test]
fn the_trash_opens_with_its_bar_and_back_returns(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let owned = own(cx);
    // Empty Trash erases the owner's Trash, as it's the app's one in `run`.
    let trash = owned.paths.trash.clone();
    cx.update(|cx| cx.global_mut::<Places>().trash = trash.clone());
    std::fs::create_dir_all(&trash).unwrap();
    for name in ["old.txt", "older.txt"] {
        std::fs::write(trash.join(name), "").unwrap();
    }
    let (browser, cx) = open(&world, &project, cx);
    assert!(!on_screen(cx, "put-back"));
    click(cx, "open-trash");
    assert_eq!(shown(&browser, cx).0, ["old.txt", "older.txt"]);
    assert_eq!(in_pane(&browser, cx, |b, _| b.trash_header.count), Some(2));
    assert!(on_screen(cx, "put-back"));
    assert!(on_screen(cx, "empty-trash"));
    assert_eq!(in_pane(&browser, cx, |b, _| b.put_back().1), None);

    // Monodrive didn't trash this, so it never guesses where it came from.
    press(cx, "down");
    let (known, why) = in_pane(&browser, cx, |b, _| b.put_back());
    assert!(known.is_empty());
    assert!(why.unwrap().contains("doesn't know where it came from"));
    click(cx, "put-back");
    assert!(trash.join("old.txt").exists());
    assert_eq!(toast(&browser, cx), None);

    // Back returns; the bar leaves with the Trash.
    press(cx, "cmd-[");
    assert_eq!(here(&browser, cx), project);
    assert!(!on_screen(cx, "put-back"));

    // Its button opens the same card as ⇧⌘⌫, and an empty Trash can't empty.
    press(cx, "cmd-]");
    click(cx, "empty-trash");
    assert!(browser.read_with(cx, |b, _| matches!(b.sheet, Some(Sheet::EmptyTrash(_)))));
    press(cx, "enter");
    assert_eq!(in_pane(&browser, cx, |b, _| b.trash_header.count), Some(0));
    click(cx, "empty-trash");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    release(owned, cx);
}

#[gpui::test]
fn put_back_returns_what_monodrive_trashed(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let owned = own(cx);
    let notes = project.canonicalize().unwrap().join("put-back-test.txt");
    std::fs::write(&notes, "keep").unwrap();
    let (browser, cx) = open(&world, &project, cx);
    // Through the window's owner, so its history knows. The file does go to
    // the real Trash, and this test takes it back out.
    let trashed = cx.update(|_, cx| {
        let owner = cx.global::<Operations>().0.clone();
        let outcome = lock(&owner)
            .submit(
                Origin::Window,
                Request::Trash {
                    paths: vec![notes.clone()],
                },
            )
            .unwrap();
        outcome.items[0].result.clone().unwrap()
    });
    assert!(!notes.exists());
    click(cx, "open-trash");
    // The Trash's rows, as if listed: this test's Trash is a temp folder.
    let entry = monodrive_core::entries::entry_at(&trashed).unwrap();
    on_pane(&browser, cx, |browser, cx| {
        browser.selection.select(&entry);
        browser.entries = vec![entry];
        cx.notify();
    });
    cx.run_until_parked();
    let (known, why) = in_pane(&browser, cx, |b, _| b.put_back());
    assert_eq!(known, vec![trashed.clone()]);
    assert_eq!(why, None);
    click(cx, "put-back");
    assert_eq!(std::fs::read_to_string(&notes).unwrap(), "keep");
    assert!(!trashed.exists());
    let said = toast(&browser, cx).unwrap();
    assert!(said.0.starts_with("Put back") && !said.1, "{said:?}");
    release(owned, cx);
}

#[gpui::test]
fn recents_list_what_was_opened_mark_what_went_and_clear(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let project = project.canonicalize().unwrap();
    let (kept, gone) = (project.join("a.txt"), project.join("gone.txt"));
    world.store.opened(&gone).unwrap();
    let (browser, cx) = open(&world, &project, cx);
    // Opening a file in the window opens it in its app and remembers it.
    press(cx, "down down down cmd-o");
    assert_eq!(*world.fake.opened.lock().unwrap(), vec![kept.clone()]);
    assert_eq!(world.store.recents().unwrap()[0], kept);

    press(cx, "cmd-shift-f");
    assert_eq!(shown(&browser, cx).0, ["a.txt", "gone.txt"]);
    let missing = in_pane(&browser, cx, |b, _| b.missing.clone());
    assert_eq!(missing, HashSet::from([gone.clone()]));
    // A gone one stays in the list, and opening it says so.
    press(cx, "down down cmd-o");
    let said = toast(&browser, cx).unwrap();
    assert_eq!(said.0, "\"gone.txt\" is no longer there.");
    assert_eq!(world.fake.opened.lock().unwrap().len(), 1);

    // What opened elsewhere shows when the watch speaks.
    world.store.opened(&project.join("zeta")).unwrap();
    world.lists_changed.try_send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).0[0], "zeta");

    // Back leaves it, and Forward comes back to it.
    press(cx, "cmd-[");
    assert!(in_pane(&browser, cx, |b, _| !b.showing_recents()));
    assert_eq!(shown(&browser, cx).0, ["docs", "zeta", "a.txt"]);
    press(cx, "cmd-]");
    assert!(in_pane(&browser, cx, |b, _| b.showing_recents()));

    // Clear Recents is in the Recents row's menu.
    let at = center(cx, "place-1");
    right_click(cx, at);
    cx.run_until_parked();
    press(cx, "up enter");
    assert_eq!(world.store.recents(), Ok(Vec::new()));
    assert!(shown(&browser, cx).0.is_empty());

    // So is the button over the list, which empties the list where it is.
    world.store.opened(&kept).unwrap();
    world.lists_changed.try_send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).0, ["a.txt"]);
    press_on(cx, "clear-recents");
    assert_eq!(world.store.recents(), Ok(Vec::new()));
    assert!(shown(&browser, cx).0.is_empty());
}

#[gpui::test]
fn connect_validates_shows_failures_and_saves_only_what_connected(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    world.store.save_server("smb://nas.local/media").unwrap();
    world
        .store
        .save_server("smb://studio.local/projects")
        .unwrap();
    let (browser, cx) = open(&world, &project, cx);
    let error = |browser: &Entity<Browser>, cx: &mut VisualTestContext| {
        browser.read_with(cx, |b, cx| match &b.sheet {
            Some(Sheet::Connect(view)) => view.read(cx).error.clone(),
            _ => panic!("the card is open"),
        })
    };
    let typed = |browser: &Entity<Browser>, cx: &mut VisualTestContext| {
        browser.read_with(cx, |b, cx| match &b.sheet {
            Some(Sheet::Connect(view)) => view.read(cx).field.read(cx).text().to_owned(),
            _ => panic!("the card is open"),
        })
    };
    press(cx, "cmd-shift-k");
    // ↩ with nothing typed, and with a password, stops here: no mount answer
    // is set, so reaching the mount would panic.
    press(cx, "enter");
    assert!(
        error(&browser, cx)
            .unwrap()
            .starts_with("Type a server address")
    );
    cx.simulate_input("smb://me:secret@nas/media");
    press(cx, "enter");
    assert!(
        error(&browser, cx)
            .unwrap()
            .starts_with("Leave the password out")
    );
    assert_eq!(world.store.servers().unwrap().len(), 2);

    // A click fills the address, ↩ connects, and a failure stays in the card.
    press_on(cx, "saved-server-1");
    assert_eq!(typed(&browser, cx), "smb://studio.local/projects");
    *world.fake.mounted.lock().unwrap() = Some(Err(
        "smb://studio.local/projects: the server didn't answer.".into(),
    ));
    press(cx, "enter");
    assert_eq!(
        error(&browser, cx).as_deref(),
        Some("smb://studio.local/projects: the server didn't answer.")
    );

    // ✕ forgets a saved one and doesn't fill the address.
    press_on(cx, "forget-server-0");
    assert_eq!(
        world.store.servers(),
        Ok(vec!["smb://studio.local/projects".to_string()])
    );
    assert_eq!(typed(&browser, cx), "smb://studio.local/projects");

    // A new one saves once it's connected; the card closes and the share
    // shows in the sidebar, without listing it.
    press(cx, "cmd-a");
    cx.simulate_input("nas2.local/photos");
    cx.run_until_parked();
    let mount: PathBuf = "/Volumes/photos".into();
    *world.fake.mounted.lock().unwrap() = Some(Ok(mount.clone()));
    world
        .fake
        .volumes
        .lock()
        .unwrap()
        .push(volume("photos", &mount, "P", 10, 5));
    press(cx, "enter");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    assert_eq!(
        world.store.servers(),
        Ok(vec![
            "smb://studio.local/projects".to_string(),
            "smb://nas2.local/photos".to_string()
        ])
    );
    assert_eq!(here(&browser, cx), project);
    assert_eq!(browser.read_with(cx, |b, _| b.sidebar.volumes.len()), 2);
    let said = toast(&browser, cx).unwrap();
    assert!(
        said.0.starts_with("Connected smb://nas2.local/photos"),
        "{said:?}"
    );

    // Esc and Cancel leave without connecting.
    press(cx, "cmd-shift-k escape");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    press(cx, "cmd-shift-k");
    click(cx, "connect-cancel");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
}

#[gpui::test]
fn a_connect_card_that_goes_away_cancels_its_mount_and_saves_nothing(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (browser, cx) = open(&world, &project, cx);
    // Settings and the palette each replace the card without dismissing it.
    for replace in ["cmd-,", "cmd-shift-p"] {
        press(cx, "cmd-shift-k");
        cx.simulate_input("nas.local/media");
        cx.run_until_parked();
        *world.fake.mounted.lock().unwrap() = Some(Ok("/Volumes/media".into()));
        let view = browser.read_with(cx, |b, _| match &b.sheet {
            Some(Sheet::Connect(view)) => view.clone(),
            _ => panic!("the card is open"),
        });
        // In flight: its work hasn't run yet.
        let cancel = view.update(cx, |view, cx| {
            view.connect(cx);
            view.connecting.clone().unwrap()
        });
        drop(view);
        assert!(!cancel.is_cancelled());
        press(cx, replace);
        assert!(browser.read_with(cx, |b, _| !matches!(b.sheet, Some(Sheet::Connect(_)))));
        assert!(cancel.is_cancelled(), "{replace}");
        assert_eq!(world.store.servers(), Ok(Vec::new()));
        press(cx, "escape");
        assert!(browser.read_with(cx, |b, _| b.sheet.is_none()), "{replace}");
    }
}

/// Whether the commands that change the selection's files would run now.
fn changing_files(cx: &mut VisualTestContext) -> [bool; 4] {
    cx.update(|window, cx| {
        [
            window.is_action_available(&MoveToTrash, cx),
            window.is_action_available(&Duplicate, cx),
            window.is_action_available(&Rename, cx),
            window.is_action_available(&NewFolderWithSelection, cx),
        ]
    })
}

#[gpui::test]
fn command_backspace_in_the_sidebar_never_reaches_the_lists_selection(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(
        vec![
            startup(),
            volume("Backup", "/Volumes/Backup".as_ref(), "B", 1000, 250),
        ],
        cx,
    );
    let owned = own(cx);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "down down down");
    let file = project.join("a.txt");
    assert_eq!(shown(&browser, cx).1, ["a.txt"]);
    let favorites = browser.read_with(cx, |b, _| b.sidebar.places.len());
    // The Overview, Recents, then each volume.
    for row in [0, 1, favorites + 2, favorites + 3] {
        browser.update_in(cx, |b, window, cx| {
            b.sidebar_cursor = row;
            window.focus(&b.sidebar_focus, cx);
            cx.notify();
        });
        cx.run_until_parked();
        // Handled here, so macOS doesn't offer it to the menu bar.
        let stroke = Keystroke::parse("cmd-backspace").unwrap();
        let handled = cx.update(|window, cx| window.dispatch_keystroke(stroke, cx));
        cx.run_until_parked();
        assert!(handled, "row {row}");
        // The menu bar's items that change these files are off, and sending
        // Move to Trash does nothing.
        assert_eq!(changing_files(cx), [false; 4], "row {row}");
        cx.dispatch_action(MoveToTrash);
        cx.run_until_parked();
        assert!(file.exists(), "row {row}");
    }
    assert_eq!(
        browser.read_with(cx, |b, _| b.sidebar.places.len()),
        favorites
    );
    // With the list's keys back, they're the list's commands again.
    let pane = pane_of(&browser, cx);
    pane.update_in(cx, |b, window, cx| b.focus_list(window, cx));
    cx.run_until_parked();
    assert_eq!(changing_files(cx), [true; 4]);
    release(owned, cx);
}

/// The palette that's open.
pub(super) fn palette(
    browser: &Entity<Browser>,
    cx: &mut VisualTestContext,
) -> Entity<PaletteView> {
    browser.read_with(cx, |b, _| match &b.sheet {
        Some(Sheet::Palette(view)) => view.clone(),
        _ => panic!("the palette is open"),
    })
}

/// Each row's title, in its section.
pub(super) fn sections(
    view: &Entity<PaletteView>,
    cx: &mut VisualTestContext,
) -> Vec<(String, Vec<String>)> {
    view.read_with(cx, |view, _| {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for row in &view.rows {
            if out.last().is_none_or(|(name, _)| name != row.section) {
                out.push((row.section.to_string(), Vec::new()));
            }
            out.last_mut().unwrap().1.push(row.title.to_string());
        }
        out
    })
}

pub(super) fn titles(view: &Entity<PaletteView>, cx: &mut VisualTestContext) -> Vec<String> {
    view.read_with(cx, |view, _| {
        view.rows.iter().map(|row| row.title.to_string()).collect()
    })
}

pub(super) fn under_keys(view: &Entity<PaletteView>, cx: &mut VisualTestContext) -> Option<String> {
    view.read_with(cx, |view, _| {
        let row = view.rows.get(view.active?)?;
        Some(row.title.to_string())
    })
}

pub(super) fn type_in(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    cx.run_until_parked();
}

fn found(path: &Path, kind: Kind) -> Entry {
    Entry {
        path: path.into(),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        kind,
        size: 0,
        modified: 0,
        availability: monodrive_core::cloud::Availability::Unknown,
    }
}

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
        commands::apply(cx);
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
    press(cx, "cmd-k");
    world.fake.volumes.lock().unwrap().remove(1);
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

#[gpui::test]
fn remote_locations_add_explain_connect_and_forget(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let project = project.canonicalize().unwrap();
    world.store.save_server("smb://nas.local/media").unwrap();
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "remote");
    press(cx, "enter");
    let shown = sections(&view, cx);
    assert_eq!(
        shown[0],
        ("SFTP servers".into(), vec!["Add Server…".into()])
    );
    assert_eq!(
        shown[1],
        ("SMB servers".into(), vec!["smb://nas.local/media".into()])
    );
    let add_enabled = view.read_with(cx, |v, _| v.rows[0].enabled);
    assert!(!add_enabled);
    // What's typed is the host. A bad one says why and saves nothing.
    type_in(cx, "-bad");
    press(cx, "enter");
    assert!(view.read_with(cx, |v, _| v.error.is_some()));
    assert_eq!(world.store.remotes(), Ok(Vec::new()));
    press(cx, "cmd-a backspace");
    assert!(view.read_with(cx, |v, _| v.error.is_none()));
    type_in(cx, "build-box");
    assert_eq!(titles(&view, cx)[0], "Add \"build-box\"");
    press(cx, "enter");
    let saved = world.store.remotes().unwrap();
    assert_eq!(saved[0].host, "build-box");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("build-box"));
    // Choosing one opens it in the pane.
    press(cx, "enter");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    let source = in_pane(&browser, cx, |pane, _| {
        pane.remote_source().map(str::to_owned)
    });
    assert_eq!(source.as_deref(), Some("build-box"));
    // ⌘⌫ forgets it.
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "remote");
    press(cx, "enter");
    assert_eq!(under_keys(&view, cx).as_deref(), Some("build-box"));
    press(cx, "cmd-backspace");
    assert_eq!(world.store.remotes(), Ok(Vec::new()));
    assert_eq!(titles(&view, cx)[0], "Add Server…");
    // A saved SMB server connects through the Connect card.
    assert_eq!(
        under_keys(&view, cx).as_deref(),
        Some("smb://nas.local/media")
    );
    *world.fake.mounted.lock().unwrap() = Some(Ok(project.join("docs")));
    press(cx, "enter");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    let said = toast(&browser, cx).unwrap().0;
    assert!(
        said.starts_with("Connected smb://nas.local/media"),
        "{said}"
    );
    // And it can be forgotten here too.
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "remote");
    press(cx, "enter");
    press(cx, "cmd-backspace");
    assert_eq!(world.store.servers(), Ok(Vec::new()));
    // Off a saved server, ⌘⌫ is the query's own: it deletes to the line's start.
    press(cx, "escape");
    press(cx, "cmd-backspace");
    assert_eq!(view.read_with(cx, |v, _| v.page), Page::Root);
    assert_eq!(under_keys(&view, cx).as_deref(), Some("New Folder"));
}

#[gpui::test]
fn drives_show_free_space_open_and_eject(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let project = project.canonicalize().unwrap();
    let backup = project.join("zeta");
    let world = world(
        vec![startup(), volume("Backup", &backup, "B", 1000, 250)],
        cx,
    );
    let owned = own(cx);
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "drives");
    press(cx, "enter");
    assert_eq!(titles(&view, cx), ["Macintosh HD", "Backup"]);
    let about = view.read_with(cx, |v, _| v.rows[1].subtitle.clone());
    assert_eq!(about.as_deref(), Some("250 bytes free"));
    // The startup disk has nothing to eject.
    press(cx, "cmd-e");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_some()));
    assert!(world.fake.ejected.lock().unwrap().is_empty());
    press(cx, "down");
    press(cx, "cmd-e");
    assert!(browser.read_with(cx, |b, _| b.sheet.is_none()));
    assert_eq!(world.fake.ejected.lock().unwrap()[0], backup);
    // Return shows a drive in the window.
    *world.fake.volumes.lock().unwrap() =
        vec![startup(), volume("Backup", &backup, "B", 1000, 250)];
    world.volumes_changed.try_send(()).unwrap();
    cx.run_until_parked();
    press(cx, "cmd-k");
    type_in(cx, "drives");
    press(cx, "enter down enter");
    assert_eq!(here(&browser, cx), backup);
    release(owned, cx);
}

#[gpui::test]
fn files_from_the_index_come_after_the_rest_and_stale_answers_drop(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let project = project.canonicalize().unwrap();
    world.store.opened(&project.join("a.txt")).unwrap();
    *world.fake.files.lock().unwrap() = vec![
        found(&project.join("zeta"), Kind::Dir),
        found(&project.join("a.txt"), Kind::File),
        found(&project.join("notes.md"), Kind::File),
    ];
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    assert!(sections(&view, cx).iter().all(|(name, _)| name != "Files"));
    type_in(cx, "a");
    let shown = sections(&view, cx);
    let files = shown.last().unwrap();
    // After everything else, without the recent that's already above it.
    assert_eq!(files.0, "Files");
    assert_eq!(files.1, ["zeta", "notes.md"]);
    // An answer to what was typed before is dropped, and today's stays.
    view.update(cx, |view, cx| {
        view.land_files(
            "something else",
            vec![found(&project.join("old"), Kind::File)],
            cx,
        )
    });
    cx.run_until_parked();
    assert_eq!(sections(&view, cx).last().unwrap().1, ["zeta", "notes.md"]);
    // A folder shows in the window; the search never started the index.
    let zeta = titles(&view, cx).iter().position(|t| t == "zeta").unwrap();
    view.update(cx, |view, cx| {
        view.active = Some(zeta);
        cx.notify();
    });
    cx.run_until_parked();
    press(cx, "enter");
    assert_eq!(here(&browser, cx), project.join("zeta"));
    assert!(browser.read_with(cx, |b, _| b.index.get().is_none()));
    // Without an index the section just isn't there.
    world.fake.files.lock().unwrap().clear();
    press(cx, "cmd-k");
    let view = palette(&browser, cx);
    type_in(cx, "a");
    assert!(sections(&view, cx).iter().all(|(name, _)| name != "Files"));
}

/// `.DS_Store` and `app.app` in `folder`, which it makes.
fn with_a_dotfile(folder: &Path) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::write(folder.join(".DS_Store"), "").unwrap();
    std::fs::create_dir(folder.join("app.app")).unwrap();
}

#[gpui::test]
fn the_trash_and_applications_never_show_hidden_files_and_other_folders_do(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let root = world.dir.path().canonicalize().unwrap();
    let (trash, applications) = (root.join("Trash"), root.join("Applications"));
    with_a_dotfile(&trash);
    with_a_dotfile(&applications);
    // A subfolder of Applications isn't Applications.
    std::fs::write(applications.join("app.app/.config"), "").unwrap();
    let (browser, cx) = open(&world, &project, cx);
    press(cx, "cmd->");
    assert_eq!(shown(&browser, cx).0, ["docs", "zeta", ".env", "a.txt"]);
    let go = |to: PathBuf, cx: &mut VisualTestContext| {
        on_pane(&browser, cx, |pane, cx| pane.navigate(to, cx));
        cx.run_until_parked();
    };
    go(applications.clone(), cx);
    assert_eq!(shown(&browser, cx).0, ["app.app"]);
    go(applications.join("app.app"), cx);
    assert_eq!(shown(&browser, cx).0, [".config"]);
    go(trash, cx);
    assert_eq!(shown(&browser, cx).0, ["app.app"]);
    // The Trash's bar counts what's shown; the link leads to the same rule.
    assert_eq!(in_pane(&browser, cx, |b, _| b.trash_header.count), Some(1));
    let link = root.join("link");
    std::os::unix::fs::symlink(&applications, &link).unwrap();
    go(link, cx);
    assert_eq!(shown(&browser, cx).0, ["app.app"]);
}

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
        commands::apply(cx);
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
    let roots = super::application_roots(Path::new("/Users/someone"));
    let system = Path::new("/System/Applications");
    assert!(roots.iter().any(|root| root == system));
    assert!(!sidebar::lists_hidden(system, true, &roots));
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("to-apps");
    std::os::unix::fs::symlink(system, &link).unwrap();
    assert!(!sidebar::lists_hidden(&link, true, &roots));
    assert!(sidebar::lists_hidden(dir.path(), true, &roots));
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

use super::drag_tests::{carry, hover, says};

#[gpui::test]
fn a_favorite_takes_files_and_the_edges_between_favorites_pin_folders(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    std::fs::write(project.join("b.txt"), "").unwrap();
    let world = world(vec![startup()], cx);
    let (one, two) = two_favorites(&world);
    let mut owned = own(cx);
    let (browser, cx) = open(&world, &project, cx);
    let (docs, file) = (row(&browser, 0, cx), row(&browser, 2, cx));
    let name_of = |path: &Path| path.file_name().unwrap().to_string_lossy().into_owned();
    // The middle of a favorite is that folder.
    let middle = center(cx, "place-2");
    carry(file, middle, "", cx);
    assert_eq!(
        says(&browser, cx).unwrap(),
        format!("Move a.txt to {}", name_of(&one))
    );
    cx.simulate_mouse_up(middle, MouseButton::Left, with(""));
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(one.join("a.txt").exists());
    // A file on a favorite's edge has nowhere to pin, so it still goes in.
    let edge = cx.debug_bounds("place-3").unwrap().top_center() + point(px(0.), px(2.));
    let file = row(&browser, 2, cx);
    carry(file, edge, "", cx);
    assert_eq!(
        says(&browser, cx).unwrap(),
        format!("Move b.txt to {}", name_of(&two))
    );
    press(cx, "escape");
    cx.simulate_mouse_up(edge, MouseButton::Left, with(""));
    // A folder on the same edge pins between the two, with the line there.
    carry(docs, edge, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Pin docs to Favorites");
    assert_eq!(browser.read_with(cx, |b, _| b.drag.pins_at()), Some(1));
    cx.simulate_mouse_up(edge, MouseButton::Left, with(""));
    cx.run_until_parked();
    let pinned = project.canonicalize().unwrap().join("docs");
    assert_eq!(saved(&world.store)[..3], [one.clone(), pinned, two.clone()]);
    // Pinning moves nothing.
    assert!(project.join("docs/inner.md").exists());
    // The Favorites header pins before the first.
    let zeta = row(&browser, 1, cx);
    let under = cx.debug_bounds("favorites-header").unwrap().center();
    carry(zeta, under, "", cx);
    assert_eq!(browser.read_with(cx, |b, _| b.drag.pins_at()), Some(0));
    cx.simulate_mouse_up(under, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert_eq!(
        saved(&world.store)[0],
        project.canonicalize().unwrap().join("zeta")
    );
    release(owned, cx);
}

#[gpui::test]
fn a_drive_takes_a_copy_as_another_volume_and_a_read_only_one_takes_nothing(
    cx: &mut TestAppContext,
) {
    let (_dir, project) = folder();
    let (ext, locked) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let drive = ext.path().canonicalize().unwrap();
    let world = world(
        vec![
            startup(),
            volume("Ext", &drive, "ext", 100, 10),
            Volume {
                read_only: true,
                ..volume("Locked", locked.path(), "locked", 100, 10)
            },
        ],
        cx,
    );
    let mut owned = own(cx);
    let (browser, cx) = open(&world, &project, cx);
    let first = favorites(&browser, cx).len() + 2;
    let (ext_row, locked_row) = (
        center(cx, &format!("place-{}", first + 1)),
        center(cx, &format!("place-{}", first + 2)),
    );
    let file = row(&browser, 2, cx);
    carry(file, ext_row, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Copy a.txt to Ext");
    // ⌘ forces a move even there.
    cx.simulate_modifiers_change(with("cmd"));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(says(&browser, cx).unwrap(), "Move a.txt to Ext");
    cx.simulate_modifiers_change(Default::default());
    cx.simulate_mouse_move(locked_row, MouseButton::Left, with(""));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(says(&browser, cx), None);
    cx.simulate_mouse_move(ext_row, MouseButton::Left, with(""));
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.simulate_mouse_up(ext_row, MouseButton::Left, with(""));
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(drive.join("a.txt").exists() && project.join("a.txt").exists());
    release(owned, cx);
}

#[gpui::test]
fn pinning_folders_already_in_favorites_lands_where_the_line_showed(cx: &mut TestAppContext) {
    let world = world(vec![startup()], cx);
    let (one, two) = two_favorites(&world);
    let root = world.dir.path().canonicalize().unwrap();
    let three = root.join("Three");
    std::fs::create_dir(&three).unwrap();
    world.store.pin(&three, Some(2), &[startup()]).unwrap();
    let (browser, cx) = open(&world, &root, cx);
    let row_of = |name: &str, cx: &mut VisualTestContext| {
        let ix = in_pane(&browser, cx, |pane, _| {
            pane.entries.iter().position(|e| e.name == name).unwrap()
        });
        row(&browser, ix, cx)
    };
    // One, to the line between Two and Three.
    let edge = cx.debug_bounds("place-4").unwrap().top_center() + point(px(0.), px(2.));
    let from = row_of("One", cx);
    carry(from, edge, "", cx);
    assert_eq!(browser.read_with(cx, |b, _| b.drag.pins_at()), Some(2));
    cx.simulate_mouse_up(edge, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert_eq!(
        saved(&world.store)[..3],
        [two.clone(), one.clone(), three.clone()]
    );
    // Two together, above the line they're dragged to: they end up right
    // above what's under it, in the order shown.
    let fourth = saved(&world.store)[3].clone();
    let (from, also) = (row_of("One", cx), row_of("Three", cx));
    cx.simulate_click(from, with(""));
    cx.simulate_click(also, with("cmd"));
    let edge = cx.debug_bounds("place-5").unwrap().top_center() + point(px(0.), px(2.));
    carry(from, edge, "", cx);
    assert_eq!(browser.read_with(cx, |b, _| b.drag.pins_at()), Some(3));
    cx.simulate_mouse_up(edge, MouseButton::Left, with(""));
    cx.run_until_parked();
    assert_eq!(saved(&world.store)[..4], [two, one, three, fourth]);
}

#[gpui::test]
fn a_folder_through_a_link_counts_as_the_volume_it_leads_to(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let ext = tempfile::tempdir().unwrap();
    let drive = ext.path().canonicalize().unwrap();
    std::fs::create_dir(drive.join("sub")).unwrap();
    std::fs::write(drive.join("sub/there.txt"), "").unwrap();
    let outside = project.join("outside");
    std::os::unix::fs::symlink(drive.join("sub"), &outside).unwrap();
    let world = world(vec![startup(), volume("Ext", &drive, "ext", 100, 10)], cx);
    let (one, _) = two_favorites(&world);
    let (browser, cx) = open(&world, &outside, cx);
    // Its path is on the startup disk, and the file is really on the drive.
    let middle = center(cx, "place-2");
    let file = row(&browser, 0, cx);
    carry(file, middle, "", cx);
    let name = one.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        says(&browser, cx).unwrap(),
        format!("Copy there.txt to {name}")
    );
    press(cx, "escape");
    cx.simulate_mouse_up(middle, MouseButton::Left, with(""));
    // Across a link the other way: the pane showing it is on the drive too.
    press(cx, "cmd-alt-s");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let right = browser.read_with(cx, |b, _| b.tabs[b.active].focused().clone());
    right.update_in(cx, |pane, window, cx| pane.go(project.clone(), window, cx));
    cx.run_until_parked();
    press(cx, "shift-tab");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let file_right = right.read_with(cx, |pane, cx| {
        let height = cx.global::<Preferences>().row_size.metrics().0;
        let ix = pane.entries.iter().position(|e| e.name == "a.txt").unwrap();
        pane.list_bounds.get().origin
            + point(
                px(60.),
                px(theme::CARD_PADDING + height * (ix as f32 + 0.5)),
            )
    });
    let space_left = {
        let bounds =
            browser.read_with(cx, |b, cx| b.tabs[b.active].pane.read(cx).list_bounds.get());
        bounds.origin + point(px(60.), px(300.))
    };
    carry(file_right, space_left, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Copy a.txt to outside");
    press(cx, "escape");
    cx.simulate_mouse_up(space_left, MouseButton::Left, with(""));
}

#[gpui::test]
fn a_target_is_read_once_per_drag_and_one_that_cant_be_stays_a_copy(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let world = world(vec![startup()], cx);
    let (one, two) = two_favorites(&world);
    let (browser, cx) = open(&world, &project, cx);
    let (first, second) = (center(cx, "place-2"), center(cx, "place-3"));
    let file = row(&browser, 2, cx);
    // On one volume: a move, however long it stays over the same place.
    carry(file, first, "", cx);
    let name = |path: &Path| path.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        says(&browser, cx).unwrap(),
        format!("Move a.txt to {}", name(&one))
    );
    for at in [second, first, second, first] {
        hover(at, "", cx);
    }
    let asked = |path: &Path| {
        let asked = world.fake.devices_asked.lock().unwrap();
        asked.iter().filter(|p| *p == path).count()
    };
    assert_eq!((asked(&one), asked(&two)), (1, 1));
    press(cx, "escape");
    cx.simulate_mouse_up(first, MouseButton::Left, with(""));
    // A place that could raise a privacy prompt is never read, so it's a copy.
    world.fake.gated.lock().unwrap().push(two.clone());
    carry(file, second, "", cx);
    assert_eq!(
        says(&browser, cx).unwrap(),
        format!("Copy a.txt to {}", name(&two))
    );
    assert_eq!(asked(&two), 1);
    press(cx, "escape");
    cx.simulate_mouse_up(second, MouseButton::Left, with(""));
}

#[gpui::test]
fn resting_on_a_favorite_or_a_drive_opens_it_in_the_pane_commands_act_on(cx: &mut TestAppContext) {
    let (_dir, project) = folder();
    let ext = tempfile::tempdir().unwrap();
    let drive = ext.path().canonicalize().unwrap();
    let world = world(vec![startup(), volume("Ext", &drive, "ext", 100, 10)], cx);
    let (one, _) = two_favorites(&world);
    let (browser, cx) = open(&world, &project, cx);
    let rest = |cx: &mut VisualTestContext| {
        cx.executor().advance_clock(super::drag::SPRING_DELAY);
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    };
    let last = favorites(&browser, cx).len() + 3;
    let (favorite, volume_row) = (center(cx, "place-2"), center(cx, &format!("place-{last}")));
    let file = row(&browser, 2, cx);
    carry(file, favorite, "", cx);
    rest(cx);
    assert_eq!(here(&browser, cx), one);
    hover(volume_row, "", cx);
    rest(cx);
    assert_eq!(here(&browser, cx), drive);
    press(cx, "escape");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    assert_eq!(here(&browser, cx), project);
    cx.simulate_mouse_up(volume_row, MouseButton::Left, with(""));
}

#[gpui::test]
fn a_folder_behind_a_privacy_prompt_never_opens_under_a_drag_but_takes_a_drop(
    cx: &mut TestAppContext,
) {
    // A temp home with Documents gated, a folder gated behind a link, and a
    // plain one to show the rest still opens.
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let (documents, real, plain) = (
        home.join("Documents"),
        home.join("real"),
        home.join("plain"),
    );
    std::fs::create_dir_all(real.join("secret")).unwrap();
    std::fs::create_dir(&documents).unwrap();
    std::fs::create_dir(&plain).unwrap();
    std::fs::write(home.join("a.txt"), "").unwrap();
    let via = home.join("via");
    std::os::unix::fs::symlink(&real, &via).unwrap();
    std::fs::write(real.join("b.txt"), "").unwrap();
    let world = world(vec![startup()], cx);
    {
        let mut gated = world.fake.gated.lock().unwrap();
        gated.push(documents.clone());
        gated.push(real.join("secret"));
    }
    let mut owned = own(cx);
    let (browser, cx) = open(&world, &home, cx);
    let row_of = |name: &str, cx: &mut VisualTestContext| {
        let ix = in_pane(&browser, cx, |pane, _| {
            pane.entries.iter().position(|e| e.name == name).unwrap()
        });
        row(&browser, ix, cx)
    };
    let rest = |cx: &mut VisualTestContext| {
        cx.executor().advance_clock(super::drag::SPRING_DELAY);
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    };
    let (file, over) = (row_of("a.txt", cx), row_of("Documents", cx));
    carry(file, over, "", cx);
    // Its volume can't be read either, so the pill says a copy.
    assert_eq!(says(&browser, cx).unwrap(), "Copy a.txt to Documents");
    rest(cx);
    assert_eq!(here(&browser, cx), home);
    press(cx, "space");
    assert_eq!(here(&browser, cx), home);
    // The rest of the home still opens, so the wait above wasn't the reason.
    let other = row_of("plain", cx);
    hover(other, "", cx);
    press(cx, "space");
    assert_eq!(here(&browser, cx), plain);
    press(cx, "escape");
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    cx.simulate_mouse_up(other, MouseButton::Left, with(""));
    // The drop still goes in: that's what the person asked for.
    carry(file, over, "", cx);
    assert_eq!(says(&browser, cx).unwrap(), "Copy a.txt to Documents");
    cx.simulate_mouse_up(over, MouseButton::Left, with(""));
    restart(&mut owned, cx);
    cx.executor().advance_clock(jobs::CHECK);
    cx.run_until_parked();
    assert!(documents.join("a.txt").exists());
    // Through a link, by where it really leads.
    let pane = pane_of(&browser, cx);
    pane.update_in(cx, |pane, window, cx| pane.go(via.clone(), window, cx));
    cx.run_until_parked();
    let (file, secret) = (row_of("b.txt", cx), row_of("secret", cx));
    carry(file, secret, "", cx);
    rest(cx);
    assert_eq!(here(&browser, cx), via);
    press(cx, "escape");
    cx.simulate_mouse_up(secret, MouseButton::Left, with(""));
    release(owned, cx);
}

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
    std::fs::write(documents.join("a.txt"), "hello").unwrap();
    let (browser, cx) = open(&world, &home, cx);
    press(cx, "cmd-alt-o");
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
        .register_repository(monodrive_core::worktrees::Registration {
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
        .register_repository(monodrive_core::worktrees::Registration {
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
    cx.executor().advance_clock(super::inspector::READ_DELAY);
    cx.run_until_parked();
    assert_eq!(shown(&browser, cx).1, ["Private"]);
    assert!(!listed(&private, cx));
    let open = cx
        .debug_bounds("column-open")
        .expect("Open in the last column");
    cx.simulate_click(open.center(), Modifiers::none());
    cx.executor().advance_clock(super::inspector::READ_DELAY);
    cx.run_until_parked();
    assert!(listed(&private, cx));
    // Its neighbor lists as soon as the selection rests on it.
    press(cx, "down");
    cx.executor().advance_clock(super::inspector::READ_DELAY);
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

/// The server's output, which ends the server it came from when the
/// connection lets go of it.
struct ServerOut {
    out: std::process::ChildStdout,
    child: std::process::Child,
}

impl std::io::Read for ServerOut {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.out.read(buf)
    }
}

impl Drop for ServerOut {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Connects every source to macOS's own `sftp-server` over pipes, with no
/// ssh, network or keys, except a host named in `refused`, whose host key
/// has changed.
pub(super) fn local_sftp(refused: &'static [&'static str]) -> monodrive_core::remote::Connector {
    use monodrive_core::remote::{RemoteError, Source, Transport};
    Arc::new(move |source: &Source| {
        if refused.contains(&source.host.as_str()) {
            return Err(RemoteError::HostKeyChanged(source.host.clone()));
        }
        let mut child = std::process::Command::new("/usr/libexec/sftp-server")
            .current_dir("/")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| RemoteError::Failed(e.to_string()))?;
        let (stdin, out) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        Ok(Transport::new(ServerOut { out, child }, stdin))
    })
}
