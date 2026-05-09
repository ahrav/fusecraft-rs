//! Error types for fusecraft-core.

use std::io;

/// The primary error type for the fusecraft-core crate.
#[derive(thiserror::Error, Debug)]
pub enum FsError {
    /// A raw errno value.
    #[error("errno({0})")]
    Errno(i32),
    /// A configuration error.
    #[error("config: {0}")]
    Config(String),
    /// An I/O error from the underlying system.
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

impl FsError {
    /// Map this error to a raw errno integer.
    pub fn as_errno(&self) -> i32 {
        match self {
            FsError::Errno(e) => *e,
            FsError::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
            FsError::Config(_) => libc::EINVAL,
        }
    }
}
