//! Monodrive's services. The GPUI app and the MCP server both call these, so
//! anything a user can do from the window an agent can do through MCP.

pub mod apps;
pub mod archives;
pub mod catalogs;
pub mod cloud;
pub mod compare;
pub mod entries;
pub mod index;
pub mod info;
pub mod mcp;
pub mod ops;
pub mod paths;
pub mod places;
pub mod remote;
pub mod search;
pub mod servers;
pub mod sizes;
pub mod storage;
pub mod user_commands;
pub mod volumes;
pub mod worktrees;

/// `~/Library/Application Support/Monodrive`.
pub fn support_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join("Library/Application Support/Monodrive"))
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
