//! Fsync-isolated fault injection tests.
//!
//! Proves that a fault rule on `[[ops.fsync.faults]]` fails fsync without
//! affecting `write` on the same inode. Every `FsOp` has its own `OpPolicy`,
//! so a rate=1.0 ENOSPC rule targeted at `FsOp::Fsync` must leave `FsOp::Write`
//! untouched — write(2) should return the full byte count, and only the
//! explicit sync point should surface the error.
//!
//! Gated behind the `fuse-tests` feature and requires Linux with `/dev/fuse`;
//! otherwise the tests compile and skip at runtime.

mod common;

use std::fs::OpenOptions;
use std::io::Write;

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

/// Write a few KB, then fsync: the write must succeed and fsync must fail
/// with ENOSPC. This is the core "per-op fault isolation" invariant — a
/// fault rule on one FsOp must not bleed into a sibling op on the same fd.
#[test]
fn fsync_enospc_does_not_affect_write() {
    skip_unless_fuse!();

    let mut config = common::default_test_config();
    // Make the file large enough that the write payload fits comfortably.
    config.files.file_size_bytes = 131_072;

    let fsync_policy = config.ops.get_mut(&FsOp::Fsync).expect("fsync policy");
    fsync_policy.faults = vec![FaultRule {
        op: FsOp::Fsync,
        errno: libc::ENOSPC,
        rate: 1.0,
    }];

    // Intentionally do NOT touch the write policy: writes must complete
    // cleanly so we can prove isolation from the fsync fault rule.
    let write_policy = config.ops.get(&FsOp::Write).expect("write policy");
    assert!(
        write_policy.faults.is_empty(),
        "write policy must have no faults for this test's isolation claim"
    );

    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("open for write");

    // Write several KiB in one call. `write_all` loops on short writes, but
    // with `write_mode = "discard"` and no fault rule the kernel should accept
    // the full payload in a single FUSE WRITE.
    let payload = vec![0xABu8; 4096];
    let n = file.write(&payload).expect("write should succeed");
    assert_eq!(
        n,
        payload.len(),
        "write must return the full byte count despite the fsync fault rule"
    );

    // Now fsync on the exact same fd. Every fsync is faulted, so this must
    // surface ENOSPC all the way to the caller.
    let err = file.sync_all().expect_err("fsync should fail with ENOSPC");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOSPC),
        "expected ENOSPC from injected fsync fault, got {err:?}"
    );
}

/// A second write *after* a failing fsync must still succeed. Confirms the
/// fault rule is strictly scoped to `FsOp::Fsync` and does not poison the
/// file descriptor or the write-side policy.
#[test]
fn write_after_failing_fsync_still_succeeds() {
    skip_unless_fuse!();

    let mut config = common::default_test_config();
    config.files.file_size_bytes = 131_072;

    let fsync_policy = config.ops.get_mut(&FsOp::Fsync).expect("fsync policy");
    fsync_policy.faults = vec![FaultRule {
        op: FsOp::Fsync,
        errno: libc::ENOSPC,
        rate: 1.0,
    }];

    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let mut file = OpenOptions::new()
        .write(true)
        .open(&file_path)
        .expect("open for write");

    let first = b"first chunk";
    assert_eq!(
        file.write(first).expect("first write"),
        first.len(),
        "first write must return full byte count"
    );

    let err = file
        .sync_all()
        .expect_err("fsync should fail with ENOSPC every time");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::ENOSPC),
        "expected ENOSPC from fsync fault, got {err:?}"
    );

    // A subsequent write must still be accepted — the fsync fault rule is
    // per-op, not per-fd, so the write-side policy is unaffected.
    let second = b"second chunk after a failed fsync";
    assert_eq!(
        file.write(second).expect("second write after failed fsync"),
        second.len(),
        "write must remain functional after a fsync fault",
    );
}
