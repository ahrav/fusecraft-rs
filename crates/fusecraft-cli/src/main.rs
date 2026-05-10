//! # fusecraft CLI
//!
//! Command-line entry point for
//! [fusecraft](https://github.com/ahrav/fusecraft-rs), a deterministic FUSE
//! filesystem simulator that models syscall-visible latency, faults, queueing,
//! and bandwidth limits. fusecraft does not simulate NFS, S3, SMB, EBS, ext4,
//! xfs, or any exact storage protocol.
//!
//! ## Subcommands
//!
//! - `fusecraft mount --config <FILE> --mountpoint <DIR>` — mount the
//!   simulated filesystem in the foreground. Ctrl-C unmounts and exits.
//! - `fusecraft validate-config --config <FILE>` — parse and validate a TOML
//!   config without mounting.
//! - `fusecraft print-default-config` — emit the built-in default config as
//!   TOML on stdout.
//!
//! See `docs/config.md` in the repository for the complete key reference and
//! `examples/` for ready-to-use configs.

use anyhow::Result;
use clap::Parser;

mod cli;
mod commands;
mod config_io;

use cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Mount { config, mountpoint } => commands::mount::run(&config, &mountpoint),
        Command::ValidateConfig { config } => commands::validate::run(&config),
        Command::PrintDefaultConfig => commands::print_default::run(),
    }
}
