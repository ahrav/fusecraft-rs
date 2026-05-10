//! FUSE mount integration tests.
//!
//! These tests are gated behind the `fuse-tests` feature and require Linux
//! with `/dev/fuse`. Without the feature or on non-Linux, the tests compile
//! and run but skip at runtime.

mod common;

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
fn mount_and_unmount() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (handle, mount_dir) = common::mount_test_fs(&config);
    assert!(mount_dir.path().exists());
    drop(handle);
}

#[test]
fn readdir_root_lists_objects_directory() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let entries: Vec<String> = std::fs::read_dir(mount_dir.path())
        .expect("read_dir on mount")
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    assert!(
        entries.iter().any(|n| n == "objects"),
        "root should contain 'objects' directory, got: {entries:?}"
    );
}

#[test]
fn read_file_returns_deterministic_content() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let data = std::fs::read(&file_path).expect("read file");
    assert_eq!(data.len(), config.files.file_size_bytes as usize);

    let data2 = std::fs::read(&file_path).expect("read file again");
    assert_eq!(data, data2, "content should be deterministic across reads");
}

#[test]
fn stat_file_returns_correct_size() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let metadata = std::fs::metadata(&file_path).expect("stat file");
    assert_eq!(metadata.len(), config.files.file_size_bytes);
    assert!(metadata.is_file());
}

#[test]
fn multiple_files_have_expected_count() {
    skip_unless_fuse!();

    let config = common::default_test_config();
    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let objects_dir = mount_dir.path().join("objects");
    let count = std::fs::read_dir(&objects_dir)
        .expect("read objects dir")
        .count();
    assert_eq!(count, config.files.inode_count as usize);
}

#[test]
fn error_injection_eio_reaches_caller() {
    skip_unless_fuse!();

    let mut config = common::default_test_config();
    // Force every read through EIO.
    let read_policy = config.ops.get_mut(&FsOp::Read).expect("read policy");
    read_policy.faults = vec![FaultRule {
        op: FsOp::Read,
        errno: libc::EIO,
        rate: 1.0,
    }];

    let (_handle, mount_dir) = common::mount_test_fs(&config);

    let file_path = common::object_path(mount_dir.path(), 0);
    let err = std::fs::read(&file_path).expect_err("read should fail with EIO");
    assert_eq!(
        err.raw_os_error(),
        Some(libc::EIO),
        "expected EIO from injected fault, got {err:?}"
    );
}
