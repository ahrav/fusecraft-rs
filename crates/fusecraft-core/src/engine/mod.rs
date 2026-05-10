//! Simulation engine: the lifecycle wrapper that binds every op invocation
//! through concurrency limiting, fault sampling, latency injection, bandwidth
//! throttling, and event/metric recording.
//!
//! [`SimEngine::run_op`] is the single entry point. Given an [`OpContext`] and a
//! closure that performs the "real" reply work (e.g. read synthetic bytes), it
//! returns the closure's result — or an injected errno — after blocking the
//! calling thread for the sampled duration.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{Config, OpPolicy};
use crate::error::FsError;
use crate::events::{Event, EventSink, NullEventSink, Outcome, now_unix_nanos};
use crate::limiter::{BandwidthLimiter, BlockingLimiter};
use crate::metrics::HistogramRecorder;
use crate::op::FsOp;
use crate::sampler::{SampleKey, sample_fault, sample_latency_us};

pub use self::context::OpContext;

mod context;

/// Per-op resources owned by the engine.
struct OpResources {
    policy: OpPolicy,
    limiter: BlockingLimiter,
    bandwidth: Option<BandwidthLimiter>,
}

/// The simulation engine.
///
/// Holds per-op limiters, event sink, and metrics recorder. Cheap to wrap in
/// `Arc<SimEngine>` — concurrent FUSE handlers can all call [`Self::run_op`]
/// without contention except on the underlying per-op limiter mutex.
pub struct SimEngine {
    seed: u64,
    resources: HashMap<FsOp, OpResources>,
    seq: AtomicU64,
    events: Arc<dyn EventSink>,
    metrics: Arc<HistogramRecorder>,
}

impl SimEngine {
    /// Build a new engine from a validated [`Config`] and event sink.
    ///
    /// Unspecified ops fall back to [`OpPolicy::default`].
    pub fn new(config: &Config, events: Arc<dyn EventSink>) -> Self {
        let mut resources = HashMap::with_capacity(FsOp::ALL.len());
        for &op in FsOp::ALL.iter() {
            let policy = config.ops.get(&op).cloned().unwrap_or_default();
            let limiter = BlockingLimiter::new(policy.concurrency_cap, policy.queue_cap);
            let bandwidth = policy.bandwidth.as_ref().map(BandwidthLimiter::new);
            resources.insert(
                op,
                OpResources {
                    policy,
                    limiter,
                    bandwidth,
                },
            );
        }
        Self {
            seed: config.seed,
            resources,
            seq: AtomicU64::new(0),
            events,
            metrics: Arc::new(HistogramRecorder::new()),
        }
    }

    /// Build an engine without event emission (metrics are still recorded).
    pub fn new_without_events(config: &Config) -> Self {
        Self::new(config, Arc::new(NullEventSink))
    }

    /// The master seed used to derive all sampler streams.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Shared handle to the metrics recorder.
    pub fn metrics(&self) -> Arc<HistogramRecorder> {
        Arc::clone(&self.metrics)
    }

    /// Execute an op lifecycle.
    ///
    /// Acquires the per-op limiter, samples fault + latency + bandwidth delay,
    /// sleeps the calling thread, runs `body` (unless faulted), and records an
    /// [`Event`] plus a histogram sample.
    ///
    /// Returns `body`'s result, or `Err(FsError::Errno(errno))` if a fault was
    /// injected or the limiter rejected the acquisition.
    pub fn run_op<F, T>(&self, ctx: OpContext, body: F) -> Result<T, FsError>
    where
        F: FnOnce() -> Result<T, FsError>,
    {
        let start = Instant::now();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let resources = self
            .resources
            .get(&ctx.op)
            .expect("all FsOps are registered in SimEngine::new");

        // 1. Acquire the limiter. If rejected, emit an error event and return.
        let guard = match resources.limiter.acquire() {
            Ok(g) => g,
            Err(err) => {
                let errno = err.as_errno();
                let total_us = start.elapsed().as_micros() as u64;
                self.emit_event(&ctx, seq, Outcome::Error, Some(errno), 0, 0, 0, total_us);
                self.metrics.record(ctx.op, total_us, true);
                return Err(FsError::Errno(errno));
            }
        };
        let queue_wait_us = guard.queue_wait.as_micros() as u64;

        // 2. Deterministic per-call sampling.
        //    FUSE requests are bounded by kernel limits (typically ≤ 1 MiB),
        //    but clamp explicitly so the truncation from `usize` to `u32` is
        //    intentional rather than implicit if a caller ever exceeds u32.
        let key = SampleKey {
            seed: self.seed,
            op: ctx.op,
            ino: ctx.ino,
            offset: ctx.offset,
            len: ctx.len.min(u32::MAX as usize) as u32,
            seq,
        };
        let fault_errno = sample_fault(&resources.policy.faults, key);
        let latency_us = sample_latency_us(&resources.policy.latency, key);

        // 3. Bandwidth only applies to data ops with a configured profile.
        let bandwidth_delay = match (&resources.bandwidth, ctx.op) {
            (Some(bw), FsOp::Read | FsOp::Write) => bw.reserve(ctx.len as u64),
            _ => Duration::ZERO,
        };

        // 4. Block the caller for the sampled duration. Faulted ops still wait —
        //    real filesystems frequently return errors slowly.
        let sleep_for = Duration::from_micros(latency_us) + bandwidth_delay;
        if !sleep_for.is_zero() {
            thread::sleep(sleep_for);
        }

        // 5. Execute (or skip) the reply closure.
        let result = match fault_errno {
            Some(errno) => Err(FsError::Errno(errno)),
            None => body(),
        };

        // 6. Record the outcome.
        let total_us = start.elapsed().as_micros() as u64;
        let bandwidth_delay_us = bandwidth_delay.as_micros() as u64;
        let (outcome, errno) = match &result {
            Ok(_) => (Outcome::Ok, None),
            Err(e) => (Outcome::Error, Some(e.as_errno())),
        };
        self.emit_event(
            &ctx,
            seq,
            outcome,
            errno,
            queue_wait_us,
            latency_us,
            bandwidth_delay_us,
            total_us,
        );
        self.metrics
            .record(ctx.op, total_us, matches!(outcome, Outcome::Error));

        // 7. Guard drops here, releasing the limiter slot.
        drop(guard);
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_event(
        &self,
        ctx: &OpContext,
        seq: u64,
        outcome: Outcome,
        errno: Option<i32>,
        queue_wait_us: u64,
        injected_latency_us: u64,
        bandwidth_delay_us: u64,
        total_duration_us: u64,
    ) {
        self.events.emit(&Event {
            ts_unix_nanos: now_unix_nanos(),
            seq,
            op: ctx.op,
            ino: ctx.ino,
            offset: ctx.offset,
            len: ctx.len,
            outcome,
            errno,
            queue_wait_us,
            injected_latency_us,
            bandwidth_delay_us,
            total_duration_us,
        });
    }
}

impl std::fmt::Debug for SimEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimEngine")
            .field("seed", &self.seed)
            .field("ops", &self.resources.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BandwidthProfile, FaultRule, LatencyProfile, OpPolicy};
    use parking_lot::Mutex;

    /// Collect events into a shared Vec for assertions.
    struct CollectingSink(Arc<Mutex<Vec<Event>>>);

    impl EventSink for CollectingSink {
        fn emit(&self, event: &Event) {
            self.0.lock().push(event.clone());
        }
    }

    fn zero_latency_policy() -> OpPolicy {
        OpPolicy {
            concurrency_cap: 4,
            queue_cap: 16,
            latency: LatencyProfile {
                base_us: 0,
                lognormal_median_us: 0.0,
                lognormal_sigma: 0.0,
                pareto_weight: 0.0,
                pareto_xm_us: 1.0,
                pareto_alpha: 1.5,
                max_us: 1_000_000,
            },
            bandwidth: None,
            faults: Vec::new(),
            size_tier: None,
        }
    }

    fn config_with(op: FsOp, policy: OpPolicy) -> Config {
        let mut cfg = Config::default();
        cfg.ops.insert(op, policy);
        cfg
    }

    #[test]
    fn zero_latency_runs_closure_and_returns_value() {
        let cfg = config_with(FsOp::Read, zero_latency_policy());
        let engine = SimEngine::new_without_events(&cfg);
        let ctx = OpContext::data(FsOp::Read, 100, 0, 4096);
        let result = engine.run_op(ctx, || Ok::<u32, FsError>(7)).unwrap();
        assert_eq!(result, 7);
    }

    #[test]
    fn injected_error_skips_closure() {
        let mut policy = zero_latency_policy();
        policy.faults = vec![FaultRule {
            op: FsOp::Read,
            errno: libc::EIO,
            rate: 1.0,
        }];
        let cfg = config_with(FsOp::Read, policy);
        let engine = SimEngine::new_without_events(&cfg);
        let ctx = OpContext::data(FsOp::Read, 100, 0, 4096);
        let ran = Arc::new(Mutex::new(false));
        let ran_clone = Arc::clone(&ran);
        let err = engine
            .run_op(ctx, move || {
                *ran_clone.lock() = true;
                Ok::<(), FsError>(())
            })
            .unwrap_err();
        match err {
            FsError::Errno(e) => assert_eq!(e, libc::EIO),
            other => panic!("expected EIO, got {other:?}"),
        }
        assert!(!*ran.lock(), "body should not run on injected fault");
    }

    #[test]
    fn event_is_emitted_for_success() {
        let events = Arc::new(Mutex::new(Vec::<Event>::new()));
        let sink: Arc<dyn EventSink> = Arc::new(CollectingSink(Arc::clone(&events)));
        let cfg = config_with(FsOp::Read, zero_latency_policy());
        let engine = SimEngine::new(&cfg, sink);

        engine
            .run_op(OpContext::data(FsOp::Read, 42, 0, 128), || {
                Ok::<_, FsError>(())
            })
            .unwrap();

        let events = events.lock();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].op, FsOp::Read);
        assert_eq!(events[0].ino, 42);
        assert_eq!(events[0].len, 128);
        assert!(matches!(events[0].outcome, Outcome::Ok));
        assert_eq!(events[0].errno, None);
    }

    #[test]
    fn event_is_emitted_for_injected_error() {
        let mut policy = zero_latency_policy();
        policy.faults = vec![FaultRule {
            op: FsOp::Write,
            errno: libc::ENOSPC,
            rate: 1.0,
        }];
        let events = Arc::new(Mutex::new(Vec::<Event>::new()));
        let sink: Arc<dyn EventSink> = Arc::new(CollectingSink(Arc::clone(&events)));
        let cfg = config_with(FsOp::Write, policy);
        let engine = SimEngine::new(&cfg, sink);

        let _ = engine.run_op(OpContext::data(FsOp::Write, 1, 0, 16), || {
            Ok::<_, FsError>(())
        });

        let events = events.lock();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].outcome, Outcome::Error));
        assert_eq!(events[0].errno, Some(libc::ENOSPC));
    }

    #[test]
    fn limiter_releases_after_injected_error() {
        let mut policy = zero_latency_policy();
        policy.concurrency_cap = 1;
        policy.queue_cap = 0;
        policy.faults = vec![FaultRule {
            op: FsOp::Read,
            errno: libc::EIO,
            rate: 1.0,
        }];
        let cfg = config_with(FsOp::Read, policy);
        let engine = SimEngine::new_without_events(&cfg);
        let ctx = OpContext::data(FsOp::Read, 1, 0, 0);

        // Sequential calls both see EIO — succeed at acquiring the limiter each time.
        for _ in 0..5 {
            let err = engine.run_op(ctx, || Ok::<(), FsError>(())).unwrap_err();
            match err {
                FsError::Errno(e) => assert_eq!(e, libc::EIO),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn latency_injection_blocks_caller() {
        let mut policy = zero_latency_policy();
        policy.latency.base_us = 20_000; // 20 ms
        let cfg = config_with(FsOp::Open, policy);
        let engine = SimEngine::new_without_events(&cfg);
        let ctx = OpContext::metadata(FsOp::Open, 1);
        let before = Instant::now();
        engine.run_op(ctx, || Ok::<_, FsError>(())).unwrap();
        assert!(
            before.elapsed() >= Duration::from_millis(15),
            "expected >=15ms delay, got {:?}",
            before.elapsed()
        );
    }

    #[test]
    fn seq_increments_monotonically() {
        let events = Arc::new(Mutex::new(Vec::<Event>::new()));
        let sink: Arc<dyn EventSink> = Arc::new(CollectingSink(Arc::clone(&events)));
        let cfg = config_with(FsOp::GetAttr, zero_latency_policy());
        let engine = SimEngine::new(&cfg, sink);
        for _ in 0..5 {
            engine
                .run_op(OpContext::metadata(FsOp::GetAttr, 1), || {
                    Ok::<_, FsError>(())
                })
                .unwrap();
        }
        let events = events.lock();
        let seqs: Vec<_> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn bandwidth_only_applies_to_read_write() {
        // Open with a nonsense bandwidth: should be ignored (no delay).
        let mut policy = zero_latency_policy();
        policy.bandwidth = Some(BandwidthProfile {
            bytes_per_sec: 1.0,
            burst_bytes: 0,
        });
        let cfg = config_with(FsOp::Open, policy);
        let engine = SimEngine::new_without_events(&cfg);
        let before = Instant::now();
        engine
            .run_op(OpContext::data(FsOp::Open, 1, 0, 1_000_000), || {
                Ok::<_, FsError>(())
            })
            .unwrap();
        // Without bandwidth applying, total wall-time should be well under 1s
        // (zero latency + no throttle).
        assert!(before.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn metrics_record_per_op() {
        let cfg = config_with(FsOp::Read, zero_latency_policy());
        let engine = SimEngine::new_without_events(&cfg);
        for _ in 0..10 {
            engine
                .run_op(OpContext::data(FsOp::Read, 1, 0, 128), || {
                    Ok::<_, FsError>(())
                })
                .unwrap();
        }
        let summary = engine.metrics().summary();
        let read = summary.iter().find(|s| s.op == FsOp::Read).unwrap();
        assert_eq!(read.count, 10);
        assert_eq!(read.errors, 0);
    }

    #[test]
    fn limiter_rejection_is_recorded_as_error() {
        let mut policy = zero_latency_policy();
        policy.concurrency_cap = 0;
        policy.queue_cap = 0;
        let events = Arc::new(Mutex::new(Vec::<Event>::new()));
        let sink: Arc<dyn EventSink> = Arc::new(CollectingSink(Arc::clone(&events)));
        let cfg = config_with(FsOp::Read, policy);
        let engine = SimEngine::new(&cfg, sink);

        let err = engine
            .run_op(
                OpContext::data(FsOp::Read, 1, 0, 0),
                || Ok::<_, FsError>(()),
            )
            .unwrap_err();
        match err {
            FsError::Errno(e) => assert_eq!(e, libc::EAGAIN),
            other => panic!("expected EAGAIN, got {other:?}"),
        }
        let events = events.lock();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].outcome, Outcome::Error));
        assert_eq!(events[0].errno, Some(libc::EAGAIN));
    }
}
