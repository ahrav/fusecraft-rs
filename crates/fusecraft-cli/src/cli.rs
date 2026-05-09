//! Command-line argument definitions using clap derive.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// fusecraft — a deterministic FUSE filesystem simulator.
#[derive(Debug, Parser)]
#[command(name = "fusecraft", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Available subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Mount the simulated filesystem at a given directory.
    Mount {
        /// Path to the TOML configuration file.
        #[arg(short, long)]
        config: PathBuf,

        /// Mountpoint directory (must exist).
        #[arg(short, long)]
        mountpoint: PathBuf,
    },

    /// Validate a configuration file without mounting.
    ValidateConfig {
        /// Path to the TOML configuration file to validate.
        #[arg(short, long)]
        config: PathBuf,
    },

    /// Print the default configuration as TOML to stdout.
    PrintDefaultConfig,
}
