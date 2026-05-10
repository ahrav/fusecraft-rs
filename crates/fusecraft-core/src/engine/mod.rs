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

use crate::config::{Config, FaultRule, LatencyProfile, OpPolicy};
use crate::error::FsError;
use crate::events::{Event, EventSink, NullEventSink, Outcome, now_unix_nanos};
use crate::limiter::{BandwidthLimiter, BlockingLimiter};
use crate::metrics::HistogramRecorder;
use crate::op::FsOp;
use crate::sampler::{SampleKey, sample_fault, sample_latency_us};

pub use self::context::OpContext;

mod context;

/// Per-op resources owned by the engine.
///
/// When `policy.size_tier` is configured, `large_bandwidth` is pre-materialized
/// at engine construction so the hot path does not allocate a limiter per call.
struct OpResources {
    policy: OpPolicy,
    limiter: BlockingLimiter,
    bandwidth: Option<BandwidthLimiter>,
    /// Pre-built bandwidth limiter for the large tier, if configured.
    large_bandwidth: Option<BandwidthLimiter>,
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
            let large_bandwidth = policy
                .size_tier
                .as_ref()
                .and_then(|t| t.large.bandwidth.as_ref())
                .map(BandwidthLimiter::new);
            resources.insert(
                op,
                OpResources {
                    policy,
                    limiter,
                    bandwidth,
                    large_bandwidth,
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
        //
        //    Select the effective (latency, faults, bandwidth) triple based on
        //    the size tier: if this op is Read or Write, a `size_tier` is
        //    configured, and `ctx.len` exceeds the threshold, swap in the
        //    large-tier profile. SampleKey is unchanged — tier selection is a
        //    policy lookup, not a sampler input, so determinism is preserved.
        let (latency_profile, faults, bandwidth) = select_tier(resources, ctx.op, ctx.len);

        let key = SampleKey {
            seed: self.seed,
            op: ctx.op,
            ino: ctx.ino,
            offset: ctx.offset,
            len: ctx.len.min(u32::MAX as usize) as u32,
            seq,
        };
        let fault_errno = sample_fault(faults, key);
        let latency_us = sample_latency_us(latency_profile, key);

        // 3. Bandwidth only applies to data ops with a configured profile.
        let bandwidth_delay = match (bandwidth, ctx.op) {
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

/// Pick the effective latency profile, fault rules, and bandwidth limiter for
/// this call.
///
/// Returns the base-policy triple unless all three conditions hold:
/// - `op` is `Read` or `Write`, and
/// - `resources.policy.size_tier` is configured, and
/// - `len` is strictly greater than `size_tier.threshold_bytes`.
///
/// Kept as a free function so it has no access to shared mutable state —
/// matches the pure-sampler discipline used elsewhere on the hot path.
fn select_tier(
    resources: &OpResources,
    op: FsOp,
    len: usize,
) -> (&LatencyProfile, &[FaultRule], Option<&BandwidthLimiter>) {
    if matches!(op, FsOp::Read | FsOp::Write) {
        if let Some(tier) = resources.policy.size_tier.as_ref() {
            if (len as u64) > tier.threshold_bytes {
                return (
                    &tier.large.latency,
                    &tier.large.faults,
                    resources.large_bandwidth.as_ref(),
                );
            }
        }
    }
    (
        &resources.policy.latency,
        &resources.policy.faults,
        resources.bandwidth.as_ref(),
    )
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
    use crate::config::{
        BandwidthProfile, FaultRule, LargeTierPolicy, LatencyProfile, OpPolicy, SizeTier,
    };
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

    // --- Size-tier routing ---

    /// Deterministic profile at a single fixed latency.
    fn fixed_latency_profile(us: u64) -> LatencyProfile {
        LatencyProfile {
            base_us: us,
            lognormal_median_us: 0.0,
            lognormal_sigma: 0.0,
            pareto_weight: 0.0,
            pareto_xm_us: 1.0,
            pareto_alpha: 1.5,
            max_us: us.max(1_000_000),
        }
    }

    #[test]
    fn size_tier_routes_reads_below_threshold_to_base() {
        // Base = 5ms, large = 200ms. A 500-byte read must hit the 5ms base.
        let mut policy = zero_latency_policy();
        policy.latency = fixed_latency_profile(5_000);
        policy.size_tier = Some(SizeTier {
            threshold_bytes: 1024,
            large: LargeTierPolicy {
                latency: fixed_latency_profile(200_000),
                bandwidth: None,
                faults: Vec::new(),
            },
        });
        let cfg = config_with(FsOp::Read, policy);
        let engine = SimEngine::new_without_events(&cfg);
        let ctx = OpContext::data(FsOp::Read, 1, 0, 500);
        let before = Instant::now();
        engine.run_op(ctx, || Ok::<_, FsError>(())).unwrap();
        let elapsed = before.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "expected base-tier latency (~5ms), got {elapsed:?}"
        );
    }

    #[test]
    fn size_tier_routes_reads_above_threshold_to_large() {
        // Base = 0us, large = 50ms. A 2000-byte read must hit the 50ms tier.
        let mut policy = zero_latency_policy();
        policy.size_tier = Some(SizeTier {
            threshold_bytes: 1024,
            large: LargeTierPolicy {
                latency: fixed_latency_profile(50_000),
                bandwidth: None,
                faults: Vec::new(),
            },
        });
        let cfg = config_with(FsOp::Read, policy);
        let engine = SimEngine::new_without_events(&cfg);
        let ctx = OpContext::data(FsOp::Read, 1, 0, 2_000);
        let before = Instant::now();
        engine.run_op(ctx, || Ok::<_, FsError>(())).unwrap();
        let elapsed = before.elapsed();
        assert!(
            elapsed >= Duration::from_millis(40),
            "expected >=40ms large-tier latency, got {elapsed:?}"
        );
    }

    #[test]
    fn size_tier_ignored_on_metadata_ops() {
        // Open with huge ctx.len must not route to the large tier even if
        // size_tier is (erroneously bypassing validation) set. Here we build
        // the engine directly without going through validate() to prove that
        // the engine's own routing gates on op kind.
        let mut policy = zero_latency_policy();
        policy.size_tier = Some(SizeTier {
            threshold_bytes: 1,
            large: LargeTierPolicy {
                latency: fixed_latency_profile(250_000),
                bandwidth: None,
                faults: vec![FaultRule {
                    op: FsOp::Open,
                    errno: libc::EIO,
                    rate: 1.0,
                }],
            },
        });
        let cfg = config_with(FsOp::Open, policy);
        let engine = SimEngine::new_without_events(&cfg);
        let ctx = OpContext::data(FsOp::Open, 1, 0, 1_000_000);
        let before = Instant::now();
        engine.run_op(ctx, || Ok::<_, FsError>(())).unwrap();
        let elapsed = before.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "metadata op must use base policy, got {elapsed:?}"
        );
    }

    #[test]
    fn size_tier_bandwidth_throttles_large_reads() {
        // Zero base latency. Large tier throttles at 1_000_000 B/s with a 100-byte
        // burst. Small read (50 bytes) stays within burst and is fast. Large
        // read (200_000 bytes) needs ~0.2s wait beyond burst.
        let mut policy = zero_latency_policy();
        policy.latency = fixed_latency_profile(0);
        policy.size_tier = Some(SizeTier {
            threshold_bytes: 1024,
            large: LargeTierPolicy {
                latency: fixed_latency_profile(0),
                bandwidth: Some(BandwidthProfile {
                    bytes_per_sec: 1_000_000.0,
                    burst_bytes: 100,
                }),
                faults: Vec::new(),
            },
        });
        let cfg = config_with(FsOp::Read, policy);
        let engine = SimEngine::new_without_events(&cfg);

        let before = Instant::now();
        engine
            .run_op(OpContext::data(FsOp::Read, 1, 0, 50), || {
                Ok::<_, FsError>(())
            })
            .unwrap();
        let small_elapsed = before.elapsed();
        assert!(
            small_elapsed < Duration::from_millis(50),
            "small read must bypass large-tier bandwidth, got {small_elapsed:?}"
        );

        let before = Instant::now();
        engine
            .run_op(OpContext::data(FsOp::Read, 1, 0, 200_000), || {
                Ok::<_, FsError>(())
            })
            .unwrap();
        let large_elapsed = before.elapsed();
        assert!(
            large_elapsed >= Duration::from_millis(150),
            "large read must be throttled (>=150ms), got {large_elapsed:?}"
        );
    }

    #[test]
    fn size_tier_faults_apply_above_threshold() {
        let mut policy = zero_latency_policy();
        policy.size_tier = Some(SizeTier {
            threshold_bytes: 1024,
            large: LargeTierPolicy {
                latency: fixed_latency_profile(0),
                bandwidth: None,
                faults: vec![FaultRule {
                    op: FsOp::Read,
                    errno: libc::EIO,
                    rate: 1.0,
                }],
            },
        });
        let cfg = config_with(FsOp::Read, policy);
        let engine = SimEngine::new_without_events(&cfg);

        // Small read (below threshold): no base fault → Ok.
        let ok = engine
            .run_op(OpContext::data(FsOp::Read, 1, 0, 100), || {
                Ok::<u8, FsError>(42)
            })
            .unwrap();
        assert_eq!(ok, 42);

        // Large read (above threshold): large tier fires EIO.
        let err = engine
            .run_op(OpContext::data(FsOp::Read, 1, 0, 4096), || {
                Ok::<u8, FsError>(42)
            })
            .unwrap_err();
        match err {
            FsError::Errno(e) => assert_eq!(e, libc::EIO),
            other => panic!("expected EIO, got {other:?}"),
        }
    }

    #[test]
    fn size_tier_uses_inherited_concurrency_cap() {
        use std::sync::Barrier;
        use std::thread;

        // Single-slot limiter. A large-tier read that sleeps on latency must
        // block a second read (small or large) from acquiring the slot, proving
        // the limiter is shared across tiers.
        let mut policy = zero_latency_policy();
        policy.concurrency_cap = 1;
        policy.queue_cap = 4;
        policy.latency = fixed_latency_profile(0);
        policy.size_tier = Some(SizeTier {
            threshold_bytes: 1024,
            large: LargeTierPolicy {
                latency: fixed_latency_profile(150_000), // 150ms
                bandwidth: None,
                faults: Vec::new(),
            },
        });
        let cfg = config_with(FsOp::Read, policy);
        let engine = Arc::new(SimEngine::new_without_events(&cfg));

        let barrier = Arc::new(Barrier::new(2));
        let e1 = Arc::clone(&engine);
        let b1 = Arc::clone(&barrier);
        let large_handle = thread::spawn(move || {
            b1.wait();
            e1.run_op(OpContext::data(FsOp::Read, 1, 0, 10_000), || {
                Ok::<_, FsError>(())
            })
            .unwrap();
        });

        let e2 = Arc::clone(&engine);
        let b2 = Arc::clone(&barrier);
        let small_handle = thread::spawn(move || {
            b2.wait();
            // Give the large op a head start to win the limiter slot.
            thread::sleep(Duration::from_millis(10));
            let before = Instant::now();
            e2.run_op(OpContext::data(FsOp::Read, 1, 0, 100), || {
                Ok::<_, FsError>(())
            })
            .unwrap();
            before.elapsed()
        });

        large_handle.join().unwrap();
        let small_wait = small_handle.join().unwrap();
        assert!(
            small_wait >= Duration::from_millis(80),
            "small read should have been blocked by the large-tier op holding the \
             single limiter slot (>=80ms), got {small_wait:?}"
        );
    }
}
