//! Integration tests for TOML config parsing and validation.

use fusecraft_core::config::{Config, DistributionConfig};
use fusecraft_core::error::ConfigError;
use fusecraft_core::op::OpKind;

#[test]
fn parse_minimal_config() {
    let toml = "";
    let cfg = Config::from_toml(toml).unwrap();
    assert_eq!(cfg.namespace.file_count, 100);
    assert_eq!(cfg.namespace.dir_count, 10);
    assert_eq!(cfg.namespace.max_depth, 4);
    assert_eq!(cfg.namespace.file_size, 4096);
    assert_eq!(cfg.concurrency.max_concurrent, 0);
    assert_eq!(cfg.bandwidth.read_bps, 0);
    assert_eq!(cfg.bandwidth.write_bps, 0);
}

#[test]
fn parse_full_config() {
    let toml = r#"
[defaults.latency]
[defaults.latency.distribution]
kind = "constant"
value_ms = 10

[defaults.fault]
probability = 0.01
errno = 5

[ops.read.latency]
[ops.read.latency.distribution]
kind = "uniform"
min_ms = 5
max_ms = 50

[ops.write.fault]
probability = 0.05
errno = 28

[concurrency]
max_concurrent = 64

[concurrency.per_op]
read = 32
write = 16

[bandwidth]
read_bps = 104857600
write_bps = 52428800

[namespace]
file_count = 1000
dir_count = 50
max_depth = 6
file_size = 65536
"#;
    let cfg = Config::from_toml(toml).unwrap();

    // Check defaults
    let defaults_latency = cfg.defaults.latency.as_ref().unwrap();
    match &defaults_latency.distribution {
        DistributionConfig::Constant { value_ms } => assert_eq!(*value_ms, 10),
        _ => panic!("expected constant distribution"),
    }

    let defaults_fault = cfg.defaults.fault.as_ref().unwrap();
    assert!((defaults_fault.probability - 0.01).abs() < f64::EPSILON);
    assert_eq!(defaults_fault.errno, 5);

    // Check per-op override for read
    let read_cfg = cfg.resolve_op(OpKind::Read);
    let read_latency = read_cfg.latency.as_ref().unwrap();
    match &read_latency.distribution {
        DistributionConfig::Uniform { min_ms, max_ms } => {
            assert_eq!(*min_ms, 5);
            assert_eq!(*max_ms, 50);
        }
        _ => panic!("expected uniform distribution for read"),
    }

    // Check per-op override for write
    let write_cfg = cfg.resolve_op(OpKind::Write);
    let write_fault = write_cfg.fault.as_ref().unwrap();
    assert!((write_fault.probability - 0.05).abs() < f64::EPSILON);
    assert_eq!(write_fault.errno, 28);

    // Check concurrency
    assert_eq!(cfg.concurrency.max_concurrent, 64);
    assert_eq!(cfg.concurrency_limit(OpKind::Read), Some(32));
    assert_eq!(cfg.concurrency_limit(OpKind::Write), Some(16));
    assert_eq!(cfg.concurrency_limit(OpKind::Lookup), Some(64)); // falls back to global

    // Check bandwidth
    assert_eq!(cfg.bandwidth.read_bps, 104_857_600);
    assert_eq!(cfg.bandwidth.write_bps, 52_428_800);

    // Check namespace
    assert_eq!(cfg.namespace.file_count, 1000);
    assert_eq!(cfg.namespace.dir_count, 50);
    assert_eq!(cfg.namespace.max_depth, 6);
    assert_eq!(cfg.namespace.file_size, 65536);
}

#[test]
fn parse_normal_distribution() {
    let toml = r#"
[defaults.latency.distribution]
kind = "normal"
mean_ms = 20.0
stddev_ms = 5.0
"#;
    let cfg = Config::from_toml(toml).unwrap();
    let latency = cfg.defaults.latency.as_ref().unwrap();
    match &latency.distribution {
        DistributionConfig::Normal { mean_ms, stddev_ms } => {
            assert!((mean_ms - 20.0).abs() < f64::EPSILON);
            assert!((stddev_ms - 5.0).abs() < f64::EPSILON);
        }
        _ => panic!("expected normal distribution"),
    }
}

#[test]
fn parse_pareto_distribution() {
    let toml = r#"
[defaults.latency.distribution]
kind = "pareto"
scale_ms = 1.0
shape = 2.5
"#;
    let cfg = Config::from_toml(toml).unwrap();
    let latency = cfg.defaults.latency.as_ref().unwrap();
    match &latency.distribution {
        DistributionConfig::Pareto { scale_ms, shape } => {
            assert!((scale_ms - 1.0).abs() < f64::EPSILON);
            assert!((shape - 2.5).abs() < f64::EPSILON);
        }
        _ => panic!("expected pareto distribution"),
    }
}

#[test]
fn validation_rejects_invalid_probability_too_high() {
    let toml = r#"
[defaults.fault]
probability = 1.5
errno = 5
"#;
    let err = Config::from_toml(toml).unwrap_err();
    match err {
        ConfigError::Validation { field, .. } => {
            assert!(field.contains("probability"));
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn validation_rejects_negative_probability() {
    let toml = r#"
[defaults.fault]
probability = -0.1
errno = 5
"#;
    let err = Config::from_toml(toml).unwrap_err();
    match err {
        ConfigError::Validation { field, .. } => {
            assert!(field.contains("probability"));
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn validation_rejects_min_greater_than_max() {
    let toml = r#"
[defaults.latency.distribution]
kind = "uniform"
min_ms = 100
max_ms = 10
"#;
    let err = Config::from_toml(toml).unwrap_err();
    match err {
        ConfigError::Validation { field, reason } => {
            assert!(field.contains("distribution"));
            assert!(reason.contains("min_ms"));
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn validation_rejects_negative_stddev() {
    let toml = r#"
[defaults.latency.distribution]
kind = "normal"
mean_ms = 10.0
stddev_ms = -1.0
"#;
    let err = Config::from_toml(toml).unwrap_err();
    match err {
        ConfigError::Validation { field, .. } => {
            assert!(field.contains("distribution"));
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn validation_rejects_zero_pareto_scale() {
    let toml = r#"
[defaults.latency.distribution]
kind = "pareto"
scale_ms = 0.0
shape = 2.0
"#;
    let err = Config::from_toml(toml).unwrap_err();
    match err {
        ConfigError::Validation { field, .. } => {
            assert!(field.contains("distribution"));
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn validation_rejects_zero_pareto_shape() {
    let toml = r#"
[defaults.latency.distribution]
kind = "pareto"
scale_ms = 1.0
shape = 0.0
"#;
    let err = Config::from_toml(toml).unwrap_err();
    match err {
        ConfigError::Validation { field, .. } => {
            assert!(field.contains("distribution"));
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn validation_rejects_zero_max_depth() {
    let toml = r#"
[namespace]
max_depth = 0
"#;
    let err = Config::from_toml(toml).unwrap_err();
    match err {
        ConfigError::Validation { field, reason } => {
            assert_eq!(field, "namespace.max_depth");
            assert!(reason.contains(">= 1"));
        }
        other => panic!("expected Validation error, got: {other:?}"),
    }
}

#[test]
fn parse_error_on_invalid_toml() {
    let toml = "this is not valid [[[ toml";
    let err = Config::from_toml(toml).unwrap_err();
    assert!(matches!(err, ConfigError::Parse(_)));
}

#[test]
fn parse_error_on_unknown_field() {
    let toml = r#"
[defaults]
unknown_field = true
"#;
    let err = Config::from_toml(toml).unwrap_err();
    assert!(matches!(err, ConfigError::Parse(_)));
}

#[test]
fn resolve_op_falls_back_to_defaults() {
    let toml = r#"
[defaults.latency.distribution]
kind = "constant"
value_ms = 42
"#;
    let cfg = Config::from_toml(toml).unwrap();
    // No per-op override for Mkdir, should get defaults
    let mkdir_cfg = cfg.resolve_op(OpKind::Mkdir);
    let latency = mkdir_cfg.latency.as_ref().unwrap();
    match &latency.distribution {
        DistributionConfig::Constant { value_ms } => assert_eq!(*value_ms, 42),
        _ => panic!("expected constant distribution"),
    }
}

#[test]
fn concurrency_limit_unlimited_when_zero() {
    let toml = r#"
[concurrency]
max_concurrent = 0
"#;
    let cfg = Config::from_toml(toml).unwrap();
    assert_eq!(cfg.concurrency_limit(OpKind::Read), None);
}

#[test]
fn op_kind_display_and_all() {
    let all = OpKind::all();
    assert_eq!(all.len(), 16);
    assert_eq!(format!("{}", OpKind::Read), "read");
    assert_eq!(format!("{}", OpKind::Write), "write");
    assert_eq!(format!("{}", OpKind::Lookup), "lookup");
}

#[test]
fn op_kind_data_vs_metadata() {
    assert!(OpKind::Read.is_data_op());
    assert!(OpKind::Write.is_data_op());
    assert!(!OpKind::Lookup.is_data_op());
    assert!(OpKind::Lookup.is_metadata_op());
    assert!(!OpKind::Read.is_metadata_op());
}

#[test]
fn expected_duration_constant() {
    let dist = DistributionConfig::Constant { value_ms: 50 };
    assert_eq!(dist.expected_duration(), std::time::Duration::from_millis(50));
}

#[test]
fn expected_duration_uniform() {
    let dist = DistributionConfig::Uniform {
        min_ms: 10,
        max_ms: 30,
    };
    assert_eq!(dist.expected_duration(), std::time::Duration::from_millis(20));
}
