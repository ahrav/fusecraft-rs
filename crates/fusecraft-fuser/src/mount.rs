//! Mount and spawn_mount entry points.

use std::path::Path;

use fusecraft_core::content::ContentModel;
use fusecraft_core::error::FsError;
use fusecraft_core::namespace::NamespaceModel;

use crate::{FaultFs, FuserMountOptions};

/// Handle to a background-mounted FUSE filesystem.
///
/// Dropping this handle will unmount the filesystem.
pub struct MountHandle {
    _session: fuser::BackgroundSession,
}

/// Mount the filesystem in the foreground (blocks until unmounted).
///
/// # Errors
/// Returns `FsError::Io` if the mount fails.
pub fn mount<N, C>(
    fs: FaultFs<N, C>,
    mountpoint: &Path,
    opts: &FuserMountOptions,
) -> Result<(), FsError>
where
    N: NamespaceModel,
    C: ContentModel,
{
    let config = opts.to_fuser_config();
    fuser::mount2(fs, mountpoint, &config).map_err(FsError::Io)
}

/// Mount the filesystem in a background thread and return a handle.
///
/// The filesystem remains mounted until the returned `MountHandle` is dropped.
///
/// # Errors
/// Returns `FsError::Io` if the mount fails.
pub fn spawn_mount<N, C>(
    fs: FaultFs<N, C>,
    mountpoint: &Path,
    opts: &FuserMountOptions,
) -> Result<MountHandle, FsError>
where
    N: NamespaceModel,
    C: ContentModel,
{
    let config = opts.to_fuser_config();
    let session = fuser::spawn_mount2(fs, mountpoint, &config).map_err(FsError::Io)?;
    Ok(MountHandle { _session: session })
}
