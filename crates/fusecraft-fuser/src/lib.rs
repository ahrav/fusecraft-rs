//! # fusecraft-fuser
//!
//! FUSE kernel adapter for
//! [fusecraft-core](https://crates.io/crates/fusecraft-core). This crate
//! supplies [`FaultFs`], a generic `fuser::Filesystem` implementation that
//! forwards every FUSE request to [`fusecraft_core::engine::SimEngine::run_op`]
//! so each operation participates in the standard 7-step lifecycle:
//! concurrency limiting, fault sampling, latency injection, bandwidth
//! throttling, and event/metric recording.
//!
//! ## What this crate does
//!
//! - Implements `fuser::Filesystem` on [`FaultFs<N, C>`] for every FUSE op
//!   fusecraft currently models (`lookup`, `getattr`, `open`, `read`, `write`,
//!   `flush`, `release`, `fsync`, `readdir`, `statfs`, `access`).
//! - Routes each handler through [`fusecraft_core::engine::SimEngine::run_op`]
//!   with a closure that produces the "real" reply (namespace lookup, content
//!   read, etc.).
//! - Converts [`fusecraft_core::error::FsError`] into kernel errnos through
//!   [`fusecraft_core::error::FsError::as_errno`].
//! - Exposes [`mount`] (foreground) and [`spawn_mount`] (background, returns a
//!   drop-to-unmount [`MountHandle`]) helpers.
//!
//! ## Quick start
//!
//! ```no_run
//! use std::path::Path;
//! use std::sync::Arc;
//!
//! use fusecraft_core::config::Config;
//! use fusecraft_core::content::DeterministicContent;
//! use fusecraft_core::engine::SimEngine;
//! use fusecraft_core::events::NullEventSink;
//! use fusecraft_core::namespace::FlatObjectNamespace;
//!
//! use fusecraft_fuser::{FaultFs, FuserMountOptions, mount};
//!
//! let config = Config::default();
//! config.validate().expect("valid config");
//!
//! let engine = Arc::new(SimEngine::new(&config, Arc::new(NullEventSink)));
//! let namespace = Arc::new(FlatObjectNamespace::new(&config.files));
//! let content = Arc::new(DeterministicContent::new(
//!     config.seed,
//!     config.files.file_size_bytes,
//! ));
//!
//! let fs = FaultFs::new(engine, namespace, content);
//! let opts = FuserMountOptions::from_mount_config(&config.mount);
//! mount(fs, Path::new("/mnt/sim"), &opts).expect("mount");
//! ```
//!
//! ## Fidelity and non-goals
//!
//! fusecraft-fuser only adapts FUSE semantics to the simulator engine. It does
//! not emulate any specific filesystem (NFS, S3, SMB, EBS, ext4, xfs, etc.).
//! See [`fusecraft-core`](https://crates.io/crates/fusecraft-core) for the
//! full fidelity model.

mod mount;
mod opts;
mod reply_helpers;

pub use mount::{MountHandle, mount, spawn_mount};
pub use opts::FuserMountOptions;

use std::ffi::OsStr;
use std::sync::Arc;
use std::time::Duration;

use fuser::{
    FileHandle, FileType, Filesystem, INodeNo, ReplyAttr, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request,
};

use fusecraft_core::content::ContentModel;
use fusecraft_core::engine::{OpContext, SimEngine};
use fusecraft_core::namespace::{DirSink, FileKind, NamespaceModel};
use fusecraft_core::op::FsOp;

use crate::reply_helpers::{attr_from_spec, errno_to_fuser};

/// Default TTL for attribute caching.
const DEFAULT_ATTR_TTL: Duration = Duration::from_secs(1);
/// Default TTL for entry (lookup) caching.
const DEFAULT_ENTRY_TTL: Duration = Duration::from_secs(1);

/// The FUSE filesystem implementation backed by `SimEngine`.
///
/// Generic over `N: NamespaceModel` (directory layout) and `C: ContentModel`
/// (synthetic data generation). Every handler delegates to `engine.run_op()`
/// with an appropriate closure.
pub struct FaultFs<N: NamespaceModel, C: ContentModel> {
    engine: Arc<SimEngine>,
    namespace: Arc<N>,
    content: Arc<C>,
    attr_ttl: Duration,
    entry_ttl: Duration,
}

impl<N: NamespaceModel, C: ContentModel> FaultFs<N, C> {
    /// Create a new `FaultFs` with default TTLs (1 second).
    pub fn new(engine: Arc<SimEngine>, namespace: Arc<N>, content: Arc<C>) -> Self {
        Self {
            engine,
            namespace,
            content,
            attr_ttl: DEFAULT_ATTR_TTL,
            entry_ttl: DEFAULT_ENTRY_TTL,
        }
    }

    /// Override the attribute and entry TTLs.
    #[must_use]
    pub fn with_ttls(mut self, attr_ttl: Duration, entry_ttl: Duration) -> Self {
        self.attr_ttl = attr_ttl;
        self.entry_ttl = entry_ttl;
        self
    }
}

impl<N: NamespaceModel, C: ContentModel> Filesystem for FaultFs<N, C> {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let ctx = OpContext::metadata(FsOp::Lookup, parent.0);
        let ns = Arc::clone(&self.namespace);
        let ttl = self.entry_ttl;

        // `run_op` invokes the closure synchronously on the calling thread, so
        // we can borrow `name` directly without an `OsString` allocation.
        match self.engine.run_op(ctx, move || {
            let ino = ns
                .lookup(parent.0, name)
                .ok_or(fusecraft_core::error::FsError::Errno(libc::ENOENT))?;
            let spec = ns
                .attr(ino)
                .ok_or(fusecraft_core::error::FsError::Errno(libc::ENOENT))?;
            Ok(spec)
        }) {
            Ok(spec) => {
                let attr = attr_from_spec(&spec);
                reply.entry(&ttl, &attr, fuser::Generation(0));
            }
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let ctx = OpContext::metadata(FsOp::GetAttr, ino.0);
        let ns = Arc::clone(&self.namespace);
        let ttl = self.attr_ttl;

        match self.engine.run_op(ctx, move || {
            ns.attr(ino.0)
                .ok_or(fusecraft_core::error::FsError::Errno(libc::ENOENT))
        }) {
            Ok(spec) => {
                let attr = attr_from_spec(&spec);
                reply.attr(&ttl, &attr);
            }
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: fuser::OpenFlags, reply: ReplyOpen) {
        let ctx = OpContext::metadata(FsOp::Open, ino.0);

        match self.engine.run_op(ctx, || Ok(())) {
            Ok(()) => reply.opened(FileHandle(0), fuser::FopenFlags::empty()),
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let content = Arc::clone(&self.content);
        // Clamp the request to the bytes actually readable so the engine sees
        // the true byte count: `ctx.len` feeds bandwidth throttling, the
        // sampler key, and event/metric payloads, and short tail reads should
        // not be charged as full-size reads.
        let requested = size as usize;
        let file_len = content.file_len(ino.0);
        let readable = if offset >= file_len {
            0
        } else {
            ((file_len - offset) as usize).min(requested)
        };
        let ctx = OpContext::data(FsOp::Read, ino.0, offset, readable);

        match self.engine.run_op(ctx, move || {
            if readable == 0 {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u8; readable];
            content.read_at(ino.0, offset, &mut buf);
            Ok(buf)
        }) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyWrite,
    ) {
        let content = Arc::clone(&self.content);
        let data_len = data.len();
        let ctx = OpContext::data(FsOp::Write, ino.0, offset, data_len);

        // `run_op` calls the closure synchronously, so the &[u8] borrow from
        // the kernel request is valid for the closure's lifetime — no copy.
        match self.engine.run_op(ctx, move || {
            content.write_at(ino.0, offset, data).map(|n| n as u32)
        }) {
            Ok(written) => reply.written(written),
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: fuser::LockOwner,
        reply: ReplyEmpty,
    ) {
        let ctx = OpContext::metadata(FsOp::Flush, ino.0);

        match self.engine.run_op(ctx, || Ok(())) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        let ctx = OpContext::metadata(FsOp::Release, ino.0);

        match self.engine.run_op(ctx, || Ok(())) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let ctx = OpContext::metadata(FsOp::Fsync, ino.0);

        match self.engine.run_op(ctx, || Ok(())) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let ns = Arc::clone(&self.namespace);
        let ctx = OpContext::metadata(FsOp::Readdir, ino.0);

        match self.engine.run_op(ctx, move || {
            let mut entries: Vec<(u64, u64, FileKind, std::ffi::OsString)> = Vec::new();
            struct Collector<'a>(&'a mut Vec<(u64, u64, FileKind, std::ffi::OsString)>);
            impl DirSink for Collector<'_> {
                fn push(&mut self, ino: u64, offset: u64, kind: FileKind, name: &OsStr) -> bool {
                    self.0.push((ino, offset, kind, name.to_owned()));
                    true
                }
            }
            ns.readdir(ino.0, offset, &mut Collector(&mut entries))?;
            Ok(entries)
        }) {
            Ok(entries) => {
                for (entry_ino, entry_offset, kind, name) in entries {
                    let ft = match kind {
                        FileKind::Dir => FileType::Directory,
                        FileKind::File => FileType::RegularFile,
                    };
                    if reply.add(INodeNo(entry_ino), entry_offset, ft, &name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    fn statfs(&self, _req: &Request, ino: INodeNo, reply: ReplyStatfs) {
        let ctx = OpContext::metadata(FsOp::Statfs, ino.0);

        match self.engine.run_op(ctx, || Ok(())) {
            Ok(()) => {
                // Return synthetic statfs: large capacity, 4K blocks.
                reply.statfs(
                    1_000_000, // blocks
                    1_000_000, // bfree
                    1_000_000, // bavail
                    1_000_000, // files
                    1_000_000, // ffree
                    4096,      // bsize
                    255,       // namelen
                    4096,      // frsize
                );
            }
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }

    /// Authorize `access(2)` lookups when the kernel is not handling permission
    /// checks itself (i.e. `default_permissions=false`). Without an explicit
    /// handler, the default `fuser::Filesystem::access` impl returns `ENOSYS`
    /// and tools like `test -r`/`test -w` fail unexpectedly. We keep it simple
    /// for the simulator: route through `run_op` so the probe participates in
    /// the normal lifecycle (faults, latency, metrics, events) and let the
    /// closure resolve the inode via the namespace. Real permission semantics
    /// are out of scope — users who want mode-bit enforcement should keep the
    /// default `default_permissions=true` and let the kernel decide.
    fn access(&self, _req: &Request, ino: INodeNo, _mask: fuser::AccessFlags, reply: ReplyEmpty) {
        let ns = Arc::clone(&self.namespace);
        let ctx = OpContext::metadata(FsOp::Access, ino.0);

        match self.engine.run_op(ctx, move || {
            ns.attr(ino.0)
                .map(|_| ())
                .ok_or(fusecraft_core::error::FsError::Errno(libc::ENOENT))
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(errno_to_fuser(e.as_errno())),
        }
    }
}
