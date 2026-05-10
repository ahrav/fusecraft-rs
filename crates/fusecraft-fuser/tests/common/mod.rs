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
use fusecraft_core::events::{EventSink, NullEventSink};
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
    mount_test_fs_with_sink(config, Arc::new(NullEventSink))
}

/// Mount a `FaultFs` under a fresh tempdir using the given config and a custom
/// [`EventSink`].
///
/// Used by tests that need to observe the event stream emitted by the engine
/// (e.g. JSONL replay or per-op accounting). `mount_test_fs` delegates here
/// with [`NullEventSink`] as the default.
#[allow(dead_code)]
pub fn mount_test_fs_with_sink(
    config: &Config,
    sink: Arc<dyn EventSink>,
) -> (MountHandle, tempfile::TempDir) {
    try_mount_test_fs_with_sink(config, sink).expect("spawn_mount")
}

/// Fallible variant that returns the underlying `FsError` when the mount
/// fails. Tests use this to tolerate environment-specific mount rejections
/// (e.g. `fusermount3` refusing unknown options) without panicking.
#[allow(dead_code)]
pub fn try_mount_test_fs_with_sink(
    config: &Config,
    sink: Arc<dyn EventSink>,
) -> Result<(MountHandle, tempfile::TempDir), fusecraft_core::error::FsError> {
    let mount_dir = tempdir();
    let engine = Arc::new(SimEngine::new(config, sink));
    let namespace = Arc::new(FlatObjectNamespace::new(&config.files));
    let content = Arc::new(DeterministicContent::new(
        config.seed,
        config.files.file_size_bytes,
    ));
    let fs = FaultFs::new(engine, namespace, content);
    let opts = FuserMountOptions::from_mount_config(&config.mount);
    let handle = spawn_mount(fs, mount_dir.path(), &opts)?;
    // Poll the mount point until the kernel publishes the mounted filesystem:
    // without a real wait, races let the test read the empty underlying
    // tempdir instead of the mount. A fixed sleep would also be flaky on
    // slow CI hosts, so we poll for the `objects` entry that the
    // `FlatObjectNamespace` guarantees at root.
    wait_for_mount(mount_dir.path());
    Ok((handle, mount_dir))
}

/// Drop-to-unmount guard wrapping a raw `fuser::BackgroundSession`.
///
/// `MountHandle` in `fusecraft-fuser` is `pub` but its field is private, so
/// tests that need to construct a session with non-default `fuser::Config`
/// (e.g. `n_threads`) can't route through `MountHandle`. This helper type is
/// the test-only twin used by [`mount_multi_threaded_fs`].
#[allow(dead_code)]
pub struct RawMountHandle {
    _session: fuser::BackgroundSession,
}

/// Mount a `FaultFs` with multiple FUSE worker threads.
///
/// The public [`fusecraft_fuser::mount`]/[`fusecraft_fuser::spawn_mount`]
/// helpers plumb a [`FuserMountOptions`] which leaves `fuser::Config::n_threads`
/// at `None` (fuser defaults to a single worker). Concurrency tests need two or
/// more worker threads so that multiple FUSE requests can really be in flight
/// at the engine layer at the same time; this helper bypasses the adapter's
/// default and builds the `fuser::Config` directly.
#[allow(dead_code)]
pub fn mount_multi_threaded_fs(
    config: &Config,
    n_threads: usize,
) -> (RawMountHandle, tempfile::TempDir) {
    use fuser::{MountOption, SessionACL};

    let mount_dir = tempdir();
    let engine = Arc::new(SimEngine::new(config, Arc::new(NullEventSink)));
    let namespace = Arc::new(FlatObjectNamespace::new(&config.files));
    let content = Arc::new(DeterministicContent::new(
        config.seed,
        config.files.file_size_bytes,
    ));
    let fs = FaultFs::new(engine, namespace, content);

    // Replicate just enough of `FuserMountOptions::to_fuser_config` to preserve
    // parity with the real adapter, but with `n_threads` set to the test's
    // chosen value. We intentionally avoid `direct_io` and `auto_unmount` here
    // — neither is needed to observe concurrency behaviour.
    let mut mount_options = Vec::new();
    mount_options.push(MountOption::FSName(config.mount.fs_name.clone()));
    mount_options.push(MountOption::Subtype(config.mount.subtype.clone()));
    if config.mount.default_permissions {
        mount_options.push(MountOption::DefaultPermissions);
    }
    if config.mount.read_only {
        mount_options.push(MountOption::RO);
    } else {
        mount_options.push(MountOption::RW);
    }

    let mut fuser_config = fuser::Config::default();
    fuser_config.mount_options = mount_options;
    fuser_config.acl = SessionACL::Owner;
    fuser_config.n_threads = Some(n_threads);

    let session = fuser::spawn_mount2(fs, mount_dir.path(), &fuser_config)
        .expect("spawn_mount2 with n_threads");
    wait_for_mount(mount_dir.path());
    (RawMountHandle { _session: session }, mount_dir)
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

/// Read a JSONL file and return each non-empty line as its raw string.
///
/// Parsing into `serde_json::Value` would require adding `serde_json` as a
/// dev-dependency of `fusecraft-fuser`, which is outside this test file's
/// ownership. Callers perform lightweight field checks via [`json_field`]
/// instead. Trailing empty lines are skipped.
#[allow(dead_code)]
pub fn read_jsonl_events(path: &Path) -> Vec<String> {
    let contents = std::fs::read_to_string(path).expect("read jsonl file");
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.to_owned())
        .collect()
}

/// Extract the string value of a top-level JSON field from a single JSONL
/// line. Returns `None` if the field is absent or the line isn't a simple
/// JSON object of the shape produced by `JsonlEventSink`.
///
/// Handles both string (`"op":"read"`) and numeric (`"seq":42`) values and
/// returns the raw substring (unquoted for strings, verbatim for numbers).
/// This is a deliberately minimal parser tuned to the `Event` serialization
/// shape in `fusecraft-core::events`, not a general JSON reader.
#[allow(dead_code)]
pub fn json_field<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let needle = format!("\"{field}\":");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if let Some(stripped) = rest.strip_prefix('"') {
        // String value: read until the next unescaped quote.
        let end = stripped.find('"')?;
        Some(&stripped[..end])
    } else {
        // Numeric / boolean / null value: read until the next `,` or `}`.
        let end = rest.find([',', '}'])?;
        Some(rest[..end].trim())
    }
}
