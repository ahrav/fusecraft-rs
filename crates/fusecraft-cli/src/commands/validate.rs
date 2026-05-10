//! `validate-config` subcommand: parse + validate a TOML config file.

use std::path::Path;

use anyhow::Result;

use crate::config_io::load_config;

/// Execute the `validate-config` subcommand.
pub fn run(config_path: &Path) -> Result<()> {
    let _config = load_config(config_path)?;
    eprintln!("config OK: {}", config_path.display());
    Ok(())
}
