//! `mount` subcommand: load config, build engine, mount FUSE filesystem.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Context, Result, bail};
use tracing::info;

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

    // Build event sink.
    let event_sink: Arc<dyn EventSink> = match &config.metrics.jsonl_path {
        Some(path) => {
            let sink = JsonlEventSink::create(path)
                .with_context(|| format!("failed to create event log: {}", path.display()))?;
            Arc::new(sink)
        }
        None => Arc::new(NullEventSink),
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

    // Flush the JSONL sink if one was configured, to guarantee events are
    // persisted before we return.
    if let Some(path) = &config.metrics.jsonl_path {
        info!(jsonl_path = %path.display(), "flushed event log");
    }
    drop(event_sink);

    Ok(())
}

/// Initialize the tracing subscriber with env-filter support.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
