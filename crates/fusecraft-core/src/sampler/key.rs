//! Deterministic per-call RNG key derivation.
//!
//! [`SampleKey`] captures every dimension of a single sampling event. Combined
//! with a stream constant (latency vs fault), the key is mixed through a
//! splitmix64 finalizer to produce a unique RNG seed — guaranteeing determinism
//! without shared mutable state.

use rand::SeedableRng;
use rand::rngs::StdRng;

use crate::op::FsOp;

/// Stream constant for latency sampling.
const LATENCY_STREAM: u64 = 0xA5A5_A5A5_A5A5_A5A5;
/// Stream constant for fault sampling.
const FAULT_STREAM: u64 = 0x5A5A_5A5A_5A5A_5A5A;

/// A fully-determined sampling key.
///
/// Every field contributes to the derived RNG seed, so identical keys produce
/// identical samples while any field change yields an independent stream.
#[derive(Copy, Clone, Debug)]
pub struct SampleKey {
    /// Master seed from config.
    pub seed: u64,
    /// The filesystem operation being performed.
    pub op: FsOp,
    /// Inode number.
    pub ino: u64,
    /// Byte offset within the file (0 for metadata ops).
    pub offset: u64,
    /// Request length in bytes (0 for metadata ops).
    pub len: u32,
    /// Monotonic sequence number (unique per call site).
    pub seq: u64,
}

/// Mix all key fields with a stream constant via splitmix64 finalizer.
fn mix(key: SampleKey, stream: u64) -> u64 {
    let mut z = key.seed
        ^ (key.op as u64).rotate_left(11)
        ^ key.ino.rotate_left(23)
        ^ key.offset.rotate_left(37)
        ^ (key.len as u64).rotate_left(47)
        ^ key.seq.rotate_left(53)
        ^ stream;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Derive a latency-stream RNG from the given key.
pub(crate) fn latency_rng(key: SampleKey) -> StdRng {
    StdRng::seed_from_u64(mix(key, LATENCY_STREAM))
}

/// Derive a fault-stream RNG from the given key.
pub(crate) fn fault_rng(key: SampleKey) -> StdRng {
    StdRng::seed_from_u64(mix(key, FAULT_STREAM))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_key() -> SampleKey {
        SampleKey {
            seed: 42,
            op: FsOp::Read,
            ino: 1,
            offset: 0,
            len: 4096,
            seq: 0,
        }
    }

    #[test]
    fn same_key_same_rng() {
        use rand::Rng;
        let key = base_key();
        let mut a = latency_rng(key);
        let mut b = latency_rng(key);
        let va: Vec<u64> = (0..10).map(|_| a.random()).collect();
        let vb: Vec<u64> = (0..10).map(|_| b.random()).collect();
        assert_eq!(va, vb);
    }

    #[test]
    fn different_seq_different_rng() {
        use rand::Rng;
        let k1 = SampleKey {
            seq: 0,
            ..base_key()
        };
        let k2 = SampleKey {
            seq: 1,
            ..base_key()
        };
        let mut r1 = latency_rng(k1);
        let mut r2 = latency_rng(k2);
        let v1: u64 = r1.random();
        let v2: u64 = r2.random();
        assert_ne!(v1, v2);
    }

    #[test]
    fn latency_and_fault_streams_differ() {
        use rand::Rng;
        let key = base_key();
        let mut lat = latency_rng(key);
        let mut flt = fault_rng(key);
        let vl: u64 = lat.random();
        let vf: u64 = flt.random();
        assert_ne!(vl, vf);
    }

    #[test]
    fn different_ops_differ() {
        use rand::Rng;
        let k1 = SampleKey {
            op: FsOp::Read,
            ..base_key()
        };
        let k2 = SampleKey {
            op: FsOp::Write,
            ..base_key()
        };
        let mut r1 = latency_rng(k1);
        let mut r2 = latency_rng(k2);
        let v1: u64 = r1.random();
        let v2: u64 = r2.random();
        assert_ne!(v1, v2);
    }
}
