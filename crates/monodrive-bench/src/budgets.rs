//! `benchmarks/budgets.toml`: the most each measured thing may take. The
//! report judges the median of this build's samples against it, and judges
//! nothing on a noisy suite.

use crate::format::Results;
use crate::stats;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Budgets {
    #[serde(rename = "budget")]
    pub budgets: Vec<Budget>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Budget {
    /// "suite/bench", as in `results.json`.
    pub bench: String,
    pub what: String,
    pub median_ms: f64,
    /// Why the budget is where it is, when it isn't obvious.
    #[serde(default)]
    pub why: Option<String>,
    /// The command that measures it, for what scripts/bench doesn't yet.
    #[serde(default)]
    pub measured_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Within {
        median_ms: f64,
    },
    Over {
        median_ms: f64,
    },
    /// Measured, but on a noisy suite.
    NotJudged {
        median_ms: f64,
    },
    /// The run didn't measure it.
    Missing,
}

pub fn load(path: &Path) -> Result<Budgets, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Each budget's status in `results`, from the build labeled "this".
pub fn judge(budgets: &Budgets, results: &Results) -> Vec<(Budget, Status)> {
    budgets
        .budgets
        .iter()
        .map(|budget| {
            let found = budget.bench.split_once('/').and_then(|(suite, bench)| {
                let run = results.suites.iter().find(|run| run.suite == suite)?;
                let series = run
                    .benches
                    .iter()
                    .find(|b| b.bench == bench)?
                    .series
                    .iter()
                    .find(|s| s.build == "this" && s.variant.is_empty() && !s.samples.is_empty())?;
                Some((run, stats::median(&series.wall())))
            });
            let status = match found {
                None => Status::Missing,
                Some((run, median_ms)) if !stats::noise(run).is_empty() => {
                    Status::NotJudged { median_ms }
                }
                Some((_, median_ms)) if median_ms <= budget.median_ms => {
                    Status::Within { median_ms }
                }
                Some((_, median_ms)) => Status::Over { median_ms },
            };
            (budget.clone(), status)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{Bench, Cache, Series, SuiteRun};
    use crate::measure::Sample;

    fn results(wall: &[f64], load: f64) -> Results {
        Results {
            version: crate::format::RESULTS,
            started: String::new(),
            builds: vec![],
            suites: vec![SuiteRun {
                suite: "core".into(),
                conditions: crate::conditions::Conditions {
                    load_before: load,
                    ..crate::conditions::tests::quiet()
                },
                rounds: wall.len(),
                notes: vec![],
                benches: vec![Bench {
                    bench: "list_dir/10000".into(),
                    title: String::new(),
                    group: String::new(),
                    items: None,
                    cache: Cache::Warm,
                    series: vec![Series {
                        build: "this".into(),
                        variant: String::new(),
                        warmup: None,
                        samples: wall
                            .iter()
                            .map(|&wall_ms| Sample {
                                wall_ms,
                                cpu_ms: None,
                                instructions: None,
                                cycles: None,
                            })
                            .collect(),
                        checks: vec![],
                        peaks: Vec::new(),
                    }],
                }],
            }],
        }
    }

    #[test]
    fn a_budget_judges_the_median_and_nothing_on_a_noisy_or_missing_run() {
        let budgets: Budgets = toml::from_str(
            r#"
            [[budget]]
            bench = "core/list_dir/10000"
            what = "List 10,000"
            median_ms = 40

            [[budget]]
            bench = "core/list_dir/1"
            what = "List one"
            median_ms = 1
            "#,
        )
        .unwrap();
        let judged = |wall: &[f64], load| -> Vec<Status> {
            judge(&budgets, &results(wall, load))
                .into_iter()
                .map(|(_, s)| s)
                .collect()
        };
        assert_eq!(
            judged(&[10., 12., 90.], 1.),
            [Status::Within { median_ms: 12. }, Status::Missing]
        );
        assert_eq!(
            judged(&[41., 45., 1.], 1.)[0],
            Status::Over { median_ms: 41. }
        );
        assert_eq!(
            judged(&[41., 45., 1.], 9.)[0],
            Status::NotJudged { median_ms: 41. }
        );
    }

    #[test]
    fn the_checked_in_budgets_read() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/budgets.toml");
        assert!(!load(&path).unwrap().budgets.is_empty());
    }
}
