//! Deterministic fault sampling.
//!
//! Evaluates a list of [`FaultRule`]s and determines whether a fault should be
//! injected for a given operation. The function is **pure**: the same
//! [`SampleKey`] always yields the same result, with no shared mutable state.

use rand::Rng;

use crate::config::FaultRule;

use super::key::{self, SampleKey};

/// Sample whether a fault should be injected.
///
/// Returns `Some(errno)` if a fault fires, `None` otherwise.
///
/// Iterates through rules matching `key.op` in order. For each matching rule,
/// draws a uniform random number and compares against the rule's rate. The
/// first rule that fires wins.
pub fn sample_fault(rules: &[FaultRule], key: SampleKey) -> Option<i32> {
    let mut rng = key::fault_rng(key);

    for rule in rules.iter().filter(|r| r.op == key.op) {
        let u: f64 = rng.random();
        if u < rule.rate {
            return Some(rule.errno);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op::FsOp;

    fn mk_key(op: FsOp, seq: u64) -> SampleKey {
        SampleKey {
            seed: 42,
            op,
            ino: 1,
            offset: 0,
            len: 4096,
            seq,
        }
    }

    fn make_rules() -> Vec<FaultRule> {
        vec![
            FaultRule {
                op: FsOp::Read,
                errno: libc::EIO,
                rate: 0.5,
            },
            FaultRule {
                op: FsOp::Write,
                errno: libc::ENOSPC,
                rate: 1.0,
            },
            FaultRule {
                op: FsOp::Read,
                errno: libc::EAGAIN,
                rate: 0.1,
            },
        ]
    }

    #[test]
    fn fault_sampling_is_deterministic() {
        let rules = make_rules();
        let key = mk_key(FsOp::Read, 7);
        assert_eq!(sample_fault(&rules, key), sample_fault(&rules, key));
    }

    #[test]
    fn one_fault_rate_always_faults() {
        let rules = vec![FaultRule {
            op: FsOp::Write,
            errno: libc::ENOSPC,
            rate: 1.0,
        }];
        for seq in 0..1000 {
            assert_eq!(
                sample_fault(&rules, mk_key(FsOp::Write, seq)),
                Some(libc::ENOSPC)
            );
        }
    }

    #[test]
    fn zero_fault_rate_never_faults() {
        let rules = vec![FaultRule {
            op: FsOp::Read,
            errno: libc::EIO,
            rate: 0.0,
        }];
        for seq in 0..1000 {
            assert_eq!(sample_fault(&rules, mk_key(FsOp::Read, seq)), None);
        }
    }

    #[test]
    fn fault_respects_op_filter() {
        // Rule targets Read, but key.op is Write → should never fire.
        let rules = vec![FaultRule {
            op: FsOp::Read,
            errno: libc::EIO,
            rate: 1.0,
        }];
        for seq in 0..100 {
            assert_eq!(sample_fault(&rules, mk_key(FsOp::Write, seq)), None);
        }
    }

    #[test]
    fn no_rules_passes() {
        assert_eq!(sample_fault(&[], mk_key(FsOp::Read, 0)), None);
    }

    #[test]
    fn first_matching_rule_wins() {
        // Two rules for Read, both rate 1.0. First should always win.
        let rules = vec![
            FaultRule {
                op: FsOp::Read,
                errno: libc::EIO,
                rate: 1.0,
            },
            FaultRule {
                op: FsOp::Read,
                errno: libc::EAGAIN,
                rate: 1.0,
            },
        ];
        for seq in 0..100 {
            assert_eq!(
                sample_fault(&rules, mk_key(FsOp::Read, seq)),
                Some(libc::EIO)
            );
        }
    }

    #[test]
    fn statistical_rate() {
        let rules = vec![FaultRule {
            op: FsOp::Read,
            errno: libc::EIO,
            rate: 0.3,
        }];

        let n = 10_000u64;
        let failures = (0..n)
            .filter(|&seq| sample_fault(&rules, mk_key(FsOp::Read, seq)).is_some())
            .count();

        let observed_rate = failures as f64 / n as f64;
        assert!(
            (0.25..=0.35).contains(&observed_rate),
            "observed rate {observed_rate} not in [0.25, 0.35]"
        );
    }
}
