//! Size-tiered read policy integration tests.
//!
//! Proves that a `size_tier` on `[ops.read]` routes requests above the
//! configured `threshold_bytes` to the large-tier latency profile, while
//! requests at or below the threshold keep the base-tier profile. The test
//! picks latencies far apart (small ~ 0 ms, large = 50 ms) so timing
//! assertions are robust across CI jitter.
//!
//! Gated behind the `fuse-tests` feature and requires Linux with `/dev/fuse`;
//! otherwise the tests compile and skip at runtime.

#![cfg(unix)]

// `common` exposes a suite of helpers that other test binaries share; this
// binary only uses a subset (the direct_io path below), so silence dead-code
// warnings for the rest rather than touching the shared module.
#[allow(dead_code)]
mod common;

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fusecraft_core::config::{Config, LargeTierPolicy, LatencyProfile, SizeTier};
use fusecraft_core::events::NullEventSink;
use fusecraft_core::op::FsOp;
use fusecraft_fuser::MountHandle;

/// Skip a test early if FUSE is not available.
macro_rules! skip_unless_fuse {
    () => {
        if !common::fuse_available() {
            eprintln!("SKIP: FUSE not available (feature disabled or not on Linux)");
            return;
        }
    };
}

/// Mount with `direct_io` enabled, or return `None` if the host's
/// `fusermount3` rejects the option. Some builds refuse unknown mount
/// options; in that case the test is a pure no-op rather than a failure, per
/// the pattern already used in `advanced_mount::direct_io_mount_round_trip`.
fn try_mount_with_direct_io(config: &Config) -> Option<(MountHandle, tempfile::TempDir)> {
    match common::try_mount_test_fs_with_sink(config, Arc::new(NullEventSink)) {
        Ok(pair) => Some(pair),
        Err(e) if e.to_string().to_lowercase().contains("direct_io") => {
            eprintln!("SKIP size-tiering test: mount rejected direct_io: {e}");
            None
        }
        Err(e) => panic!("unexpected mount failure for size-tiering test: {e:?}"),
    }
}

/// Small reads hit the base tier (effectively zero latency); a single large
/// read hits the large tier and pays a ~50 ms base delay. We assert the large
/// read's elapsed time is orders of magnitude above the small read's — far
/// more than any plausible CI-jitter margin — so the only thing that can
/// explain the gap is the tier swap.
#[test]
fn large_read_routes_to_large_tier_latency() {
    skip_unless_fuse!();

    // Threshold intentionally small (8 KiB) so a single `pread` with a small
    // buffer cleanly stays in the base tier while a single `pread` with a
    // moderate buffer cleanly crosses it. The kernel will not split a 64 KiB
    // read into multiple FUSE READ requests at the default `max_read`, so the
    // large-tier latency is paid exactly once per userspace `pread` call.
    const THRESHOLD_BYTES: u64 = 8 * 1024;
    const SMALL_LEN: usize = 4 * 1024;
    const LARGE_LEN: usize = 64 * 1024;

    let mut config = common::default_test_config();
    // Make the backing file large enough to cover both read ranges.
    config.files.file_size_bytes = 256 * 1024;
    // Disable default_permissions so the read succeeds regardless of the
    // process's uid/gid mapping in sandboxed CI environments.
    config.mount.default_permissions = false;
    // CRITICAL: enable direct_io to bypass the kernel's readahead path. Without
    // this, a userspace 4 KiB read triggers a 128 KiB kernel readahead FUSE
    // READ request — `ctx.len` then crosses any small threshold and the whole
    // tier distinction becomes unobservable from timing alone.
    config.mount.direct_io = true;

    let read_policy = config.ops.get_mut(&FsOp::Read).expect("read policy");
    // Base (small) tier: zero latency.
    read_policy.latency = LatencyProfile {
        base_us: 0,
        lognormal_median_us: 0.0,
        lognormal_sigma: 0.0,
        pareto_weight: 0.0,
        pareto_xm_us: 1.0,
        pareto_alpha: 1.5,
        max_us: 1_000_000,
    };
    // Large tier: 50 ms base latency, no lognormal/pareto noise so timing is
    // dominated by the deterministic base.
    read_policy.size_tier = Some(SizeTier {
        threshold_bytes: THRESHOLD_BYTES,
        large: LargeTierPolicy {
            latency: LatencyProfile {
                base_us: 50_000,
                lognormal_median_us: 0.0,
                lognormal_sigma: 0.0,
                pareto_weight: 0.0,
                pareto_xm_us: 1.0,
                pareto_alpha: 1.5,
                max_us: 1_000_000,
            },
            bandwidth: None,
            faults: Vec::new(),
        },
    });

    let Some((_handle, mount_dir)) = try_mount_with_direct_io(&config) else {
        return;
    };

    let file_path = common::object_path(mount_dir.path(), 0);
    let file = OpenOptions::new()
        .read(true)
        .open(&file_path)
        .expect("open for read");

    // Warm-up / baseline: a small read at offset 0. We measure this to prove
    // the base tier is fast, not to pin an absolute floor — CI timing is
    // noisy, so we only assert the *ratio* between small and large.
    let mut small_buf = vec![0u8; SMALL_LEN];
    let small_start = Instant::now();
    let n_small = file
        .read_at(&mut small_buf, 0)
        .expect("base-tier read should succeed");
    let small_elapsed = small_start.elapsed();
    assert_eq!(
        n_small, SMALL_LEN,
        "base-tier read should return the full requested byte count"
    );

    // Single large read at a non-overlapping offset. Use `read_at` (pread) to
    // skip repositioning the implicit fd offset and to avoid any interaction
    // with the libstd `BufReader` wrapping story.
    let mut large_buf = vec![0u8; LARGE_LEN];
    let large_start = Instant::now();
    let n_large = file
        .read_at(&mut large_buf, (64 * 1024) as u64)
        .expect("large-tier read should succeed");
    let large_elapsed = large_start.elapsed();
    assert_eq!(
        n_large, LARGE_LEN,
        "large-tier read should return the full requested byte count"
    );

    // Large-tier hard floor: the injected base_us is 50 ms, so the large
    // read must take at least ~40 ms (loose bound for CI jitter).
    assert!(
        large_elapsed >= Duration::from_millis(40),
        "expected large-tier read to block for ~50ms base_us, got {large_elapsed:?}"
    );

    // Small-tier ceiling: the base tier injects 0 latency, so even with
    // kernel + FUSE roundtrip overhead the small read must finish well
    // under the large-tier floor. A 5x safety margin keeps this robust.
    let ratio = large_elapsed.as_micros() as f64 / small_elapsed.as_micros().max(1) as f64;
    assert!(
        ratio >= 5.0,
        "expected large-tier read to be at least 5x slower than base-tier read, \
         got small={small_elapsed:?} large={large_elapsed:?} ratio={ratio:.2}"
    );
}

/// A read strictly at `len == threshold_bytes` must stay in the base tier —
/// `select_tier` uses `>` (not `>=`), so exactly-at-threshold is a small op.
/// This test pins the boundary so a future refactor can't silently flip the
/// comparison without breaking a visible test.
#[test]
fn read_at_exact_threshold_stays_in_base_tier() {
    skip_unless_fuse!();

    const THRESHOLD_BYTES: u64 = 16 * 1024;

    let mut config = common::default_test_config();
    config.files.file_size_bytes = 64 * 1024;
    config.mount.default_permissions = false;
    // direct_io pins the FUSE READ size to the userspace request length;
    // without it kernel readahead rewrites `ctx.len` and the boundary test
    // becomes a coin-flip. See `large_read_routes_to_large_tier_latency`.
    config.mount.direct_io = true;

    let read_policy = config.ops.get_mut(&FsOp::Read).expect("read policy");
    read_policy.latency = LatencyProfile {
        base_us: 0,
        lognormal_median_us: 0.0,
        lognormal_sigma: 0.0,
        pareto_weight: 0.0,
        pareto_xm_us: 1.0,
        pareto_alpha: 1.5,
        max_us: 1_000_000,
    };
    read_policy.size_tier = Some(SizeTier {
        threshold_bytes: THRESHOLD_BYTES,
        large: LargeTierPolicy {
            // If the boundary were accidentally `>=`, this 200 ms latency
            // would surface in the test elapsed time.
            latency: LatencyProfile {
                base_us: 200_000,
                lognormal_median_us: 0.0,
                lognormal_sigma: 0.0,
                pareto_weight: 0.0,
                pareto_xm_us: 1.0,
                pareto_alpha: 1.5,
                max_us: 1_000_000,
            },
            bandwidth: None,
            faults: Vec::new(),
        },
    });

    let Some((_handle, mount_dir)) = try_mount_with_direct_io(&config) else {
        return;
    };

    let file_path = common::object_path(mount_dir.path(), 0);
    let mut file = OpenOptions::new()
        .read(true)
        .open(&file_path)
        .expect("open for read");

    let mut buf = vec![0u8; THRESHOLD_BYTES as usize];
    let start = Instant::now();
    let n = file.read(&mut buf).expect("threshold read should succeed");
    let elapsed = start.elapsed();

    assert_eq!(n, THRESHOLD_BYTES as usize);
    // Loose ceiling — the large-tier floor is 200 ms, so anything under
    // 100 ms proves the base tier was selected.
    assert!(
        elapsed < Duration::from_millis(100),
        "read at exact threshold must use base tier (expected < 100ms), \
         got {elapsed:?}"
    );
}
