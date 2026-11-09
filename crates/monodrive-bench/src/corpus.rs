//! The test files every search tool runs against. A seed fixes them, and
//! `manifest.tsv` beside them is the answer key for accuracy.

use crate::rng::Rng;
use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

const COMMON: &[&str] = &[
    "report", "invoice", "notes", "draft", "backup", "config", "budget", "meeting", "summary",
    "photo", "contract", "final",
];
const WORDS: &[&str] = &[
    "alpha",
    "archive",
    "audio",
    "beta",
    "billing",
    "branch",
    "cache",
    "client",
    "cover",
    "data",
    "design",
    "export",
    "family",
    "guide",
    "import",
    "index",
    "label",
    "layout",
    "letter",
    "local",
    "manual",
    "market",
    "model",
    "module",
    "network",
    "office",
    "order",
    "output",
    "payment",
    "plan",
    "preview",
    "profile",
    "receipt",
    "release",
    "review",
    "sample",
    "schedule",
    "server",
    "slide",
    "source",
    "statement",
    "studio",
    "survey",
    "table",
    "ticket",
    "travel",
    "update",
    "vendor",
    "video",
    "weekly",
];
const RARE: &[&str] = &["zephyr"];
const ACCENTED: &[&str] = &["relatório", "résumé", "Übersicht", "日本語メモ"];
const PACKAGES: &[&str] = &[
    "lodash",
    "react",
    "typescript",
    "webpack",
    "babel",
    "eslint",
    "chalk",
    "commander",
    "debug",
    "semver",
    "glob",
    "minimatch",
];

const DOCUMENT: &[&str] = &["pdf", "docx", "xlsx", "pptx", "txt", "md", "csv", "pages"];
const CODE: &[&str] = &["rs", "ts", "tsx", "js", "json", "py", "swift", "toml", "md"];
const PACKAGE_FILE: &[&str] = &["js", "d.ts", "json", "map", "md"];
const TEXT: &[&str] = &[
    "txt", "md", "csv", "rs", "ts", "tsx", "js", "json", "py", "swift", "toml",
];

/// How one kind of folder looks: its top folder, how often a new one of it
/// appears, and how many files it holds at most.
#[derive(Clone, Copy)]
enum Shape {
    Documents,
    Downloads,
    Photos,
    Code,
    Packages,
}

impl Shape {
    const ALL: [Shape; 5] = [
        Shape::Documents,
        Shape::Downloads,
        Shape::Photos,
        Shape::Code,
        Shape::Packages,
    ];

    fn top(self) -> &'static str {
        match self {
            Shape::Documents => "Documents",
            Shape::Downloads => "Downloads",
            Shape::Photos => "Pictures",
            Shape::Code => "Projects",
            Shape::Packages => "Projects/app/node_modules",
        }
    }

    fn weight(self) -> usize {
        match self {
            Shape::Documents => 3,
            Shape::Downloads => 1,
            Shape::Photos => 1,
            Shape::Code => 3,
            Shape::Packages => 4,
        }
    }

    fn most_files(self) -> usize {
        match self {
            Shape::Documents => 40,
            Shape::Downloads => 200,
            Shape::Photos => 300,
            Shape::Code => 30,
            Shape::Packages => 15,
        }
    }

    fn folder(self, rng: &mut Rng) -> String {
        match self {
            Shape::Documents | Shape::Downloads => title(&word(rng)),
            Shape::Photos => format!("{}-{:02}", 2015 + rng.below(12), 1 + rng.below(12)),
            Shape::Code => (*rng.pick(&["src", "lib", "tests", "docs", "core", "api"])).to_string(),
            Shape::Packages => (*rng.pick(PACKAGES)).to_string(),
        }
    }

    fn file(self, rng: &mut Rng) -> String {
        match self {
            Shape::Documents | Shape::Downloads => document(rng),
            Shape::Photos => {
                if rng.below(4) == 0 {
                    format!(
                        "Screenshot {}-{:02}-{:02} at {:02}.{:02}.{:02}.png",
                        2015 + rng.below(12),
                        1 + rng.below(12),
                        1 + rng.below(28),
                        rng.below(24),
                        rng.below(60),
                        rng.below(60),
                    )
                } else {
                    format!("IMG_{:04}.HEIC", rng.below(10_000))
                }
            }
            Shape::Code => format!("{}_{}.{}", word(rng), word(rng), rng.pick(CODE)),
            Shape::Packages => format!("{}.{}", word(rng), rng.pick(PACKAGE_FILE)),
        }
    }
}

/// A word, weighted so a query can be common, rare, or accented.
fn word(rng: &mut Rng) -> String {
    let roll = rng.below(20_000);
    let word = match roll {
        0..=9 => rng.pick(RARE),
        10..=50 => rng.pick(ACCENTED),
        51..=6_000 => rng.pick(COMMON),
        _ => rng.pick(WORDS),
    };
    (*word).to_string()
}

fn title(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn document(rng: &mut Rng) -> String {
    let (first, second) = (word(rng), word(rng));
    let first = if rng.below(4) == 0 {
        title(&first)
    } else {
        first
    };
    let ext = rng.pick(DOCUMENT);
    if rng.below(3) == 0 {
        format!("{first}-{second}.{ext}")
    } else {
        format!("{first}-{second}-{}.{ext}", 2015 + rng.below(12))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// From the corpus root, with parents listed before what they hold.
    pub path: String,
    pub is_dir: bool,
    pub size: usize,
}

pub fn plan(seed: u64, entries: usize) -> Vec<Item> {
    let mut rng = Rng::new(seed);
    let mut items = Vec::with_capacity(entries + 400);
    // APFS ignores case here, so two names may not differ only by it.
    let mut taken = HashSet::new();
    let weighted: Vec<Shape> = Shape::ALL
        .into_iter()
        .flat_map(|shape| std::iter::repeat_n(shape, shape.weight()))
        .collect();
    let mut folders: Vec<Vec<String>> = Vec::new();
    for top in ["Projects", "Projects/app"] {
        taken.insert(top.to_lowercase());
        items.push(dir(top));
    }
    for shape in Shape::ALL {
        if taken.insert(shape.top().to_lowercase()) {
            items.push(dir(shape.top()));
        }
        folders.push(vec![shape.top().to_string()]);
    }
    while items.len() < entries {
        let shape = *rng.pick(&weighted);
        let known = &mut folders[shape as usize];
        let parent = rng.pick(known).clone();
        let folder = unique(&parent, &shape.folder(&mut rng), &mut taken);
        items.push(dir(&folder));
        known.push(folder.clone());
        for _ in 0..1 + rng.below(shape.most_files()) {
            let path = unique(&folder, &shape.file(&mut rng), &mut taken);
            let is_text = TEXT.iter().any(|ext| path.ends_with(&format!(".{ext}")));
            let size = if is_text {
                64 + rng.below(4_000)
            } else {
                rng.below(4_096)
            };
            items.push(Item {
                path,
                is_dir: false,
                size,
            });
        }
    }
    items.truncate(entries);
    items
}

fn dir(path: &str) -> Item {
    Item {
        path: path.to_string(),
        is_dir: true,
        size: 0,
    }
}

/// `name` in `parent`, with a number before its extension if that's taken,
/// the way Finder names a copy.
fn unique(parent: &str, name: &str, taken: &mut HashSet<String>) -> String {
    let (stem, ext) = name
        .split_once('.')
        .map_or((name, String::new()), |(stem, ext)| {
            (stem, format!(".{ext}"))
        });
    let mut path = format!("{parent}/{name}");
    let mut n = 2;
    while !taken.insert(path.to_lowercase()) {
        path = format!("{parent}/{stem} {n}{ext}");
        n += 1;
    }
    path
}

/// Writes the corpus to `root/corpus`, its answer key to `root/manifest.tsv`
/// and its seed to `root/seed`. Refuses a `root/corpus` that exists, and removes
/// nothing.
pub fn write(root: &Path, seed: u64, entries: usize) -> io::Result<()> {
    let corpus = root.join("corpus");
    fs::create_dir(&corpus)?;
    fs::write(root.join("seed"), seed.to_string())?;
    let items = plan(seed, entries);
    let mut manifest = io::BufWriter::new(fs::File::create(root.join("manifest.tsv"))?);
    let mut rng = Rng::new(seed ^ 0xC0_4B_05);
    let mut bytes = Vec::new();
    for item in &items {
        let path = corpus.join(&item.path);
        if item.is_dir {
            fs::create_dir(&path)?;
        } else {
            contents(&mut rng, item.size, &mut bytes);
            fs::write(&path, &bytes)?;
        }
        let kind = if item.is_dir { "dir" } else { "file" };
        writeln!(manifest, "{}\t{kind}\t{}", item.path, item.size)?;
    }
    manifest.flush()
}

/// Words, so text files read as text to Spotlight's importers.
fn contents(rng: &mut Rng, size: usize, bytes: &mut Vec<u8>) {
    bytes.clear();
    while bytes.len() < size {
        bytes.extend_from_slice(rng.pick(WORDS).as_bytes());
        bytes.push(b' ');
    }
    bytes.truncate(size);
}

pub fn read_seed(root: &Path) -> io::Result<u64> {
    fs::read_to_string(root.join("seed"))?
        .trim()
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not a number"))
}

pub fn read_manifest(path: &Path) -> io::Result<Vec<Item>> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "a bad manifest line");
    io::BufReader::new(fs::File::open(path)?)
        .lines()
        .map(|line| {
            let line = line?;
            let mut fields = line.split('\t');
            let (Some(path), Some(kind), Some(size)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Err(bad());
            };
            Ok(Item {
                path: path.to_string(),
                is_dir: kind == "dir",
                size: size.parse().map_err(|_| bad())?,
            })
        })
        .collect()
}

/// The form two names compare in: no case, no accents, one normalization.
pub fn fold(name: &str) -> String {
    name.nfd()
        .filter(|c| !is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .collect()
}

/// One normalization for a whole path, since tools print it as the disk has
/// it and Spotlight may decompose it.
pub fn normalize(path: &str) -> String {
    path.nfc().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_fixes_the_corpus_and_another_changes_it() {
        assert_eq!(plan(7, 3_000), plan(7, 3_000));
        assert_ne!(plan(7, 3_000), plan(8, 3_000));
        assert_eq!(plan(7, 3_000).len(), 3_000);
    }

    #[test]
    fn no_two_names_differ_only_by_case_or_lack_a_parent() {
        let items = plan(7, 20_000);
        let mut seen = HashSet::new();
        for item in &items {
            assert!(seen.insert(item.path.to_lowercase()), "{}", item.path);
            if let Some((parent, _)) = item.path.rsplit_once('/') {
                assert!(seen.contains(&parent.to_lowercase()), "{}", item.path);
            }
        }
    }

    #[test]
    fn the_disk_holds_what_the_manifest_says() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), 7, 500).unwrap();
        let items = read_manifest(&root.path().join("manifest.tsv")).unwrap();
        assert_eq!(items, plan(7, 500));
        assert_eq!(read_seed(root.path()).unwrap(), 7);
        for item in &items {
            let meta = fs::metadata(root.path().join("corpus").join(&item.path)).unwrap();
            assert_eq!(meta.is_dir(), item.is_dir, "{}", item.path);
            if !item.is_dir {
                assert_eq!(meta.len() as usize, item.size, "{}", item.path);
            }
        }
        assert!(write(root.path(), 7, 500).is_err());
    }

    #[test]
    fn folding_drops_case_and_accents_in_either_normalization() {
        assert_eq!(fold("Relatório"), "relatorio");
        assert_eq!(fold("Relato\u{301}rio"), "relatorio");
        assert_eq!(normalize("Relato\u{301}rio"), "Relatório");
    }
}
