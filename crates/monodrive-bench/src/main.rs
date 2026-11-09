//! Monodrive's benchmarks. Run them through `scripts/bench`, which builds in
//! release, makes the worktree `--against` needs and attaches what a suite
//! needs. See `docs/performance.md`.

// What the macOS-only suites share with the platform suite goes unused in a
// Linux build.
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_variables))]

mod budgets;
mod conditions;
#[cfg(target_os = "macos")]
mod corpus;
mod format;
mod measure;
mod report;
mod rng;
mod runner;
#[cfg(target_os = "macos")]
mod spotlight;
mod stats;
mod suites;
mod tree;

use std::path::PathBuf;
use std::process::ExitCode;
use suites::Suite;

const USAGE: &str = "usage:
  monodrive-bench run [suite...] --out <folder> [--rounds N] [--seed N] [--tree N] [--against <tool> --against-commit <commit>]
  monodrive-bench run-one --suite <suite> --fixture <folder>
  monodrive-bench report <results.json> [saved results.json]
  monodrive-bench merge <results.json> <more results.json>
  monodrive-bench shape <folder> <source>
  monodrive-bench search-real <copy of index.bin> --out <folder> [--rounds N]
  monodrive-bench gate <results.json>
  monodrive-bench counters
  monodrive-bench corpus <volume> --entries N --seed N
  monodrive-bench spotlight-count <volume>
  monodrive-bench spotlight <volume> --out <folder> [--rounds N] [--allow-partial]";

enum Task {
    Run(runner::Plan),
    RunOne {
        suite: Suite,
        fixture: PathBuf,
    },
    Report {
        results: PathBuf,
        saved: Option<PathBuf>,
    },
    Merge {
        into: PathBuf,
        from: PathBuf,
    },
    Shape {
        folder: PathBuf,
        source: String,
    },
    Counters,
    Gate {
        results: PathBuf,
    },
    #[cfg(target_os = "macos")]
    SearchReal {
        index: PathBuf,
        out: PathBuf,
        rounds: usize,
    },
    #[cfg(target_os = "macos")]
    Corpus {
        volume: PathBuf,
        entries: usize,
        seed: u64,
    },
    #[cfg(target_os = "macos")]
    SpotlightCount {
        volume: PathBuf,
    },
    #[cfg(target_os = "macos")]
    Spotlight {
        volume: PathBuf,
        out: PathBuf,
        rounds: usize,
        allow_partial: bool,
    },
}

fn number<T: std::str::FromStr>(flag: &str, value: Option<String>) -> Result<T, String> {
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("{flag} needs a number"))
}

/// The words before the first flag, and the flags.
struct Args {
    words: Vec<String>,
    out: Option<PathBuf>,
    rounds: Option<usize>,
    seed: Option<u64>,
    entries: Option<usize>,
    tree: Option<usize>,
    suite: Option<String>,
    fixture: Option<PathBuf>,
    against: Option<PathBuf>,
    against_commit: Option<String>,
    allow_partial: bool,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = args.peekable();
    let mut parsed = Args {
        words: Vec::new(),
        out: None,
        rounds: None,
        seed: None,
        entries: None,
        tree: None,
        suite: None,
        fixture: None,
        against: None,
        against_commit: None,
        allow_partial: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => parsed.out = args.next().map(PathBuf::from),
            "--rounds" => parsed.rounds = Some(number(&arg, args.next())?),
            "--seed" => parsed.seed = Some(number(&arg, args.next())?),
            "--entries" => parsed.entries = Some(number(&arg, args.next())?),
            "--tree" => parsed.tree = Some(number(&arg, args.next())?),
            "--suite" => parsed.suite = args.next(),
            "--fixture" => parsed.fixture = args.next().map(PathBuf::from),
            "--against" => parsed.against = args.next().map(PathBuf::from),
            "--against-commit" => parsed.against_commit = args.next(),
            "--allow-partial" => parsed.allow_partial = true,
            flag if flag.starts_with("--") => return Err(format!("{flag}: unknown flag")),
            _ => parsed.words.push(arg),
        }
    }
    Ok(parsed)
}

fn decode(args: impl Iterator<Item = String>) -> Result<Task, String> {
    let mut args = parse(args)?;
    if args.words.is_empty() {
        return Err("no task".into());
    }
    let task = args.words.remove(0);
    let path = |words: &[String], at: usize| -> Result<PathBuf, String> {
        words
            .get(at)
            .map(PathBuf::from)
            .ok_or_else(|| format!("{task} needs a path"))
    };
    match task.as_str() {
        "run" => {
            let suites = if args.words.is_empty() {
                Suite::defaults()
            } else {
                args.words
                    .iter()
                    .map(|name| Suite::parse(name))
                    .collect::<Result<_, _>>()?
            };
            let against = match (args.against, args.against_commit) {
                (Some(tool), Some(commit)) => Some((tool, commit)),
                (None, None) => None,
                _ => return Err("--against and --against-commit go together".into()),
            };
            Ok(Task::Run(runner::Plan {
                suites,
                rounds: args.rounds.unwrap_or(20),
                fixtures: suites::Fixtures {
                    seed: args.seed.unwrap_or(1),
                    tree: args.tree.unwrap_or(20_000),
                },
                out: args.out.ok_or("--out is required")?,
                against,
            }))
        }
        "run-one" => Ok(Task::RunOne {
            suite: Suite::parse(&args.suite.ok_or("--suite is required")?)?,
            fixture: args.fixture.ok_or("--fixture is required")?,
        }),
        "report" => Ok(Task::Report {
            results: path(&args.words, 0)?,
            saved: args.words.get(1).map(PathBuf::from),
        }),
        "merge" => Ok(Task::Merge {
            into: path(&args.words, 0)?,
            from: path(&args.words, 1)?,
        }),
        "shape" => Ok(Task::Shape {
            folder: path(&args.words, 0)?,
            source: args.words.get(1).cloned().ok_or("shape needs a source")?,
        }),
        "counters" => Ok(Task::Counters),
        "gate" => Ok(Task::Gate {
            results: path(&args.words, 0)?,
        }),
        #[cfg(target_os = "macos")]
        "search-real" => Ok(Task::SearchReal {
            index: path(&args.words, 0)?,
            out: args.out.ok_or("--out is required")?,
            rounds: args.rounds.unwrap_or(20),
        }),
        #[cfg(target_os = "macos")]
        "corpus" => Ok(Task::Corpus {
            volume: path(&args.words, 0)?,
            entries: args.entries.ok_or("--entries is required")?,
            seed: args.seed.ok_or("--seed is required")?,
        }),
        #[cfg(target_os = "macos")]
        "spotlight-count" => Ok(Task::SpotlightCount {
            volume: path(&args.words, 0)?,
        }),
        #[cfg(target_os = "macos")]
        "spotlight" => Ok(Task::Spotlight {
            volume: path(&args.words, 0)?,
            out: args.out.ok_or("--out is required")?,
            rounds: args.rounds.unwrap_or(30),
            allow_partial: args.allow_partial,
        }),
        other => Err(format!("{other}: unknown task")),
    }
}

/// `results.json`'s version, to tell the Spotlight comparison's from the suites'.
fn version(path: &std::path::Path) -> Result<u32, String> {
    #[derive(serde::Deserialize)]
    struct Versioned {
        version: u32,
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice::<Versioned>(&bytes)
        .map(|v| v.version)
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn run(task: Task) -> Result<(), String> {
    match task {
        Task::Run(plan) => {
            let results = runner::run(&plan)?;
            let page = report::write(&results, None)?;
            println!(
                "Raw samples: {}\nReport: {}",
                results.display(),
                page.display()
            );
            Ok(())
        }
        Task::RunOne { suite, fixture } => runner::run_one(suite, &fixture),
        Task::Report { results, saved } => {
            let page = match version(&results)? {
                format::RESULTS => report::write(&results, saved.as_deref())?,
                #[cfg(target_os = "macos")]
                1 => spotlight::report::write(&results)?,
                other => return Err(format!("{}: unknown version {other}", results.display())),
            };
            println!("{}", page.display());
            Ok(())
        }
        Task::Merge { into, from } => {
            let read = |path: &PathBuf| -> Result<format::Results, String> {
                let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
                serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
            };
            let (mut merged, more) = (read(&into)?, read(&from)?);
            merged.suites.extend(more.suites);
            let json = serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?;
            std::fs::write(&into, json).map_err(|e| format!("{}: {e}", into.display()))?;
            println!("{}", report::write(&into, None)?.display());
            Ok(())
        }
        Task::Shape { folder, source } => {
            let shape = tree::measure(&folder, &source)
                .map_err(|e| format!("{}: {e}", folder.display()))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&shape).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        Task::Gate { results } => {
            let bytes =
                std::fs::read(&results).map_err(|e| format!("{}: {e}", results.display()))?;
            let results: format::Results = serde_json::from_slice(&bytes)
                .map_err(|e| format!("{}: {e}", results.display()))?;
            let slower = report::slower_instructions(&results);
            for line in &slower {
                println!("{line}");
            }
            if slower.is_empty() {
                println!("No result takes more instructions than the baseline's.");
                Ok(())
            } else {
                Err(format!(
                    "{} results take more instructions than the baseline's",
                    slower.len()
                ))
            }
        }
        Task::Counters => {
            // Whether this machine's counters work: CI's virtual Macs may read 0.
            let (sample, sum) =
                measure::here(|| (0..50_000_000u64).fold(0u64, |a, b| a ^ b.wrapping_mul(31)));
            println!(
                "{} instructions, {} cycles, {} ms CPU, {:.1} ms wall (sum {sum})",
                sample.instructions.map_or("no".into(), |n| n.to_string()),
                sample.cycles.map_or("no".into(), |n| n.to_string()),
                sample.cpu_ms.map_or("no".into(), |n| format!("{n:.1}")),
                sample.wall_ms,
            );
            Ok(())
        }
        #[cfg(target_os = "macos")]
        Task::SearchReal { index, out, rounds } => {
            let run = suites::search::real(&index, rounds)?;
            let results = format::Results {
                version: format::RESULTS,
                started: String::new(),
                builds: vec![format::Build {
                    label: "this".into(),
                    commit: String::new(),
                }],
                suites: vec![run],
            };
            std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
            let path = out.join("results.json");
            let json = serde_json::to_vec_pretty(&results).map_err(|e| e.to_string())?;
            std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("{}", report::write(&path, None)?.display());
            Ok(())
        }
        #[cfg(target_os = "macos")]
        Task::Corpus {
            volume,
            entries,
            seed,
        } => corpus::write(&volume, seed, entries).map_err(|e| e.to_string()),
        #[cfg(target_os = "macos")]
        Task::SpotlightCount { volume } => {
            println!("{}", spotlight::spotlight_count(&volume.join("corpus"))?);
            Ok(())
        }
        #[cfg(target_os = "macos")]
        Task::Spotlight {
            volume,
            out,
            rounds,
            allow_partial,
        } => spotlight::run(&volume, &out, rounds, allow_partial),
    }
}

fn main() -> ExitCode {
    let task = match decode(std::env::args().skip(1)) {
        Ok(task) => task,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match run(task) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("monodrive-bench: {error}");
            ExitCode::FAILURE
        }
    }
}
