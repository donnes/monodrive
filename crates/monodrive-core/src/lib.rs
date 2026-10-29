//! Monodrive's services. The GPUI app and the MCP server both call these, so
//! anything a user can do from the window an agent can do through MCP.

pub mod entries;
pub mod index;
pub mod mcp;
pub mod paths;

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
