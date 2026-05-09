//! `mount` subcommand: load config, build engine, mount FUSE filesystem.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use fusecraft_core::content::DeterministicContent;
use fusecraft_core::engine::SimEngine;
use fusecraft_core::events::{EventSink, JsonlEventSink, NullEventSink};
use fusecraft_core::namespace::FlatObjectNamespace;
use fusecraft_fuser::{FaultFs, FuserMountOptions, spawn_mount};

use crate::config_io::load_config;

/// Execute the `mount` subcommand.
pub fn run(config_path: &Path, mountpoint: &Path) -> Result<()> {
    // Validate mountpoint exists and is a directory.
    if !mountpoint.is_dir() {
        bail!(
            "mountpoint does not exist or is not a directory: {}",
            mountpoint.display()
        );
    }

    // Load and validate configuration.
    let config = load_config(config_path)?;

    // Initialize tracing subscriber.
    init_tracing();

    info!(
        seed = config.seed,
        inode_count = config.files.inode_count,
        file_size_bytes = config.files.file_size_bytes,
        "fusecraft starting"
    );

    // Build event sink. Keep a concrete handle to the JSONL sink (if any) so
    // we can explicitly `flush()` it on shutdown — the `Arc<dyn EventSink>`
    // alone isn't enough because the engine keeps another Arc alive, which
    // would defer (and silently swallow errors from) the BufWriter drop.
    let (event_sink, jsonl_sink): (Arc<dyn EventSink>, Option<Arc<JsonlEventSink>>) =
        match &config.metrics.jsonl_path {
            Some(path) => {
                let sink =
                    Arc::new(JsonlEventSink::create(path).with_context(|| {
                        format!("failed to create event log: {}", path.display())
                    })?);
                let as_dyn: Arc<dyn EventSink> = Arc::clone(&sink) as Arc<dyn EventSink>;
                (as_dyn, Some(sink))
            }
            None => (Arc::new(NullEventSink), None),
        };

    // Build models and engine.
    let engine = Arc::new(SimEngine::new(&config, Arc::clone(&event_sink)));
    let namespace = Arc::new(FlatObjectNamespace::new(&config.files));
    let content = Arc::new(DeterministicContent::new(
        config.seed,
        config.files.file_size_bytes,
    ));

    let fs = FaultFs::new(Arc::clone(&engine), namespace, content);
    let mount_opts = FuserMountOptions::from_mount_config(&config.mount);

    // `auto_unmount` is only honored by fusermount when `allow_other` is also
    // enabled. We don't currently expose `allow_other` in the CLI/config, so
    // surface the silent downgrade explicitly rather than letting users
    // assume the option took effect.
    if config.mount.auto_unmount {
        warn!(
            "mount.auto_unmount=true is configured but requires allow_other, which is not \
             currently exposed; relying on drop-based unmount instead"
        );
    }

    let handle = spawn_mount(fs, mountpoint, &mount_opts)
        .map_err(|e| anyhow::anyhow!("failed to mount: {e}"))?;

    info!(mountpoint = %mountpoint.display(), "filesystem mounted — press Ctrl+C to unmount");

    let (shutdown_tx, shutdown_rx) = mpsc::sync_channel::<()>(1);
    ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(());
    })
    .context("failed to set Ctrl+C handler")?;

    let _ = shutdown_rx.recv();

    info!("shutting down...");

    drop(handle);

    engine.metrics().print_summary();

    // Flush the JSONL sink (if one was configured) before returning so buffered
    // events land on disk and any I/O error surfaces to the user. Dropping the
    // `Arc<dyn EventSink>` here is not enough: the engine still holds another
    // Arc to the same sink, so the BufWriter wouldn't actually drop until the
    // whole function returns and engine falls out of scope.
    if let (Some(path), Some(sink)) = (&config.metrics.jsonl_path, &jsonl_sink) {
        match sink.flush() {
            Ok(()) => info!(jsonl_path = %path.display(), "flushed event log"),
            Err(e) => warn!(jsonl_path = %path.display(), error = %e, "failed to flush event log"),
        }
    }
    drop(event_sink);
    drop(jsonl_sink);

    Ok(())
}

/// Initialize the tracing subscriber with env-filter support.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // `try_init` is safe to call when a global subscriber is already set
    // (e.g. test harness, library embedding): it just returns an error we
    // can ignore instead of panicking.
    let _ = fmt().with_env_filter(filter).try_init();
}
