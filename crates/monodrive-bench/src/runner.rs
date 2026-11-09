//! Runs suites in rounds. Each round of each build is its own `run-one`
//! process, so a build from another commit runs its own code on the same
//! fixtures. Builds take turns, A B B A, so drift hits both, and round 0 of
//! each is thrown away so both start warm. Samples from different runs are
//! never compared.

use crate::conditions::{self, Conditions};
use crate::format::{self, Build, Results, Round, SuiteRun};
use crate::suites::{self, Suite};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Plan {
    pub suites: Vec<Suite>,
    pub rounds: usize,
    pub fixtures: suites::Fixtures,
    pub out: PathBuf,
    /// Another commit's build of this tool, and that commit.
    pub against: Option<(PathBuf, String)>,
}

struct Runner {
    build: Build,
    binary: PathBuf,
}

fn commit_here() -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let commit = git(&["rev-parse", "--short", "HEAD"]);
    if git(&["status", "--porcelain", "--untracked-files=no"]).is_empty() {
        commit
    } else {
        format!("{commit} with uncommitted changes")
    }
}

/// The order builds run in, round by round: A B, B A, A B, ...
pub fn turn(round: usize, builds: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..builds).collect();
    if round % 2 == 1 {
        order.reverse();
    }
    order
}

/// One round, by `binary`, read back from what it prints.
fn spawn_round(binary: &Path, suite: Suite, fixture: &Path) -> Result<Round, String> {
    let out = Command::new(binary)
        .args(["run-one", "--suite", suite.name(), "--fixture"])
        .arg(fixture)
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{} run-one --suite {}: {}",
            binary.display(),
            suite.name(),
            out.status
        ));
    }
    let round: Round = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{} printed no round: {e}", binary.display()))?;
    if round.version != format::ROUND {
        return Err(format!(
            "{} prints rounds in version {}, and this tool reads {}",
            binary.display(),
            round.version,
            format::ROUND
        ));
    }
    Ok(round)
}

fn suite(plan: &Plan, out: &Path, runners: &[Runner], suite: Suite) -> Result<SuiteRun, String> {
    // Absolute, as the owner wants its paths.
    let fixture = out.join("fixtures").join(suite.name());
    std::fs::create_dir_all(&fixture).map_err(|e| format!("{}: {e}", fixture.display()))?;
    eprintln!("{}: making fixtures", suite.name());
    let measured = suite.prepare(&fixture, &plan.fixtures).and_then(|()| {
        let mut run = SuiteRun {
            suite: suite.name().to_string(),
            conditions: Conditions::now(&fixture),
            rounds: plan.rounds,
            notes: Vec::new(),
            benches: Vec::new(),
        };
        let mut taking_part: Vec<&Runner> = runners.iter().collect();
        for round in 0..=plan.rounds {
            for index in turn(round, taking_part.len()) {
                let runner = taking_part[index];
                match spawn_round(&runner.binary, suite, &fixture) {
                    Ok(measured) => run.add(&runner.build.label, measured, round == 0),
                    // A baseline from before a suite existed can't run it.
                    Err(error) if round == 0 && runner.build.label != "this" => {
                        run.notes.push(format!(
                            "{} ({}) didn't run: {error}",
                            runner.build.label, runner.build.commit
                        ));
                        taking_part.retain(|r| r.build.label == "this");
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }
            eprintln!(
                "{}: round {round} of {}{}, load {:.1}",
                suite.name(),
                plan.rounds,
                if round == 0 {
                    " (warm-up, thrown away)"
                } else {
                    ""
                },
                conditions::load()
            );
        }
        run.conditions.load_after = conditions::load();
        Ok(run)
    });
    let finished = suite.finish(&fixture);
    // Deleting would reach into an image that's still attached.
    let deleted = finished
        .clone()
        .and_then(|()| suites::delete_fixture(&fixture));
    match (measured, finished, deleted) {
        (Ok(run), Ok(()), Ok(())) => Ok(run),
        (measured, finished, deleted) => Err([measured.err(), finished.err(), deleted.err()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; ")),
    }
}

/// Runs `plan` and writes `results.json`, returning its path.
pub fn run(plan: &Plan) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&plan.out).map_err(|e| format!("{}: {e}", plan.out.display()))?;
    let out =
        std::fs::canonicalize(&plan.out).map_err(|e| format!("{}: {e}", plan.out.display()))?;
    let mut runners = vec![Runner {
        build: Build {
            label: "this".into(),
            commit: commit_here(),
        },
        binary: std::env::current_exe().map_err(|e| e.to_string())?,
    }];
    if let Some((binary, commit)) = &plan.against {
        runners.push(Runner {
            build: Build {
                label: "baseline".into(),
                commit: commit.clone(),
            },
            binary: binary.clone(),
        });
    }
    let started = Command::new("date")
        .args(["-u", "+%Y-%m-%d %H:%M UTC"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    let mut results = Results {
        version: format::RESULTS,
        started,
        builds: runners.iter().map(|r| r.build.clone()).collect(),
        suites: Vec::new(),
    };
    let path = out.join("results.json");
    for &each in &plan.suites {
        results.suites.push(suite(plan, &out, &runners, each)?);
        // Written after each suite, so a later failure keeps what finished.
        let json = serde_json::to_vec_pretty(&results).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    // Each suite deleted its own fixtures; only their empty parent is left.
    suites::delete_fixture(&out.join("fixtures"))?;
    Ok(path)
}

/// `run-one`: measures one round and prints it as JSON.
pub fn run_one(suite: Suite, fixture: &Path) -> Result<(), String> {
    // A pinned QoS class, so the scheduler treats every round alike. Threads
    // the round starts inherit it.
    #[cfg(target_os = "macos")]
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
    }
    let measured = suite.round(fixture)?;
    let round = Round {
        version: format::ROUND,
        measured,
    };
    let json = serde_json::to_string(&round).map_err(|e| e.to_string())?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_take_turns_a_b_b_a() {
        let order: Vec<usize> = (0..4).flat_map(|round| turn(round, 2)).collect();
        assert_eq!(order, [0, 1, 1, 0, 0, 1, 1, 0]);
    }
}
