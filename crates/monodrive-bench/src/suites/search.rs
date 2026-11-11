//! Name search on a generated index of 3 million entries. Nothing crawls and
//! the live index stays closed: `prepare` builds the index from listings made
//! in memory, as fsearch's own `snap` example does, and saves it in the
//! fixture. The names come from `corpus`, the Spotlight comparison's model of
//! real folders.
//!
//! Two levels. `Index::load` and a `Searcher` time the matching alone. An
//! `Engine` on a copy of the same file adds what the app goes through, with
//! `HOME` pointed at an empty folder so its content index has nothing to read.
//! It still follows FSEvents for the whole disk and lists the folders that
//! change into its overlay, so its rows depend on what else is writing to the
//! disk, and each one's check says how much it applied.
//! fsearch's internals stay in this file; an fsearch bump can break it, and
//! the entry-count checks are what catch that.

use crate::corpus;
use crate::format::{Cache, Measured};
use crate::measure::{self, Sample};
use crate::spotlight::CASES;
use fsearch::index::Index;
use fsearch::live::Live;
use fsearch::query::Searcher;
use fsearch::walk::{KIND_DIR, KIND_FILE, Listing, RawEnt};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const ENTRIES: usize = 3_000_000;
/// Where the generated names claim to be. Nothing is there, so no disk event
/// ever touches them.
pub const ROOT: &str = "/MonodriveBench";
/// `SEARCH_LIMIT` in the browser's pane.
const LIMIT: usize = 200;

pub fn index_dir(fixture: &Path) -> PathBuf {
    fixture.join("index")
}

/// Entries the index should hold: the corpus, its root, and "/".
pub fn expected_entries(fixture: &Path) -> Result<usize, String> {
    let path = index_dir(fixture).join("entries");
    std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .trim()
        .parse()
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `items` as fsearch's listings: one per folder, with "/" as listing 0 and
/// `ROOT` as listing 1.
pub fn listings(items: &[corpus::Item]) -> Vec<Listing> {
    let mut listings = vec![
        Listing {
            id: 0,
            names: ROOT.as_bytes()[1..].to_vec(),
            ents: vec![RawEnt {
                name_off: 0,
                name_len: (ROOT.len() - 1) as u16,
                kind: KIND_DIR,
                size: 0,
                mtime: 0,
                child: 1,
            }],
        },
        Listing {
            id: 1,
            names: Vec::new(),
            ents: Vec::new(),
        },
    ];
    let mut ids: HashMap<&str, usize> = HashMap::from([("", 1)]);
    for (i, item) in items.iter().enumerate() {
        let (parent, name) = item.path.rsplit_once('/').unwrap_or(("", &item.path));
        let child = if item.is_dir {
            let id = listings.len();
            ids.insert(&item.path, id);
            listings.push(Listing {
                id: id as u32,
                names: Vec::new(),
                ents: Vec::new(),
            });
            id as u32
        } else {
            fsearch::walk::NONE
        };
        let listing = &mut listings[ids[parent]];
        listing.ents.push(RawEnt {
            name_off: listing.names.len() as u32,
            name_len: name.len() as u16,
            kind: if item.is_dir { KIND_DIR } else { KIND_FILE },
            size: item.size as u64,
            // Spread over 2023 and 2024, so recency ranks differ.
            mtime: 1_672_531_200 + (i as u32).wrapping_mul(2_654_435_761) % 63_072_000,
            child,
        });
        listing.names.extend_from_slice(name.as_bytes());
    }
    listings
}

/// `generated` with the real folder `real` beside it, under its real path:
/// the folders above it are listed from "/", as fsearch's `snap` example does.
fn with_real(generated: Vec<Listing>, real: &Path) -> Result<Vec<Listing>, String> {
    let real_text = real.to_str().ok_or("the fixture's path isn't UTF-8")?;
    let mut listed = fsearch::walk::scan(real_text.as_bytes(), 8);
    let parts: Vec<&str> = real_text.split('/').filter(|p| !p.is_empty()).collect();
    let depth = parts.len() as u32;
    for listing in &mut listed {
        listing.id += depth;
        for entry in &mut listing.ents {
            if entry.child != fsearch::walk::NONE {
                entry.child += depth;
            }
        }
    }
    for (i, part) in parts.iter().enumerate() {
        listed.push(Listing {
            id: i as u32,
            names: part.as_bytes().to_vec(),
            ents: vec![RawEnt {
                name_off: 0,
                name_len: part.len() as u16,
                kind: KIND_DIR,
                size: 0,
                mtime: 0,
                child: i as u32 + 1,
            }],
        });
    }
    // The generated listings follow, and their "/" joins the real one.
    let offset = listed.len() as u32 - 1;
    let mut generated = generated.into_iter();
    let root = generated.next().ok_or("no generated listings")?;
    let slash = listed
        .iter_mut()
        .find(|l| l.id == 0)
        .ok_or("no / listing")?;
    for mut entry in root.ents {
        entry.name_off += slash.names.len() as u32;
        entry.child += offset;
        slash.ents.push(entry);
    }
    slash.names.extend_from_slice(&root.names);
    for mut listing in generated {
        listing.id += offset;
        for entry in &mut listing.ents {
            if entry.child != fsearch::walk::NONE {
                entry.child += offset;
            }
        }
        listed.push(listing);
    }
    Ok(listed)
}

pub fn prepare(fixture: &Path, seed: u64) -> Result<(), String> {
    prepare_with(fixture, seed, None)
}

/// Builds the index, with the real folder `real` in it too when given.
pub fn prepare_with(fixture: &Path, seed: u64, real: Option<&Path>) -> Result<(), String> {
    let dir = index_dir(fixture);
    std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let items = corpus::plan(seed, ENTRIES);
    let mut listed = listings(&items);
    if let Some(real) = real {
        listed = with_real(listed, real)?;
    }
    // Every entry of every listing, and "/".
    let expected = listed.iter().map(|l| l.ents.len()).sum::<usize>() + 1;
    let index = Index::build(listed, 0, fsearch::query::now_secs(), b"/nonexistent-home");
    if index.n != expected {
        return Err(format!("fsearch built {} entries, not {expected}", index.n));
    }
    index
        .save(&dir.join("index.bin"))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    // An Engine rescans the consent-gated folders when this is missing.
    std::fs::write(dir.join("skipped"), "").map_err(|e| e.to_string())?;
    std::fs::write(dir.join("entries"), expected.to_string()).map_err(|e| e.to_string())
}

fn measured(bench: String, title: String, group: &str, sample: Sample, check: String) -> Measured {
    Measured {
        bench,
        title,
        group: group.to_string(),
        variant: String::new(),
        items: None,
        cache: Cache::Warm,
        sample,
        check,
        peak_bytes: None,
    }
}

fn query(text: &str) -> Result<fsearch::Query, String> {
    let mut query = fsearch::Query::parse(text, ROOT)?;
    query.limit = LIMIT;
    Ok(query)
}

fn searcher(fixture: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    let path = index_dir(fixture).join("index.bin");
    let (sample, index) = measure::here(|| Index::load(&path));
    let index = index.ok_or_else(|| format!("{}: fsearch couldn't load it", path.display()))?;
    let expected = expected_entries(fixture)?;
    if index.n != expected {
        return Err(format!(
            "{}: {} entries, not {expected}",
            path.display(),
            index.n
        ));
    }
    out.push(measured(
        "index/load".into(),
        "Load index.bin".into(),
        "Open the index",
        sample,
        format!("{} entries", index.n),
    ));
    let live = Live::new(index);
    // As `Engine`'s search pool.
    let pool = rayon::ThreadPoolBuilder::new()
        .start_handler(|_| unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
        })
        .build()
        .map_err(|e| e.to_string())?;
    for case in CASES {
        let query = query(case.text)?;
        // Untimed, so the pages it reads are in memory for every round alike.
        pool.install(|| Searcher { live: &live }.search(&query));
        let (sample, hits) =
            measure::here(|| pool.install(|| Searcher { live: &live }.search(&query)));
        out.push(measured(
            format!("searcher/{}", case.label.replace(' ', "-")),
            format!("{} ({})", case.label, case.text),
            "Match names, Searcher",
            sample,
            format!("{} results", hits.len()),
        ));
    }
    Ok(())
}

fn engine(fixture: &Path, out: &mut Vec<Measured>) -> Result<(), String> {
    let scratch = fixture.join(format!("engine-{}", std::process::id()));
    let (dir, home) = (scratch.join("index"), scratch.join("home"));
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    // A clone, since an Engine saves over the index it owns.
    let status = std::process::Command::new("cp")
        .args(["-c", "-R"])
        .arg(index_dir(fixture))
        .arg(&dir)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cp -c {}: {status}", dir.display()));
    }
    let expected = expected_entries(fixture)?;
    let result = (|| {
        // An Engine with no index it can load crawls the whole disk.
        if Index::load(&dir.join("index.bin")).is_none_or(|index| index.n != expected) {
            return Err(format!("{}: not the generated index", dir.display()));
        }
        let home_text = home.to_str().ok_or("home isn't UTF-8")?.to_string();
        let started = std::time::Instant::now();
        let engine = fsearch::Engine::start(fsearch::Options {
            dir: dir.clone(),
            home: home_text,
            skip: Some(Vec::new()),
        })?;
        let first = query(CASES[0].text)?;
        let (mut sample, hits) = measure::here(|| engine.search(&first));
        sample.wall_ms = started.elapsed().as_secs_f64() * 1e3;
        let from = out.len();
        out.push(measured(
            "engine/first".into(),
            format!("Start an Engine and answer \"{}\"", CASES[0].text),
            "Start the index",
            sample,
            format!("{} results", hits?.len()),
        ));
        for case in CASES {
            let query = query(case.text)?;
            engine.search(&query)?;
            let (sample, hits) = measure::here(|| engine.search(&query));
            out.push(measured(
                format!("engine/{}", case.label.replace(' ', "-")),
                format!("{} ({})", case.label, case.text),
                "Match names, Engine",
                sample,
                format!("{} results", hits?.len()),
            ));
        }
        let status = engine.status();
        if status.entries != expected {
            return Err(format!(
                "the Engine's index holds {} entries, not the {expected} generated: it rebuilt or crawled",
                status.entries
            ));
        }
        for row in &mut out[from..] {
            row.check = format!(
                "{}, {} entries from disk changes applied",
                row.check, status.overlay
            );
        }
        Ok(())
    })();
    crate::suites::delete_fixture(&scratch)?;
    result
}

/// `scripts/bench search-real`: the queries on a copy of the real index, through
/// `Index::load` and a `Searcher` only, so no `Engine`, watcher or lock. It
/// keeps times and hit counts, never paths, and runs only when asked.
pub fn real(index: &Path, rounds: usize) -> Result<crate::format::SuiteRun, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let mut run = crate::format::SuiteRun {
        suite: "search-real".into(),
        conditions: crate::conditions::Conditions::now(index),
        rounds,
        notes: Vec::new(),
        benches: Vec::new(),
    };
    let (sample, loaded) = measure::here(|| Index::load(index));
    let loaded = loaded.ok_or_else(|| format!("{}: fsearch couldn't load it", index.display()))?;
    let entries = loaded.n;
    let live = Live::new(loaded);
    let load = measured(
        "index/load".into(),
        "Load index.bin".into(),
        "Open the index",
        sample,
        format!("{} entries", crate::report::thousands(entries)),
    );
    let round = |measured| crate::format::Round {
        version: crate::format::ROUND,
        measured,
    };
    run.add("this", round(vec![load]), false);
    // Round 0 warms up and is thrown away, as in every suite.
    for each in 0..=rounds {
        let mut measured_now = Vec::new();
        for case in CASES {
            let mut query = fsearch::Query::parse(case.text, &home)?;
            query.limit = LIMIT;
            let (sample, hits) = measure::here(|| Searcher { live: &live }.search(&query));
            measured_now.push(measured(
                format!("searcher/{}", case.label.replace(' ', "-")),
                format!("{} ({})", case.label, case.text),
                "Match names on this Mac's index, Searcher",
                sample,
                format!("{} results", hits.len()),
            ));
        }
        run.add("this", round(measured_now), each == 0);
    }
    run.conditions.load_after = crate::conditions::load();
    Ok(run)
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let mut out = Vec::new();
    searcher(fixture, &mut out)?;
    engine(fixture, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_index_holds_every_item_and_finds_them_by_name() {
        let items = corpus::plan(5, 5_000);
        let index = Index::build(
            listings(&items),
            0,
            fsearch::query::now_secs(),
            b"/nonexistent-home",
        );
        assert_eq!(index.n, items.len() + 2);
        let live = Live::new(index);
        let mut query = fsearch::Query::parse("report", ROOT).unwrap();
        query.limit = items.len();
        let hits = Searcher { live: &live }.search(&query);
        let mut path = Vec::new();
        let found: Vec<String> = hits
            .iter()
            .map(|hit| {
                live.base.path(hit.idx as usize, &mut path);
                String::from_utf8_lossy(&path).into_owned()
            })
            .collect();
        let expected = crate::spotlight::expected_paths(ROOT, &items, &["report"]);
        assert!(!expected.is_empty());
        assert_eq!(crate::spotlight::score(&found, &expected, None).missed, 0);
    }

    fn found(live: &Live, text: &str) -> Vec<String> {
        let mut query = fsearch::Query::parse(text, ROOT).unwrap();
        query.limit = 50;
        let mut path = Vec::new();
        Searcher { live }
            .search(&query)
            .iter()
            .map(|hit| {
                live.base.path(hit.idx as usize, &mut path);
                String::from_utf8_lossy(&path).into_owned()
            })
            .collect()
    }

    #[test]
    fn a_real_folder_joins_the_generated_names_under_its_own_path() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("qqneedle.rs"), "x").unwrap();
        let items = corpus::plan(5, 1_000);
        let listed = with_real(listings(&items), &real).unwrap();
        let expected = listed.iter().map(|l| l.ents.len()).sum::<usize>() + 1;
        let index = Index::build(listed, 0, fsearch::query::now_secs(), b"/nonexistent-home");
        assert_eq!(index.n, expected);
        let live = Live::new(index);
        let needle = real.join("qqneedle.rs").to_string_lossy().into_owned();
        assert_eq!(found(&live, "qqneedle"), [needle]);
        assert!(found(&live, "report").iter().all(|p| p.starts_with(ROOT)));
        assert!(!found(&live, "report").is_empty());
    }
}
