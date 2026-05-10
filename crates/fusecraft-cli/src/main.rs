//! fusecraft CLI entry point.

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
