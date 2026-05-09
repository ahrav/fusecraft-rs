//! Deterministic synthetic content generation.
//!
//! Bytes are a pure function of `(ino, offset, seed)` — nothing is stored and
//! generation is O(1) per byte.

use crate::error::FsError;

use super::ContentModel;

/// Generate a single deterministic byte from `(ino, offset, seed)`.
///
/// Uses a Murmur-style finalizer over mixed inputs so that any change in
/// (ino, offset, seed) produces an uncorrelated output byte.
#[inline]
pub fn synth_byte(ino: u64, offset: u64, seed: u64) -> u8 {
    let mut x = seed ^ ino.rotate_left(17) ^ offset.rotate_left(31);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    (x ^ (x >> 33)) as u8
}

/// Deterministic content store backed by [`synth_byte`].
///
/// The same `(ino, offset)` pair always yields the same byte for a given seed,
/// making this suitable for reproducible simulator runs.
#[derive(Debug, Clone)]
pub struct DeterministicContent {
    seed: u64,
    file_len: u64,
}

impl DeterministicContent {
    /// Create a new deterministic content store.
    pub fn new(seed: u64, file_len: u64) -> Self {
        Self { seed, file_len }
    }

    /// The seed used to derive synthetic bytes.
    pub fn seed(&self) -> u64 {
        self.seed
    }
}

impl ContentModel for DeterministicContent {
    fn file_len(&self, _ino: u64) -> u64 {
        self.file_len
    }

    fn read_at(&self, ino: u64, offset: u64, dst: &mut [u8]) {
        for (i, b) in dst.iter_mut().enumerate() {
            *b = synth_byte(ino, offset.wrapping_add(i as u64), self.seed);
        }
    }

    fn write_at(&self, _ino: u64, _offset: u64, data: &[u8]) -> Result<usize, FsError> {
        // MVP: discard writes but acknowledge the configured length.
        Ok(data.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_byte_is_deterministic() {
        assert_eq!(synth_byte(100, 0, 1), synth_byte(100, 0, 1));
    }

    #[test]
    fn synth_byte_varies_by_seed() {
        let seeds: Vec<u8> = (0..32).map(|s| synth_byte(100, 0, s)).collect();
        let unique: std::collections::HashSet<_> = seeds.iter().copied().collect();
        assert!(unique.len() > 4, "seeds should vary output");
    }

    #[test]
    fn synth_byte_varies_by_ino_and_offset() {
        assert_ne!(synth_byte(100, 0, 0), synth_byte(101, 0, 0));
        assert_ne!(synth_byte(100, 0, 0), synth_byte(100, 1, 0));
    }

    #[test]
    fn read_at_fills_entire_buffer() {
        let c = DeterministicContent::new(42, 1024);
        let mut buf = [0u8; 128];
        c.read_at(7, 10, &mut buf);
        for (i, b) in buf.iter().enumerate() {
            assert_eq!(*b, synth_byte(7, 10 + i as u64, 42));
        }
    }

    #[test]
    fn read_at_empty_buffer_is_noop() {
        let c = DeterministicContent::new(1, 1024);
        let mut buf: [u8; 0] = [];
        c.read_at(1, 0, &mut buf);
    }

    #[test]
    fn read_at_does_not_clamp_against_file_len() {
        // Engine is responsible for slicing; the content model fills dst unconditionally.
        let c = DeterministicContent::new(1, 16);
        let mut buf = [0u8; 32];
        c.read_at(1, 0, &mut buf);
        // Every byte must be written (not the default 0 for at least one).
        let nonzero = buf.iter().filter(|&&b| b != 0).count();
        assert!(nonzero > 0);
    }

    #[test]
    fn write_at_returns_len_and_discards() {
        let c = DeterministicContent::new(1, 1024);
        let n = c.write_at(1, 0, b"hello").unwrap();
        assert_eq!(n, 5);
        // After the write, a subsequent read returns the original synthetic bytes
        // (write was discarded).
        let mut buf = [0u8; 5];
        c.read_at(1, 0, &mut buf);
        for (i, b) in buf.iter().enumerate() {
            assert_eq!(*b, synth_byte(1, i as u64, 1));
        }
    }

    #[test]
    fn file_len_reports_configured_size() {
        let c = DeterministicContent::new(1, 65536);
        assert_eq!(c.file_len(0), 65536);
    }

    #[test]
    fn usable_via_trait_object() {
        let c: Box<dyn ContentModel> = Box::new(DeterministicContent::new(99, 128));
        let mut buf = [0u8; 8];
        c.read_at(1, 0, &mut buf);
        assert_eq!(c.file_len(1), 128);
        assert_eq!(c.write_at(1, 0, b"abc").unwrap(), 3);
    }
}
