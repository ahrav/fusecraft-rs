//! Sampler module: deterministic probability sampling for latency and fault injection.
//!
//! This module provides pure functions that produce deterministic results from a
//! [`SampleKey`]. No shared mutable state is required — each call derives its own
//! RNG from the key, making the API safe for concurrent use without locks.
//!
//! - [`sample_latency_us`]: Latency from a composite lognormal + Pareto distribution.
//! - [`sample_fault`]: Fault injection based on per-operation rules.
//! - [`SampleKey`]: The fully-determined input that drives both samplers.

mod fault;
mod key;
mod latency;

pub use self::fault::sample_fault;
pub use self::key::SampleKey;
pub use self::latency::sample_latency_us;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{FaultRule, LatencyProfile};
    use crate::op::FsOp;

    fn mk_key(op: FsOp, seq: u64) -> SampleKey {
        SampleKey {
            seed: 7,
            op,
            ino: 1,
            offset: 0,
            len: 0,
            seq,
        }
    }

    #[test]
    fn latency_and_fault_independent() {
        // Changing fault rules should not affect latency output (different streams).
        let profile = LatencyProfile::default();
        let key = mk_key(FsOp::Read, 99);

        let lat = sample_latency_us(&profile, key);

        // Calling fault with the same key should not change the latency result.
        let rules = vec![FaultRule {
            op: FsOp::Read,
            errno: libc::EIO,
            rate: 1.0,
        }];
        let _fault = sample_fault(&rules, key);
        let lat_again = sample_latency_us(&profile, key);

        assert_eq!(lat, lat_again);
    }

    #[test]
    fn different_seeds_different_results() {
        let profile = LatencyProfile::default();
        let k1 = SampleKey {
            seed: 1,
            op: FsOp::Read,
            ino: 1,
            offset: 0,
            len: 0,
            seq: 0,
        };
        let k2 = SampleKey { seed: 2, ..k1 };
        // Very unlikely to be equal with different seeds.
        let results: Vec<bool> = (0..50)
            .map(|seq| {
                sample_latency_us(&profile, SampleKey { seq, ..k1 })
                    == sample_latency_us(&profile, SampleKey { seq, ..k2 })
            })
            .collect();
        // At least some should differ.
        assert!(results.iter().any(|&eq| !eq));
    }
}
