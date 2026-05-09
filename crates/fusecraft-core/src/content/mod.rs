//! Content module: synthetic data generation for virtual files.

use crate::error::FsError;

pub mod deterministic;

pub use deterministic::{DeterministicContent, synth_byte};

/// Trait for content models consumed by the SimEngine.
///
/// Implementations supply file length, byte-level reads, and (possibly discarded)
/// writes. The engine is generic over `C: ContentModel`, allowing users to plug
/// in custom content strategies.
pub trait ContentModel: Send + Sync + 'static {
    /// Return the logical length of the file at `ino`.
    fn file_len(&self, ino: u64) -> u64;

    /// Fill `dst` with bytes starting at `offset` for the given `ino`.
    ///
    /// The caller is responsible for slicing `dst` to the readable range —
    /// implementations MUST fill every byte of `dst` without checking
    /// `file_len` themselves.
    fn read_at(&self, ino: u64, offset: u64, dst: &mut [u8]);

    /// Write `data` at `offset` for the given `ino`.
    ///
    /// For the MVP discard model, implementations may drop the data and return
    /// `Ok(data.len())`.
    fn write_at(&self, ino: u64, offset: u64, data: &[u8]) -> Result<usize, FsError>;
}
