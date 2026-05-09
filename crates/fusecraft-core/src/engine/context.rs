//! Per-op context passed into [`crate::engine::SimEngine::run_op`].

use crate::op::FsOp;

/// Describes a single operation about to be executed.
///
/// The engine combines this with its internal monotonic sequence to build a
/// [`crate::sampler::SampleKey`] for deterministic latency/fault draws.
#[derive(Copy, Clone, Debug)]
pub struct OpContext {
    pub op: FsOp,
    pub ino: u64,
    pub offset: u64,
    pub len: usize,
}

impl OpContext {
    /// Convenience constructor for metadata ops with no offset/len.
    pub fn metadata(op: FsOp, ino: u64) -> Self {
        Self {
            op,
            ino,
            offset: 0,
            len: 0,
        }
    }

    /// Convenience constructor for data ops.
    pub fn data(op: FsOp, ino: u64, offset: u64, len: usize) -> Self {
        Self {
            op,
            ino,
            offset,
            len,
        }
    }
}
