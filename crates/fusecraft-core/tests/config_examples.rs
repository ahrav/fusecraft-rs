//! Integration test: parse and validate every example config in examples/*.toml.
//!
//! This test ensures that all shipped example configurations are well-formed
//! and pass `Config::validate()`. It fails loudly if the examples/ directory
//! is empty or missing — indicating a broken build.

use std::path::PathBuf;

use fusecraft_core::config::Config;

/// Locate the workspace-root `examples/` directory.
fn examples_dir() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/fusecraft-core -> workspace root is two levels up
    manifest_dir
        .parent()
        .expect("parent of crate dir")
        .parent()
        .expect("workspace root")
        .join("examples")
}

/// Collect all `*.toml` files in the examples directory.
fn collect_example_configs() -> Vec<PathBuf> {
    let dir = examples_dir();
    if !dir.exists() {
        return Vec::new();
    }

    let mut configs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read examples/ directory")
        .filter_map(|entry| {
            let entry = entry.expect("read dir entry");
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    configs.sort();
    configs
}

#[test]
fn all_example_configs_parse_and_validate() {
    let configs = collect_example_configs();

    assert!(
        !configs.is_empty(),
        "No example config files found in {dir}. \
         The examples/ directory must contain at least one *.toml file.",
        dir = examples_dir().display()
    );

    let mut failures: Vec<String> = Vec::new();

    for path in &configs {
        let filename = path.file_name().unwrap().to_string_lossy();
        let contents = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{filename}: failed to read file: {e}"));
                continue;
            }
        };

        let cfg: Config = match toml::from_str(&contents) {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("{filename}: TOML parse error: {e}"));
                continue;
            }
        };

        if let Err(e) = cfg.validate() {
            failures.push(format!("{filename}: validation error: {e}"));
        }
    }

    assert!(
        failures.is_empty(),
        "Example config failures ({count}/{total}):\n{details}",
        count = failures.len(),
        total = configs.len(),
        details = failures.join("\n"),
    );

    // Print summary on success (visible with --nocapture).
    eprintln!(
        "config_examples: successfully parsed and validated {} example configs",
        configs.len()
    );
}

#[test]
fn examples_directory_exists() {
    let dir = examples_dir();
    assert!(
        dir.exists(),
        "examples/ directory does not exist at {path}. \
         It should be created at the workspace root.",
        path = dir.display()
    );
}
