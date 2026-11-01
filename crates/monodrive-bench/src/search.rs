//! Name search compared across fsearch, Spotlight, `find` and `fd`, on one
//! corpus. Tools run in turns, and a sample counts only once its tool has
//! run on that query, so drift and caches hit them all alike.
//!
//! fsearch runs in this process, as it does in the app, with no `Engine`, so
//! nothing crawls and the live index stays closed. So does Spotlight through
//! `MDQuery`, and that's the pair the verdict compares. `mdfind`, `find` and
//! `fd` are what a person or agent would run, and carry a process's cost.

use crate::conditions::{self, Conditions};
use crate::corpus::{self, Item};
use crate::report;
use crate::spotlight;
use crate::stats::{self, Verdict};
use fsearch::index::Index;
use fsearch::live::Live;
use fsearch::query::Searcher;
use fsearch::walk::{self, KIND_DIR, Listing, RawEnt};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// `SEARCH_LIMIT` in the browser's pane.
const WINDOW_LIMIT: usize = 200;

/// Builds of the index after the first, with the volume's files in memory.
const REBUILDS: usize = 10;

/// One query, in each tool's terms. `wanted` is what a person typing `text`
/// is after, which is the answer key: names holding all of it, without
/// regard to case or accents. `words` is what tools with no typo tolerance
/// are asked for.
#[derive(Clone, Copy)]
pub struct Case {
    pub label: &'static str,
    pub text: &'static str,
    pub words: &'static [&'static str],
    pub wanted: &'static [&'static str],
}

pub const CASES: &[Case] = &[
    Case {
        label: "rare word",
        text: "zephyr",
        words: &["zephyr"],
        wanted: &["zephyr"],
    },
    Case {
        label: "common word",
        text: "report",
        words: &["report"],
        wanted: &["report"],
    },
    Case {
        label: "middle of a word",
        text: "port",
        words: &["port"],
        wanted: &["port"],
    },
    Case {
        label: "two letters",
        text: "re",
        words: &["re"],
        wanted: &["re"],
    },
    Case {
        label: "Japanese",
        text: "日本語",
        words: &["日本語"],
        wanted: &["日本語"],
    },
    Case {
        label: "word and number",
        text: "report 2024",
        words: &["report", "2024"],
        wanted: &["report", "2024"],
    },
    Case {
        label: "name with extension",
        text: "report.pdf",
        words: &["report.pdf"],
        wanted: &["report.pdf"],
    },
    Case {
        label: "typo",
        text: "reprot",
        words: &["reprot"],
        wanted: &["report"],
    },
    Case {
        label: "no accent typed",
        text: "relatorio",
        words: &["relatorio"],
        wanted: &["relatorio"],
    },
    Case {
        label: "no match",
        text: "qqxzzv",
        words: &["qqxzzv"],
        wanted: &["qqxzzv"],
    },
];

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    /// As the window asks: the best `WINDOW_LIMIT`.
    FsearchWindow,
    /// Every match, which is what the other tools return.
    FsearchAll,
    SpotlightApi,
    Mdfind,
    Find,
    Fd,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::FsearchWindow => "fsearch, top 200",
            Tool::FsearchAll => "fsearch, all",
            Tool::SpotlightApi => "Spotlight, MDQuery",
            Tool::Mdfind => "mdfind",
            Tool::Find => "find",
            Tool::Fd => "fd",
        }
    }
}

fn fd_pattern(words: &[&str]) -> String {
    let escaped: Vec<String> = words.iter().map(|w| w.replace('.', r"\.")).collect();
    match escaped.as_slice() {
        [a, b] => format!("{a}.*{b}|{b}.*{a}"),
        _ => escaped.join(""),
    }
}

fn process(tool: Tool, root: &Path, words: &[&str]) -> Command {
    let mut command = match tool {
        Tool::Mdfind => {
            let mut c = Command::new("mdfind");
            c.arg("-onlyin").arg(root).arg(spotlight::predicate(words));
            c
        }
        Tool::Find => {
            let mut c = Command::new("find");
            c.arg(root);
            for word in words {
                c.arg("-iname").arg(format!("*{word}*"));
            }
            c
        }
        Tool::Fd => {
            let mut c = Command::new("fd");
            // Everything, as the others see it, and only the name, as they match.
            c.args(["--unrestricted", "--ignore-case", "--absolute-path"])
                .arg(fd_pattern(words))
                .arg(root);
            c
        }
        Tool::FsearchWindow | Tool::FsearchAll | Tool::SpotlightApi => {
            unreachable!("these run in this process")
        }
    };
    command.stderr(std::process::Stdio::null());
    command
}

fn lines(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| line.trim_end_matches('/').to_string())
        .collect()
}

struct Fsearch {
    live: Live,
    root: String,
    pool: rayon::ThreadPool,
    build_ms: f64,
    bytes: usize,
}

impl Fsearch {
    fn build(root: &Path) -> Result<Self, String> {
        let root_text = root
            .to_str()
            .ok_or_else(|| format!("{}: not UTF-8", root.display()))?
            .to_string();
        let started = Instant::now();
        let index = Index::build(
            listings(root_text.as_bytes()),
            0,
            fsearch::query::now_secs(),
            root_text.as_bytes(),
        );
        let bytes = index.bytes();
        let live = Live::new(index);
        let build_ms = started.elapsed().as_secs_f64() * 1e3;
        // As `Engine`'s search pool.
        let pool = rayon::ThreadPoolBuilder::new()
            .start_handler(|_| {
                // SAFETY: sets this thread's own QoS class.
                unsafe {
                    libc::pthread_set_qos_class_self_np(
                        libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
                        0,
                    );
                }
            })
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            live,
            root: root_text,
            pool,
            build_ms,
            bytes,
        })
    }

    /// Proof the index holds the corpus: its entries, plus the root and the
    /// folders above it, which `listings` adds, and "/", always entry 0.
    fn check_count(&self, root: &Path, entries: usize) -> Result<(), String> {
        let expected = entries + root.ancestors().count();
        let indexed = self.live.base.n;
        if indexed == expected {
            Ok(())
        } else {
            Err(format!("fsearch indexed {indexed} entries, not {expected}"))
        }
    }

    fn search(&self, text: &str, limit: usize) -> Result<Vec<String>, String> {
        let mut query = fsearch::Query::parse(text, &self.root)?;
        query.limit = limit;
        let hits = self
            .pool
            .install(|| Searcher { live: &self.live }.search(&query));
        let mut path = Vec::new();
        Ok(hits
            .iter()
            .map(|hit| {
                self.live.base.path(hit.idx as usize, &mut path);
                String::from_utf8_lossy(&path).into_owned()
            })
            .collect())
    }
}

/// `root`'s tree under a listing for each of its ancestors, since listing 0
/// has to be "/". fsearch's own `snap` example does the same.
fn listings(root: &[u8]) -> Vec<Listing> {
    let mut listed = walk::scan(root, 8);
    let parts: Vec<&[u8]> = root
        .split(|b| *b == b'/')
        .filter(|p| !p.is_empty())
        .collect();
    let depth = parts.len() as u32;
    for listing in &mut listed {
        listing.id += depth;
        for entry in &mut listing.ents {
            if entry.child != walk::NONE {
                entry.child += depth;
            }
        }
    }
    for (i, part) in parts.iter().enumerate() {
        listed.push(Listing {
            id: i as u32,
            names: part.to_vec(),
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
    listed
}

/// How a tool's results compare with the answer key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accuracy {
    pub hits: usize,
    pub expected: usize,
    /// How many of the expected names fit within the tool's limit.
    pub reachable: usize,
    pub found: usize,
    pub missed: usize,
    /// Results that don't hold the words. For a typo-tolerant tool these may
    /// still be useful, so this is not an error count.
    pub extra: usize,
}

pub fn expected_paths(root: &str, items: &[Item], wanted: &[&str]) -> HashSet<String> {
    let wanted: Vec<String> = wanted.iter().map(|w| corpus::fold(w)).collect();
    items
        .iter()
        .filter(|item| {
            let name = item.path.rsplit('/').next().unwrap_or(&item.path);
            let name = corpus::fold(name);
            wanted.iter().all(|word| name.contains(word.as_str()))
        })
        .map(|item| corpus::normalize(&format!("{root}/{}", item.path)))
        .collect()
}

pub fn score(found: &[String], expected: &HashSet<String>, limit: Option<usize>) -> Accuracy {
    let found: HashSet<String> = found.iter().map(|path| corpus::normalize(path)).collect();
    let correct = found.intersection(expected).count();
    let reachable = limit.map_or(expected.len(), |limit| limit.min(expected.len()));
    Accuracy {
        hits: found.len(),
        expected: expected.len(),
        reachable,
        found: correct,
        missed: reachable.saturating_sub(correct),
        extra: found.len() - correct,
    }
}

#[derive(Serialize, Deserialize)]
pub struct ToolResult {
    pub tool: Tool,
    pub name: String,
    /// Round 0 only warms each tool up, and isn't in `samples_ms`.
    pub warmup_ms: f64,
    pub samples_ms: Vec<f64>,
    pub accuracy: Accuracy,
    /// False if the number of results changed between rounds, which would
    /// mean the samples didn't all measure the same work.
    pub stable: bool,
}

#[derive(Serialize, Deserialize)]
pub struct CaseResult {
    pub label: String,
    pub text: String,
    pub tools: Vec<ToolResult>,
    pub fsearch_against_spotlight: Verdict,
}

#[derive(Serialize, Deserialize)]
pub struct Results {
    pub version: u32,
    pub seed: u64,
    pub conditions: Conditions,
    pub noise: Vec<String>,
    pub entries: usize,
    pub spotlight_indexed: usize,
    /// The first build of the run, right after `scripts/bench` remounts the volume.
    pub index_build_ms: f64,
    #[serde(default)]
    pub index_rebuild_ms: Vec<f64>,
    /// From the first file written until Spotlight had every entry, if
    /// `scripts/bench prepare` saw all of it.
    #[serde(default)]
    pub spotlight_index_seconds: Option<f64>,
    pub index_bytes: usize,
    pub process_start_ms: Vec<f64>,
    pub cases: Vec<CaseResult>,
}

fn has_fd() -> bool {
    Command::new("fd").arg("--version").output().is_ok()
}

pub fn spotlight_count(root: &Path) -> Result<usize, String> {
    let out = Command::new("mdfind")
        .arg("-count")
        .arg("-onlyin")
        .arg(root)
        .arg("kMDItemContentTypeTree == \"public.item\"")
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .map_err(|_| "mdfind didn't print a count".to_string())
}

fn time_process(mut command: Command) -> Result<(f64, Vec<String>), String> {
    let started = Instant::now();
    let out = command.output().map_err(|error| error.to_string())?;
    let elapsed = started.elapsed().as_secs_f64() * 1e3;
    Ok((elapsed, lines(&out.stdout)))
}

/// Runs the comparison and writes `out/results.json`. Prints a table too.
pub fn run(volume: &Path, out: &Path, rounds: usize, allow_partial: bool) -> Result<(), String> {
    let root = volume.join("corpus");
    let root_text = root
        .to_str()
        .ok_or_else(|| format!("{}: not UTF-8", root.display()))?
        .to_string();
    let items = corpus::read_manifest(&volume.join("manifest.tsv"))
        .map_err(|error| format!("{}: {error}", volume.join("manifest.tsv").display()))?;
    let seed = corpus::read_seed(volume)
        .map_err(|error| format!("{}: {error}", volume.join("seed").display()))?;
    let spotlight_indexed = spotlight_count(&root)?;
    if spotlight_indexed < items.len() && !allow_partial {
        return Err(format!(
            "Spotlight has indexed {spotlight_indexed} of {} entries. Wait for it, or pass --allow-partial to measure anyway.",
            items.len()
        ));
    }
    let partial = (spotlight_indexed < items.len()).then(|| {
        format!(
            "Spotlight had indexed only {spotlight_indexed} of {} entries",
            items.len()
        )
    });
    let mut conditions = Conditions::now(volume);

    let engine = Fsearch::build(&root)?;
    engine.check_count(&root, items.len())?;
    let index_rebuild_ms = (0..REBUILDS)
        .map(|_| Fsearch::build(&root).map(|again| again.build_ms))
        .collect::<Result<Vec<_>, _>>()?;
    let spotlight_index_seconds = std::fs::read_to_string(volume.join("spotlight-seconds"))
        .ok()
        .and_then(|text| text.trim().parse().ok());

    let mut tools = vec![
        Tool::FsearchWindow,
        Tool::FsearchAll,
        Tool::SpotlightApi,
        Tool::Mdfind,
        Tool::Find,
    ];
    if has_fd() {
        tools.push(Tool::Fd);
    }

    let mut process_start_ms = Vec::with_capacity(rounds);
    let mut cases = Vec::new();
    for case in CASES {
        let expected = expected_paths(&root_text, &items, case.wanted);
        let mut order = tools.clone();
        let mut results: Vec<ToolResult> = Vec::new();
        for round in 0..=rounds {
            for &tool in &order {
                let (elapsed, found) = match tool {
                    Tool::FsearchWindow | Tool::FsearchAll => {
                        let limit = if tool == Tool::FsearchWindow {
                            WINDOW_LIMIT
                        } else {
                            items.len() + 1
                        };
                        let started = Instant::now();
                        let found = engine.search(case.text, limit)?;
                        (started.elapsed().as_secs_f64() * 1e3, found)
                    }
                    Tool::SpotlightApi => {
                        let predicate = spotlight::predicate(case.words);
                        let started = Instant::now();
                        let found = spotlight::search(&root, &predicate)?;
                        (started.elapsed().as_secs_f64() * 1e3, found)
                    }
                    _ => time_process(process(tool, &root, case.words))?,
                };
                let limit = (tool == Tool::FsearchWindow).then_some(WINDOW_LIMIT);
                let accuracy = score(&found, &expected, limit);
                match results.iter_mut().find(|result| result.tool == tool) {
                    Some(result) => {
                        result.samples_ms.push(elapsed);
                        result.stable &= result.accuracy == accuracy;
                    }
                    None => results.push(ToolResult {
                        tool,
                        name: tool.name().to_string(),
                        warmup_ms: elapsed,
                        samples_ms: Vec::with_capacity(rounds),
                        accuracy,
                        stable: true,
                    }),
                }
            }
            order.reverse();
            if round > 0 && cases.is_empty() {
                let (elapsed, _) = time_process(Command::new("/usr/bin/true"))?;
                process_start_ms.push(elapsed);
            }
        }
        let samples = |tool| {
            results
                .iter()
                .find(|result| result.tool == tool)
                .map_or(&[][..], |result| result.samples_ms.as_slice())
        };
        let verdict = stats::compare(samples(Tool::FsearchAll), samples(Tool::SpotlightApi));
        cases.push(CaseResult {
            label: case.label.to_string(),
            text: case.text.to_string(),
            fsearch_against_spotlight: verdict,
            tools: results,
        });
    }
    conditions.load_after = conditions::load();

    let mut noise = conditions.noise();
    noise.extend(partial);
    let results = Results {
        version: 1,
        seed,
        noise,
        conditions,
        entries: items.len(),
        spotlight_indexed,
        index_build_ms: engine.build_ms,
        index_rebuild_ms,
        spotlight_index_seconds,
        index_bytes: engine.bytes,
        process_start_ms,
        cases,
    };
    std::fs::create_dir_all(out).map_err(|error| error.to_string())?;
    let json = serde_json::to_vec_pretty(&results).map_err(|error| error.to_string())?;
    std::fs::write(out.join("results.json"), json).map_err(|error| error.to_string())?;
    print_table(&results);
    let page = report::write(&out.join("results.json"))?;
    println!(
        "\nRaw samples: {}\nReport: {}",
        out.join("results.json").display(),
        page.display()
    );
    Ok(())
}

fn print_table(results: &Results) {
    for reason in &results.noise {
        println!("NOISY RUN: {reason}");
    }
    println!(
        "{} entries, {} in Spotlight. fsearch built its index in {:.0} ms, {:.1} MB in memory. Starting a process takes {:.2} ms, included in mdfind, find and fd.\n",
        results.entries,
        results.spotlight_indexed,
        results.index_build_ms,
        results.index_bytes as f64 / 1e6,
        stats::median(&results.process_start_ms),
    );
    println!(
        "fsearch rebuilt its index in {:.1} ms. Spotlight took {}.\n",
        stats::median(&results.index_rebuild_ms),
        results
            .spotlight_index_seconds
            .map_or("an untimed while".to_string(), |s| format!("{s:.0} s")),
    );
    for case in &results.cases {
        println!("{} ({:?})", case.label, case.text);
        println!(
            "  {:<20}{:>10}{:>10}{:>10}{:>5}{:>9}{:>9}{:>8}{:>7}",
            "", "median ms", "min", "p90", "n", "results", "expected", "missed", "extra"
        );
        for tool in &case.tools {
            let s = &tool.samples_ms;
            let min = s.iter().copied().fold(f64::INFINITY, f64::min);
            println!(
                "  {:<20}{:>10.2}{:>10.2}{:>10.2}{:>5}{:>9}{:>9}{:>8}{:>7}{}",
                tool.name,
                stats::median(s),
                min,
                stats::percentile(s, 0.9),
                s.len(),
                tool.accuracy.hits,
                tool.accuracy.expected,
                tool.accuracy.missed,
                tool.accuracy.extra,
                if tool.stable {
                    ""
                } else {
                    "  results changed between rounds"
                },
            );
        }
        match case.fsearch_against_spotlight {
            Verdict::Faster { low, high } => {
                println!(
                    "  fsearch, all, is {low:.1} to {high:.1} times faster than Spotlight through MDQuery\n"
                );
            }
            Verdict::Slower { low, high } => {
                println!(
                    "  fsearch, all, is {low:.1} to {high:.1} times slower than Spotlight through MDQuery\n"
                );
            }
            Verdict::NoClearDifference => {
                println!("  no clear difference from Spotlight through MDQuery\n")
            }
            Verdict::TooFewSamples => println!("  too few samples to compare\n"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str) -> Item {
        Item {
            path: path.to_string(),
            is_dir: false,
            size: 0,
        }
    }

    #[test]
    fn the_answer_key_reads_names_not_folders_and_ignores_case_and_accents() {
        let items = [
            item("report/notes.txt"),
            item("a/Q3-REPORT.pdf"),
            item("a/relato\u{301}rio.pdf"),
        ];
        let report = expected_paths("/r", &items, &["report"]);
        assert_eq!(report, HashSet::from(["/r/a/Q3-REPORT.pdf".to_string()]));
        assert_eq!(expected_paths("/r", &items, &["relatorio"]).len(), 1);
        assert!(expected_paths("/r", &items, &["report", "2024"]).is_empty());
    }

    #[test]
    fn a_score_counts_misses_against_what_the_limit_allows() {
        let expected: HashSet<String> = ["/a", "/b", "/c"].map(String::from).into();
        let found = ["/a".to_string(), "/x".to_string()];
        let scored = score(&found, &expected, None);
        assert_eq!((scored.missed, scored.extra, scored.hits), (2, 1, 2));
        assert_eq!(score(&found, &expected, Some(1)).missed, 0);
    }

    #[test]
    fn a_score_matches_a_path_in_either_normalization() {
        let expected: HashSet<String> = ["/relatório".to_string()].into();
        let decomposed = ["/relato\u{301}rio".to_string()];
        assert_eq!(score(&decomposed, &expected, None).missed, 0);
    }

    #[test]
    fn every_query_but_no_match_has_answers_in_a_small_corpus() {
        let items = corpus::plan(1, 10_000);
        for case in CASES {
            let expected = expected_paths("/r", &items, case.wanted);
            assert_eq!(
                expected.is_empty(),
                case.label == "no match",
                "{}",
                case.label
            );
        }
    }

    #[test]
    fn fds_two_word_pattern_takes_either_order() {
        assert_eq!(fd_pattern(&["a.b", "2"]), r"a\.b.*2|2.*a\.b");
        assert_eq!(fd_pattern(&["a.b"]), r"a\.b");
    }

    #[test]
    fn fsearch_finds_the_corpus_it_was_built_on() {
        let dir = tempfile::tempdir().unwrap();
        let volume = dir.path().canonicalize().unwrap();
        corpus::write(&volume, 7, 2_000).unwrap();
        let items = corpus::read_manifest(&volume.join("manifest.tsv")).unwrap();
        let root = volume.join("corpus");
        let engine = Fsearch::build(&root).unwrap();
        engine.check_count(&root, items.len()).unwrap();
        assert!(engine.check_count(&root, items.len() + 1).is_err());
        let root_text = root.to_str().unwrap();
        let expected = expected_paths(root_text, &items, &["report"]);
        assert!(!expected.is_empty());
        let found = engine.search("report", items.len() + 1).unwrap();
        assert_eq!(score(&found, &expected, None).missed, 0);
    }
}
