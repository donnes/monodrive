//! The suites. The runner makes each suite's fixtures once, in `prepare`;
//! `run-one` then measures one round on them, in a process of its own, for
//! whichever build is being measured. A fixture's layout is the contract
//! between builds of different commits, so change it only with `format::ROUND`.

#[cfg(target_os = "macos")]
pub mod compare;
#[cfg(target_os = "macos")]
pub mod core;
#[cfg(target_os = "macos")]
pub mod mcp;
pub mod platform;
#[cfg(target_os = "macos")]
pub mod search;
#[cfg(target_os = "macos")]
pub mod storage;
pub mod workload;

use crate::format::Measured;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Suite {
    Platform,
    #[cfg(target_os = "macos")]
    Core,
    #[cfg(target_os = "macos")]
    Search,
    #[cfg(target_os = "macos")]
    Storage,
    #[cfg(target_os = "macos")]
    Mcp,
    Workload,
    #[cfg(target_os = "macos")]
    Compare,
}

impl Suite {
    /// What `scripts/bench` runs with no suite named.
    pub fn defaults() -> Vec<Suite> {
        #[cfg(target_os = "macos")]
        return vec![
            Suite::Platform,
            Suite::Core,
            Suite::Search,
            Suite::Storage,
            Suite::Mcp,
            Suite::Workload,
            Suite::Compare,
        ];
        #[cfg(not(target_os = "macos"))]
        return vec![Suite::Platform, Suite::Workload];
    }

    pub fn name(self) -> &'static str {
        match self {
            Suite::Platform => "platform",
            #[cfg(target_os = "macos")]
            Suite::Core => "core",
            #[cfg(target_os = "macos")]
            Suite::Search => "search",
            #[cfg(target_os = "macos")]
            Suite::Storage => "storage",
            #[cfg(target_os = "macos")]
            Suite::Mcp => "mcp",
            Suite::Workload => "workload",
            #[cfg(target_os = "macos")]
            Suite::Compare => "compare",
        }
    }

    pub fn parse(name: &str) -> Result<Suite, String> {
        match name {
            "platform" => Ok(Suite::Platform),
            #[cfg(target_os = "macos")]
            "core" => Ok(Suite::Core),
            #[cfg(target_os = "macos")]
            "search" => Ok(Suite::Search),
            #[cfg(target_os = "macos")]
            "storage" => Ok(Suite::Storage),
            #[cfg(target_os = "macos")]
            "mcp" => Ok(Suite::Mcp),
            "workload" => Ok(Suite::Workload),
            #[cfg(target_os = "macos")]
            "compare" => Ok(Suite::Compare),
            other => Err(format!(
                "{other}: no such suite on {}",
                std::env::consts::OS
            )),
        }
    }

    /// Makes the suite's fixtures in the empty folder `fixture`.
    pub fn prepare(self, fixture: &Path, made: &Fixtures) -> Result<(), String> {
        let seed = made.seed;
        match self {
            #[cfg(target_os = "macos")]
            Suite::Platform => {
                let shape = crate::tree::load(&shapes(), "node_modules")?;
                let nodes = crate::tree::plan(&shape, seed, made.tree);
                let tree = fixture.join(platform::TREE);
                crate::tree::write(&tree, &nodes).map_err(|e| format!("{}: {e}", tree.display()))
            }
            #[cfg(not(target_os = "macos"))]
            Suite::Platform => Ok(()),
            #[cfg(target_os = "macos")]
            Suite::Core => core::prepare(fixture, seed, made.tree),
            #[cfg(target_os = "macos")]
            Suite::Search => search::prepare(fixture, seed),
            #[cfg(target_os = "macos")]
            Suite::Storage => storage::prepare(fixture, seed),
            #[cfg(target_os = "macos")]
            Suite::Mcp => mcp::prepare(fixture, seed),
            Suite::Workload => workload::prepare(fixture, seed),
            #[cfg(target_os = "macos")]
            Suite::Compare => compare::prepare(fixture, seed, made.tree),
        }
    }

    /// One round, as `run-one` measures it.
    pub fn round(self, fixture: &Path) -> Result<Vec<Measured>, String> {
        match self {
            Suite::Platform => platform::round(fixture),
            #[cfg(target_os = "macos")]
            Suite::Core => core::round(fixture),
            #[cfg(target_os = "macos")]
            Suite::Search => search::round(fixture),
            #[cfg(target_os = "macos")]
            Suite::Storage => storage::round(fixture),
            #[cfg(target_os = "macos")]
            Suite::Mcp => mcp::round(fixture),
            Suite::Workload => workload::round(fixture),
            #[cfg(target_os = "macos")]
            Suite::Compare => compare::round(fixture),
        }
    }

    /// Undoes what `prepare` did outside `fixture`, such as an attached disk
    /// image. The runner deletes `fixture` itself.
    pub fn finish(self, fixture: &Path) -> Result<(), String> {
        match self {
            #[cfg(target_os = "macos")]
            Suite::Core => core::finish(fixture),
            #[cfg(target_os = "macos")]
            Suite::Compare => compare::finish(fixture),
            _ => Ok(()),
        }
    }
}

/// What the runner makes fixtures from.
pub struct Fixtures {
    pub seed: u64,
    /// Entries in the generated `node_modules` that whole-folder clones and
    /// jobs copy.
    pub tree: usize,
}

/// `clonefile`'s flag to clone a link rather than what it points to, which
/// libc doesn't define.
#[cfg(target_os = "macos")]
pub const CLONE_NOFOLLOW: u32 = 0x0001;

/// `benchmarks/shapes`, in the checkout the tool was built from.
pub fn shapes() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/shapes")
}

/// Items inside `dir`, without following links.
pub fn walk_count(dir: &Path) -> std::io::Result<usize> {
    let mut count = 0;
    let mut folders = vec![dir.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for item in std::fs::read_dir(&folder)? {
            let item = item?;
            count += 1;
            if item.file_type()?.is_dir() {
                folders.push(item.path());
            }
        }
    }
    Ok(count)
}

/// A short path that leads to `target`, a folder in a fixture. A Unix
/// socket's path can't pass 103 bytes, and the owner's socket sits deep in
/// the fixture, or under `HOME` for the MCP server. Only the link is in /tmp;
/// the files stay in the fixture. Dropping it removes the link.
#[cfg(target_os = "macos")]
pub struct ShortPath(std::path::PathBuf);

#[cfg(target_os = "macos")]
impl ShortPath {
    pub fn to(target: &Path, name: &str) -> Result<Self, String> {
        let link = std::path::PathBuf::from(format!("/tmp/mdb-{}-{name}", std::process::id()));
        std::os::unix::fs::symlink(target, &link)
            .map_err(|e| format!("{}: {e}", link.display()))?;
        Ok(Self(link))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[cfg(target_os = "macos")]
impl Drop for ShortPath {
    fn drop(&mut self) {
        #[expect(
            clippy::disallowed_methods,
            reason = "the benchmark's own link, never the user's files"
        )]
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Deletes a folder the benchmark made. Never point it at anything else.
pub fn delete_fixture(path: &Path) -> Result<(), String> {
    #[expect(
        clippy::disallowed_methods,
        reason = "the benchmark's own generated fixture, never the user's files"
    )]
    std::fs::remove_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))
}
