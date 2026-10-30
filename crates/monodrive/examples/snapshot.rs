//! Draws the browser to PNGs through GPUI's headless Metal renderer: no
//! window on screen, no screen recording, no index. Agents use it to look
//! at a UI change.
//!
//! `cargo run -p monodrive --example snapshot -- [folder]` writes
//! `target/snapshots/list.png`, `grid.png` and `activity.png`. Without a
//! folder it draws a made-up one, so the pictures show nobody's files.
//! Activity shows real jobs, run by an operation owner on a temp folder.

use gpui::{AppContext, Focusable, HeadlessAppContext, px, size};
use monodrive::browser::{Browser, Operations, ShowActivity};
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
    let scratch = tempfile::tempdir_in(&out)?;
    let client = jobs(scratch.path())?;
    cx.update(|cx| cx.set_global(Operations(Arc::new(Mutex::new(client)))));
    let window = cx.open_window(size(px(1100.), px(700.)), |_, cx| {
        cx.new(|cx| Browser::new(folder.clone(), cx))
    })?;
    cx.run_until_parked();
    // Actions reach the browser through what has focus.
    cx.update_window(window.into(), |root, window, cx| {
        if let Ok(browser) = root.downcast::<Browser>() {
            window.focus(&browser.focus_handle(cx), cx);
        }
        window.dispatch_action(Box::new(ShowActivity), cx)
    })?;
    cx.run_until_parked();
    let path = out.join("activity.png");
    cx.capture_screenshot(window.into())?.save(&path)?;
    println!("{}", path.display());
    Ok(())
}

/// An owner on `dir` that has run a finished copy, one stopped at a name
/// that's taken, and a move that failed.
fn jobs(dir: &Path) -> gpui::Result<ops::Client> {
    let paths = ops::Paths {
        state: dir.join("state"),
        settings: dir.join("settings.json"),
    };
    let owner = ops::Owner::bind(paths.clone()).map_err(std::io::Error::other)?;
    std::thread::spawn(move || owner.serve());
    let (from, into) = (dir.join("Drafts"), dir.join("Invoices"));
    std::fs::create_dir_all(from.join("Old"))?;
    std::fs::create_dir_all(&into)?;
    for (name, size) in [("Brief.pdf", 240_000), ("notes.md", 3_000)] {
        std::fs::write(from.join(name), vec![b'x'; size])?;
    }
    let mut client = ops::Client::new(paths, None);
    let names = |names: &[&str]| names.iter().map(|name| from.join(name)).collect();
    let agent = Origin::Agent {
        client: Some("claude-code".into()),
    };
    for (origin, request) in [
        (
            Origin::Window,
            Request::Copy {
                paths: names(&["Brief.pdf", "notes.md"]),
                into: into.clone(),
                conflict: Conflict::Stop,
            },
        ),
        (
            agent.clone(),
            Request::Copy {
                paths: names(&["notes.md"]),
                into: into.clone(),
                conflict: Conflict::Stop,
            },
        ),
        (
            agent,
            // A folder can't go into itself.
            Request::Move {
                paths: vec![from.clone()],
                into: from.join("Old"),
                conflict: Conflict::Stop,
            },
        ),
    ] {
        client
            .submit(origin, request)
            .map_err(std::io::Error::other)?;
        // Each finishes in moments; the next copy needs this one's files.
        while client
            .jobs()
            .map_err(std::io::Error::other)?
            .iter()
            .any(|job| job.state.active())
        {
            std::thread::yield_now();
        }
    }
    Ok(client)
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
