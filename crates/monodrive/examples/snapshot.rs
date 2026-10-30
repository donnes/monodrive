//! Draws the browser to PNGs through GPUI's headless Metal renderer: no
//! window on screen, no screen recording, no index. Agents use it to look
//! at a UI change.
//!
//! `cargo run -p monodrive --example snapshot -- [folder]` writes
//! `target/snapshots/list.png`, `grid.png`, `rename.png`, `menu.png`,
//! `menu-grid.png`, `menu-folder.png`, `menu-trash.png`, `jobs.png`, `ring-waiting.png`,
//! `rename-error.png`, `rename-error-grid.png`, `toast.png` and
//! `empty-trash.png`.
//! Without a folder it draws a made-up one, so the pictures show nobody's
//! files. Rename edits the fifth name in place. The menus are the context
//! menu on the second entry, in the list and the grid, and on the folder.
//! The rest use an operation owner on a temp folder. The popover and the
//! ring show an agent's move there, waiting at a name that's taken. Then the
//! last entry is renamed to a name that's taken, then to a free one, which
//! is undone after the picture, and Empty Trash counts a Trash of 12 items.
//!
//! The sidebar shows a made-up set of volumes and a temp folder's favorites,
//! so nothing real is listed, mounted or ejected. The last pictures are
//! `sidebar-edit.png`, `connect.png`, `connect-failed.png`, `trash.png` and
//! `recents.png`. Then `palette.png`, `palette-query.png`, the palette's pages
//! (`palette-remote.png`, `palette-cloud.png`, `palette-drives.png`,
//! `palette-favorites.png`), `palette-selection.png`, `goto.png`, `settings-menu.png` and one
//! `settings-<page>.png` for each Settings page, `background.png`, the list
//! over a picture that runs from bright to dark, `narrow.png`, where the path
//! collapses, and `no-path-bar.png`.
//!
//! `tabs-one.png`, `tabs-three.png` and `tabs-many.png` show the top bar, and
//! `split.png` and `split-boxy.png` two panes with the keys on the right one.
//! `sidebar-closed.png` and `sidebar-closed-split.png` have no sidebar, and
//! `unavailable.png` is a restored tab whose drive is gone.
//!
//! `drag-move.png` and `drag-copy.png` catch a drag over a folder, with its
//! outline and pill, and `drag-pin.png` one between favorites, with the line.
//!
//! `onboarding-welcome.png`, `onboarding-access.png`,
//! `onboarding-access-asked.png`, `onboarding-access-granted.png`,
//! `onboarding-access-reopen.png`, `onboarding-agents.png`,
//! `onboarding-agents-skipped.png`, `onboarding-ready.png` and
//! `onboarding-ready-skipped.png` show each step and what the list shows for
//! it, with a made-up Mac behind them.
//!
//! `inspector-file.png`, `inspector-folder.png`, `inspector-several.png`,
//! `inspector-split.png` and `inspector-boxy.png` show the inspector, with a
//! made-up folder total. `open-with.png` and `open-with-always.png` show the
//! Open With submenu on made-up choices of the system's own apps, and
//! `settings-general.png` the Terminal row.
//!
//! All of it twice, once dark in `target/snapshots` and once light in
//! `target/snapshots/light`.

use gpui::{Action, HeadlessAppContext as Cx, Keystroke};
use gpui::{
    AppContext, Focusable, HeadlessAppContext, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, point, px, size,
};
use monodrive::agents::{self, Harness};
use monodrive::browser::{
    Browser, CancelRename, CommitRename, ConnectToServer, Drives, EditFavorites, EmptyTrash,
    EnclosingFolder, ExtendNext, FocusSearch, GoRecents, GoToFolder, GoTrash, NewTab, OpenSelected,
    OpenSettings, OpenWith, Operations, Places, Rename, SelectLast, SelectNext, ShowContextMenu,
    ShowCopiesAndMoves, ToggleSplit, Undo,
};
use monodrive::connect;
use monodrive::native::{self, Native, SizeDone};
use monodrive::onboarding::{Onboarding, Setup, Step};
use monodrive::preferences::{Preferences, Theme, View, WorkspaceStyle};
use monodrive::session::{PaneState, StopState, TabState, WindowState};
use monodrive_core::apps::App as MacApp;
use monodrive_core::cloud::{Availability, CloudFolder};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::index::Index;
use monodrive_core::info::{self, Info};
use monodrive_core::ops::{self, Conflict, Origin, Request};
use monodrive_core::places::{Favorite, Pinned, Store};
use monodrive_core::servers::Cancel;
use monodrive_core::sizes::FolderSize;
use monodrive_core::volumes::{Volume, VolumeId};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

fn main() -> gpui::Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&out)?;
    // The path bar would show the `..`s.
    let out = out.canonicalize()?;
    let folder = match std::env::args_os().nth(1) {
        Some(folder) => PathBuf::from(folder),
        None => fill(&out)?,
    };
    shoot(&out, &folder, Theme::Dark)?;
    let light = out.join("light");
    std::fs::create_dir_all(&light)?;
    shoot(&light, &folder, Theme::Light)
}

/// Every picture, in `theme`, into `out`.
fn shoot(out: &Path, folder: &Path, theme: Theme) -> gpui::Result<()> {
    let base = Preferences {
        theme,
        ..Preferences::default()
    };
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::bench_text_system(),
        Arc::new(monodrive::assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    let scratch = tempfile::tempdir_in(out)?;
    let trash = scratch.path().join("Trash");
    std::fs::create_dir_all(&trash)?;
    let places = made_up_places(scratch.path(), &trash)?;
    cx.update(|cx| {
        cx.set_global(places);
        cx.set_global(native::MadeUp(Arc::new(MadeUp)));
    });
    onboarding(&mut cx, out, &base)?;
    for (name, view) in [("list", View::List), ("grid", View::Grid)] {
        cx.update(|cx| {
            cx.set_global(Preferences {
                view,
                ..base.clone()
            })
        });
        let window = open(&mut cx, folder)?;
        // The listing lands, then a second pass lays out the grid's columns.
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, _| window.refresh())?;
        }
        cx.run_until_parked();
        let path = out.join(format!("{name}.png"));
        cx.capture_screenshot(window.into())?.save(&path)?;
        println!("{}", path.display());
    }
    cx.update(|cx| cx.set_global(base.clone()));
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    // Each command needs the frame drawn after the last: what's selected
    // decides which commands the browser takes.
    for action in [&SelectNext as &dyn gpui::Action; 5]
        .into_iter()
        .chain([&Rename as &dyn gpui::Action])
    {
        cx.update_window(window.into(), |root, window, cx| {
            if let Ok(browser) = root.downcast::<Browser>() {
                window.focus(&browser.focus_handle(cx), cx);
            }
            window.dispatch_action(action.boxed_clone(), cx);
            window.refresh();
        })?;
        cx.run_until_parked();
    }
    let path = out.join("rename.png");
    cx.capture_screenshot(window.into())?.save(&path)?;
    println!("{}", path.display());
    for (name, view, selected) in [
        ("menu", View::List, 2),
        ("menu-grid", View::Grid, 2),
        ("menu-folder", View::List, 0),
    ] {
        cx.update(|cx| {
            cx.set_global(Preferences {
                view,
                ..base.clone()
            })
        });
        let window = open(&mut cx, folder)?;
        cx.run_until_parked();
        let actions = std::iter::repeat_n(&SelectNext as &dyn Action, selected)
            .chain([&ShowContextMenu as &dyn Action]);
        for action in actions {
            dispatch(&mut cx, window, action)?;
        }
        // The menu opens once the frame showing the selection has drawn.
        cx.update_window(window.into(), |_, window, _| window.refresh())?;
        cx.run_until_parked();
        let path = out.join(format!("{name}.png"));
        cx.capture_screenshot(window.into())?.save(&path)?;
        println!("{}", path.display());
    }
    // The keys on Move to Trash: a letter jumps to the row starting with it.
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    for action in [&SelectNext as &dyn Action, &SelectNext, &ShowContextMenu] {
        dispatch(&mut cx, window, action)?;
    }
    cx.update_window(window.into(), |_, window, _| window.refresh())?;
    cx.run_until_parked();
    type_text(&mut cx, window, "m")?;
    save(&mut cx, window, out, "menu-trash")?;

    let paths = ops::Paths {
        state: scratch.path().join("state"),
        settings: scratch.path().join("settings.json"),
        trash,
    };
    for ix in 1..=12 {
        std::fs::write(paths.trash.join(format!("old {ix}.txt")), "")?;
    }
    // An agent's move waiting at a name that's taken. Its owner exits once
    // the job has stopped and the agent has gone; the next one has it.
    let (drafts, invoices) = (
        scratch.path().join("Drafts"),
        scratch.path().join("Invoices"),
    );
    for folder in [&drafts, &invoices] {
        std::fs::create_dir_all(folder)?;
        std::fs::write(folder.join("notes.md"), "notes")?;
    }
    let serve = || -> gpui::Result<std::thread::JoinHandle<std::io::Result<()>>> {
        let owner = ops::Owner::bind(paths.clone()).map_err(std::io::Error::other)?;
        Ok(std::thread::spawn(move || owner.serve()))
    };
    let serving = serve()?;
    let agent = Origin::Agent {
        client: Some("claude-code".into()),
    };
    let request = Request::Move {
        paths: vec![drafts.join("notes.md")],
        into: invoices,
        conflict: Conflict::Stop,
    };
    ops::Client::new(paths.clone(), None)
        .submit(agent, request)
        .map_err(std::io::Error::other)?;
    let _ = serving.join();
    serve()?;
    let client = ops::Client::new(paths, None);
    cx.update(|cx| cx.set_global(Operations(Arc::new(Mutex::new(client)))));
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    // The popover asks the owner; closed, the ring stays with its dot.
    dispatch(&mut cx, window, &ShowCopiesAndMoves)?;
    save(&mut cx, window, out, "jobs")?;
    dispatch(&mut cx, window, &ShowCopiesAndMoves)?;
    save(&mut cx, window, out, "ring-waiting")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    // The last entry, renamed to a name that's taken: the owner refuses it.
    dispatch(&mut cx, window, &SelectLast)?;
    dispatch(&mut cx, window, &Rename)?;
    let taken = names(folder)
        .into_iter()
        .find(|name| name.contains('.'))
        .unwrap_or_default();
    type_text(&mut cx, window, &taken)?;
    dispatch(&mut cx, window, &CommitRename)?;
    save(&mut cx, window, out, "rename-error")?;
    dispatch(&mut cx, window, &CancelRename)?;
    cx.update(|cx| {
        cx.set_global(Preferences {
            view: View::Grid,
            ..base.clone()
        })
    });
    dispatch(&mut cx, window, &Rename)?;
    type_text(&mut cx, window, &taken)?;
    dispatch(&mut cx, window, &CommitRename)?;
    save(&mut cx, window, out, "rename-error-grid")?;
    cx.update(|cx| cx.set_global(base.clone()));
    // A free name, then Undo once it's drawn.
    dispatch(&mut cx, window, &CancelRename)?;
    dispatch(&mut cx, window, &Rename)?;
    type_text(&mut cx, window, "Renamed")?;
    dispatch(&mut cx, window, &CommitRename)?;
    save(&mut cx, window, out, "toast")?;
    dispatch(&mut cx, window, &Undo)?;
    dispatch(&mut cx, window, &EmptyTrash)?;
    save(&mut cx, window, out, "empty-trash")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &EditFavorites)?;
    save(&mut cx, window, out, "sidebar-edit")?;
    dispatch(&mut cx, window, &EditFavorites)?;
    dispatch(&mut cx, window, &ConnectToServer)?;
    save(&mut cx, window, out, "connect")?;
    type_text(&mut cx, window, "smb://studio.local/projects")?;
    dispatch(&mut cx, window, &connect::Go)?;
    save(&mut cx, window, out, "connect-failed")?;
    dispatch(&mut cx, window, &connect::Dismiss)?;
    dispatch(&mut cx, window, &GoTrash)?;
    save(&mut cx, window, out, "trash")?;
    dispatch(&mut cx, window, &GoRecents)?;
    save(&mut cx, window, out, "recents")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    let palette = named(&mut cx, "monodrive::OpenPalette")?;
    dispatch(&mut cx, window, &*palette)?;
    save(&mut cx, window, out, "palette")?;
    type_text(&mut cx, window, "re")?;
    save(&mut cx, window, out, "palette-query")?;
    let open_page = named(&mut cx, "palette::OpenPage")?;
    for (name, page) in [
        ("palette-remote", "Remote locations"),
        ("palette-cloud", "Cloud storage"),
        ("palette-drives", "Drives"),
        ("palette-favorites", "Favorites"),
    ] {
        let window = open(&mut cx, folder)?;
        cx.run_until_parked();
        dispatch(&mut cx, window, &*palette)?;
        type_text(&mut cx, window, page)?;
        dispatch(&mut cx, window, &*open_page)?;
        save(&mut cx, window, out, name)?;
    }

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &SelectNext)?;
    dispatch(&mut cx, window, &*palette)?;
    save(&mut cx, window, out, "palette-selection")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &GoToFolder)?;
    save(&mut cx, window, out, "goto")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &OpenSettings)?;
    settle();
    save(&mut cx, window, out, "settings-general")?;
    // Tab onto the page list, whose arrows change pages.
    let next = named(&mut cx, "settings::FocusNext")?;
    dispatch(&mut cx, window, &*next)?;
    for page in ["window", "appearance", "keyboard", "agents"] {
        press(&mut cx, window, "down")?;
        save(&mut cx, window, out, &format!("settings-{page}"))?;
    }

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &OpenSettings)?;
    settle();
    dispatch(&mut cx, window, &*next)?;
    press(&mut cx, window, "down")?;
    press(&mut cx, window, "down")?;
    // Past the other pages and the header's two buttons: the theme's menu.
    for _ in 0..6 {
        dispatch(&mut cx, window, &*next)?;
    }
    press(&mut cx, window, "down")?;
    save(&mut cx, window, out, "settings-menu")?;

    let window = cx.open_window(size(px(640.), px(420.)), |_, cx| {
        let folder = folder.to_path_buf();
        cx.new(|cx| Browser::new(folder, cx))
    })?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &SelectLast)?;
    save(&mut cx, window, out, "narrow")?;

    cx.update(|cx| {
        cx.set_global(Preferences {
            show_path_bar: false,
            ..base.clone()
        })
    });
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    save(&mut cx, window, out, "no-path-bar")?;

    let picture = scratch.path().join("background.png");
    let (bright, dark) = (0xf4 as f32, 0x18 as f32);
    image::RgbaImage::from_fn(1100, 700, |x, y| {
        let along = (x as f32 / 1100. + y as f32 / 700.) / 2.;
        let shade = (bright + (dark - bright) * along) as u8;
        image::Rgba([shade, shade, shade, 255])
    })
    .save(&picture)?;
    cx.update(|cx| {
        cx.set_global(Preferences {
            background: Some(picture),
            ..base.clone()
        })
    });
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    save(&mut cx, window, out, "background")?;
    shoot_tabs(&mut cx, out, folder, &base)?;
    shoot_drags(&mut cx, out, folder, &base)?;
    shoot_inspector(&mut cx, out, folder, &base)
}

/// Past the inspector's wait, with its read drawn.
fn rest(cx: &mut Cx) {
    cx.advance_clock(std::time::Duration::from_millis(300));
    cx.run_until_parked();
}

fn shoot_inspector(cx: &mut Cx, out: &Path, folder: &Path, base: &Preferences) -> gpui::Result<()> {
    let on = Preferences {
        show_inspector: true,
        ..base.clone()
    };
    cx.update(|cx| cx.set_global(on.clone()));
    // In the made-up folder, Brief.pdf, Design and notes.md by their rows.
    let select = |cx: &mut Cx, window, rows: usize| -> gpui::Result<()> {
        for action in std::iter::repeat_n(&SelectNext as &dyn Action, rows) {
            dispatch(cx, window, action)?;
        }
        Ok(())
    };
    let window = open(cx, folder)?;
    cx.run_until_parked();
    select(cx, window, 6)?;
    rest(cx);
    save(cx, window, out, "inspector-file")?;
    let window = open(cx, folder)?;
    cx.run_until_parked();
    select(cx, window, 1)?;
    rest(cx);
    save(cx, window, out, "inspector-folder")?;
    dispatch(cx, window, &ExtendNext)?;
    dispatch(cx, window, &ExtendNext)?;
    rest(cx);
    save(cx, window, out, "inspector-several")?;

    let window = open(cx, folder)?;
    cx.run_until_parked();
    dispatch(cx, window, &ToggleSplit)?;
    select(cx, window, 11)?;
    rest(cx);
    save(cx, window, out, "inspector-split")?;

    cx.update(|cx| {
        cx.set_global(Preferences {
            workspace_style: WorkspaceStyle::Boxy,
            ..on.clone()
        })
    });
    let window = open(cx, folder)?;
    cx.run_until_parked();
    select(cx, window, 6)?;
    rest(cx);
    save(cx, window, out, "inspector-boxy")?;

    // The File menu's way in, then the second kind of row.
    cx.update(|cx| cx.set_global(base.clone()));
    let window = open(cx, folder)?;
    cx.run_until_parked();
    select(cx, window, 6)?;
    dispatch(cx, window, &OpenWith)?;
    cx.update_window(window.into(), |_, window, _| window.refresh())?;
    cx.run_until_parked();
    save(cx, window, out, "open-with")?;
    let up = named(cx, "context_menu::Up")?;
    let run = named(cx, "context_menu::Run")?;
    dispatch(cx, window, &*up)?;
    dispatch(cx, window, &*run)?;
    save(cx, window, out, "open-with-always")?;
    Ok(())
}

/// Sends mouse events to `window` as a person's hand would, and draws.
fn mouse(
    cx: &mut Cx,
    window: gpui::WindowHandle<Browser>,
    events: Vec<PlatformInput>,
) -> gpui::Result<()> {
    for event in events {
        cx.update_window(window.into(), |_, window, cx| {
            window.dispatch_event(event, cx);
            window.refresh();
        })?;
        cx.run_until_parked();
    }
    Ok(())
}

fn press_at(position: Point<Pixels>, modifiers: Modifiers) -> PlatformInput {
    PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count: 1,
        first_mouse: false,
    })
}

fn release_at(position: Point<Pixels>, modifiers: Modifiers) -> PlatformInput {
    PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position,
        modifiers,
        click_count: 1,
    })
}

fn drag_at(position: Point<Pixels>, modifiers: Modifiers) -> PlatformInput {
    PlatformInput::MouseMove(MouseMoveEvent {
        position,
        pressed_button: Some(MouseButton::Left),
        modifiers,
    })
}

/// A drag from `from` caught over `to`, drawn, then cancelled.
fn shoot_drag(
    cx: &mut Cx,
    window: gpui::WindowHandle<Browser>,
    (from, to): (Point<Pixels>, Point<Pixels>),
    keys: Modifiers,
    out: &Path,
    name: &str,
) -> gpui::Result<()> {
    let started = from + point(px(8.), px(0.));
    mouse(
        cx,
        window,
        vec![
            press_at(from, keys),
            drag_at(started, keys),
            drag_at(to, keys),
        ],
    )?;
    save(cx, window, out, name)?;
    cx.update_window(window.into(), |_, window, cx| {
        cx.stop_active_drag(window);
        window.refresh();
    })?;
    mouse(cx, window, vec![release_at(to, keys)])
}

fn shoot_drags(cx: &mut Cx, out: &Path, folder: &Path, base: &Preferences) -> gpui::Result<()> {
    cx.update(|cx| cx.set_global(base.clone()));
    let window = open(cx, folder)?;
    cx.run_until_parked();
    // The list's rows and the sidebar's favorites, as the default frame lays
    // them out. Row 1 is a folder, and rows 5 to 7 are files.
    let row = |ix: f32| point(px(300.), px(73. + 28. * (ix + 0.5)));
    shoot_drag(
        cx,
        window,
        (row(5.), row(1.)),
        Modifiers::default(),
        out,
        "drag-move",
    )?;
    // Three selected, and ⌥ held.
    let chosen = Modifiers {
        platform: true,
        ..Modifiers::default()
    };
    mouse(
        cx,
        window,
        [row(6.), row(7.)]
            .into_iter()
            .flat_map(|at| [press_at(at, chosen), release_at(at, chosen)])
            .collect(),
    )?;
    let option = Modifiers {
        alt: true,
        ..Modifiers::default()
    };
    shoot_drag(cx, window, (row(6.), row(1.)), option, out, "drag-copy")?;
    // A folder, onto the edge between Recents and the first favorite.
    let between = point(px(120.), px(142.));
    shoot_drag(
        cx,
        window,
        (row(2.), between),
        Modifiers::default(),
        out,
        "drag-pin",
    )
}

/// The top bar with tabs, and the panes they hold.
fn shoot_tabs(cx: &mut Cx, out: &Path, folder: &Path, base: &Preferences) -> gpui::Result<()> {
    cx.update(|cx| cx.set_global(base.clone()));
    let window = open(cx, folder)?;
    cx.run_until_parked();
    save(cx, window, out, "tabs-one")?;
    for action in [
        &NewTab as &dyn Action,
        &SelectNext,
        &OpenSelected,
        &NewTab,
        &EnclosingFolder,
    ] {
        dispatch(cx, window, action)?;
    }
    save(cx, window, out, "tabs-three")?;
    for _ in 0..24 {
        dispatch(cx, window, &NewTab)?;
    }
    save(cx, window, out, "tabs-many")?;
    for (name, style) in [
        ("split", WorkspaceStyle::Rounded),
        ("split-boxy", WorkspaceStyle::Boxy),
    ] {
        cx.update(|cx| {
            cx.set_global(Preferences {
                workspace_style: style,
                ..base.clone()
            })
        });
        let window = open(cx, folder)?;
        cx.run_until_parked();
        for action in [
            &SelectNext as &dyn Action,
            &SelectNext,
            &ToggleSplit,
            &SelectNext,
            &OpenSelected,
        ] {
            dispatch(cx, window, action)?;
        }
        save(cx, window, out, name)?;
    }
    cx.update(|cx| {
        cx.set_global(Preferences {
            show_sidebar: false,
            ..base.clone()
        })
    });
    let window = open(cx, folder)?;
    cx.run_until_parked();
    save(cx, window, out, "sidebar-closed")?;
    for action in [&NewTab as &dyn Action, &ToggleSplit, &FocusSearch] {
        dispatch(cx, window, action)?;
    }
    save(cx, window, out, "sidebar-closed-split")?;
    cx.update(|cx| cx.set_global(base.clone()));
    let place = Favorite {
        path: "/Volumes/Backup/Photos".into(),
        volume: Some(Pinned {
            id: VolumeId::Uuid("gone".into()),
            name: "Backup".into(),
            mount: "/Volumes/Backup".into(),
        }),
    };
    let state = WindowState {
        tabs: vec![TabState {
            panes: vec![PaneState {
                history: vec![StopState {
                    place,
                    recents: false,
                }],
                at: 0,
                view: View::List,
                sort: Default::default(),
            }],
            second_focused: false,
            ratio: 0.5,
        }],
        active: 0,
        frame: None,
    };
    let window = cx.open_window(size(px(1100.), px(700.)), |_, cx| {
        cx.new(|cx| Browser::restored(state, cx))
    })?;
    cx.run_until_parked();
    save(cx, window, out, "unavailable")?;
    Ok(())
}

/// Lets the Settings sheet finish fading in, which goes by the clock.
#[expect(
    clippy::disallowed_methods,
    reason = "A picture of the sheet, not a test; the fade has no event to wait on."
)]
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(300));
}

fn open(cx: &mut Cx, folder: &Path) -> gpui::Result<gpui::WindowHandle<Browser>> {
    let folder = folder.to_path_buf();
    cx.open_window(size(px(1100.), px(700.)), |_, cx| {
        cx.new(|cx| Browser::new(folder, cx))
    })
}

/// An action the library keeps private, by its name.
fn named(cx: &mut Cx, name: &str) -> gpui::Result<Box<dyn Action>> {
    cx.update(|cx| cx.build_action(name, None))
        .map_err(std::io::Error::other)
        .map_err(Into::into)
}

/// Sends a key with no text to what has focus.
fn press(cx: &mut Cx, window: gpui::WindowHandle<Browser>, key: &str) -> gpui::Result<()> {
    let keystroke = Keystroke::parse(key).map_err(std::io::Error::other)?;
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx);
        window.refresh();
    })?;
    cx.run_until_parked();
    Ok(())
}

/// Volumes that aren't there, each showing something the sidebar can show.
struct MadeUp;

fn system_app(name: &str) -> MacApp {
    MacApp {
        name: name.into(),
        bundle_id: None,
        path: format!("/System/Applications/{name}.app").into(),
    }
}

/// Real facts about the snapshot's own folder; a total and apps that are the
/// same on every Mac.
impl Native for MadeUp {
    fn info(&self, path: &Path) -> Result<Info, String> {
        info::info(path)
    }

    fn needs_consent(&self, _: &Path) -> bool {
        false
    }

    fn known_size(&self, _: &Path) -> Option<FolderSize> {
        None
    }

    fn measure(&self, _: PathBuf, done: SizeDone) -> Box<dyn Send> {
        done(Ok(FolderSize {
            bytes: 1_284_000_000,
            complete: true,
            ..FolderSize::default()
        }));
        Box::new(())
    }

    fn invalidate(&self, _: &Path) {}

    fn forget_sizes(&self) {}

    fn can_open(&self, _: &Path) -> Vec<MacApp> {
        ["Notes", "Preview", "TextEdit"].map(system_app).to_vec()
    }

    fn default_for(&self, _: &Path) -> Option<MacApp> {
        Some(system_app("Preview"))
    }

    fn open_with(&self, _: &[PathBuf], _: &Path) -> Result<(), String> {
        Ok(())
    }

    fn make_default(&self, _: &Path, _: &Path) -> Result<(), String> {
        Ok(())
    }

    fn choose_app(&self, done: Box<dyn FnOnce(Option<PathBuf>)>) {
        done(None);
    }

    fn installed(&self, _: &str) -> bool {
        true
    }
}

impl Drives for MadeUp {
    fn list(&self) -> Vec<Volume> {
        const GB: u64 = 1_000_000_000;
        let volume = |name: &str, total: u64, free: u64| Volume {
            name: name.into(),
            mount: Path::new("/Volumes").join(name),
            disk: None,
            id: Some(VolumeId::Uuid(name.into())),
            total: total * GB,
            free: free * GB,
            read_only: false,
            network: false,
            ejectable: true,
            removable: true,
            startup: false,
        };
        vec![
            Volume {
                mount: "/".into(),
                ejectable: false,
                removable: false,
                startup: true,
                ..volume("Macintosh HD", 494, 178)
            },
            volume("Backup", 2000, 340),
            Volume {
                read_only: true,
                ..volume("Installer", 14, 0)
            },
            Volume {
                network: true,
                ejectable: false,
                removable: false,
                id: Some(VolumeId::Url("smb://nas.local/media".into())),
                ..volume("media", 8000, 5100)
            },
        ]
    }

    fn eject(&self, _: &Path, _: &mut ops::Client) -> Result<String, String> {
        Err("Nothing here ejects.".into())
    }

    fn mount(&self, address: &str, _: &Arc<Cancel>) -> Result<PathBuf, String> {
        Err(format!("{address}: the server didn't answer."))
    }

    fn open(&self, _: &Path, _: &gpui::App) {}

    fn needs_consent(&self, _: &Path) -> bool {
        false
    }

    fn cloud(&self) -> Vec<CloudFolder> {
        let root = Path::new("/Users/you/Library");
        vec![
            CloudFolder {
                name: "iCloud Drive".into(),
                path: root.join("Mobile Documents/com~apple~CloudDocs"),
            },
            CloudFolder {
                name: "Dropbox".into(),
                path: root.join("CloudStorage/Dropbox"),
            },
            CloudFolder {
                name: "GoogleDrive-you@example.com".into(),
                path: root.join("CloudStorage/GoogleDrive-you@example.com"),
            },
        ]
    }

    fn find_files(
        &self,
        _: &OnceLock<Result<Index, String>>,
        query: &str,
        _: usize,
    ) -> Result<Vec<Entry>, String> {
        Ok(["Reports", "report-final.pdf", "report-draft.pdf"]
            .into_iter()
            .filter(|name| name.to_lowercase().contains(&query.to_lowercase()))
            .map(|name| Entry {
                path: Path::new("/Users/you/Documents").join(name),
                name: name.into(),
                kind: if name.contains('.') {
                    Kind::File
                } else {
                    Kind::Dir
                },
                size: 0,
                modified: 0,
                availability: Availability::Unknown,
            })
            .collect())
    }
}

/// A Mac for onboarding to ask about: no real permission, index or agent is
/// looked at.
struct MadeUpSetup {
    granted: bool,
    index_lacks: bool,
}

impl Setup for MadeUpSetup {
    fn full_disk_access(&self) -> bool {
        self.granted
    }

    fn index_lacks_access(&self) -> bool {
        self.index_lacks
    }

    fn saved_index(&self) -> bool {
        false
    }

    fn start_index(&self) -> Result<Index, String> {
        Err("Nothing here starts.".into())
    }

    fn open_privacy_settings(&self, _: &gpui::App) {}

    fn app(&self) -> PathBuf {
        "/Applications/Monodrive.app".into()
    }

    fn reveal(&self, _: &Path, _: &gpui::App) {}

    fn relaunch(&self, _: &mut gpui::App) {}

    fn agents(&self) -> (Vec<Harness>, Vec<Harness>) {
        (
            vec![Harness::ClaudeCode, Harness::Codex, Harness::Cursor],
            if self.granted {
                vec![Harness::Codex]
            } else {
                Vec::new()
            },
        )
    }

    fn apply_agent(&self, _: Harness, _: agents::Action) -> Result<(), String> {
        Ok(())
    }
}

/// Each step of onboarding, in its own window.
fn onboarding(cx: &mut Cx, out: &Path, base: &Preferences) -> gpui::Result<()> {
    cx.update(|cx| cx.set_global(base.clone()));
    for (name, step, granted, index_lacks, asked) in [
        ("welcome", Step::Welcome, false, false, false),
        ("access", Step::FullDiskAccess, false, false, false),
        ("access-asked", Step::FullDiskAccess, false, false, true),
        ("access-granted", Step::FullDiskAccess, true, false, false),
        ("access-reopen", Step::FullDiskAccess, true, true, false),
        ("agents", Step::Agents, true, false, false),
        ("ready", Step::Ready, true, false, false),
        ("agents-skipped", Step::FullDiskAccess, false, false, false),
        ("ready-skipped", Step::FullDiskAccess, false, false, false),
    ] {
        let setup = Arc::new(MadeUpSetup {
            granted,
            index_lacks,
        });
        let window = cx.open_window(size(px(760.), px(520.)), |_, cx| {
            cx.new(|cx| {
                let mut view = Onboarding::new(step, setup, cx);
                if asked {
                    view.open_settings(cx);
                }
                view
            })
        })?;
        // Skipping goes through what Not Now and Skip do.
        if name.ends_with("-skipped") {
            cx.update_window(window.into(), |root, window, cx| {
                if let Ok(view) = root.downcast::<Onboarding>() {
                    view.update(cx, |view, cx| {
                        view.not_now(cx);
                        if name == "ready-skipped" {
                            view.primary(window, cx);
                        }
                    });
                }
            })?;
        }
        // The logo decodes, then a second pass draws it.
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, _| window.refresh())?;
        }
        cx.run_until_parked();
        let path = out.join(format!("onboarding-{name}.png"));
        cx.capture_screenshot(window.into())?.save(&path)?;
        println!("{}", path.display());
    }
    Ok(())
}

/// A temp folder's places: the default favorites, one on a drive that isn't
/// connected, saved servers, and recents with one file gone.
fn made_up_places(dir: &Path, trash: &Path) -> gpui::Result<Places> {
    let dir = dir.canonicalize()?;
    let store = Store {
        places: dir.join("places.json"),
        recents: dir.join("recents.json"),
    };
    let mount = dir.join("Archive");
    let photos = mount.join("Photos");
    std::fs::create_dir_all(&photos)?;
    let archive = Volume {
        name: "Archive".into(),
        mount,
        disk: None,
        id: Some(VolumeId::Uuid("Archive".into())),
        total: 0,
        free: 0,
        read_only: false,
        network: false,
        ejectable: true,
        removable: true,
        startup: false,
    };
    store
        .pin(&photos, None, &[archive])
        .map_err(std::io::Error::other)?;
    for address in ["smb://nas.local/media", "smb://studio.local/projects"] {
        store.save_server(address).map_err(std::io::Error::other)?;
    }
    for host in ["build-box", "me@studio.local"] {
        store
            .add_remote(host, None, None)
            .map_err(std::io::Error::other)?;
    }
    for name in ["gone.pdf", "Brief.pdf"] {
        let opened = dir.join(name);
        if name != "gone.pdf" {
            std::fs::write(&opened, "")?;
        }
        store.opened(&opened).map_err(std::io::Error::other)?;
    }
    Ok(Places {
        store,
        trash: trash.into(),
        applications: Vec::new(),
        drives: Arc::new(MadeUp),
    })
}

fn save(
    cx: &mut Cx,
    window: gpui::WindowHandle<Browser>,
    out: &Path,
    name: &str,
) -> gpui::Result<()> {
    cx.update_window(window.into(), |_, window, _| window.refresh())?;
    cx.run_until_parked();
    let path = out.join(format!("{name}.png"));
    cx.capture_screenshot(window.into())?.save(&path)?;
    println!("{}", path.display());
    Ok(())
}

/// Types `text` into what has focus, as one keystroke's characters.
fn type_text(cx: &mut Cx, window: gpui::WindowHandle<Browser>, text: &str) -> gpui::Result<()> {
    cx.update_window(window.into(), |_, window, cx| {
        let keystroke = Keystroke {
            key: "x".into(),
            key_char: Some(text.into()),
            ..Keystroke::default()
        };
        window.dispatch_keystroke(keystroke, cx);
        window.refresh();
    })?;
    cx.run_until_parked();
    Ok(())
}

/// The folder's names, sorted.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Sends `action` to the browser in `window` as a key would, and draws.
fn dispatch(
    cx: &mut Cx,
    window: gpui::WindowHandle<Browser>,
    action: &dyn Action,
) -> gpui::Result<()> {
    cx.update_window(window.into(), |root, window, cx| {
        if let Ok(browser) = root.downcast::<Browser>()
            && window.focused(cx).is_none()
        {
            window.focus(&browser.focus_handle(cx), cx);
        }
        window.dispatch_action(action.boxed_clone(), cx);
        window.refresh();
    })?;
    cx.run_until_parked();
    Ok(())
}

/// A folder like a real one: a few folders, documents, pictures and an app.
/// It stays at the same path between runs, so before and after pictures
/// differ only where the change does.
fn fill(dir: &Path) -> std::io::Result<PathBuf> {
    let root = dir.join("Projects");
    for folder in ["Design", "Invoices", "monodrive", "Screenshots"] {
        std::fs::create_dir_all(root.join(folder))?;
    }
    // An empty bundle would draw as a broken app; a link draws the real one.
    let app = root.join("Calculator.app");
    if app.symlink_metadata().is_err() {
        std::os::unix::fs::symlink("/System/Applications/Calculator.app", app)?;
    }
    for file in [
        "Brief.pdf",
        "notes.md",
        "budget.xlsx",
        "main.rs",
        "photo.jpg",
        "archive.zip",
        "Cargo.toml",
        "README",
    ] {
        std::fs::write(root.join(file), "")?;
    }
    Ok(root)
}
