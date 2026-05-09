//! Namespace module: virtual filesystem tree (inodes, directory entries).
//!
//! This module provides the virtual filesystem layout — mapping inodes to file names
//! and directory structures without touching any real filesystem.
//!
//! The engine is generic over `NamespaceModel`, allowing different layout strategies.

pub mod flat;

use std::ffi::OsStr;
use std::time::SystemTime;

use crate::error::FsError;

pub use self::flat::FlatObjectNamespace;

/// The root inode number used by FUSE.
pub const FUSE_ROOT_INO: u64 = 1;

/// Kind of file in the namespace.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    Dir,
    File,
}

/// Full file attributes for a namespace entry.
#[derive(Copy, Clone, Debug)]
pub struct FileAttrSpec {
    /// Inode number.
    pub ino: u64,
    /// File size in bytes (0 for directories).
    pub size: u64,
    /// Whether this is a directory or regular file.
    pub kind: FileKind,
    /// Unix permission mode (e.g. 0o755 for dirs, 0o644 for files).
    pub mode: u32,
    /// Number of hard links.
    pub nlink: u32,
    /// Owner user ID.
    pub uid: u32,
    /// Owner group ID.
    pub gid: u32,
    /// Last modification time.
    pub mtime: SystemTime,
}

/// Push-based directory entry sink for `readdir`.
///
/// Implementations receive entries one at a time and return `false` to stop iteration.
pub trait DirSink {
    /// Push an entry into the sink.
    ///
    /// Returns `true` to continue iteration, `false` to stop.
    fn push(&mut self, ino: u64, offset: u64, kind: FileKind, name: &OsStr) -> bool;
}

/// Trait for namespace models consumed by the SimEngine.
///
/// The engine is generic over `N: NamespaceModel` to allow different layout strategies.
pub trait NamespaceModel: Send + Sync + 'static {
    /// Look up a child by name within a parent directory.
    ///
    /// Returns the child's inode number, or `None` if not found.
    fn lookup(&self, parent: u64, name: &OsStr) -> Option<u64>;

    /// Get the full attributes for an inode.
    ///
    /// Returns `None` if the inode does not exist.
    fn attr(&self, ino: u64) -> Option<FileAttrSpec>;

    /// Read directory entries starting at `offset`, pushing them into `sink`.
    ///
    /// Returns an error if `ino` is not a directory.
    fn readdir(&self, ino: u64, offset: u64, sink: &mut dyn DirSink) -> Result<(), FsError>;
}
