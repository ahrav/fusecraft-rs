//! Extended FUSE integration tests.
//!
//! These cover behaviours that the basic mount/read/write suites don't exercise:
//! `statfs` synthetic constants, concurrency-cap rejection with `EAGAIN`,
//! observable bandwidth throttling, `direct_io` mounts, and the JSONL event
//! sink end-to-end — including determinism across two runs with the same seed.
//!
//! Like the other FUSE integration tests they are gated on the `fuse-tests`
//! feature and Linux with `/dev/fuse`. Without both, the tests compile and
//! skip at runtime.
//!
//! The test file itself is `#![cfg(unix)]`-gated because it uses Unix-only
//! APIs directly (`libc::statvfs`, `std::os::unix::ffi::OsStrExt::as_bytes`).
//! The rest of the `fusecraft-fuser` crate is transitively Unix-only through
//! the `fuser` dependency, but stating the constraint here makes the
//! compile-time contract explicit and keeps rustdoc happy on non-Unix hosts.

#![cfg(unix)]

mod common;

use std::ffi::CString;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use fusecraft_core::config::BandwidthProfile;
use fusecraft_core::events::JsonlEventSink;
use fusecraft_core::op::FsOp;

/// Skip a test early if FUSE is not available.
macro_rules! skip_unless_fuse {
    () => {
        if !common::fuse_available() {
            eprintln!("SKIP: FUSE not available (feature disabled or not on Linux)");
            return;
        }
    };
}

#[test]
fn statfs_reports_synthetic_constants() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    // `statvfs(2)` is the portable way to reach the kernel's statfs reply.
    // fusecraft-fuser hard-codes block size 4096 and namelen 255; this test
    // pins those constants so a future edit to `lib.rs::statfs` can't silently
    // change what callers observe.
    let c_path =
        CString::new(mount_dir.path().as_os_str().as_bytes()).expect("mount path contains no NUL");
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    assert_eq!(rc, 0, "statvfs failed: {}", std::io::Error::last_os_error());

    // `statvfs` field widths differ per-target (u32 on some libcs, u64 on
    // others). The `{ field }` bracing lets rustc infer the type from the
    // integer literal on the RHS without a redundant-cast warning, while
    // still pinning the numeric value the kernel returned.
    assert_eq!({ stat.f_bsize }, 4096, "block size mismatch");
    assert_eq!({ stat.f_frsize }, 4096, "fragment size mismatch");
    assert_eq!({ stat.f_namemax }, 255, "namelen mismatch");
}

#[test]
fn concurrency_cap_rejects_with_eagain() {
    skip_unless_fuse!();

    // Shape the read policy so a single slow read holds the only slot while
    // the queue (cap=0) immediately rejects the contender with EAGAIN. 200 ms
    // of base latency is plenty of head-room for the second thread's open+read
    // to arrive before the first releases the guard.
    //
    // NOTE: this test mounts via `common::mount_multi_threaded_fs` instead of
    // the standard `mount_test_fs` helper. fuser's default `n_threads = 1`
    // serialises every FUSE request on a single worker thread, which would
    // hide the limiter entirely (the second read would just wait in the
    // worker queue rather than racing for the limiter slot). We bump
    // `n_threads` to 2 so the two handler threads really do try to acquire
    // the limiter concurrently.
    let mut config = common::default_test_config();
    let read_policy = config.ops.get_mut(&FsOp::Read).expect("read policy");
    read_policy.concurrency_cap = 1;
    read_policy.queue_cap = 0;
    read_policy.latency.base_us = 200_000;

    let (_handle, mount_dir) = common::mount_multi_threaded_fs(&config, 2);
    // Use two distinct objects so the Linux page cache can't short-circuit
    // the contender's read with bytes produced by the winner — the read
    // limiter is per-op (not per-inode), so contention still applies across
    // different files, while the separate page-cache scopes force both reads
    // to traverse the FUSE handler.
    let path_a = common::object_path(mount_dir.path(), 0);
    let path_b = common::object_path(mount_dir.path(), 1);

    let barrier = Arc::new(Barrier::new(2));
    let t1 = {
        let path = path_a.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            std::fs::read(&path)
        })
    };
    let t2 = {
        let path = path_b.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            // Small spin-free sleep so the second read arrives after the first
            // has acquired the limiter slot — without this the two threads race
            // fairly and either of them could be the "winner", making the
            // "exactly one EAGAIN" assertion flaky.
            std::thread::sleep(Duration::from_millis(20));
            std::fs::read(&path)
        })
    };

    let r1 = t1.join().expect("thread 1 join");
    let r2 = t2.join().expect("thread 2 join");

    let mut successes = 0usize;
    let mut eagain = 0usize;
    for r in [&r1, &r2] {
        match r {
            Ok(_) => successes += 1,
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => eagain += 1,
            Err(e) => panic!("unexpected error: {e:?}"),
        }
    }
    assert_eq!(successes, 1, "exactly one read should succeed");
    assert_eq!(eagain, 1, "exactly one read should see EAGAIN");
}

#[test]
fn bandwidth_throttle_observable() {
    skip_unless_fuse!();

    // 1 MiB/s sustained with a small 4 KiB burst, reading a 1 MiB file. The
    // reservation math should impose roughly a full second of delay. We use a
    // loose 500 ms floor so CI jitter, kernel batching, and the 4 KiB burst
    // allowance don't cause flakes.
    let mut config = common::default_test_config();
    config.files.file_size_bytes = 1 << 20; // 1 MiB
    let read_policy = config.ops.get_mut(&FsOp::Read).expect("read policy");
    read_policy.concurrency_cap = 8;
    read_policy.queue_cap = 32;
    read_policy.latency.base_us = 0;
    read_policy.bandwidth = Some(BandwidthProfile {
        bytes_per_sec: 1_048_576.0,
        burst_bytes: 4096,
    });

    let (_handle, mount_dir) = common::mount_test_fs(&config);
    let file_path = common::object_path(mount_dir.path(), 0);

    let start = Instant::now();
    let data = std::fs::read(&file_path).expect("read throttled file");
    let elapsed = start.elapsed();

    assert_eq!(data.len(), config.files.file_size_bytes as usize);
    assert!(
        elapsed >= Duration::from_millis(500),
        "expected >=500ms with 1 MiB/s throttle on 1 MiB, got {elapsed:?}"
    );
}

#[test]
fn direct_io_mount_round_trip() {
    skip_unless_fuse!();

    // First, capture the baseline deterministic bytes for object 0 without
    // direct_io. This lets us assert the direct_io path returns byte-identical
    // content — `DeterministicContent` is seeded by `(seed, ino, offset)`, so
    // any drift would indicate a regression in how the adapter serves reads
    // when the page cache is bypassed.
    let baseline_config = common::default_test_config();
    let baseline_bytes = {
        let (_handle, mount_dir) = common::mount_test_fs(&baseline_config);
        let file_path = common::object_path(mount_dir.path(), 0);
        std::fs::read(&file_path).expect("baseline read")
    };

    let mut config = common::default_test_config();
    config.mount.direct_io = true;

    // Some `fusermount3` builds reject the `direct_io` mount option
    // (it is not on the userspace-accepted list on all hosts — the
    // per-file `FOPEN_DIRECT_IO` open flag is the portable path). If the
    // mount itself fails *with an error that clearly names `direct_io`*,
    // skip the rest of the test rather than failing the suite on an
    // environment-specific limitation of the adapter. Any other mount
    // failure is a real regression and must fail the test — the previous
    // catch-all `Err(_) => return` arm masked genuine adapter bugs.
    let mount_result = common::try_mount_test_fs_with_sink(
        &config,
        Arc::new(fusecraft_core::events::NullEventSink),
    );
    let (_handle, mount_dir) = match mount_result {
        Ok(pair) => pair,
        Err(e) if e.to_string().to_lowercase().contains("direct_io") => {
            eprintln!("SKIP direct_io_mount_round_trip: mount rejected direct_io: {e}");
            return;
        }
        Err(e) => panic!("unexpected mount failure for direct_io test: {e:?}"),
    };

    let file_path = common::object_path(mount_dir.path(), 0);
    let data = std::fs::read(&file_path).expect("direct_io read");

    assert_eq!(data.len(), config.files.file_size_bytes as usize);
    assert_eq!(
        data, baseline_bytes,
        "direct_io mount should return the same deterministic bytes"
    );
}

#[test]
fn jsonl_event_sink_writes_one_event_per_op() {
    skip_unless_fuse!();

    let tmp = common::tempdir();
    let jsonl_path = tmp.path().join("events.jsonl");

    let mut config = common::default_test_config();
    config.metrics.jsonl_path = Some(jsonl_path.clone());

    let sink = Arc::new(JsonlEventSink::create(&jsonl_path).expect("create jsonl sink"));
    // Hold our own reference so we can flush after the mount handle drops.
    let sink_for_flush: Arc<JsonlEventSink> = Arc::clone(&sink);

    {
        let (_handle, mount_dir) = common::mount_test_fs_with_sink(
            &config,
            sink as Arc<dyn fusecraft_core::events::EventSink>,
        );

        // Drive a known sequence of ops:
        //   - open + read + release on object 0
        //   - getattr on object 1
        let file_path_0 = common::object_path(mount_dir.path(), 0);
        let file_path_1 = common::object_path(mount_dir.path(), 1);

        {
            let mut f = OpenOptions::new()
                .read(true)
                .open(&file_path_0)
                .expect("open object 0");
            let mut buf = vec![0u8; 128];
            let _ = f.read(&mut buf).expect("read object 0");
            // `drop(f)` triggers close(2) -> FLUSH + RELEASE handlers.
        }
        let _meta = std::fs::metadata(&file_path_1).expect("stat object 1");
    }
    // The BackgroundSession has been dropped; force any buffered events out.
    sink_for_flush.flush().expect("flush sink");

    let lines = common::read_jsonl_events(&jsonl_path);
    assert!(
        !lines.is_empty(),
        "JSONL sink should record at least one event"
    );

    // Every line must be a JSON object with the fields we require downstream.
    for line in &lines {
        assert!(
            line.starts_with('{') && line.ends_with('}'),
            "line is not a JSON object: {line}"
        );
        assert!(
            common::json_field(line, "op").is_some(),
            "line missing op field: {line}"
        );
        assert!(
            common::json_field(line, "seq").is_some(),
            "line missing seq field: {line}"
        );
        assert!(
            common::json_field(line, "outcome").is_some(),
            "line missing outcome field: {line}"
        );
    }

    let ops_seen: std::collections::HashSet<String> = lines
        .iter()
        .filter_map(|l| common::json_field(l, "op").map(|s| s.to_owned()))
        .collect();
    // FsOp serializes in lowercase (see `#[serde(rename_all = "lowercase")]`).
    //
    // Only the ops the test directly drives are required:
    //   - `open`     (from `OpenOptions::open`)
    //   - `read`     (from `f.read`)
    //   - `release`  (from `drop(f)`)
    //
    // `getattr` was intentionally removed from this set. `FaultFs::lookup`
    // returns attributes with a nonzero entry TTL (1s by default), so many
    // Linux kernels satisfy the downstream `std::fs::metadata` call from
    // lookup cache without issuing a distinct `getattr` FUSE request. That
    // would make this assertion a host/kernel-dependent coin-flip; we assert
    // only on ops the workload is guaranteed to emit.
    for expected in ["open", "read", "release"] {
        assert!(
            ops_seen.contains(expected),
            "expected op {expected:?} in JSONL events, got: {ops_seen:?}"
        );
    }

    // Verify the core "one event per op" invariant the test name promises.
    //
    // `SimEngine::run_op` calls `AtomicU64::fetch_add(1, ...)` exactly once
    // per invocation, passes the returned `seq` into the sample key, and
    // emits exactly one event before releasing the limiter slot. Therefore,
    // if the sink writes one event per `run_op` call, the `seq` values
    // across all emitted events must form a contiguous range starting at 0
    // (no holes → no dropped events; no duplicates → no double-emits).
    //
    // A previous version of this test only checked set membership of op
    // names, which collapsed duplicated events and could not catch a sink
    // that emitted the same event twice. The seq-density check below is
    // the strongest form of that invariant that is observable from the
    // JSONL output alone.
    let mut seqs: Vec<u64> = lines
        .iter()
        .filter_map(|l| common::json_field(l, "seq")?.parse::<u64>().ok())
        .collect();
    assert_eq!(
        seqs.len(),
        lines.len(),
        "every JSONL line must contain a parseable `seq`; lines={lines:?}"
    );
    seqs.sort_unstable();
    for (i, s) in seqs.iter().enumerate() {
        assert_eq!(
            *s as usize, i,
            "seq gap or duplicate at position {i}: seq={s}, seqs={seqs:?} — \
             this violates the one-event-per-op invariant"
        );
    }

    // Ops driven *exactly once* by the workload must each show up *exactly
    // once*. `open` is the open(2) syscall on object 0 (single OpenOptions),
    // and `release` is its matching close (single `drop(f)`). The kernel
    // never splits either into multiple FUSE requests. `read` is left as
    // set-membership above because some kernels may split a read into
    // multiple FUSE READ requests when their max_read limit is below the
    // user-space buffer size.
    let mut op_counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for line in &lines {
        if let Some(op) = common::json_field(line, "op") {
            let key: &'static str = match op {
                "open" => "open",
                "release" => "release",
                _ => continue,
            };
            *op_counts.entry(key).or_insert(0) += 1;
        }
    }
    assert_eq!(
        op_counts.get("open").copied().unwrap_or_default(),
        1,
        "workload opens object 0 exactly once; op_counts={op_counts:?}"
    );
    assert_eq!(
        op_counts.get("release").copied().unwrap_or_default(),
        1,
        "workload releases object 0 exactly once; op_counts={op_counts:?}"
    );
}

#[test]
fn determinism_replay_same_seed_same_events() {
    skip_unless_fuse!();

    fn run(path: &std::path::Path) {
        let mut config = common::default_test_config();
        // Pin everything that feeds `SampleKey`: seed, file_size_bytes, and
        // the per-op policies default to zero latency already.
        config.seed = 12345;
        config.metrics.jsonl_path = Some(path.to_path_buf());

        let sink = Arc::new(JsonlEventSink::create(path).expect("create jsonl sink"));
        let sink_for_flush: Arc<JsonlEventSink> = Arc::clone(&sink);

        {
            let (_handle, mount_dir) = common::mount_test_fs_with_sink(
                &config,
                sink as Arc<dyn fusecraft_core::events::EventSink>,
            );
            // Identical workload both runs: a full read of object 0 and 1.
            let _ = std::fs::read(common::object_path(mount_dir.path(), 0)).expect("read 0");
            let _ = std::fs::read(common::object_path(mount_dir.path(), 1)).expect("read 1");
        }
        sink_for_flush.flush().expect("flush sink");
    }

    let tmp = common::tempdir();
    let path_a = tmp.path().join("run_a.jsonl");
    let path_b = tmp.path().join("run_b.jsonl");

    run(&path_a);
    run(&path_b);

    let lines_a = common::read_jsonl_events(&path_a);
    let lines_b = common::read_jsonl_events(&path_b);

    // Compare the ordered per-read `injected_latency_us` values. Other ops
    // are excluded because metadata traversals can vary slightly by what
    // the kernel caches between runs; `Read` is the strongest determinism
    // signal because the test drives it end-to-end.
    //
    // We deliberately drop the global `seq` counter from the comparison.
    // `seq` is a per-engine `AtomicU64` that ticks once per op (lookup,
    // getattr, open, read, release, …), so any run-to-run drift in how
    // many metadata ops the kernel issues before each read would shift the
    // Read events' `seq` values even though the injected latencies are
    // identical. Comparing latency-only keeps the test focused on the
    // determinism invariant (same seed + same SampleKey inputs → same
    // sampler output) without coupling it to kernel caching heuristics.
    fn read_latencies(lines: &[String]) -> Vec<u64> {
        lines
            .iter()
            .filter(|l| common::json_field(l, "op") == Some("read"))
            .filter_map(|l| {
                let lat = common::json_field(l, "injected_latency_us")?
                    .parse::<u64>()
                    .ok()?;
                Some(lat)
            })
            .collect()
    }

    let reads_a = read_latencies(&lines_a);
    let reads_b = read_latencies(&lines_b);

    assert!(!reads_a.is_empty(), "run A produced no Read events");
    assert_eq!(
        reads_a.len(),
        reads_b.len(),
        "both runs should record the same number of Read events, got {} vs {}",
        reads_a.len(),
        reads_b.len()
    );
    assert_eq!(
        reads_a, reads_b,
        "per-read injected_latency_us must match across runs with the same seed"
    );
}
