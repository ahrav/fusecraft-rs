//! Filesystem operation types for the simulator.
//!
//! Each variant represents a FUSE operation that the simulator can intercept
//! and apply latency, fault, concurrency, or bandwidth injection to.

use serde::{Deserialize, Serialize};

/// A filesystem operation kind that can be intercepted by the simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    /// lookup(parent, name)
    Lookup,
    /// getattr(ino)
    Getattr,
    /// setattr(ino, attrs)
    Setattr,
    /// readdir(ino)
    Readdir,
    /// open(ino, flags)
    Open,
    /// read(ino, offset, size)
    Read,
    /// write(ino, offset, data)
    Write,
    /// create(parent, name, mode)
    Create,
    /// unlink(parent, name)
    Unlink,
    /// mkdir(parent, name, mode)
    Mkdir,
    /// rmdir(parent, name)
    Rmdir,
    /// rename(parent, name, newparent, newname)
    Rename,
    /// fsync(ino)
    Fsync,
    /// flush(ino)
    Flush,
    /// release(ino)
    Release,
    /// statfs()
    Statfs,
}

impl OpKind {
    /// Returns all defined operation kinds.
    pub fn all() -> &'static [OpKind] {
        &[
            OpKind::Lookup,
            OpKind::Getattr,
            OpKind::Setattr,
            OpKind::Readdir,
            OpKind::Open,
            OpKind::Read,
            OpKind::Write,
            OpKind::Create,
            OpKind::Unlink,
            OpKind::Mkdir,
            OpKind::Rmdir,
            OpKind::Rename,
            OpKind::Fsync,
            OpKind::Flush,
            OpKind::Release,
            OpKind::Statfs,
        ]
    }

    /// Returns true if this operation transfers data (read/write).
    pub fn is_data_op(self) -> bool {
        matches!(self, OpKind::Read | OpKind::Write)
    }

    /// Returns true if this operation is metadata-only.
    pub fn is_metadata_op(self) -> bool {
        !self.is_data_op()
    }
}

impl std::fmt::Display for OpKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OpKind::Lookup => "lookup",
            OpKind::Getattr => "getattr",
            OpKind::Setattr => "setattr",
            OpKind::Readdir => "readdir",
            OpKind::Open => "open",
            OpKind::Read => "read",
            OpKind::Write => "write",
            OpKind::Create => "create",
            OpKind::Unlink => "unlink",
            OpKind::Mkdir => "mkdir",
            OpKind::Rmdir => "rmdir",
            OpKind::Rename => "rename",
            OpKind::Fsync => "fsync",
            OpKind::Flush => "flush",
            OpKind::Release => "release",
            OpKind::Statfs => "statfs",
        };
        f.write_str(s)
    }
}
