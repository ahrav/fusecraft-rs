//! Integration tests for TOML config parsing and validation.

use fusecraft_core::config::{Config, FaultRule, LatencyProfile, OpPolicy, parse_errno};
use fusecraft_core::error::FsError;
use fusecraft_core::op::FsOp;

/// Helper: serialize Config to TOML string.
fn to_toml(cfg: &Config) -> String {
    toml::to_string(cfg).expect("serialize to TOML")
}

/// Helper: parse a TOML string into Config.
fn from_toml(s: &str) -> Result<Config, toml::de::Error> {
    toml::from_str(s)
}

#[test]
fn round_trip_default_config() {
    let original = Config::default();
    let toml_str = to_toml(&original);
    let parsed: Config = from_toml(&toml_str).expect("parse default config TOML");

    assert_eq!(parsed.seed, original.seed);
    assert_eq!(parsed.files.inode_count, original.files.inode_count);
    assert_eq!(parsed.files.file_size_bytes, original.files.file_size_bytes);
    assert_eq!(parsed.files.root_layout, original.files.root_layout);
    assert_eq!(parsed.files.write_mode, original.files.write_mode);
    assert_eq!(parsed.mount.fs_name, original.mount.fs_name);
    assert_eq!(parsed.mount.auto_unmount, original.mount.auto_unmount);
    assert!(parsed.ops.is_empty());
    parsed.validate().expect("default config must validate");
}

#[test]
fn parse_ops_read_example() {
    let toml_str = r#"
seed = 12345

[files]
inode_count = 500
file_size_bytes = 131072
root_layout = "flat"
write_mode = "inmemory"

[ops.read]
concurrency_cap = 32
queue_cap = 128

[ops.read.latency]
base_us = 50
lognormal_median_us = 200.0
lognormal_sigma = 0.8
pareto_weight = 0.05
pareto_xm_us = 5000.0
pareto_alpha = 1.2
max_us = 500000

[ops.read.bandwidth]
mib_per_sec = 100.0
burst_bytes = 1048576

[[ops.read.faults]]
op = "read"
errno = "EIO"
rate = 0.001

[[ops.read.faults]]
op = "read"
errno = "EAGAIN"
rate = 0.01
"#;
    let cfg: Config = from_toml(toml_str).expect("parse ops.read example");
    cfg.validate().expect("ops.read example must validate");

    assert_eq!(cfg.seed, 12345);
    assert_eq!(cfg.files.inode_count, 500);
    assert_eq!(cfg.files.file_size_bytes, 131072);

    let read_policy = cfg.ops.get(&FsOp::Read).expect("read policy exists");
    assert_eq!(read_policy.concurrency_cap, 32);
    assert_eq!(read_policy.queue_cap, 128);
    assert_eq!(read_policy.latency.base_us, 50);
    assert!((read_policy.latency.lognormal_median_us - 200.0).abs() < f64::EPSILON);
    assert!((read_policy.latency.lognormal_sigma - 0.8).abs() < f64::EPSILON);
    assert!((read_policy.latency.pareto_weight - 0.05).abs() < f64::EPSILON);
    assert!((read_policy.latency.pareto_xm_us - 5000.0).abs() < f64::EPSILON);
    assert!((read_policy.latency.pareto_alpha - 1.2).abs() < f64::EPSILON);
    assert_eq!(read_policy.latency.max_us, 500_000);

    let bw = read_policy.bandwidth.as_ref().expect("bandwidth exists");
    let expected_bps = 100.0 * 1024.0 * 1024.0;
    assert!((bw.bytes_per_sec - expected_bps).abs() < 1.0);
    assert_eq!(bw.burst_bytes, 1_048_576);

    assert_eq!(read_policy.faults.len(), 2);
    assert_eq!(read_policy.faults[0].errno, libc::EIO);
    assert!((read_policy.faults[0].rate - 0.001).abs() < f64::EPSILON);
    assert_eq!(read_policy.faults[1].errno, libc::EAGAIN);
}

#[test]
fn parse_errno_bogus_returns_config_error() {
    let result = parse_errno("BOGUS");
    assert!(result.is_err());
    match result.unwrap_err() {
        FsError::Config(msg) => assert!(msg.contains("unknown errno")),
        other => panic!("expected FsError::Config, got: {other:?}"),
    }
}

#[test]
fn parse_errno_known_values() {
    assert_eq!(parse_errno("EIO").unwrap(), libc::EIO);
    assert_eq!(parse_errno("ENOENT").unwrap(), libc::ENOENT);
    assert_eq!(parse_errno("ESTALE").unwrap(), libc::ESTALE);
    assert_eq!(parse_errno("ENOSPC").unwrap(), libc::ENOSPC);
    assert_eq!(parse_errno("EAGAIN").unwrap(), libc::EAGAIN);
    assert_eq!(parse_errno("EINTR").unwrap(), libc::EINTR);
}

#[test]
fn validate_rejects_zero_inode_count() {
    let mut cfg = Config::default();
    cfg.files.inode_count = 0;
    let err = cfg.validate().unwrap_err();
    match err {
        FsError::Config(msg) => assert!(msg.contains("inode_count")),
        other => panic!("expected Config error, got: {other:?}"),
    }
}

#[test]
fn validate_rejects_zero_file_size_bytes() {
    let mut cfg = Config::default();
    cfg.files.file_size_bytes = 0;
    let err = cfg.validate().unwrap_err();
    match err {
        FsError::Config(msg) => assert!(msg.contains("file_size_bytes")),
        other => panic!("expected Config error, got: {other:?}"),
    }
}

#[test]
fn validate_rejects_zero_concurrency_cap() {
    let mut cfg = Config::default();
    cfg.ops.insert(
        FsOp::Read,
        OpPolicy {
            concurrency_cap: 0,
            ..OpPolicy::default()
        },
    );
    let err = cfg.validate().unwrap_err();
    match err {
        FsError::Config(msg) => assert!(msg.contains("concurrency_cap")),
        other => panic!("expected Config error, got: {other:?}"),
    }
}

#[test]
fn validate_rejects_max_us_less_than_base_us() {
    let mut cfg = Config::default();
    cfg.ops.insert(
        FsOp::Write,
        OpPolicy {
            latency: LatencyProfile {
                base_us: 1000,
                max_us: 500,
                ..LatencyProfile::default()
            },
            ..OpPolicy::default()
        },
    );
    let err = cfg.validate().unwrap_err();
    match err {
        FsError::Config(msg) => assert!(msg.contains("max_us")),
        other => panic!("expected Config error, got: {other:?}"),
    }
}

#[test]
fn validate_rejects_fault_rate_above_one() {
    let mut cfg = Config::default();
    cfg.ops.insert(
        FsOp::Read,
        OpPolicy {
            faults: vec![FaultRule {
                op: FsOp::Read,
                errno: libc::EIO,
                rate: 1.5,
            }],
            ..OpPolicy::default()
        },
    );
    let err = cfg.validate().unwrap_err();
    match err {
        FsError::Config(msg) => assert!(msg.contains("rate")),
        other => panic!("expected Config error, got: {other:?}"),
    }
}

#[test]
fn validate_rejects_pareto_weight_above_one() {
    let mut cfg = Config::default();
    cfg.ops.insert(
        FsOp::Fsync,
        OpPolicy {
            latency: LatencyProfile {
                pareto_weight: 1.5,
                ..LatencyProfile::default()
            },
            ..OpPolicy::default()
        },
    );
    let err = cfg.validate().unwrap_err();
    match err {
        FsError::Config(msg) => assert!(msg.contains("pareto_weight")),
        other => panic!("expected Config error, got: {other:?}"),
    }
}

#[test]
fn default_config_validates() {
    Config::default()
        .validate()
        .expect("default config is valid");
}
