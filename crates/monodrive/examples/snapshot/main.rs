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
//! files. A folder you pass is only read: `rename-error.png`,
//! `rename-error-grid.png` and `toast.png` really rename a file, so they
//! always use the made-up one. Rename edits the fifth name in place. The menus are the context
//! menu on the second entry, in the list and the grid, and on the folder.
//! The rest use an operation owner on a temp folder. The popover and the
//! ring show an agent's move there, waiting at a name that's taken. Then the
//! last entry is renamed to a name that's taken, then to a free one, which
//! is undone after the picture, and Empty Trash counts a Trash of 12 items.
//!
//! The sidebar shows a made-up set of volumes and a temp folder's favorites,
//! so nothing real is listed, mounted or ejected. The last pictures are
//! `sidebar-keys.png`, `sidebar-edit.png`, `sidebar-folded.png`, with Favorites folded,
//! `sidebar-edit-locations.png`, with Installer hidden,
//! `connect.png`, `connect-failed.png`, `trash.png`,
//! `recents.png` and `recents-grid.png`, then `updates.png`. The Overview's are last: see `shoot_overview`. Then `palette.png`, `palette-query.png`, the palette's pages
//! (`palette-remote.png`, `palette-cloud.png`, `palette-drives.png`,
//! `palette-favorites.png`), `server.png` and `server-failed.png`, a saved
//! SFTP server opened from there and one that won't connect,
//! `palette-selection.png`, `goto.png`, `goto-error.png`, `address.png`,
//! `address-completions.png` and `address-error.png`, the path bar edited as
//! a path, `settings-menu.png` and one
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
//! `onboarding-welcome.png`, `onboarding-checklist.png`,
//! `onboarding-checklist-asked.png`, `onboarding-checklist-granted.png` and
//! `onboarding-checklist-reopen.png` show each step, with a made-up Mac behind
//! them.
//!
//! `inspector-file.png`, `inspector-folder.png`, `inspector-several.png`,
//! `inspector-split.png` and `inspector-boxy.png` show the inspector, with a
//! made-up folder total. `open-with.png` and `open-with-always.png` show the
//! Open With submenu on made-up choices of the system's own apps, and
//! `settings-general.png` the Terminal row.
//!
//! `columns.png`, `columns-preview.png`, `columns-several.png`,
//! `columns-empty.png` and `columns-split.png` show column view.
//!
//! `search-contents.png` is a content search with its matching lines,
//! `search-menu.png` the search field's menu over it, with made-up saved and
//! recent searches, and `search-save.png` that menu naming a new one.
//! `search-filter.png` is the field offering kinds for the filter being
//! typed after a finished one became a token, `search-typo.png` a bad filter
//! marked where it sits beside two tokens, and `search-token-menu.png` the
//! menu of the token the keys are on.
//!
//! `batch-rename.png` is rename in the list with several selected, replacing
//! text, `batch-rename-number.png` numbering them, `batch-rename-problem.png`
//! a name that's taken, `batch-rename-grid.png` the bar from a grid and
//! `batch-rename-narrow.png` in a narrow pane, and `batch-rename-suggest.png`
//! names read from the files. Nothing is applied. `organize.png` is Organize's
//! review of the folder. `preview.png` is the preview of the last entry, and
//! `preview-edit.png` its tools.
//!
//! `downloads.png` and `downloads-grid.png` show a file downloading and a
//! folder whose download failed.
//!
//! All of it twice, once dark in `target/snapshots` and once light in
//! `target/snapshots/light`.

mod made_up;
mod pages;

use made_up::*;
use pages::*;

use gpui::{Action, HeadlessAppContext as Cx, Keystroke};
use gpui::{
    AppContext, Capslock, Focusable, HeadlessAppContext, Modifiers, ModifiersChangedEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, point,
    px, size,
};
use monodrive::Operations;
use monodrive::agents::{self, Harness};
use monodrive::browser::Browser;
use monodrive::commands::{
    Back, CancelRename, CheckForUpdates, CommitRename, ConnectToServer, EditFavorites,
    EditLocations, EditPath, EditPreview, EmptyTrash, EnclosingFolder, ExpandFolder, ExtendNext,
    ExtendPrevious, FindDuplicates, FocusNext, FocusSearch, GoAirDrop, GoOverview, GoRecents,
    GoToFolder, GoTrash, GoWorktrees, MoveToTrash, NewTab, OpenSelected, OpenSettings, OpenWith,
    OrganizeFolder, QuickLook, Rename, SaveSearch, SearchContents, SelectLast, SelectNext,
    SelectPrevious, ShowContextMenu, ShowCopiesAndMoves, ShowSearches, SuggestNames, ToggleSplit,
    Undo, ViewAsGrid, ViewAsList,
};
use monodrive::connect;
use monodrive::downloads;
use monodrive::finder;
use monodrive::native::{self, Native, SizeDone};
use monodrive::onboarding::{Onboarding, Setup, Step};
use monodrive::preferences::{
    Collapsed, Drive, HiddenDrive, Preferences, Theme, View, WorkspaceStyle,
};
use monodrive::session::{PaneState, Shows, StopState, TabState, WindowState};
use monodrive::sharing::{AirDrop, Discoverable};
use monodrive::system::{Drives, Places};
use monodrive::updates::{self, Updates};
use monodrive_core::Origin;
use monodrive_core::apps::App as MacApp;
use monodrive_core::cloud::{Asked, Availability, CloudFolder, Progress, Quota};
use monodrive_core::entries::{Entry, Kind};
use monodrive_core::index::{Found, Hit, Index, LineMatch};
use monodrive_core::info::{self, Info};
use monodrive_core::ops::{self, Conflict, Request};
use monodrive_core::places::Source;
use monodrive_core::places::{Favorite, Pinned, RecentSearch, SavedSearch, Store};
use monodrive_core::remote::{Connections, RemoteError, Transport};
use monodrive_core::search::{Looks, Scope, Search};
use monodrive_core::servers::Cancel;
use monodrive_core::sizes::FolderSize;
use monodrive_core::updates::{Change, Channel, Notes, Release};
use monodrive_core::volumes::{Volume, VolumeId};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

fn main() -> gpui::Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&out)?;
    // The path bar would show the `..`s.
    let out = out.canonicalize()?;
    let made_up = fill(&out)?;
    let folder = std::env::args_os()
        .nth(1)
        .map_or_else(|| made_up.clone(), PathBuf::from);
    shoot(&out, &folder, &made_up, Theme::Dark)?;
    let light = out.join("light");
    std::fs::create_dir_all(&light)?;
    shoot(&light, &folder, &made_up, Theme::Light)
}

/// Every picture, in `theme`, into `out`.
fn shoot(out: &Path, folder: &Path, made_up: &Path, theme: Theme) -> gpui::Result<()> {
    let base = Preferences {
        theme,
        ..Preferences::default()
    };
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::bench_text_system(),
        Arc::new(monodrive::assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    // The system's temp folder: the owner's socket path has a short limit,
    // and a worktree's `out` runs past it.
    let scratch = tempfile::tempdir()?;
    let trash = scratch.path().join("Trash");
    std::fs::create_dir_all(&trash)?;
    let places = made_up_places(scratch.path(), &trash)?;
    cx.update(|cx| {
        cx.set_global(places);
        cx.set_global(native::MadeUp(Arc::new(MadeUp)));
    });
    onboarding(&mut cx, out, &base)?;
    shoot_search(&mut cx, out, folder, &base)?;
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

    // An archive that won't read, then one browsed with two entries selected.
    let downloads = scratch.path().join("Downloads");
    std::fs::create_dir_all(&downloads)?;
    std::fs::write(downloads.join("broken.zip"), "not a zip")?;
    site_assets(&downloads.join("site-assets.zip"))?;
    let window = open(&mut cx, &downloads)?;
    cx.run_until_parked();
    for action in [&SelectNext as &dyn Action, &OpenSelected] {
        dispatch(&mut cx, window, action)?;
    }
    save(&mut cx, window, out, "archive-error")?;
    for action in [
        &Back as &dyn Action,
        &SelectNext,
        &OpenSelected,
        &SelectNext,
        &SelectNext,
        &SelectNext,
        &ExtendNext,
    ] {
        dispatch(&mut cx, window, action)?;
    }
    save(&mut cx, window, out, "archive")?;

    let window = open(&mut cx, made_up)?;
    cx.run_until_parked();
    // The last entry, renamed to a name that's taken: the owner refuses it.
    dispatch(&mut cx, window, &SelectLast)?;
    dispatch(&mut cx, window, &Rename)?;
    let taken = names(made_up)
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
    settle();
    save(&mut cx, window, out, "toast")?;
    dispatch(&mut cx, window, &Undo)?;
    dispatch(&mut cx, window, &EmptyTrash)?;
    save(&mut cx, window, out, "empty-trash")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    // The keys show only in the active window, which a headless one isn't.
    cx.update_window(window.into(), |_, window, _| window.activate_window())?;
    cx.run_until_parked();
    mouse(
        &mut cx,
        window,
        vec![PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers: Modifiers::command(),
            capslock: Capslock::default(),
        })],
    )?;
    save(&mut cx, window, out, "sidebar-keys")?;
    mouse(
        &mut cx,
        window,
        vec![PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers: Modifiers::none(),
            capslock: Capslock::default(),
        })],
    )?;
    dispatch(&mut cx, window, &EditFavorites)?;
    save(&mut cx, window, out, "sidebar-edit")?;
    dispatch(&mut cx, window, &EditFavorites)?;
    cx.update(|cx| {
        cx.set_global(Preferences {
            collapsed: Collapsed {
                favorites: true,
                locations: false,
            },
            ..base.clone()
        })
    });
    save(&mut cx, window, out, "sidebar-folded")?;
    cx.update(|cx| {
        cx.set_global(Preferences {
            collapsed: Collapsed {
                favorites: true,
                locations: false,
            },
            hidden_drives: vec![HiddenDrive {
                name: "Installer".into(),
                drive: Drive::Volume(VolumeId::Uuid("Installer".into())),
            }],
            ..base.clone()
        })
    });
    dispatch(&mut cx, window, &EditLocations)?;
    save(&mut cx, window, out, "sidebar-edit-locations")?;
    dispatch(&mut cx, window, &EditLocations)?;
    cx.update(|cx| cx.set_global(base.clone()));
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
    dispatch(&mut cx, window, &ViewAsGrid)?;
    // A second pass lays out the grid's columns.
    cx.update_window(window.into(), |_, window, _| window.refresh())?;
    cx.run_until_parked();
    save(&mut cx, window, out, "recents-grid")?;
    dispatch(&mut cx, window, &ViewAsList)?;

    // A made-up release, on a copy that can't update itself, so nothing goes
    // to the network. Only this picture shows it.
    let release = Release {
        version: "0.2.0".parse()?,
        published: "2026-10-30T12:00:00Z".into(),
        notes: Notes {
            highlights: None,
            changes: vec![
                Change {
                    title: Some("Archive search.".into()),
                    text: "Search finds files inside zip and tar archives without opening them."
                        .into(),
                },
                Change {
                    title: Some("Network drives.".into()),
                    text: "Eject works on network drives again.".into(),
                },
            ],
            more: Some("Plus 4 more bug fixes and improvements.".into()),
            stats: Some("12 changes from 2 contributors".into()),
        },
        page: "https://github.com/donnes/monodrive/releases".into(),
        disk_image: String::new(),
        size: 0,
    };
    cx.update(|cx| {
        let updates = cx.new(|_| Updates::new(Channel::Stable, vec![release], Some(None)));
        cx.set_global(updates::Shared(updates));
    });
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &CheckForUpdates)?;
    save(&mut cx, window, out, "updates")?;
    // The same card, while the pointer rests on the sidebar's update control.
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    // Left of Settings, at the sidebar's foot, as the default frame lays it out.
    let control = point(px(188.), px(680.));
    mouse(&mut cx, window, vec![move_to(control)])?;
    cx.advance_clock(std::time::Duration::from_millis(300));
    save(&mut cx, window, out, "updates-peek")?;
    // Nothing out: the control checks, and says so under the pointer.
    cx.update(|cx| {
        let updates = cx.new(|_| Updates::new(Channel::Stable, Vec::new(), Some(None)));
        cx.set_global(updates::Shared(updates));
    });
    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    mouse(&mut cx, window, vec![move_to(control)])?;
    cx.advance_clock(std::time::Duration::from_secs(1));
    save(&mut cx, window, out, "updates-idle")?;
    cx.update(|cx| cx.remove_global::<updates::Shared>());

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

    // A server opened from the palette's page, then one that won't connect.
    let run = named(&mut cx, "palette::Run")?;
    for (name, server) in [("server", "build-box"), ("server-failed", "studio")] {
        // Tall enough that the sidebar shows the servers under the drives.
        let window = open_at(&mut cx, folder, 1100., 960.)?;
        cx.run_until_parked();
        dispatch(&mut cx, window, &*palette)?;
        type_text(&mut cx, window, "Remote locations")?;
        dispatch(&mut cx, window, &*open_page)?;
        type_text(&mut cx, window, server)?;
        dispatch(&mut cx, window, &*run)?;
        if name == "server" {
            dispatch(&mut cx, window, &SelectNext)?;
        }
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
    type_text(
        &mut cx,
        window,
        "/Volumes/Archive/Projects/2026/Clients/Northwind Traders/Quarterly Reports/Final Versions/Signed Copies Sent To The Board Of Directors For Review.pdf",
    )?;
    let go = named(&mut cx, "goto::Go")?;
    dispatch(&mut cx, window, &*go)?;
    save(&mut cx, window, out, "goto-error")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &EditPath)?;
    save(&mut cx, window, out, "address")?;
    let typed = monodrive_core::paths::abbreviate(folder);
    type_text(&mut cx, window, &format!("{typed}/"))?;
    save(&mut cx, window, out, "address-completions")?;
    type_text(&mut cx, window, "nowhere/at/all")?;
    dispatch(&mut cx, window, &*go)?;
    save(&mut cx, window, out, "address-error")?;

    let window = open(&mut cx, folder)?;
    cx.run_until_parked();
    dispatch(&mut cx, window, &OpenSettings)?;
    settle();
    save(&mut cx, window, out, "settings-general")?;
    // Tab onto the page list, whose arrows change pages.
    let next = named(&mut cx, "settings::FocusNext")?;
    dispatch(&mut cx, window, &*next)?;
    for page in [
        "window",
        "appearance",
        "keyboard",
        "agents",
        "notifications",
    ] {
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
    shoot_batch_rename(&mut cx, out, folder, &base)?;
    shoot_inspector(&mut cx, out, folder, &base)?;
    shoot_worktrees(&mut cx, out, folder, &base)?;
    shoot_airdrop(&mut cx, out, folder)?;
    shoot_duplicates(&mut cx, out, &base)?;
    shoot_columns(&mut cx, out, made_up, &base)?;
    shoot_overview(&mut cx, out, folder, &base)?;
    shoot_downloads(&mut cx, out, made_up, &base)
}

/// `downloads.png` and `downloads-grid.png`: Brief.pdf downloading and the
/// Invoices folder's download failed, from the made-up macOS. Last, since
/// those stay on their way for every window after.
fn shoot_downloads(
    cx: &mut Cx,
    out: &Path,
    made_up: &Path,
    base: &Preferences,
) -> gpui::Result<()> {
    let paths = vec![made_up.join("Brief.pdf"), made_up.join("Invoices")];
    cx.update(|cx| {
        downloads::get(cx).update(cx, |downloads, cx| {
            downloads.download(paths, std::rc::Rc::new(|_, _, _| {}), cx)
        })
    });
    for (name, view) in [("downloads", View::List), ("downloads-grid", View::Grid)] {
        cx.update(|cx| {
            cx.set_global(Preferences {
                view,
                ..base.clone()
            })
        });
        let window = open(cx, made_up)?;
        for _ in 0..2 {
            cx.run_until_parked();
            cx.update_window(window.into(), |_, window, _| window.refresh())?;
        }
        cx.run_until_parked();
        save(cx, window, out, name)?;
    }
    Ok(())
}

/// Column view in the made-up folder: `columns.png` with monodrive selected
/// and its files in the last column, `columns-preview.png` a column further
/// in with DESIGN.md previewed, `columns-several.png` two selected there,
/// `columns-empty.png` an empty folder selected, and `columns-split.png` the
/// narrow panes of Split View.
fn shoot_columns(cx: &mut Cx, out: &Path, folder: &Path, base: &Preferences) -> gpui::Result<()> {
    cx.update(|cx| {
        cx.set_global(Preferences {
            view: View::Columns,
            ..base.clone()
        })
    });
    let select = |cx: &mut Cx, window, rows: usize| -> gpui::Result<()> {
        for action in std::iter::repeat_n(&SelectNext as &dyn Action, rows) {
            dispatch(cx, window, action)?;
        }
        rest(cx);
        Ok(())
    };
    let window = open(cx, folder)?;
    cx.run_until_parked();
    select(cx, window, 3)?;
    save(cx, window, out, "columns")?;
    dispatch(cx, window, &ExpandFolder)?;
    select(cx, window, 3)?;
    save(cx, window, out, "columns-preview")?;
    dispatch(cx, window, &ExtendNext)?;
    save(cx, window, out, "columns-several")?;
    let window = open(cx, folder)?;
    cx.run_until_parked();
    select(cx, window, 1)?;
    save(cx, window, out, "columns-empty")?;
    dispatch(cx, window, &ToggleSplit)?;
    select(cx, window, 3)?;
    save(cx, window, out, "columns-split")?;
    cx.update(|cx| cx.set_global(base.clone()));
    Ok(())
}

/// The last five rows selected, in the bar that renames them.
fn batch_rename_in(cx: &mut Cx, window: gpui::WindowHandle<Browser>) -> gpui::Result<()> {
    dispatch(cx, window, &SelectLast)?;
    for _ in 0..4 {
        dispatch(cx, window, &ExtendPrevious)?;
    }
    dispatch(cx, window, &Rename)
}

fn open_batch_rename(cx: &mut Cx, folder: &Path) -> gpui::Result<gpui::WindowHandle<Browser>> {
    let window = open(cx, folder)?;
    cx.run_until_parked();
    batch_rename_in(cx, window)?;
    Ok(window)
}

fn shoot_batch_rename(
    cx: &mut Cx,
    out: &Path,
    folder: &Path,
    base: &Preferences,
) -> gpui::Result<()> {
    let click = |cx: &mut Cx, window, at: Point<Pixels>| {
        let events = vec![
            press_at(at, Modifiers::none()),
            release_at(at, Modifiers::none()),
        ];
        mouse(cx, window, events)
    };
    let window = open_batch_rename(cx, folder)?;
    type_text(cx, window, "o")?;
    dispatch(cx, window, &FocusNext)?;
    type_text(cx, window, "0")?;
    save(cx, window, out, "batch-rename")?;
    // Number, in the bar's first line, which puts the keys in Name.
    click(cx, window, point(px(362.), px(61.)))?;
    type_text(cx, window, "Trip")?;
    dispatch(cx, window, &FocusNext)?;
    type_text(cx, window, "001")?;
    save(cx, window, out, "batch-rename-number")?;

    // Include Extension, at the end of that line, then a name that's taken.
    let window = open_batch_rename(cx, folder)?;
    click(cx, window, point(px(1016.), px(61.)))?;
    type_text(cx, window, "notes.md")?;
    dispatch(cx, window, &FocusNext)?;
    type_text(cx, window, "Brief.pdf")?;
    save(cx, window, out, "batch-rename-problem")?;

    let window = open(cx, folder)?;
    cx.run_until_parked();
    dispatch(cx, window, &SelectLast)?;
    for _ in 0..4 {
        dispatch(cx, window, &ExtendPrevious)?;
    }
    dispatch(cx, window, &SuggestNames)?;
    cx.run_until_parked();
    save(cx, window, out, "batch-rename-suggest")?;

    let window = open(cx, folder)?;
    cx.run_until_parked();
    dispatch(cx, window, &OrganizeFolder)?;
    cx.run_until_parked();
    save(cx, window, out, "organize")?;

    let window = open(cx, folder)?;
    cx.run_until_parked();
    dispatch(cx, window, &SelectLast)?;
    dispatch(cx, window, &QuickLook)?;
    cx.run_until_parked();
    save(cx, window, out, "preview")?;
    dispatch(cx, window, &EditPreview)?;
    cx.run_until_parked();
    save(cx, window, out, "preview-edit")?;

    cx.update(|cx| {
        cx.set_global(Preferences {
            view: View::Grid,
            ..base.clone()
        })
    });
    let window = open_batch_rename(cx, folder)?;
    type_text(cx, window, "o")?;
    save(cx, window, out, "batch-rename-grid")?;
    cx.update(|cx| cx.set_global(base.clone()));

    let window = open(cx, folder)?;
    cx.run_until_parked();
    dispatch(cx, window, &ToggleSplit)?;
    batch_rename_in(cx, window)?;
    type_text(cx, window, "o")?;
    save(cx, window, out, "batch-rename-narrow")
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

fn move_to(position: Point<Pixels>) -> PlatformInput {
    PlatformInput::MouseMove(MouseMoveEvent {
        position,
        pressed_button: None,
        modifiers: Modifiers::default(),
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
    // A folder, onto the edge between the first two favorites.
    let between = point(px(120.), px(170.));
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
                    shows: Shows::Folder,
                    remote: None,
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

/// A content search on made-up results, and the search field's menu over it.
fn shoot_search(cx: &mut Cx, out: &Path, folder: &Path, base: &Preferences) -> gpui::Result<()> {
    cx.update(|cx| cx.set_global(base.clone()));
    let store = cx.update(|cx| cx.global::<Places>().store.clone());
    let keep = |name: &str, query: &str, scope: Scope| SavedSearch {
        name: name.into(),
        query: query.into(),
        scope,
    };
    for saved in [
        keep("Open todos", "contains:todo kind:code", Scope::Everywhere),
        keep(
            "Big videos",
            "kind:video size:>1gb",
            Scope::Folder("/Users/you/Movies".into()),
        ),
    ] {
        store
            .save_search(saved, None)
            .map_err(std::io::Error::other)?;
    }
    for query in ["invoice 2026", "contains:\"fix me\" ext:rs"] {
        let recent = RecentSearch {
            query: query.into(),
            scope: Scope::Everywhere,
        };
        store.searched(recent).map_err(std::io::Error::other)?;
    }
    let window = open(cx, folder)?;
    cx.run_until_parked();
    dispatch(cx, window, &SearchContents)?;
    type_text(cx, window, "todo")?;
    // A content search waits for the typing to stop.
    rest(cx);
    save(cx, window, out, "search-contents")?;
    dispatch(cx, window, &ShowSearches)?;
    let down = named(cx, "context_menu::Down")?;
    for _ in 0..7 {
        dispatch(cx, window, &*down)?;
    }
    save(cx, window, out, "search-menu")?;
    let dismiss = named(cx, "context_menu::Dismiss")?;
    dispatch(cx, window, &*dismiss)?;
    dispatch(cx, window, &SaveSearch)?;
    save(cx, window, out, "search-save")?;
    dispatch(cx, window, &*dismiss)?;
    dispatch(cx, window, &*dismiss)?;
    // Back to names, with what's typed selected to type over.
    dispatch(cx, window, &SearchContents)?;
    type_text(cx, window, "invoice modified:30d kind:")?;
    save(cx, window, out, "search-filter")?;
    type_text(cx, window, "document size:big ")?;
    save(cx, window, out, "search-typo")?;
    // Onto the last token, and its menu.
    for name in [
        "text_field::Home",
        "text_field::Backspace",
        "search_field::OpenChip",
    ] {
        let action = named(cx, name)?;
        dispatch(cx, window, &*action)?;
    }
    save(cx, window, out, "search-token-menu")?;
    Ok(())
}

/// Lets the Settings sheet finish fading in, which goes by the clock.
#[expect(
    clippy::disallowed_methods,
    reason = "A picture of a sheet or a toast, not a test; the entrance has no event to wait on."
)]
fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(300));
}

fn open(cx: &mut Cx, folder: &Path) -> gpui::Result<gpui::WindowHandle<Browser>> {
    open_at(cx, folder, 1100., 700.)
}

fn open_at(
    cx: &mut Cx,
    folder: &Path,
    width: f32,
    height: f32,
) -> gpui::Result<gpui::WindowHandle<Browser>> {
    let folder = folder.to_path_buf();
    cx.open_window(size(px(width), px(height)), |_, cx| {
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

/// A small website's files as a ZIP, as a download would be.
fn site_assets(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path)?);
    let options = zip::write::SimpleFileOptions::default();
    for name in ["fonts/", "images/"] {
        zip.add_directory(name, options)
            .map_err(std::io::Error::other)?;
    }
    for (name, size) in [
        ("fonts/Inter.woff2", 98_000),
        ("images/hero.jpg", 412_000),
        ("index.html", 14_000),
        ("README.md", 3_000),
        ("styles.css", 22_000),
    ] {
        zip.start_file(name, options)
            .map_err(std::io::Error::other)?;
        zip.write_all(&vec![b'a'; size])?;
    }
    zip.finish().map_err(std::io::Error::other)?;
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
    let project = root.join("monodrive");
    for folder in ["crates/monodrive", "crates/monodrive-core", "docs"] {
        std::fs::create_dir_all(project.join(folder))?;
    }
    for file in ["Cargo.toml", "DESIGN.md", "README.md", "AGENTS.md"] {
        std::fs::write(project.join(file), "")?;
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

/// The server's output, which ends the server when the connection lets go.
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

/// Saved servers reach macOS's own sftp-server over pipes, with no ssh,
/// network or keys; the one at studio.local has a changed host key.
fn local_sftp() -> monodrive_core::remote::Connector {
    Arc::new(|source: &Source| {
        if source.host.ends_with("studio.local") {
            return Err(RemoteError::HostKeyChanged(source.host.clone()));
        }
        let mut child = std::process::Command::new("/usr/libexec/sftp-server")
            .current_dir("/")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| RemoteError::Failed(e.to_string()))?;
        let (Some(stdin), Some(out)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(RemoteError::Failed("sftp-server has no pipes.".into()));
        };
        Ok(Transport::new(ServerOut { out, child }, stdin))
    })
}
