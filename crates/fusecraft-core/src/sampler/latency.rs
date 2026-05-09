//! Deterministic latency sampling.
//!
//! Produces a delay (in microseconds) from a composite distribution:
//!   latency = base + body, clamped to max_us.
//!
//! The body is either a lognormal sample (with probability `1 - pareto_weight`)
//! or a Pareto tail sample (with probability `pareto_weight`).
//!
//! The function is **pure**: the same [`SampleKey`] always yields the same result,
//! with no shared mutable state. Uses the Box-Muller transform for standard
//! normal generation (no `rand_distr` dependency).

use rand::Rng;

use crate::config::LatencyProfile;

use super::key::{self, SampleKey};

/// Sample a latency in microseconds, fully determined by `profile` and `key`.
pub fn sample_latency_us(profile: &LatencyProfile, key: SampleKey) -> u64 {
    let mut rng = key::latency_rng(key);

    let base = profile.base_us as f64;

    // Body: mixture of lognormal and Pareto tail.
    let body = if profile.pareto_weight > 0.0 {
        let u: f64 = rng.random();
        if u < profile.pareto_weight {
            // Pareto tail replaces the lognormal body.
            sample_pareto(&mut rng, profile.pareto_xm_us, profile.pareto_alpha)
        } else {
            sample_lognormal(
                &mut rng,
                profile.lognormal_median_us,
                profile.lognormal_sigma,
            )
        }
    } else {
        sample_lognormal(
            &mut rng,
            profile.lognormal_median_us,
            profile.lognormal_sigma,
        )
    };

    let total = base + body;

    // Clamp to max_us and convert to integer microseconds (saturating round).
    let clamped = total.min(profile.max_us as f64).max(0.0);
    let rounded = clamped.round() as u64;
    rounded.min(profile.max_us)
}

/// Sample from a lognormal distribution using Box-Muller.
///
/// Parameters:
/// - `median`: the median of the lognormal (= exp(mu) where mu = ln(median))
/// - `sigma`: the shape parameter (standard deviation of the underlying normal)
fn sample_lognormal(rng: &mut impl Rng, median: f64, sigma: f64) -> f64 {
    if median <= 0.0 || sigma <= 0.0 {
        return median.max(0.0);
    }
    let mu = median.ln();
    let z = sample_standard_normal(rng);
    (mu + sigma * z).exp()
}

/// Sample from a Pareto (Type I) distribution using inverse CDF.
///
/// Formula: x = xm / U^(1/alpha), where U ~ Uniform(0,1).
fn sample_pareto(rng: &mut impl Rng, xm: f64, alpha: f64) -> f64 {
    let u: f64 = rng.random::<f64>().max(f64::MIN_POSITIVE);
    xm / u.powf(1.0 / alpha)
}

/// Generate a standard normal variate using the Box-Muller transform.
///
/// Consumes two uniform(0,1) samples and returns one normal(0,1) sample.
fn sample_standard_normal(rng: &mut impl Rng) -> f64 {
    let u1: f64 = rng.random::<f64>().max(f64::MIN_POSITIVE);
    let u2: f64 = rng.random::<f64>();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::FsOp;

    fn mk_key(seq: u64) -> SampleKey {
        SampleKey {
            seed: 42,
            op: FsOp::Read,
            ino: 1,
            offset: 0,
            len: 4096,
            seq,
        }
    }

    #[test]
    fn latency_sampling_is_deterministic() {
        let profile = LatencyProfile::default();
        let key = mk_key(7);
        assert_eq!(
            sample_latency_us(&profile, key),
            sample_latency_us(&profile, key)
        );
    }

    #[test]
    fn latency_sampling_respects_max() {
        let profile = LatencyProfile {
            max_us: 100,
            base_us: 0,
            ..LatencyProfile::default()
        };
        for seq in 0..10_000 {
            let key = SampleKey {
                seed: 1,
                op: FsOp::Read,
                ino: 0,
                offset: 0,
                len: 0,
                seq,
            };
            assert!(sample_latency_us(&profile, key) <= 100);
        }
    }

    #[test]
    fn latency_sampling_respects_base() {
        // With lognormal_median=0, pareto_weight=0, sigma=0: result == base_us.
        let profile = LatencyProfile {
            base_us: 250,
            lognormal_median_us: 0.0,
            lognormal_sigma: 0.0,
            pareto_weight: 0.0,
            pareto_xm_us: 1000.0,
            pareto_alpha: 1.5,
            max_us: 1_000_000,
        };

        for seq in 0..100 {
            let us = sample_latency_us(&profile, mk_key(seq));
            assert_eq!(us, 250);
        }
    }

    #[test]
    fn different_ops_get_different_streams() {
        let profile = LatencyProfile::default();
        let mk = |op| SampleKey {
            seed: 1,
            op,
            ino: 1,
            offset: 0,
            len: 0,
            seq: 0,
        };
        // Run 100 pairs and check not ALL equal.
        let pairs_equal: Vec<bool> = (0..100)
            .map(|seq| {
                let read_key = SampleKey {
                    seq,
                    ..mk(FsOp::Read)
                };
                let write_key = SampleKey {
                    seq,
                    ..mk(FsOp::Write)
                };
                sample_latency_us(&profile, read_key) == sample_latency_us(&profile, write_key)
            })
            .collect();
        assert!(
            pairs_equal.iter().any(|&eq| !eq),
            "different ops should produce different streams"
        );
    }

    #[test]
    fn pareto_tail_fires() {
        let profile = LatencyProfile {
            base_us: 0,
            lognormal_median_us: 1.0,
            lognormal_sigma: 0.01,
            pareto_weight: 1.0,
            pareto_xm_us: 1000.0,
            pareto_alpha: 1.5,
            max_us: 10_000_000,
        };

        let samples: Vec<u64> = (0..100)
            .map(|seq| sample_latency_us(&profile, mk_key(seq)))
            .collect();
        let above_xm = samples.iter().filter(|&&s| s >= 1000).count();
        assert!(
            above_xm > 90,
            "expected most samples >= 1000, got {above_xm}/100"
        );
    }

    #[test]
    fn lognormal_distribution_reasonable() {
        let profile = LatencyProfile {
            base_us: 0,
            lognormal_median_us: 100.0,
            lognormal_sigma: 0.5,
            pareto_weight: 0.0,
            pareto_xm_us: 1000.0,
            pareto_alpha: 1.5,
            max_us: 1_000_000,
        };

        let mut samples: Vec<u64> = (0..10_000)
            .map(|seq| sample_latency_us(&profile, mk_key(seq)))
            .collect();
        samples.sort();
        let median = samples[samples.len() / 2];
        // Median of lognormal(ln(100), 0.5) should be ~100.
        assert!(
            (50..200).contains(&median),
            "median {median} not in expected range 50..200"
        );
    }
}
