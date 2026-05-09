//! `print-default-config` subcommand: emit the default config as TOML to stdout.

use anyhow::Result;

use crate::config_io::default_config_toml;

/// Execute the `print-default-config` subcommand.
pub fn run() -> Result<()> {
    let toml_text = default_config_toml()?;
    print!("{toml_text}");
    Ok(())
}
