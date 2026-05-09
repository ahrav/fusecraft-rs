//! Configuration types and TOML parsing for fusecraft-rs.
//!
//! The simulator is configured via a TOML file that specifies latency
//! distributions, fault injection rules, concurrency limits, and bandwidth
//! throttling per operation kind.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;
use crate::op::OpKind;

/// Top-level simulator configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Global defaults applied to all operations unless overridden.
    #[serde(default)]
    pub defaults: OpConfig,

    /// Per-operation overrides keyed by operation kind.
    #[serde(default)]
    pub ops: HashMap<OpKind, OpConfig>,

    /// Concurrency limiting configuration.
    #[serde(default)]
    pub concurrency: ConcurrencyConfig,

    /// Bandwidth throttling configuration.
    #[serde(default)]
    pub bandwidth: BandwidthConfig,

    /// Namespace (virtual filesystem tree) configuration.
    #[serde(default)]
    pub namespace: NamespaceConfig,
}

/// Per-operation configuration: latency injection and fault injection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpConfig {
    /// Latency injection settings.
    #[serde(default)]
    pub latency: Option<LatencyConfig>,

    /// Fault injection settings.
    #[serde(default)]
    pub fault: Option<FaultConfig>,
}

/// Latency injection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyConfig {
    /// The distribution to sample latency from.
    pub distribution: DistributionConfig,
}

/// A probability distribution for sampling durations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DistributionConfig {
    /// Fixed constant latency.
    Constant {
        /// Duration in milliseconds.
        value_ms: u64,
    },
    /// Uniform distribution between min and max.
    Uniform {
        /// Minimum duration in milliseconds.
        min_ms: u64,
        /// Maximum duration in milliseconds.
        max_ms: u64,
    },
    /// Normal (Gaussian) distribution.
    Normal {
        /// Mean in milliseconds.
        mean_ms: f64,
        /// Standard deviation in milliseconds.
        stddev_ms: f64,
    },
    /// Pareto distribution for heavy-tailed latency.
    Pareto {
        /// Scale parameter (minimum value) in milliseconds.
        scale_ms: f64,
        /// Shape parameter (alpha).
        shape: f64,
    },
}

/// Fault injection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultConfig {
    /// Probability of injecting a fault (0.0 to 1.0).
    pub probability: f64,

    /// The errno value to return when a fault is injected.
    pub errno: i32,
}

/// Concurrency limiting configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyConfig {
    /// Maximum number of concurrent in-flight operations (0 = unlimited).
    #[serde(default)]
    pub max_concurrent: u32,

    /// Per-operation concurrency limits.
    #[serde(default)]
    pub per_op: HashMap<OpKind, u32>,
}

impl Default for ConcurrencyConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 0,
            per_op: HashMap::new(),
        }
    }
}

/// Bandwidth throttling configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BandwidthConfig {
    /// Maximum read bandwidth in bytes per second (0 = unlimited).
    #[serde(default)]
    pub read_bps: u64,

    /// Maximum write bandwidth in bytes per second (0 = unlimited).
    #[serde(default)]
    pub write_bps: u64,
}

impl Default for BandwidthConfig {
    fn default() -> Self {
        Self {
            read_bps: 0,
            write_bps: 0,
        }
    }
}

/// Virtual filesystem namespace configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceConfig {
    /// Number of files to pre-populate in the virtual tree.
    #[serde(default = "default_file_count")]
    pub file_count: u64,

    /// Number of directories to pre-populate.
    #[serde(default = "default_dir_count")]
    pub dir_count: u64,

    /// Maximum depth of the directory tree.
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,

    /// Default file size in bytes for synthetic content.
    #[serde(default = "default_file_size")]
    pub file_size: u64,
}

fn default_file_count() -> u64 {
    100
}
fn default_dir_count() -> u64 {
    10
}
fn default_max_depth() -> u32 {
    4
}
fn default_file_size() -> u64 {
    4096
}

impl Default for NamespaceConfig {
    fn default() -> Self {
        Self {
            file_count: default_file_count(),
            dir_count: default_dir_count(),
            max_depth: default_max_depth(),
            file_size: default_file_size(),
        }
    }
}

impl Config {
    /// Parse a TOML string into a validated `Config`.
    pub fn from_toml(toml_str: &str) -> Result<Self, ConfigError> {
        let config: Config =
            toml::from_str(toml_str).map_err(|e| ConfigError::Parse(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Load and parse a TOML config file from the given path.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            ConfigError::Parse(format!("failed to read {}: {}", path.display(), e))
        })?;
        Self::from_toml(&content)
    }

    /// Validate all configuration invariants.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if let Some(ref latency) = self.defaults.latency {
            validate_latency(latency, "defaults.latency")?;
        }
        if let Some(ref fault) = self.defaults.fault {
            validate_fault(fault, "defaults.fault")?;
        }

        for (op, op_cfg) in &self.ops {
            let prefix = format!("ops.{op}");
            if let Some(ref latency) = op_cfg.latency {
                validate_latency(latency, &prefix)?;
            }
            if let Some(ref fault) = op_cfg.fault {
                validate_fault(fault, &prefix)?;
            }
        }

        if self.namespace.max_depth == 0 {
            return Err(ConfigError::Validation {
                field: "namespace.max_depth".into(),
                reason: "must be >= 1".into(),
            });
        }

        Ok(())
    }

    /// Resolve the effective `OpConfig` for a given operation kind.
    ///
    /// Per-op settings override defaults.
    pub fn resolve_op(&self, op: OpKind) -> &OpConfig {
        self.ops.get(&op).unwrap_or(&self.defaults)
    }

    /// Returns the effective concurrency limit for an operation.
    /// Returns `None` if unlimited.
    pub fn concurrency_limit(&self, op: OpKind) -> Option<u32> {
        if let Some(&limit) = self.concurrency.per_op.get(&op) {
            if limit > 0 {
                return Some(limit);
            }
        }
        if self.concurrency.max_concurrent > 0 {
            Some(self.concurrency.max_concurrent)
        } else {
            None
        }
    }
}

impl DistributionConfig {
    /// Convert this distribution config to a Duration representing the mean/expected value.
    /// Useful for display and diagnostics.
    pub fn expected_duration(&self) -> Duration {
        match self {
            DistributionConfig::Constant { value_ms } => Duration::from_millis(*value_ms),
            DistributionConfig::Uniform { min_ms, max_ms } => {
                Duration::from_millis((min_ms + max_ms) / 2)
            }
            DistributionConfig::Normal { mean_ms, .. } => {
                Duration::from_millis(*mean_ms as u64)
            }
            DistributionConfig::Pareto { scale_ms, shape } => {
                if *shape > 1.0 {
                    let mean = scale_ms * shape / (shape - 1.0);
                    Duration::from_millis(mean as u64)
                } else {
                    // Mean is infinite for shape <= 1
                    Duration::from_millis(*scale_ms as u64)
                }
            }
        }
    }
}

fn validate_latency(latency: &LatencyConfig, prefix: &str) -> Result<(), ConfigError> {
    match &latency.distribution {
        DistributionConfig::Uniform { min_ms, max_ms } => {
            if min_ms > max_ms {
                return Err(ConfigError::Validation {
                    field: format!("{prefix}.distribution"),
                    reason: format!("min_ms ({min_ms}) must be <= max_ms ({max_ms})"),
                });
            }
        }
        DistributionConfig::Normal { stddev_ms, .. } => {
            if *stddev_ms < 0.0 {
                return Err(ConfigError::Validation {
                    field: format!("{prefix}.distribution"),
                    reason: "stddev_ms must be >= 0".into(),
                });
            }
        }
        DistributionConfig::Pareto { scale_ms, shape } => {
            if *scale_ms <= 0.0 {
                return Err(ConfigError::Validation {
                    field: format!("{prefix}.distribution"),
                    reason: "scale_ms must be > 0".into(),
                });
            }
            if *shape <= 0.0 {
                return Err(ConfigError::Validation {
                    field: format!("{prefix}.distribution"),
                    reason: "shape must be > 0".into(),
                });
            }
        }
        DistributionConfig::Constant { .. } => {}
    }
    Ok(())
}

fn validate_fault(fault: &FaultConfig, prefix: &str) -> Result<(), ConfigError> {
    if fault.probability < 0.0 || fault.probability > 1.0 {
        return Err(ConfigError::Validation {
            field: format!("{prefix}.probability"),
            reason: format!(
                "probability must be between 0.0 and 1.0, got {}",
                fault.probability
            ),
        });
    }
    Ok(())
}
