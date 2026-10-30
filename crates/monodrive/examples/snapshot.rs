//! Draws the browser to PNGs through GPUI's headless Metal renderer: no
//! window on screen, no screen recording, no index. Agents use it to look
//! at a UI change.
//!
//! `cargo run -p monodrive --example snapshot -- [folder]` writes
//! `target/snapshots/list.png`, `grid.png`, `rename.png`, `menu.png`,
//! `menu-grid.png`, `menu-folder.png`, `jobs.png`, `ring-waiting.png`,
//! `rename-error.png`, `toast.png` and `empty-trash.png`.
//! Without a folder it draws a made-up one, so the pictures show nobody's
//! files. Rename edits the fifth name in place. The menus are the context
//! menu on the second entry, in the list and the grid, and on the folder.
//! The rest use an operation owner on a temp folder. The popover and the
//! ring show an agent's move there, waiting at a name that's taken. Then the
//! last entry is renamed to a name that's taken, then to a free one, which
//! is undone after the picture, and Empty Trash counts a Trash of 12 items.

use gpui::{Action, HeadlessAppContext as Cx, Keystroke};
use gpui::{AppContext, Focusable, HeadlessAppContext, px, size};
use monodrive::browser::{
    Browser, CancelRename, CommitRename, EmptyTrash, Operations, Rename, SelectLast, SelectNext,
    ShowContextMenu, ShowCopiesAndMoves, Undo,
};
use monodrive::preferences::{Preferences, View};
use monodrive_core::ops::{self, Conflict, Origin, Request};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn main() -> gpui::Result<()> {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&out)?;
    // The path bar would show the `..`s.
    let out = out.canonicalize()?;
    let folder = match std::env::args_os().nth(1) {
        Some(folder) => PathBuf::from(folder),
        None => fill(&out)?,
    };
    let mut cx = HeadlessAppContext::with_platform(
        gpui_platform::bench_text_system(),
        Arc::new(monodrive::assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    for (name, view) in [("list", View::List), ("grid", View::Grid)] {
        cx.update(|cx| {
            cx.set_global(Preferences {
                view,
                ..Preferences::default()
            })
        });
        let folder = folder.clone();
        let window = cx.open_window(size(px(1100.), px(700.)), |_, cx| {
            cx.new(|cx| Browser::new(folder, cx))
        })?;
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
    cx.update(|cx| cx.set_global(Preferences::default()));
    let window = cx.open_window(size(px(1100.), px(700.)), |_, cx| {
        cx.new(|cx| Browser::new(folder.clone(), cx))
    })?;
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
                ..Preferences::default()
            })
        });
        let window = cx.open_window(size(px(1100.), px(700.)), |_, cx| {
            cx.new(|cx| Browser::new(folder.clone(), cx))
        })?;
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
    let scratch = tempfile::tempdir_in(&out)?;
    let paths = ops::Paths {
        state: scratch.path().join("state"),
        settings: scratch.path().join("settings.json"),
        trash: scratch.path().join("Trash"),
    };
    std::fs::create_dir_all(&paths.trash)?;
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
    let window = cx.open_window(size(px(1100.), px(700.)), |_, cx| {
        cx.new(|cx| Browser::new(folder.clone(), cx))
    })?;
    cx.run_until_parked();
    // The popover asks the owner; closed, the ring stays with its dot.
    dispatch(&mut cx, window, &ShowCopiesAndMoves)?;
    save(&mut cx, window, &out, "jobs")?;
    dispatch(&mut cx, window, &ShowCopiesAndMoves)?;
    save(&mut cx, window, &out, "ring-waiting")?;

    let window = cx.open_window(size(px(1100.), px(700.)), |_, cx| {
        cx.new(|cx| Browser::new(folder.clone(), cx))
    })?;
    cx.run_until_parked();
    // The last entry, renamed to a name that's taken: the owner refuses it.
    dispatch(&mut cx, window, &SelectLast)?;
    dispatch(&mut cx, window, &Rename)?;
    let taken = names(&folder)
        .into_iter()
        .find(|name| name.contains('.'))
        .unwrap_or_default();
    type_text(&mut cx, window, &taken)?;
    dispatch(&mut cx, window, &CommitRename)?;
    save(&mut cx, window, &out, "rename-error")?;
    // A free name, then Undo once it's drawn.
    dispatch(&mut cx, window, &CancelRename)?;
    dispatch(&mut cx, window, &Rename)?;
    type_text(&mut cx, window, "Renamed")?;
    dispatch(&mut cx, window, &CommitRename)?;
    save(&mut cx, window, &out, "toast")?;
    dispatch(&mut cx, window, &Undo)?;
    dispatch(&mut cx, window, &EmptyTrash)?;
    save(&mut cx, window, &out, "empty-trash")?;
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
