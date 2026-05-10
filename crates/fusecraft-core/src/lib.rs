//! # fusecraft-core
//!
//! Deterministic simulator core for [fusecraft](https://github.com/ahrav/fusecraft-rs):
//! the policy, sampling, concurrency, and metrics logic that makes a FUSE
//! mount behave like a flaky, latency-prone, bandwidth-limited filesystem —
//! reproducibly.
//!
//! This crate contains no FUSE kernel glue. It exposes a single simulation
//! engine that filesystem adapters (such as
//! [`fusecraft-fuser`](https://crates.io/crates/fusecraft-fuser)) call into
//! from their handler threads.
//!
//! ## What this crate models
//!
//! fusecraft models only syscall-visible behavior:
//!
//! - **Latency** — per-op delay drawn from `base + lognormal + Pareto tail`, clamped.
//! - **Faults** — probabilistic errno injection (`EIO`, `ENOENT`, `ESTALE`, `ENOSPC`, `EAGAIN`, `EINTR`).
//! - **Queueing** — per-op concurrency cap with bounded wait queue; overflow returns `EAGAIN`.
//! - **Bandwidth** — token-bucket throttle on `read`/`write`.
//! - **Determinism** — every sample is a pure function of `(seed, op, ino, offset, len, seq)`.
//!
//! fusecraft does **not** simulate NFS, S3, SMB, EBS, ext4, xfs, or any exact
//! storage protocol. See [`docs/fidelity.md`] in the repository for the
//! full non-goals list.
//!
//! [`docs/fidelity.md`]: https://github.com/ahrav/fusecraft-rs/blob/main/docs/fidelity.md
//!
//! ## The central abstraction: [`engine::SimEngine::run_op`]
//!
//! Every operation flows through one function. Given an [`engine::OpContext`]
//! and a closure producing the "real" reply, the engine wraps it in a fixed
//! 7-step lifecycle:
//!
//! 1. Acquire the per-op [`limiter::BlockingLimiter`] slot (rejection → `EAGAIN`).
//! 2. Build a [`sampler::SampleKey`] from `(seed, op, ino, offset, len, seq)`.
//! 3. Draw fault errno + latency + bandwidth delay via the deterministic samplers.
//! 4. Sleep the calling thread for `latency + bandwidth_delay`.
//! 5. Run the closure unless a fault was injected.
//! 6. Emit one [`events::Event`] and one [`metrics::HistogramRecorder`] sample.
//! 7. Release the concurrency slot.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use fusecraft_core::config::Config;
//! use fusecraft_core::engine::{OpContext, SimEngine};
//! use fusecraft_core::error::FsError;
//! use fusecraft_core::events::NullEventSink;
//! use fusecraft_core::op::FsOp;
//!
//! let config = Config::default();
//! config.validate().expect("valid config");
//! let engine = SimEngine::new(&config, Arc::new(NullEventSink));
//!
//! let ctx = OpContext::data(FsOp::Read, 42, 0, 4096);
//! let result: Result<Vec<u8>, FsError> = engine.run_op(ctx, || {
//!     // Your "real" read logic here — or return a stub.
//!     Ok(vec![0u8; 4096])
//! });
//! ```
//!
//! ## Pluggable models
//!
//! The engine is generic over two traits callers implement:
//!
//! - [`content::ContentModel`] — how bytes are produced for `read`/`write`.
//!   [`content::DeterministicContent`] is the reference impl.
//! - [`namespace::NamespaceModel`] — directory layout and inode → name mapping.
//!   [`namespace::FlatObjectNamespace`] is the reference impl.
//!
//! ## Configuration
//!
//! [`config::Config`] is derived from Serde and round-trips through TOML. See
//! `docs/config.md` in the repository for the full key reference.
//!
//! ## Hot-path discipline
//!
//! - [`events::EventSink::emit`] must not return errors or panic.
//! - No async, no Tokio. The engine blocks `std::thread` via
//!   `parking_lot::{Mutex, Condvar}` inside the limiter.

pub mod config;
pub mod error;
pub mod op;

pub mod content;
pub mod events;
pub mod limiter;
pub mod metrics;
pub mod namespace;
pub mod sampler;

pub mod engine;
