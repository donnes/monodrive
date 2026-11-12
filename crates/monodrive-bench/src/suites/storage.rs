//! Storage inspection, as Find Duplicates and the MCP tools run it: a scan
//! of a 100,000-entry `node_modules`, and finding the duplicates among
//! files shaped like a Downloads folder's, each with the most memory the
//! process grew by. Every round is its own process, so the footprint's
//! high-water mark starts from the round.

use crate::format::{Cache, Measured};
use crate::measure::{self, Sample};
use crate::rng::Rng;
use monodrive_core::storage::{self, Match, Progress};
use std::path::Path;

const TREE: &str = "storage-tree";
const TREE_ENTRIES: usize = 100_000;
const DOWNLOADS: &str = "storage-downloads";
/// Files, of which `COPIES` repeat earlier ones and `DECOYS` match an
/// earlier one's size but not its middle.
const FILES: usize = 48;
const COPIES: usize = 16;
const DECOYS: usize = 4;
const MB: usize = 1_000_000;

pub fn prepare(fixture: &Path, seed: u64) -> Result<(), String> {
    let shape = crate::tree::load(&super::shapes(), "node_modules")?;
    let nodes = crate::tree::plan(&shape, seed, TREE_ENTRIES);
    let tree = fixture.join(TREE);
    crate::tree::write(&tree, &nodes).map_err(|e| format!("{}: {e}", tree.display()))?;

    let downloads = fixture.join(DOWNLOADS);
    std::fs::create_dir_all(&downloads).map_err(|e| format!("{}: {e}", downloads.display()))?;
    let mut rng = Rng::new(seed);
    let mut made: Vec<Vec<u8>> = Vec::new();
    for i in 0..FILES {
        let bytes = if (FILES - COPIES - DECOYS..FILES - DECOYS).contains(&i) {
            made[rng.below(made.len())].clone()
        } else if i >= FILES - DECOYS {
            let mut decoy = made[i - COPIES - DECOYS].clone();
            let middle = decoy.len() / 2;
            decoy[middle] ^= 0xff;
            decoy
        } else {
            let size = MB + rng.below(3 * MB);
            (0..size).map(|_| rng.next() as u8).collect()
        };
        let path = downloads.join(format!("file {i:02}.bin"));
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        made.push(bytes);
    }
    Ok(())
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    // The smaller work first, so the tree's scan sets its own high-water
    // mark rather than standing under the duplicates'.
    let before = footprint();
    let progress = Progress::default();
    let report = storage::scan(&fixture.join(DOWNLOADS), &progress, None)?;
    let (sample, found) = measure::here(|| report.duplicates(&|_| true, &progress, None));
    let verified = found
        .groups
        .iter()
        .filter(|group| group.matched == Match::Content)
        .count();
    let read = progress.read.load(std::sync::atomic::Ordering::Relaxed);
    let compared = measured(
        "duplicates/downloads",
        "Find duplicates among 48 files",
        report.root().usage.files,
        sample,
        format!("{verified} groups verified, {} MB read", read / MB as u64),
        peak_since(before),
    );
    drop((report, found));

    let before = footprint();
    let (sample, report) =
        measure::here(|| storage::scan(&fixture.join(TREE), &Progress::default(), None));
    let report = report?;
    let items = report.root().usage.files + report.folders.len() as u64;
    let scanned = measured(
        "scan/node_modules",
        "Scan 100,000 entries",
        items,
        sample,
        format!("{items} entries"),
        peak_since(before),
    );
    Ok(vec![scanned, compared])
}

fn measured(
    bench: &str,
    title: &str,
    items: u64,
    sample: Sample,
    check: String,
    peak_bytes: Option<u64>,
) -> Measured {
    Measured {
        bench: bench.into(),
        title: title.into(),
        group: "Storage inspection".into(),
        variant: String::new(),
        items: Some(items),
        cache: Cache::Warm,
        sample,
        check,
        peak_bytes,
    }
}

/// This process's footprint now and at its highest, as Activity Monitor
/// counts memory.
fn footprint() -> Option<(u64, u64)> {
    // SAFETY: an all-zero rusage_info_v4 is valid, and the call writes only
    // into it.
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let read = unsafe {
        libc::proc_pid_rusage(
            std::process::id() as libc::pid_t,
            libc::RUSAGE_INFO_V4,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        )
    };
    (read == 0).then_some((info.ri_phys_footprint, info.ri_lifetime_max_phys_footprint))
}

/// How far the footprint's high-water mark rose above where it stood.
fn peak_since(before: Option<(u64, u64)>) -> Option<u64> {
    let ((now, _), (_, highest)) = (before?, footprint()?);
    Some(highest.saturating_sub(now))
}
