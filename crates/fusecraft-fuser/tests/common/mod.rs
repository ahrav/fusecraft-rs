//! Shared test utilities for fusecraft-fuser integration tests.
//!
//! Helpers for feature/platform gating, temporary mount directories, default
//! test configurations, and a turnkey `mount_test_fs` that produces a live
//! `MountHandle` on a fresh mount point.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fusecraft_core::config::{
    Config, FilesConfig, LatencyProfile, MountConfig, OpPolicy, RootLayout, WriteMode,
};
use fusecraft_core::content::DeterministicContent;
use fusecraft_core::engine::SimEngine;
use fusecraft_core::events::NullEventSink;
use fusecraft_core::namespace::FlatObjectNamespace;
use fusecraft_core::op::FsOp;

use fusecraft_fuser::{FaultFs, FuserMountOptions, MountHandle, spawn_mount};

/// Check whether FUSE is available on the current system.
///
/// Returns `true` only when the `fuse-tests` feature is enabled, we're on
/// Linux, and `/dev/fuse` exists. Tests should call this at the top and skip
/// (return early) when `false`.
pub fn fuse_available() -> bool {
    if !cfg!(feature = "fuse-tests") {
        return false;
    }

    #[cfg(target_os = "linux")]
    {
        std::path::Path::new("/dev/fuse").exists()
    }

    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Create a temporary directory suitable for use as a FUSE mount point.
///
/// The returned `TempDir` is automatically cleaned up when dropped.
pub fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("failed to create temporary directory for FUSE mount")
}

/// Build a default `Config` suitable for integration tests.
///
/// Uses minimal settings for fast test execution: 10 inodes, 4 KiB files, zero
/// latency injection, and no faults. `auto_unmount` is disabled because it
/// requires `allow_other` (and usually `user_allow_other` in /etc/fuse.conf);
/// tests rely on drop-based unmount via `MountHandle` instead.
pub fn default_test_config() -> Config {
    let zero_latency = OpPolicy {
        concurrency_cap: 8,
        queue_cap: 32,
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
    };

    let mut ops = std::collections::HashMap::new();
    for op in FsOp::ALL {
        ops.insert(op, zero_latency.clone());
    }

    Config {
        seed: 1,
        mount: MountConfig {
            fs_name: "fusecraft-test".into(),
            subtype: "sim-test".into(),
            // `auto_unmount` requires `allow_other`, which in turn requires
            // `user_allow_other` in /etc/fuse.conf on many hosts. We rely on
            // drop-based unmount via `MountHandle` instead.
            auto_unmount: false,
            default_permissions: false,
            read_only: false,
            direct_io: false,
        },
        files: FilesConfig {
            inode_count: 10,
            file_size_bytes: 4096,
            root_layout: RootLayout::default(),
            write_mode: WriteMode::default(),
        },
        ops,
        metrics: Default::default(),
    }
}

/// Mount a `FaultFs` under a fresh tempdir using the given config.
///
/// Returns the `MountHandle` (drop to unmount) together with the `TempDir`
/// owning the mount point. Callers must keep both alive for the duration of
/// the test.
pub fn mount_test_fs(config: &Config) -> (MountHandle, tempfile::TempDir) {
    let mount_dir = tempdir();
    let engine = Arc::new(SimEngine::new(config, Arc::new(NullEventSink)));
    let namespace = Arc::new(FlatObjectNamespace::new(&config.files));
    let content = Arc::new(DeterministicContent::new(
        config.seed,
        config.files.file_size_bytes,
    ));
    let fs = FaultFs::new(engine, namespace, content);
    let opts = FuserMountOptions::from_mount_config(&config.mount);
    let handle = spawn_mount(fs, mount_dir.path(), &opts).expect("spawn_mount");
    // Poll the mount point until the kernel publishes the mounted filesystem:
    // without a real wait, races let the test read the empty underlying
    // tempdir instead of the mount. A fixed sleep would also be flaky on
    // slow CI hosts, so we poll for the `objects` entry that the
    // `FlatObjectNamespace` guarantees at root.
    wait_for_mount(mount_dir.path());
    (handle, mount_dir)
}

/// Poll until the FUSE mount point reports an `objects` child, or give up
/// after a bounded timeout so tests fail fast with a useful message instead
/// of silently racing the kernel.
fn wait_for_mount(path: &std::path::Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let ready = std::fs::read_dir(path)
            .ok()
            .map(|it| {
                it.filter_map(Result::ok)
                    .any(|e| e.file_name() == "objects")
            })
            .unwrap_or(false);
        if ready {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "FUSE mount at {} did not become ready within timeout",
            path.display(),
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Return the path to the workspace root (two levels above fusecraft-fuser).
#[allow(dead_code)]
pub fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .expect("parent of crate dir")
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

/// Build the path to the object file at `index` inside a live mount.
///
/// Mirrors `FlatObjectNamespace::index_to_name`: index `n` maps to the
/// 6-digit zero-padded file `objects/NNNNNN`. Centralising this keeps every
/// test in sync with the layout guaranteed by `FlatObjectNamespace` and
/// avoids five copies of `mount.join("objects").join("000000")`.
#[allow(dead_code)]
pub fn object_path(mount_dir: &Path, index: u64) -> PathBuf {
    mount_dir
        .join("objects")
        .join(FlatObjectNamespace::index_to_name(index))
}
