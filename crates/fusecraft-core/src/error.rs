//! Error types for fusecraft-core.

use std::io;

use thiserror::Error;

/// Top-level error type for the fusecraft-core library.
#[derive(Debug, Error)]
pub enum FusecraftError {
    /// An I/O error from the underlying system.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// A configuration parsing or validation error.
    #[error("config error: {0}")]
    Config(#[from] ConfigError),

    /// The requested inode was not found in the namespace.
    #[error("inode not found: {0}")]
    InodeNotFound(u64),

    /// A concurrency limit was exceeded.
    #[error("concurrency limit exceeded: max {max}, current {current}")]
    ConcurrencyLimitExceeded {
        /// The configured maximum concurrency.
        max: u32,
        /// The current number of in-flight operations.
        current: u32,
    },

    /// A bandwidth limit was exceeded.
    #[error("bandwidth limit exceeded")]
    BandwidthLimitExceeded,

    /// An injected fault triggered an error return.
    #[error("injected fault: errno {errno}")]
    InjectedFault {
        /// The errno value to return to the caller.
        errno: i32,
    },
}

/// Errors specific to configuration parsing and validation.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// The TOML input could not be parsed.
    #[error("TOML parse error: {0}")]
    Parse(String),

    /// A configuration value failed validation.
    #[error("validation error: {field}: {reason}")]
    Validation {
        /// The field that failed validation.
        field: String,
        /// The reason it failed.
        reason: String,
    },

    /// A required field was missing.
    #[error("missing required field: {0}")]
    MissingField(String),
}

/// A convenience type alias for Results in this crate.
pub type Result<T> = std::result::Result<T, FusecraftError>;
