//! What samples say. See the rules in `.plans/24-benchmarks-and-reports.md`.

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
    if a.len() < FEWEST_SAMPLES || b.len() < FEWEST_SAMPLES {
        return Verdict::TooFewSamples;
    }
    let mut rng = Rng::new(0x5EED);
    let mut ratios: Vec<f64> = (0..RESAMPLES)
        .map(|_| median(&resample(b, &mut rng)) / median(&resample(a, &mut rng)))
        .collect();
    ratios.sort_by(f64::total_cmp);
    let (low, high) = (percentile(&ratios, 0.025), percentile(&ratios, 0.975));
    if low > 1. {
        Verdict::Faster { low, high }
    } else if high < 1. {
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
    fn a_median_of_an_even_count_is_between_the_middle_two() {
        assert_eq!(median(&[4., 1., 2., 3.]), 2.5);
        assert_eq!(median(&[3., 1., 2.]), 2.);
    }

    #[test]
    fn too_few_samples_make_no_claim() {
        let verdict = compare(&jittered(1., 9), &jittered(100., 30));
        assert_eq!(verdict, Verdict::TooFewSamples);
    }
}
