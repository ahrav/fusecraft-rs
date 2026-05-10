//! FUSE write/fsync/flush/release integration tests.
//!
//! These tests are gated behind the `fuse-tests` feature and require Linux
//! with `/dev/fuse`. Without the feature or on non-Linux, the tests compile
//! and run but skip at runtime.

mod common;

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

use fusecraft_core::config::FaultRule;
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
fn write_is_acknowledged_with_correct_length() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("open for write");

    let payload = b"hello fusecraft";
    let n = file.write(payload).expect("write");
    assert_eq!(n, payload.len(), "write should acknowledge full length");
}

#[test]
fn write_does_not_mutate_subsequent_reads() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let file_size = config.files.file_size_bytes as usize;
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);

    // Black-box baseline: read the file before any write to capture whatever
    // deterministic content the engine will produce for this inode/seed. This
    // avoids reconstructing `expected` via `synth_byte` + a hard-coded inode,
    // so the test stays valid even if the content scheme or inode layout
    // changes in `fusecraft-core`.
    let before = std::fs::read(&file_path).expect("baseline read");
    assert_eq!(before.len(), file_size);

    {
        let mut file = OpenOptions::new()
            .write(true)
            .open(&file_path)
            .expect("open for write");
        file.seek(SeekFrom::Start(0)).expect("seek");
        file.write_all(b"OVERWRITE").expect("write");
        file.flush().expect("flush");
    }

    let data = std::fs::read(&file_path).expect("read after write");

    assert_eq!(
        data, before,
        "Discard write_mode must not mutate subsequent deterministic reads"
    );
}

#[test]
fn write_error_injection_reaches_caller() {
    skip_unless_fuse!();

    let mut config = common::default_test_config();
    let write_policy = config.ops.get_mut(&FsOp::Write).expect("write policy");
    write_policy.faults = vec![FaultRule {
        op: FsOp::Write,
        errno: libc::ENOSPC,
        rate: 1.0,
    }];

    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("open for write");

    let err = file.write_all(b"payload").expect_err("write should fail");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOSPC),
        "expected ENOSPC from injected fault, got {err:?}"
    );
}

#[test]
fn flush_and_release_complete_successfully() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);

    {
        let mut file = OpenOptions::new()
            .write(true)
            .open(&file_path)
            .expect("open for write");
        file.write_all(b"abc").expect("write");
        // NOTE: we deliberately do *not* call `file.flush()` — on Unix
        // `<File as Write>::flush()` is a no-op (libstd returns `Ok(())`
        // without touching the fd), so it does not drive the FUSE flush
        // handler. The handler is exercised below when the `File` is
        // dropped: `close(2)` causes the kernel to send FLUSH followed by
        // RELEASE to our filesystem, so the reopen proves both paths
        // completed without leaking state or panicking.
    }
    let _reopen = OpenOptions::new()
        .read(true)
        .open(&file_path)
        .expect("reopen after release");
}

#[test]
fn flush_latency_is_observed_on_close() {
    skip_unless_fuse!();

    // Inject latency on the FLUSH op and measure `drop(file)` — which on
    // Unix performs `close(2)` and therefore triggers the FUSE flush
    // handler. This is the only place in libstd that actually drives a
    // FUSE flush, so it's the right place to pin a regression test:
    // `<File as Write>::flush()` is a no-op on this platform.
    let mut config = common::default_test_config();
    let flush_policy = config.ops.get_mut(&FsOp::Flush).expect("flush policy");
    flush_policy.latency.base_us = 50_000;

    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("open for write");
    file.write_all(b"flush me").expect("write");

    let start = std::time::Instant::now();
    drop(file);
    let elapsed = start.elapsed();

    // Loose lower bound (40 ms against 50 ms injected) tolerates CI jitter
    // and the fact that RELEASE runs in the same close() syscall and adds
    // a small amount of its own wall time.
    assert!(
        elapsed >= std::time::Duration::from_millis(40),
        "expected close-triggered flush to block for ~50ms, got {elapsed:?}"
    );
}

#[test]
fn fsync_latency_is_observed() {
    skip_unless_fuse!();

    let mut config = common::default_test_config();
    let fsync_policy = config.ops.get_mut(&FsOp::Fsync).expect("fsync policy");
    fsync_policy.latency.base_us = 50_000;

    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("open for write");
    file.write_all(b"sync me").expect("write");

    let start = std::time::Instant::now();
    file.sync_all().expect("fsync");
    let elapsed = start.elapsed();

    // Loose lower bound (40 ms against 50 ms injected) tolerates CI jitter.
    assert!(
        elapsed >= std::time::Duration::from_millis(40),
        "expected fsync to block for ~50ms, got {elapsed:?}"
    );
}

#[test]
fn read_only_mount_rejects_write() {
    skip_unless_fuse!();

    let mut config = common::default_test_config();
    config.mount.read_only = true;

    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let err = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect_err("open for write on RO mount should fail");

    let errno = err.raw_os_error();
    assert!(
        errno == Some(libc::EROFS) || errno == Some(libc::EACCES),
        "expected EROFS or EACCES on RO mount, got {err:?}"
    );
}
