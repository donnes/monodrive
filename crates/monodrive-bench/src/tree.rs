//! Folder trees for fixtures, with the shape of a real one. `measure` reads a
//! real folder's depths and sizes, never its names, into a JSON file checked
//! in under `benchmarks/shapes`. `plan` makes a tree of any size with that
//! shape from a seed, and `write` puts it on disk.

use crate::rng::Rng;
use serde::{Deserialize, Serialize};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Counts by depth, where depth 0 is directly inside the measured folder,
/// and file sizes by power of two: bucket `k` holds sizes from 2^(k-1) to
/// 2^k - 1, and bucket 0 the empty files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub source: String,
    pub folders: Vec<u64>,
    pub files: Vec<u64>,
    pub links: Vec<u64>,
    pub file_sizes: Vec<u64>,
}

fn bump(counts: &mut Vec<u64>, at: usize) {
    if counts.len() <= at {
        counts.resize(at + 1, 0);
    }
    counts[at] += 1;
}

fn bucket(size: u64) -> usize {
    (u64::BITS - size.leading_zeros()) as usize
}

/// `dir`'s shape, without following links. Unreadable folders count as empty.
pub fn measure(dir: &Path, source: &str) -> io::Result<Shape> {
    let mut shape = Shape {
        source: source.to_string(),
        folders: Vec::new(),
        files: Vec::new(),
        links: Vec::new(),
        file_sizes: Vec::new(),
    };
    let mut folders = vec![(dir.to_path_buf(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(items) = std::fs::read_dir(&folder) else {
            continue;
        };
        for item in items.filter_map(Result::ok) {
            let Ok(meta) = item.metadata() else { continue };
            if meta.is_symlink() {
                bump(&mut shape.links, depth);
            } else if meta.is_dir() {
                bump(&mut shape.folders, depth);
                folders.push((item.path(), depth + 1));
            } else {
                bump(&mut shape.files, depth);
                bump(&mut shape.file_sizes, bucket(meta.size()));
            }
        }
    }
    Ok(shape)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Folder,
    File(u64),
    /// A relative link to the first file in its folder, or to the folder.
    Link,
}

#[derive(Clone, Debug)]
pub struct Node {
    /// Relative to the tree's root, parents before children.
    pub path: PathBuf,
    pub kind: Kind,
}

/// Scales each count by `scale`, keeping at least one of anything there was.
fn scaled(counts: &[u64], scale: f64) -> Vec<usize> {
    counts
        .iter()
        .map(|&n| match n {
            0 => 0,
            n => ((n as f64 * scale).round() as usize).max(1),
        })
        .collect()
}

fn pick_weighted(rng: &mut Rng, weights: &[u64]) -> usize {
    let total: u64 = weights.iter().sum();
    let mut at = rng.next() % total.max(1);
    for (index, &weight) in weights.iter().enumerate() {
        if at < weight {
            return index;
        }
        at -= weight;
    }
    weights.len() - 1
}

const EXTENSIONS: &[&str] = &[
    "js", "ts", "json", "md", "map", "cjs", "mjs", "txt", "png", "pdf",
];

/// About `entries` items with `shape`'s depths and sizes. A folder's parent
/// is drawn with a skew toward the first folders at its depth, so some
/// folders are crowded and most are small, as in real trees.
pub fn plan(shape: &Shape, seed: u64, entries: usize) -> Vec<Node> {
    let measured: u64 = shape
        .folders
        .iter()
        .chain(&shape.files)
        .chain(&shape.links)
        .sum();
    let scale = entries as f64 / measured.max(1) as f64;
    let (folders, files, links) = (
        scaled(&shape.folders, scale),
        scaled(&shape.files, scale),
        scaled(&shape.links, scale),
    );
    let depths = folders.len().max(files.len()).max(links.len());
    let at = |counts: &[usize], depth: usize| counts.get(depth).copied().unwrap_or(0);
    let mut rng = Rng::new(seed);
    // Folders at each depth, as indexes into `nodes`. Depth -1 is the root.
    let mut levels: Vec<Vec<usize>> = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    let parent = |rng: &mut Rng, levels: &[Vec<usize>], depth: usize| -> Option<usize> {
        if depth == 0 {
            return None;
        }
        let above = &levels[depth - 1];
        let u = rng.below(1 << 20) as f64 / f64::from(1 << 20);
        Some(above[((u * u) * above.len() as f64) as usize])
    };
    let mut serial = 0;
    for depth in 0..depths {
        // Anything at this depth needs a folder above it.
        if depth > 0 && levels[depth - 1].is_empty() {
            break;
        }
        let mut level = Vec::new();
        let mut add = |rng: &mut Rng, nodes: &mut Vec<Node>, kind: Kind, stem: &str| {
            let under = parent(rng, &levels, depth);
            let base = under.map_or_else(PathBuf::new, |i| nodes[i].path.clone());
            serial += 1;
            let name = match kind {
                Kind::File(_) => format!("{stem}{serial}.{}", rng.pick(EXTENSIONS)),
                _ => format!("{stem}{serial}"),
            };
            nodes.push(Node {
                path: base.join(name),
                kind,
            });
            nodes.len() - 1
        };
        for _ in 0..at(&folders, depth) {
            level.push(add(&mut rng, &mut nodes, Kind::Folder, "folder-"));
        }
        for _ in 0..at(&files, depth) {
            let bucket = pick_weighted(&mut rng, &shape.file_sizes);
            let size = match bucket {
                0 => 0,
                k => (1u64 << (k - 1)) + rng.next() % (1u64 << (k - 1)),
            };
            add(&mut rng, &mut nodes, Kind::File(size), "file-");
        }
        for _ in 0..at(&links, depth) {
            add(&mut rng, &mut nodes, Kind::Link, "link-");
        }
        levels.push(level);
    }
    nodes
}

/// Writes `nodes` under `root`, which must not exist yet.
pub fn write(root: &Path, nodes: &[Node]) -> io::Result<()> {
    std::fs::create_dir(root)?;
    let pattern: Vec<u8> = (0..65_536u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let mut first_file: std::collections::HashMap<PathBuf, PathBuf> = Default::default();
    for node in nodes {
        let path = root.join(&node.path);
        let folder = node
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        match node.kind {
            Kind::Folder => std::fs::create_dir(&path)?,
            Kind::File(size) => {
                let mut file = std::fs::File::create_new(&path)?;
                let mut left = size as usize;
                while left > 0 {
                    let n = left.min(pattern.len());
                    io::Write::write_all(&mut file, &pattern[..n])?;
                    left -= n;
                }
                first_file
                    .entry(folder)
                    .or_insert_with(|| PathBuf::from(path.file_name().unwrap_or_default()));
            }
            Kind::Link => {
                let target = first_file
                    .get(&folder)
                    .cloned()
                    .unwrap_or_else(|| ".".into());
                std::os::unix::fs::symlink(target, &path)?;
            }
        }
    }
    Ok(())
}

/// The shapes checked in under `benchmarks/shapes`.
pub fn load(shapes: &Path, name: &str) -> Result<Shape, String> {
    let path = shapes.join(format!("{name}.json"));
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_planned_tree_keeps_the_shape_it_was_given_and_its_seed_fixes_it() {
        let shape = Shape {
            source: "test".into(),
            folders: vec![10, 100, 50],
            files: vec![20, 400, 800, 300],
            links: vec![0, 30],
            file_sizes: vec![10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 80, 10],
        };
        let nodes = plan(&shape, 3, 17_100);
        let written = tempfile::tempdir().unwrap();
        let root = written.path().join("tree");
        write(&root, &nodes).unwrap();
        let again = measure(&root, "test").unwrap();
        // Ten times the measured 1,710, at the same depths.
        assert_eq!(again.folders, [100, 1_000, 500]);
        assert_eq!(again.files, [200, 4_000, 8_000, 3_000]);
        assert_eq!(again.links, [0, 300]);
        assert_eq!(again.file_sizes.iter().sum::<u64>(), 15_200);
        // A tenth of the files are empty, give or take the draw.
        assert!(
            (1_350..1_700).contains(&again.file_sizes[0]),
            "{:?}",
            again.file_sizes
        );
        let same: Vec<PathBuf> = plan(&shape, 3, 17_100)
            .into_iter()
            .map(|n| n.path)
            .collect();
        assert!(same.iter().eq(nodes.iter().map(|n| &n.path)));
    }
}
