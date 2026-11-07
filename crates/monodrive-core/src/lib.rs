//! Monodrive's services. The GPUI app and the MCP server both call these, so
//! anything a user can do from the window an agent can do through MCP.

pub mod apps;
pub mod archives;
pub mod cloud;
pub mod edits;
pub mod entries;
pub mod index;
pub mod info;
pub mod mcp;
pub mod naming;
pub mod ops;
pub mod overview;
pub mod paths;
pub mod places;
pub mod preview;
pub mod remote;
pub mod search;
pub mod servers;
pub mod sizes;
pub mod storage;
pub mod thumbnails;
pub mod updates;
pub mod user_commands;
pub mod volumes;
pub mod worktrees;

/// Nightlies carry a version Cargo.toml can't, which `scripts/release` passes in.
pub const VERSION: &str = match option_env!("MONODRIVE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Whether this build came from anywhere but `scripts/release`, the only thing
/// that sets `MONODRIVE_VERSION`. A dev build keeps its own support folder,
/// operation owner and index, so it can run beside the installed app.
pub const DEV: bool = option_env!("MONODRIVE_VERSION").is_none();

/// `~/Library/Application Support/Monodrive`, or `Monodrive (Dev)` in a dev build.
pub fn support_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let name = if DEV { "Monodrive (Dev)" } else { "Monodrive" };
    Some(
        std::path::PathBuf::from(home)
            .join("Library/Application Support")
            .join(name),
    )
}

/// Gives a new dev support folder the installed app's settings and a clone of
/// its saved index, so the first run skips onboarding and the disk crawl. Only
/// reads the installed app's files, and fsearch saves `index.bin` by renaming,
/// so a copy is always a whole index; events since then replay when it loads.
pub fn seed_dev_support() {
    let (true, Some(support), Some(home)) = (DEV, support_dir(), std::env::var_os("HOME")) else {
        return;
    };
    let library = std::path::Path::new(&home).join("Library/Application Support");
    let seed = |from: std::path::PathBuf, to: std::path::PathBuf| {
        if to.exists() || !from.is_file() {
            return;
        }
        let next = to.with_extension("seeding");
        let copied = to
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::copy(&from, &next))
            .and_then(|_| std::fs::rename(&next, &to));
        if let Err(e) = copied {
            eprintln!("monodrive: couldn't copy {}: {e}", from.display());
        }
    };
    seed(
        library.join("Monodrive/settings.json"),
        support.join("settings.json"),
    );
    seed(
        library.join("FSearch/index.bin"),
        support.join("FSearch/index.bin"),
    );
}

/// `~` and `~/rest` resolved against `$HOME`; other paths unchanged.
pub fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            let mut full = std::path::PathBuf::from(home);
            full.push(rest.trim_start_matches('/'));
            full
        }
        _ => path.into(),
    }
}
