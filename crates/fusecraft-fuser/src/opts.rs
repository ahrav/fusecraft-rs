//! FUSE mount options configuration.

use fusecraft_core::config::MountConfig;
use fuser::{Config as FuserConfig, MountOption, SessionACL};

/// High-level mount options for the fusecraft FUSE adapter.
///
/// Converts to `fuser::Config` + `Vec<MountOption>` for the actual mount call.
#[derive(Clone, Debug)]
pub struct FuserMountOptions {
    /// Filesystem name reported to the kernel (appears in /proc/mounts).
    pub fs_name: String,
    /// Filesystem subtype (e.g. "sim").
    pub subtype: String,
    /// Automatically unmount when the process exits.
    pub auto_unmount: bool,
    /// Enable kernel-level permission checking.
    pub default_permissions: bool,
    /// Mount as read-only.
    pub read_only: bool,
    /// Enable direct I/O (bypass page cache).
    pub direct_io: bool,
    /// Allow other users to access the mount.
    pub allow_other: bool,
}

impl FuserMountOptions {
    /// Build `FuserMountOptions` from a `MountConfig`.
    pub fn from_mount_config(cfg: &MountConfig) -> Self {
        Self {
            fs_name: cfg.fs_name.clone(),
            subtype: cfg.subtype.clone(),
            auto_unmount: cfg.auto_unmount,
            default_permissions: cfg.default_permissions,
            read_only: cfg.read_only,
            direct_io: cfg.direct_io,
            allow_other: false,
        }
    }

    /// Convert into the `fuser::Config` used by `fuser::mount2` / `fuser::spawn_mount2`.
    pub(crate) fn to_fuser_config(&self) -> FuserConfig {
        let mut mount_options = Vec::new();

        mount_options.push(MountOption::FSName(self.fs_name.clone()));
        mount_options.push(MountOption::Subtype(self.subtype.clone()));

        // `AutoUnmount` is rejected by fusermount unless one of `AllowOther` /
        // `AllowRoot` is also present (fusermount3 ties them together for
        // safety). We rely on `SessionACL::All` below — set via `allow_other`
        // — to satisfy that requirement. If the caller didn't opt into
        // `allow_other`, suppress `AutoUnmount` too; the kernel still unmounts
        // on session drop, so `MountHandle::drop` keeps working either way.
        if self.auto_unmount && self.allow_other {
            mount_options.push(MountOption::AutoUnmount);
        }
        if self.default_permissions {
            mount_options.push(MountOption::DefaultPermissions);
        }
        if self.read_only {
            mount_options.push(MountOption::RO);
        } else {
            mount_options.push(MountOption::RW);
        }
        if self.direct_io {
            mount_options.push(MountOption::CUSTOM("direct_io".into()));
        }

        // SessionACL::All and SessionACL::RootAndOwner both translate to the
        // `allow_other` mount flag, which fusermount3 rejects unless the
        // system is configured with `user_allow_other`. Use `Owner` when the
        // caller hasn't asked for broader access so unprivileged mounts work
        // on stock installs.
        let acl = if self.allow_other {
            SessionACL::All
        } else {
            SessionACL::Owner
        };

        let mut config = FuserConfig::default();
        config.mount_options = mount_options;
        config.acl = acl;
        config
    }
}
