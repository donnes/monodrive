//! The JSON a run writes. Raw samples are the record: the report reads only
//! `results.json`, so no chart can show a number the run didn't produce.

use crate::conditions::Conditions;
use crate::measure::Sample;
use serde::{Deserialize, Serialize};

/// `results.json`'s version. 1 is the Spotlight comparison's own format.
pub const RESULTS: u32 = 2;

/// What `run-one` prints, so a build from another commit can be read. Bump it
/// only for a change an older reader can't skip.
pub const ROUND: u32 = 1;

/// Whether the files were already in the disk cache. Warm and cold results
/// never share a chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cache {
    Warm,
    Cold,
}

/// One measurement from one round, as `run-one` prints it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Measured {
    /// Stable across commits, so two builds' samples meet: `list_dir/10000`.
    pub bench: String,
    /// What the reader sees: "List 10,000 entries".
    pub title: String,
    /// The chart it belongs in: "Listing".
    pub group: String,
    /// What did the work when one bench is done several ways, such as
    /// "Monodrive" against "cp -c". Empty for the build itself.
    #[serde(default)]
    pub variant: String,
    /// Items the work covered, for a time per item.
    #[serde(default)]
    pub items: Option<u64>,
    pub cache: Cache,
    pub sample: Sample,
    /// Proof the work happened, such as "10000 entries". Rounds that disagree
    /// didn't all measure the same work, and the report says so.
    pub check: String,
    /// How far the process's memory footprint rose during the work, for
    /// benches that report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Round {
    pub version: u32,
    pub measured: Vec<Measured>,
}

/// A build the run measured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    /// "this" for the working tree, "baseline" for `--against`.
    pub label: String,
    pub commit: String,
}

/// Every round's samples for one bench, done one way, by one build.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Series {
    pub build: String,
    pub variant: String,
    /// Round 0, thrown away so every build starts warm.
    pub warmup: Option<Sample>,
    pub samples: Vec<Sample>,
    /// Each distinct check, in the order seen. More than one means the rounds
    /// measured different work.
    pub checks: Vec<String>,
    /// Each sample's `Measured::peak_bytes`, where the bench reports it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub peaks: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bench {
    pub bench: String,
    pub title: String,
    pub group: String,
    pub items: Option<u64>,
    pub cache: Cache,
    pub series: Vec<Series>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuiteRun {
    pub suite: String,
    pub conditions: Conditions,
    pub rounds: usize,
    /// What the run couldn't do, such as a baseline without this suite.
    #[serde(default)]
    pub notes: Vec<String>,
    pub benches: Vec<Bench>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Results {
    pub version: u32,
    /// When the run started, in UTC.
    #[serde(default)]
    pub started: String,
    pub builds: Vec<Build>,
    pub suites: Vec<SuiteRun>,
}

impl SuiteRun {
    /// Files one round's measurements under their benches.
    pub fn add(&mut self, build: &str, round: Round, warmup: bool) {
        for measured in round.measured {
            let index = match self.benches.iter().position(|b| b.bench == measured.bench) {
                Some(index) => index,
                None => {
                    self.benches.push(Bench {
                        bench: measured.bench.clone(),
                        title: measured.title.clone(),
                        group: measured.group.clone(),
                        items: measured.items,
                        cache: measured.cache,
                        series: Vec::new(),
                    });
                    self.benches.len() - 1
                }
            };
            let bench = &mut self.benches[index];
            let series = match bench
                .series
                .iter()
                .position(|s| s.build == build && s.variant == measured.variant)
            {
                Some(index) => &mut bench.series[index],
                None => {
                    bench.series.push(Series {
                        build: build.to_string(),
                        variant: measured.variant.clone(),
                        warmup: None,
                        samples: Vec::new(),
                        checks: Vec::new(),
                        peaks: Vec::new(),
                    });
                    bench.series.last_mut().expect("just pushed")
                }
            };
            if warmup {
                series.warmup = Some(measured.sample);
            } else {
                series.samples.push(measured.sample);
                series.peaks.extend(measured.peak_bytes);
            }
            if !series.checks.contains(&measured.check) {
                series.checks.push(measured.check);
            }
        }
    }
}

impl Series {
    pub fn wall(&self) -> Vec<f64> {
        self.samples.iter().map(|s| s.wall_ms).collect()
    }

    /// Instructions per sample, or nothing if any sample lacks them.
    pub fn instructions(&self) -> Vec<f64> {
        self.samples
            .iter()
            .map(|s| s.instructions.map(|n| n as f64))
            .collect::<Option<Vec<f64>>>()
            .unwrap_or_default()
    }

    /// What the series calls itself in a chart: its variant, its build, or both.
    pub fn name(&self, builds: usize) -> String {
        match (self.variant.is_empty(), builds > 1) {
            (true, _) => self.build.clone(),
            (false, false) => self.variant.clone(),
            (false, true) => format!("{}, {}", self.variant, self.build),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured(bench: &str, check: &str, wall_ms: f64) -> Measured {
        Measured {
            bench: bench.into(),
            title: bench.into(),
            group: "g".into(),
            variant: String::new(),
            items: None,
            cache: Cache::Warm,
            sample: Sample {
                wall_ms,
                cpu_ms: None,
                instructions: None,
                cycles: None,
            },
            check: check.into(),
            peak_bytes: None,
        }
    }

    #[test]
    fn rounds_file_under_their_bench_and_build_and_the_warmup_stays_apart() {
        let mut run = SuiteRun {
            suite: "s".into(),
            conditions: crate::conditions::tests::quiet(),
            rounds: 2,
            notes: Vec::new(),
            benches: Vec::new(),
        };
        let round = |wall, check: &str| Round {
            version: ROUND,
            measured: vec![measured("a", check, wall)],
        };
        run.add("this", round(9., "1 entry"), true);
        run.add("baseline", round(8., "1 entry"), true);
        run.add("this", round(1., "1 entry"), false);
        run.add("baseline", round(2., "1 entry"), false);
        run.add("this", round(3., "2 entries"), false);
        let [bench] = run.benches.as_slice() else {
            panic!("{:?}", run.benches)
        };
        let this = &bench.series[0];
        assert_eq!((this.build.as_str(), this.wall()), ("this", vec![1., 3.]));
        assert_eq!(this.warmup.map(|s| s.wall_ms), Some(9.));
        assert_eq!(this.checks, ["1 entry", "2 entries"]);
        assert_eq!(bench.series[1].wall(), vec![2.]);
    }
}
