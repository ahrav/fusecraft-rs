//! Limiter module: blocking concurrency control and bandwidth rate limiting.
//!
//! This module provides two key primitives for FUSE handler threads:
//!
//! - [`BlockingLimiter`]: A semaphore-like concurrency limiter that blocks
//!   calling threads when the cap is reached (using `parking_lot`).
//! - [`BandwidthLimiter`]: A token-bucket rate limiter that returns a
//!   [`std::time::Duration`] for the caller to sleep, without blocking internally.
//!
//! Both types are `Send + Sync` and designed for use from real OS threads
//! (no async runtime required).

pub mod bandwidth;
pub mod concurrency;

pub use bandwidth::{BandwidthLimiter, BandwidthStats};
pub use concurrency::{BlockingLimiter, LimiterGuard, LimiterStats};
