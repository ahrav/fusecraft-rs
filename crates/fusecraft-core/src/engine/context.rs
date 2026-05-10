//! Per-op context passed into [`crate::engine::SimEngine::run_op`].

use crate::op::FsOp;

/// Describes a single operation about to be executed.
///
/// The engine combines this with its internal monotonic sequence to build a
/// [`crate::sampler::SampleKey`] for deterministic latency/fault draws.
///
/// Two byte counts are tracked separately:
///
/// - [`OpContext::len`] is the *effective* byte count charged against
///   bandwidth, mixed into the sampler key, and reported in events/metrics.
///   It reflects the bytes that will actually be moved — for example, a read
///   near EOF is clamped to the readable tail so short tail reads are not
///   billed as full-size reads.
/// - [`OpContext::requested_len`] is the caller's *originally requested* byte
///   count, used only for size-tier routing. This keeps tier selection keyed
///   to the application's intent (workload pattern) rather than to incidental
///   file geometry, so a 1 MiB read near EOF with only 500 readable bytes
///   still routes to the large tier.
///
/// For callers that do not distinguish the two (e.g. writes, which carry the
/// exact byte count the caller supplied), [`OpContext::data`] sets both fields
/// to the same value.
#[derive(Copy, Clone, Debug)]
pub struct OpContext {
    pub op: FsOp,
    pub ino: u64,
    pub offset: u64,
    pub len: usize,
    pub requested_len: usize,
}

impl OpContext {
    /// Convenience constructor for metadata ops with no offset/len.
    pub fn metadata(op: FsOp, ino: u64) -> Self {
        Self {
            op,
            ino,
            offset: 0,
            len: 0,
            requested_len: 0,
        }
    }

    /// Convenience constructor for data ops.
    ///
    /// Sets both [`OpContext::len`] and [`OpContext::requested_len`] to `len`.
    /// When the caller needs to distinguish the effective byte count from the
    /// originally requested byte count (e.g. a read clamped to the readable
    /// tail), override `requested_len` with [`OpContext::with_requested_len`].
    pub fn data(op: FsOp, ino: u64, offset: u64, len: usize) -> Self {
        Self {
            op,
            ino,
            offset,
            len,
            requested_len: len,
        }
    }

    /// Override the caller's originally requested byte count.
    ///
    /// Use this when the caller knows the request was clamped before reaching
    /// the engine (e.g. a read near EOF) and size-tier routing should still
    /// key off the original request size.
    #[must_use]
    pub fn with_requested_len(mut self, requested_len: usize) -> Self {
        self.requested_len = requested_len;
        self
    }
}
