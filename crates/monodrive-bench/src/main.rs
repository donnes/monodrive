//! Benchmarks that measure Monodrive against what people use instead. Run it
//! through `scripts/bench`, which makes the volume it needs.

mod conditions;
mod corpus;
mod report;
mod rng;
mod search;
mod spotlight;
mod stats;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:
  monodrive-bench corpus <volume> --entries N --seed N
  monodrive-bench spotlight-count <volume>
  monodrive-bench search <volume> --out <folder> [--rounds N] [--allow-partial]
  monodrive-bench report <results.json>";

enum Task {
    Corpus {
        volume: PathBuf,
        entries: usize,
        seed: u64,
    },
    SpotlightCount {
        volume: PathBuf,
    },
    Search {
        volume: PathBuf,
        out: PathBuf,
        rounds: usize,
        allow_partial: bool,
    },
    Report {
        results: PathBuf,
    },
}

fn number<T: std::str::FromStr>(flag: &str, value: Option<String>) -> Result<T, String> {
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| format!("{flag} needs a number"))
}

fn decode(mut args: impl Iterator<Item = String>) -> Result<Task, String> {
    let task = args.next().ok_or("no task")?;
    let path = PathBuf::from(args.next().ok_or("no path")?);
    let mut entries = None;
    let mut seed = None;
    let mut out = None;
    let mut rounds = 30;
    let mut allow_partial = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--entries" => entries = Some(number(&flag, args.next())?),
            "--seed" => seed = Some(number(&flag, args.next())?),
            "--rounds" => rounds = number(&flag, args.next())?,
            "--out" => out = args.next().map(PathBuf::from),
            "--allow-partial" => allow_partial = true,
            other => return Err(format!("{other}: unknown flag")),
        }
    }
    match task.as_str() {
        "corpus" => Ok(Task::Corpus {
            volume: path,
            entries: entries.ok_or("--entries is required")?,
            seed: seed.ok_or("--seed is required")?,
        }),
        "spotlight-count" => Ok(Task::SpotlightCount { volume: path }),
        "report" => Ok(Task::Report { results: path }),
        "search" => Ok(Task::Search {
            volume: path,
            out: out.ok_or("--out is required")?,
            rounds,
            allow_partial,
        }),
        other => Err(format!("{other}: unknown task")),
    }
}

fn run(task: Task) -> Result<(), String> {
    match task {
        Task::Corpus {
            volume,
            entries,
            seed,
        } => corpus::write(&volume, seed, entries).map_err(|error| error.to_string()),
        Task::SpotlightCount { volume } => {
            println!("{}", search::spotlight_count(&volume.join("corpus"))?);
            Ok(())
        }
        Task::Search {
            volume,
            out,
            rounds,
            allow_partial,
        } => search::run(&volume, &out, rounds, allow_partial),
        Task::Report { results } => {
            println!("{}", report::write(&results)?.display());
            Ok(())
        }
    }
}

fn main() -> ExitCode {
    match decode(std::env::args().skip(1)).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}
