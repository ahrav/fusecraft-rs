//! Configuration loading and serialization helpers.

use std::path::Path;

use anyhow::{Context, Result};
use fusecraft_core::config::Config;

/// Load and validate a [`Config`] from a TOML file.
pub fn load_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    let config: Config =
        toml::from_str(&text).with_context(|| format!("invalid TOML in {}", path.display()))?;
    config
        .validate()
        .with_context(|| format!("config validation failed for {}", path.display()))?;
    Ok(config)
}

/// Serialize the default [`Config`] to TOML.
pub fn default_config_toml() -> Result<String> {
    let config = Config::default();
    toml::to_string_pretty(&config).context("failed to serialize default config to TOML")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Create a temp file with the given contents and return its path.
    fn write_temp(contents: &str) -> std::path::PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fusecraft-cli-test-{}-{}.toml",
            std::process::id(),
            id,
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn load_valid_minimal_config() {
        let path = write_temp("[files]\ninode_count = 10\nfile_size_bytes = 4096\n");
        let cfg = load_config(&path).unwrap();
        assert_eq!(cfg.files.inode_count, 10);
        assert_eq!(cfg.files.file_size_bytes, 4096);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_config_rejects_invalid_inode_count() {
        let path = write_temp("[files]\ninode_count = 0\nfile_size_bytes = 4096\n");
        let err = load_config(&path).unwrap_err();
        assert!(
            format!("{err:?}").contains("inode_count"),
            "error should mention inode_count: {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_config_rejects_bad_toml() {
        let path = write_temp("not valid [[[ toml");
        let err = load_config(&path).unwrap_err();
        assert!(
            format!("{err:?}").contains("TOML"),
            "error should mention TOML: {err:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_config_rejects_missing_file() {
        // Build a guaranteed-nonexistent path inside the temp dir so the
        // test doesn't false-fail on hosts that happen to have a file at
        // a fixed /tmp path.
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let missing = std::env::temp_dir().join(format!(
            "fusecraft-cli-missing-{}-{}.toml",
            std::process::id(),
            id,
        ));
        let err = load_config(&missing).unwrap_err();
        assert!(
            format!("{err:?}").contains("read config file"),
            "error should mention reading: {err:?}"
        );
    }

    #[test]
    fn default_config_toml_roundtrips() {
        let text = default_config_toml().unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        parsed.validate().unwrap();
        assert_eq!(parsed.seed, 42);
        assert_eq!(parsed.files.inode_count, 1000);
    }
}
