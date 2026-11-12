//! What samples say. See the rules in `.plans/24-benchmarks-and-reports.md`.

use crate::format::SuiteRun;
use crate::rng::Rng;
use serde::{Deserialize, Serialize};

/// Fewer than this and a comparison makes no claim.
const FEWEST_SAMPLES: usize = 10;
const RESAMPLES: usize = 2_000;

pub fn percentile(samples: &[f64], fraction: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

pub fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        f64::midpoint(sorted[middle - 1], sorted[middle])
    } else {
        sorted[middle]
    }
}

/// A series whose p90 is past this many times its median is spread too wide.
const WIDE: f64 = 1.5;
/// More than this share of a suite's series spread wide makes it noisy.
const WIDE_SHARE: f64 = 0.25;

/// How far the slow samples run past the middle: p90 over the median.
pub fn spread(samples: &[f64]) -> f64 {
    percentile(samples, 0.9) / median(samples)
}

/// Why a suite's numbers shouldn't be judged: its conditions, or too many
/// series spread wide. Budgets don't pass or fail on a noisy suite.
pub fn noise(run: &SuiteRun) -> Vec<String> {
    let mut reasons = run.conditions.noise();
    let spreads: Vec<f64> = run
        .benches
        .iter()
        .flat_map(|bench| &bench.series)
        .filter(|series| series.samples.len() >= FEWEST_SAMPLES)
        .map(|series| spread(&series.wall()))
        .collect();
    let wide = spreads.iter().filter(|s| **s > WIDE).count();
    if wide as f64 > spreads.len() as f64 * WIDE_SHARE {
        reasons.push(format!(
            "{wide} of {} results have a p90 past {WIDE} times their median",
            spreads.len()
        ));
    }
    reasons
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Faster { low: f64, high: f64 },
    Slower { low: f64, high: f64 },
    NoClearDifference,
    TooFewSamples,
}

/// Whether `a` beat `b`, from a 95% bootstrap interval on `b`'s median over
/// `a`'s. `low` and `high` bound how many times faster or slower it was. The
/// seed is fixed, so the same samples always get the same verdict.
pub fn compare(a: &[f64], b: &[f64]) -> Verdict {
    compare_past(a, b, 0.)
}

/// Instruction counts vary by up to about 3% for kernel-heavy work, so a
/// smaller difference in them isn't one.
pub const INSTRUCTION_FLOOR: f64 = 0.03;

/// `compare`, where the whole interval must also be more than `floor` past
/// even, such as 0.03 for 3%, to count as a difference.
pub fn compare_past(a: &[f64], b: &[f64], floor: f64) -> Verdict {
    if a.len() < FEWEST_SAMPLES || b.len() < FEWEST_SAMPLES {
        return Verdict::TooFewSamples;
    }
    let mut rng = Rng::new(0x5EED);
    let mut ratios: Vec<f64> = (0..RESAMPLES)
        .map(|_| median(&resample(b, &mut rng)) / median(&resample(a, &mut rng)))
        .collect();
    ratios.sort_by(f64::total_cmp);
    let (low, high) = (percentile(&ratios, 0.025), percentile(&ratios, 0.975));
    if low > 1. + floor {
        Verdict::Faster { low, high }
    } else if high < 1. / (1. + floor) {
        Verdict::Slower {
            low: 1. / high,
            high: 1. / low,
        }
    } else {
        Verdict::NoClearDifference
    }
}

fn resample(samples: &[f64], rng: &mut Rng) -> Vec<f64> {
    (0..samples.len()).map(|_| *rng.pick(samples)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jittered(center: f64, n: usize) -> Vec<f64> {
        let mut rng = Rng::new(center.to_bits());
        (0..n)
            .map(|_| center * (0.9 + 0.2 * rng.below(1_000) as f64 / 1_000.))
            .collect()
    }

    #[test]
    fn a_real_difference_gets_a_verdict_either_way() {
        let (fast, slow) = (jittered(1., 30), jittered(10., 30));
        let Verdict::Faster { low, high } = compare(&fast, &slow) else {
            panic!("{:?}", compare(&fast, &slow));
        };
        assert!(low > 8. && high < 12., "{low} {high}");
        assert!(matches!(compare(&slow, &fast), Verdict::Slower { .. }));
    }

    #[test]
    fn the_same_spread_is_no_clear_difference() {
        let verdict = compare(&jittered(5., 30), &jittered(5.0001, 30));
        assert_eq!(verdict, Verdict::NoClearDifference);
    }

    #[test]
    fn a_difference_inside_the_floor_is_none() {
        // Tight counts 1% apart: a real difference, but inside a 3% floor.
        let (a, b) = (jittered(100., 30), jittered(101., 30));
        let (a, b): (Vec<f64>, Vec<f64>) = (
            a.iter().map(|x| 100. + (x - 100.) / 100.).collect(),
            b.iter().map(|x| 101. + (x - 101.) / 100.).collect(),
        );
        assert!(matches!(compare(&a, &b), Verdict::Faster { .. }));
        assert_eq!(
            compare_past(&a, &b, INSTRUCTION_FLOOR),
            Verdict::NoClearDifference
        );
        let doubled: Vec<f64> = a.iter().map(|x| x * 2.).collect();
        assert!(matches!(
            compare_past(&doubled, &a, INSTRUCTION_FLOOR),
            Verdict::Slower { .. }
        ));
    }

    #[test]
    fn a_median_of_an_even_count_is_between_the_middle_two() {
        assert_eq!(median(&[4., 1., 2., 3.]), 2.5);
        assert_eq!(median(&[3., 1., 2.]), 2.);
    }

    fn run(spreads: &[f64], load: f64) -> SuiteRun {
        use crate::format::{Bench, Cache, Series};
        use crate::measure::Sample;
        let series = |spread: f64| Series {
            build: "this".into(),
            variant: String::new(),
            warmup: None,
            // Eight at 1 and two at `spread`, so p90 over median is `spread`.
            samples: (0..10)
                .map(|i| Sample {
                    wall_ms: if i >= 8 { spread } else { 1. },
                    cpu_ms: None,
                    instructions: None,
                    cycles: None,
                })
                .collect(),
            checks: vec![],
            peaks: Vec::new(),
        };
        SuiteRun {
            suite: "s".into(),
            conditions: crate::conditions::Conditions {
                load_before: load,
                ..crate::conditions::tests::quiet()
            },
            rounds: 10,
            notes: vec![],
            benches: spreads
                .iter()
                .map(|&spread| Bench {
                    bench: "b".into(),
                    title: "b".into(),
                    group: "g".into(),
                    items: None,
                    cache: Cache::Warm,
                    series: vec![series(spread)],
                })
                .collect(),
        }
    }

    #[test]
    fn a_suite_is_noisy_when_busy_or_when_too_many_results_spread_wide() {
        assert!(noise(&run(&[1., 1.2, 2., 1.], 1.)).is_empty());
        assert_eq!(noise(&run(&[1., 1.2, 2., 1.], 9.)).len(), 1);
        let wide = noise(&run(&[3., 1.2, 2., 1.], 1.));
        assert_eq!(
            wide,
            ["2 of 4 results have a p90 past 1.5 times their median"]
        );
    }

    #[test]
    fn too_few_samples_make_no_claim() {
        let verdict = compare(&jittered(1., 9), &jittered(100., 30));
        assert_eq!(verdict, Verdict::TooFewSamples);
    }
}
