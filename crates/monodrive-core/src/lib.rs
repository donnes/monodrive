//! Monodrive's services. The GPUI app and the MCP server both call these, so
//! anything a user can do from the window an agent can do through MCP.

pub mod apps;
pub mod archives;
pub mod cloud;
pub mod disk;
pub mod edits;
pub mod entries;
pub mod index;
pub mod info;
pub mod localsend;
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
pub mod usage;
pub mod user_commands;
pub mod volumes;
pub mod worktrees;

/// Who asked. An agent is known by the name its MCP client gives, not by
/// the person behind it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Origin {
    Window,
    Agent { client: Option<String> },
}

/// Nightlies carry a version Cargo.toml can't, which `scripts/release` passes in.
pub const VERSION: &str = match option_env!("MONODRIVE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// Whether this build came from anywhere but `scripts/release`, the only thing
/// that sets `MONODRIVE_VERSION`. A dev build keeps its own support folder,
/// operation owner and index, so it can run beside the installed app.
pub const DEV: bool = option_env!("MONODRIVE_VERSION").is_none();

const NAME: &str = if DEV { "Monodrive (Dev)" } else { "Monodrive" };

/// `~/Library/Application Support/Monodrive`, or `Monodrive (Dev)` in a dev build.
pub fn support_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        std::path::PathBuf::from(home)
            .join("Library/Application Support")
            .join(NAME),
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

/// Sends stderr that would go to `/dev/null`, as an app opened from Finder's
/// does, to `~/Library/Logs/Monodrive/monodrive.log`, where Console finds it.
/// fsearch reports there each time it relists the disk, and why.
pub fn log_to_file() {
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::fs::MetadataExt;
    const KEEP: u64 = 4 << 20;
    let discarded = std::fs::metadata("/dev/null").is_ok_and(|null| {
        std::io::stderr()
            .as_fd()
            .try_clone_to_owned()
            .and_then(|fd| std::fs::File::from(fd).metadata())
            .is_ok_and(|err| (err.dev(), err.ino()) == (null.dev(), null.ino()))
    });
    let (true, Some(home)) = (discarded, std::env::var_os("HOME")) else {
        return;
    };
    let dir = std::path::Path::new(&home).join("Library/Logs").join(NAME);
    let log = dir.join("monodrive.log");
    if std::fs::metadata(&log).is_ok_and(|m| m.len() > KEEP) {
        let _ = std::fs::rename(&log, dir.join("monodrive.old.log"));
    }
    let opened = std::fs::create_dir_all(&dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
    });
    // Nowhere to report a failure: stderr is /dev/null.
    if let Ok(file) = opened {
        // SAFETY: both descriptors are open; dup2 leaves stderr on the file
        // after `file` closes its own.
        unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) };
    }
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
