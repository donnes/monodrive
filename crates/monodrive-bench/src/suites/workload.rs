//! Real work a developer waits on, the same on a Mac and a PC: installing a
//! pinned lockfile with pnpm from a store already on disk, adding a Git
//! worktree, and deleting both. It measures the platform, not Monodrive, so
//! a Mac against a PC shows what plan 22's worktree setup is up against.
//!
//! The store is fetched once into `target/bench/pnpm-store` and kept between
//! runs, so a run downloads nothing after the first. The project and the
//! repository live in the fixture.

use crate::format::{Cache, Measured};
use crate::measure;
use crate::suites::{delete_fixture, walk_count};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Nothing here runs a package's build script, so pnpm needn't refuse the
/// ones nobody approved.
const NO_BUILDS: &str = "--config.strict-dep-builds=false";

/// Files in the repository a worktree checks out.
const REPO_FILES: usize = 10_000;

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

fn workload() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/workload")
}

/// The store `prepare` fetches into, kept in the running checkout's
/// `target/bench`.
fn store() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench/pnpm-store")
}

/// Where `prepare` wrote the store's path, so a baseline built in another
/// checkout installs from the same store.
fn store_record(fixture: &Path) -> PathBuf {
    fixture.join("store-path")
}

fn run(command: &mut Command) -> Result<String, String> {
    let out = command.output().map_err(|e| format!("{command:?}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "{command:?}: {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub fn prepare(fixture: &Path, seed: u64) -> Result<(), String> {
    let project = fixture.join("project");
    std::fs::create_dir(&project).map_err(io(&project))?;
    for name in ["package.json", "pnpm-lock.yaml"] {
        std::fs::copy(workload().join(name), project.join(name)).map_err(io(&project))?;
    }
    // Downloads only what the store lacks: nothing after the first run.
    run(Command::new("pnpm")
        .args(["fetch", "--frozen-lockfile", NO_BUILDS, "--store-dir"])
        .arg(store())
        .current_dir(&project))?;
    let store = std::fs::canonicalize(store()).map_err(io(&store()))?;
    std::fs::write(store_record(fixture), store.as_os_str().as_encoded_bytes())
        .map_err(io(fixture))?;
    // A repository of generated files with a real tree's shape, the same on
    // every machine for a seed.
    let repo = fixture.join("repo");
    let shape = crate::tree::load(&crate::suites::shapes(), "node_modules")?;
    let nodes: Vec<_> = crate::tree::plan(&shape, seed, REPO_FILES)
        .into_iter()
        .filter(|node| node.kind != crate::tree::Kind::Link)
        .collect();
    crate::tree::write(&repo, &nodes).map_err(io(&repo))?;
    let git = |args: &[&str]| run(Command::new("git").arg("-C").arg(&repo).args(args));
    git(&["init", "--quiet"])?;
    git(&["add", "--all"])?;
    git(&[
        "-c",
        "user.name=monodrive-bench",
        "-c",
        "user.email=bench@monodrive.invalid",
        "commit",
        "--quiet",
        "--no-gpg-sign",
        "-m",
        "fixture",
    ])?;
    Ok(())
}

fn measured(
    bench: &str,
    title: &str,
    items: usize,
    sample: measure::Sample,
    check: String,
) -> Measured {
    Measured {
        bench: bench.into(),
        title: title.into(),
        group: "Developer work".into(),
        variant: String::new(),
        items: Some(items as u64),
        cache: Cache::Warm,
        sample,
        check,
    }
}

fn child(command: &mut Command) -> Result<measure::Sample, String> {
    let (sample, ran) = measure::child(command)?;
    if ran.success {
        Ok(sample)
    } else {
        Err(format!("{command:?} failed"))
    }
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let work = fixture.join(format!("round-{}", std::process::id()));
    std::fs::create_dir(&work).map_err(io(&work))?;
    let result = (|| {
        let mut out = Vec::new();
        let project = work.join("project");
        std::fs::create_dir(&project).map_err(io(&project))?;
        for name in ["package.json", "pnpm-lock.yaml"] {
            std::fs::copy(fixture.join("project").join(name), project.join(name))
                .map_err(io(&project))?;
        }
        let pnpm = run(Command::new("pnpm").arg("--version"))?;
        let record = store_record(fixture);
        let store = std::fs::read_to_string(&record).map_err(io(&record))?;
        let sample = child(
            Command::new("pnpm")
                .args([
                    "install",
                    "--offline",
                    "--frozen-lockfile",
                    "--ignore-scripts",
                    NO_BUILDS,
                    "--reporter",
                    "silent",
                    "--store-dir",
                ])
                .arg(store.trim())
                .current_dir(&project),
        )?;
        let modules = project.join("node_modules");
        let installed = walk_count(&modules).map_err(io(&modules))?;
        out.push(measured(
            "pnpm-install",
            "pnpm install of a pinned lockfile from the store",
            installed,
            sample,
            format!("{installed} entries, pnpm {pnpm}"),
        ));
        let sample = child(Command::new("rm").arg("-rf").arg(&modules))?;
        out.push(measured(
            "rm-node-modules",
            "rm -rf the node_modules it made",
            installed,
            sample,
            format!("{} left", u8::from(modules.exists())),
        ));

        let repo = fixture.join("repo");
        let tree = work.join("worktree");
        let sample = child(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "add", "--quiet", "--detach"])
                .arg(&tree)
                .arg("HEAD"),
        )?;
        let files = walk_count(&tree).map_err(io(&tree))?;
        out.push(measured(
            "git-worktree-add",
            "git worktree add of a generated repository",
            files,
            sample,
            format!("{files} entries checked out"),
        ));
        let sample = child(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["worktree", "remove", "--force"])
                .arg(&tree),
        )?;
        out.push(measured(
            "git-worktree-remove",
            "git worktree remove of it",
            files,
            sample,
            format!("{} left", u8::from(tree.exists())),
        ));
        Ok(out)
    })();
    let _ = run(Command::new("git")
        .arg("-C")
        .arg(fixture.join("repo"))
        .args(["worktree", "prune"]));
    delete_fixture(&work)?;
    result
}
