//! Monodrive against what someone would use instead, each with its fastest
//! documented flags: a copy against `cp -c`, `ditto` and `rsync -a`, a
//! listing against `find`, and Move to Trash against `rm -rf`. Losses stay in
//! the report. Each row's Monodrive variant is called the way the window and
//! MCP call it; the others run as processes, as a person or agent runs them,
//! so their rows include starting a process, which `platform` measures alone.

use crate::format::{Cache, Measured};
use crate::measure::{self, Sample};
use crate::suites::core::{Jobs, clone_into, hdiutil};
use crate::suites::{delete_fixture, walk_count};
use monodrive_core::entries::{Sort, list_dir};
use monodrive_core::ops::Request;
use std::path::Path;
use std::process::Command;

const LISTED: usize = 100_000;
const IMAGE: &str = "trash.sparseimage";
const VOLUME: &str = "trash-volume";

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

pub fn prepare(fixture: &Path, seed: u64, tree_entries: usize) -> Result<(), String> {
    let listed = fixture.join("listed");
    std::fs::create_dir(&listed).map_err(io(&listed))?;
    for i in 0..LISTED {
        let path = listed.join(format!("file {i}.txt"));
        std::fs::write(&path, i.to_le_bytes()).map_err(io(&path))?;
    }
    let shape = crate::tree::load(&crate::suites::shapes(), "node_modules")?;
    let nodes = crate::tree::plan(&shape, seed, tree_entries);
    let tree = fixture.join("node_modules");
    crate::tree::write(&tree, &nodes).map_err(io(&tree))?;
    std::fs::create_dir(fixture.join("into")).map_err(io(fixture))?;
    let (image, volume) = (fixture.join(IMAGE), fixture.join(VOLUME));
    let (image_text, volume_text) = (image.to_string_lossy(), volume.to_string_lossy());
    hdiutil(&[
        "create",
        "-quiet",
        "-size",
        "4g",
        "-fs",
        "APFS",
        "-type",
        "SPARSE",
        "-volname",
        "MonodriveBenchCompare",
        &image_text,
    ])?;
    std::fs::create_dir(&volume).map_err(io(&volume))?;
    hdiutil(&[
        "attach",
        "-quiet",
        "-nobrowse",
        "-noautoopen",
        "-mountpoint",
        &volume_text,
        &image_text,
    ])?;
    crate::tree::write(&volume.join("master"), &nodes).map_err(io(&volume))
}

pub fn finish(fixture: &Path) -> Result<(), String> {
    crate::suites::core::detach(&fixture.join(VOLUME))
}

fn measured(
    bench: &str,
    title: &str,
    variant: &str,
    items: usize,
    sample: Sample,
    check: String,
) -> Measured {
    Measured {
        bench: bench.into(),
        title: title.into(),
        group: title.into(),
        variant: variant.into(),
        items: Some(items as u64),
        cache: Cache::Warm,
        sample,
        check,
        peak_bytes: None,
    }
}

fn process(command: &mut Command) -> Result<(Sample, Vec<u8>), String> {
    let (sample, ran) = measure::child(command)?;
    if ran.success {
        Ok((sample, ran.stdout))
    } else {
        Err(format!("{command:?} failed"))
    }
}

fn copies(fixture: &Path, jobs: &mut Jobs, out: &mut Vec<Measured>) -> Result<(), String> {
    let tree = fixture.join("node_modules");
    let into = fixture.join("into");
    let entries = walk_count(&tree).map_err(io(&tree))?;
    let title = "Copy a node_modules";
    let to = into.join("node_modules");
    let check = |sample: Sample, variant: &str, out: &mut Vec<Measured>| -> Result<(), String> {
        let copied = walk_count(&to).map_err(io(&to))?;
        delete_fixture(&to)?;
        out.push(measured(
            "copy",
            title,
            variant,
            entries,
            sample,
            format!("{copied} entries"),
        ));
        Ok(())
    };
    let (sample, done) = measure::here(|| {
        jobs.run(
            Request::Copy {
                paths: vec![tree.clone()],
                into: into.clone(),
                conflict: Default::default(),
            },
            true,
        )
    });
    done?;
    check(sample, "Monodrive", out)?;
    let (sample, _) = process(Command::new("cp").arg("-c").arg("-R").arg(&tree).arg(&into))?;
    check(sample, "cp -c -R", out)?;
    let (sample, _) = process(Command::new("ditto").arg(&tree).arg(&to))?;
    check(sample, "ditto", out)?;
    let (sample, _) = process(Command::new("rsync").arg("-a").arg(&tree).arg(&into))?;
    check(sample, "rsync -a", out)
}

fn listing(fixture: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    let dir = fixture.join("listed");
    let title = "List 100,000 entries";
    let (sample, entries) = measure::here(|| list_dir(&dir, false, Sort::default()));
    let entries = entries.map_err(io(&dir))?;
    out.push(measured(
        "list",
        title,
        "Monodrive",
        LISTED,
        sample,
        format!("{} entries", entries.len()),
    ));
    let (sample, printed) =
        process(
            Command::new("find")
                .arg(&dir)
                .args(["-mindepth", "1", "-maxdepth", "1"]),
        )?;
    let lines = printed
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .count();
    out.push(measured(
        "list",
        title,
        "find, unsorted",
        LISTED,
        sample,
        format!("{lines} entries"),
    ));
    let (sample, printed) = process(Command::new("ls").arg("-1").arg(&dir))?;
    let lines = printed
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .count();
    out.push(measured(
        "list",
        title,
        "ls -1, sorted",
        LISTED,
        sample,
        format!("{lines} entries"),
    ));
    Ok(())
}

/// Move to Trash is one rename per item, so it should beat deleting a big
/// folder file by file. On the suite's own disk image.
fn trash(fixture: &Path, jobs: &mut Jobs, out: &mut Vec<Measured>) -> Result<(), String> {
    let volume = fixture.join(VOLUME);
    let title = "Get a node_modules out of the way";
    let (trashed, removed) = (volume.join("trashed"), volume.join("removed"));
    clone_into(&volume.join("master"), &trashed)?;
    clone_into(&volume.join("master"), &removed)?;
    let entries = walk_count(&trashed).map_err(io(&trashed))?;
    let (sample, done) = measure::here(|| {
        jobs.run(
            Request::Trash {
                paths: vec![trashed.clone()],
            },
            false,
        )
    });
    let landed = done?;
    out.push(measured(
        "trash",
        title,
        "Monodrive",
        entries,
        sample,
        format!("{} moved to the Trash", landed.len()),
    ));
    for path in landed.iter().filter(|p| p.starts_with(&volume)) {
        delete_fixture(path)?;
    }
    let (sample, _) = process(Command::new("rm").arg("-rf").arg(&removed))?;
    out.push(measured(
        "trash",
        title,
        "rm -rf",
        entries,
        sample,
        format!("{} left", u8::from(removed.exists())),
    ));
    Ok(())
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let mut out = Vec::new();
    let mut jobs = Jobs::start(fixture)?;
    copies(fixture, &mut jobs, &mut out)?;
    listing(fixture, &mut out)?;
    trash(fixture, &mut jobs, &mut out)?;
    Ok(out)
}
