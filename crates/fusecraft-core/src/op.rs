//! Filesystem operation types for the simulator.

use serde::{Deserialize, Serialize};

/// A filesystem operation kind that the simulator can intercept.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FsOp {
    Lookup,
    GetAttr,
    Open,
    Read,
    Write,
    Flush,
    Release,
    Fsync,
    Readdir,
    Statfs,
}

impl FsOp {
    /// All defined operation kinds.
    pub const ALL: [FsOp; 10] = [
        FsOp::Lookup,
        FsOp::GetAttr,
        FsOp::Open,
        FsOp::Read,
        FsOp::Write,
        FsOp::Flush,
        FsOp::Release,
        FsOp::Fsync,
        FsOp::Readdir,
        FsOp::Statfs,
    ];

    /// Return the string representation of this operation.
    pub fn as_str(self) -> &'static str {
        match self {
            FsOp::Lookup => "lookup",
            FsOp::GetAttr => "getattr",
            FsOp::Open => "open",
            FsOp::Read => "read",
            FsOp::Write => "write",
            FsOp::Flush => "flush",
            FsOp::Release => "release",
            FsOp::Fsync => "fsync",
            FsOp::Readdir => "readdir",
            FsOp::Statfs => "statfs",
        }
    }
}
