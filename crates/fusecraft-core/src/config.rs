//! Configuration types and TOML parsing for fusecraft-rs.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::FsError;
use crate::op::FsOp;

/// Top-level simulator configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// RNG seed for reproducibility.
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// FUSE mount options.
    #[serde(default)]
    pub mount: MountConfig,
    /// Virtual filesystem file layout.
    #[serde(default)]
    pub files: FilesConfig,
    /// Per-operation policies.
    #[serde(default)]
    pub ops: HashMap<FsOp, OpPolicy>,
    /// Metrics/telemetry configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,
}

/// FUSE mount configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MountConfig {
    /// Filesystem name reported to the kernel.
    #[serde(default = "default_fs_name")]
    pub fs_name: String,
    /// Filesystem subtype.
    #[serde(default = "default_subtype")]
    pub subtype: String,
    /// Whether to auto-unmount on process exit.
    #[serde(default = "default_true")]
    pub auto_unmount: bool,
    /// Whether to enable default_permissions checking.
    #[serde(default = "default_true")]
    pub default_permissions: bool,
    /// Mount as read-only.
    #[serde(default)]
    pub read_only: bool,
    /// Enable direct I/O (bypass page cache).
    #[serde(default)]
    pub direct_io: bool,
}

/// Virtual filesystem layout configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FilesConfig {
    /// Number of inodes to pre-create.
    #[serde(default = "default_inode_count")]
    pub inode_count: u64,
    /// Default file size in bytes.
    #[serde(default = "default_file_size_bytes")]
    pub file_size_bytes: u64,
    /// Root directory layout strategy.
    #[serde(default)]
    pub root_layout: RootLayout,
    /// How writes are handled.
    #[serde(default)]
    pub write_mode: WriteMode,
}

/// Root directory layout strategy.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootLayout {
    /// All files in a single flat directory.
    #[default]
    Flat,
}

/// Write handling mode.
///
/// The MVP supports only `Discard` — written bytes are accepted (so `write(2)`
/// succeeds and reports the expected byte count) and then dropped. This
/// preserves the determinism contract for reads, which always return bytes
/// derived from `(ino, offset, seed)`. Additional modes may be added later.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WriteMode {
    /// Discard written data immediately.
    #[default]
    Discard,
}

/// Per-operation policy: concurrency, latency, bandwidth, faults.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpPolicy {
    /// Maximum concurrent in-flight operations of this type.
    #[serde(default = "default_concurrency_cap")]
    pub concurrency_cap: usize,
    /// Maximum queued operations waiting for a concurrency slot.
    #[serde(default)]
    pub queue_cap: usize,
    /// Latency injection profile.
    #[serde(default)]
    pub latency: LatencyProfile,
    /// Optional bandwidth throttling.
    #[serde(default)]
    pub bandwidth: Option<BandwidthProfile>,
    /// Fault injection rules.
    #[serde(default)]
    pub faults: Vec<FaultRule>,
    /// Optional size-keyed alternate profile for Read/Write.
    ///
    /// When an op's requested length exceeds `size_tier.threshold_bytes`, the
    /// engine swaps in the large-tier latency/bandwidth/faults in place of the
    /// base ones. Metadata ops (Open, Fsync, Lookup, etc.) always use the base
    /// policy regardless of this field; validation rejects `size_tier` on
    /// non-data ops.
    #[serde(default)]
    pub size_tier: Option<SizeTier>,
}

/// Size-keyed alternate profile for Read/Write ops.
///
/// Models workloads where small and large I/O follow different performance
/// characteristics — e.g. grotto-rs's 128 KiB split between pre-buffered hits
/// and NFS streaming.
///
/// Note: `concurrency_cap` and `queue_cap` are intentionally absent here. The
/// engine runs a single [`crate::limiter::BlockingLimiter`] per op, and the
/// tier split happens *after* limiter admission. Splitting concurrency across
/// tiers would require two queues per op and break the single-queue invariant
/// that the limiter is built on, so the large tier inherits both caps from
/// the base policy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SizeTier {
    /// Length threshold in bytes. Requests with `len > threshold_bytes` route
    /// to the large tier. Must be non-zero (a zero threshold is equivalent to
    /// setting the base policy directly).
    pub threshold_bytes: u64,
    /// Alternate policy applied when the threshold is exceeded.
    pub large: LargeTierPolicy,
}

/// The latency / bandwidth / fault triple that replaces the base policy when
/// a Read/Write exceeds the [`SizeTier`] threshold.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LargeTierPolicy {
    /// Latency profile for large ops.
    #[serde(default)]
    pub latency: LatencyProfile,
    /// Optional bandwidth throttling for large ops.
    #[serde(default)]
    pub bandwidth: Option<BandwidthProfile>,
    /// Fault rules for large ops.
    #[serde(default)]
    pub faults: Vec<FaultRule>,
}

/// Latency injection profile.
///
/// The model is: base + lognormal body + pareto tail, clamped to max.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatencyProfile {
    /// Fixed base latency in microseconds.
    #[serde(default)]
    pub base_us: u64,
    /// Median of the lognormal component (microseconds).
    #[serde(default = "default_lognormal_median")]
    pub lognormal_median_us: f64,
    /// Sigma (shape) of the lognormal component.
    #[serde(default = "default_lognormal_sigma")]
    pub lognormal_sigma: f64,
    /// Weight of the Pareto tail component (0.0 to 1.0).
    #[serde(default)]
    pub pareto_weight: f64,
    /// Scale (xm) of the Pareto distribution (microseconds).
    #[serde(default = "default_pareto_xm")]
    pub pareto_xm_us: f64,
    /// Shape (alpha) of the Pareto distribution.
    #[serde(default = "default_pareto_alpha")]
    pub pareto_alpha: f64,
    /// Maximum latency clamp in microseconds.
    #[serde(default = "default_max_us")]
    pub max_us: u64,
}

/// Bandwidth throttling profile.
#[derive(Clone, Debug, Serialize)]
pub struct BandwidthProfile {
    /// Sustained bandwidth in bytes per second.
    pub bytes_per_sec: f64,
    /// Burst allowance in bytes.
    pub burst_bytes: u64,
}

/// Intermediate struct for TOML deserialization of BandwidthProfile.
/// The TOML uses `mib_per_sec` which we convert to `bytes_per_sec`.
#[derive(Deserialize)]
struct BandwidthProfileRaw {
    mib_per_sec: f64,
    burst_bytes: u64,
}

impl<'de> Deserialize<'de> for BandwidthProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = BandwidthProfileRaw::deserialize(deserializer)?;
        Ok(BandwidthProfile {
            bytes_per_sec: raw.mib_per_sec * 1024.0 * 1024.0,
            burst_bytes: raw.burst_bytes,
        })
    }
}

/// A fault injection rule.
#[derive(Clone, Debug, Serialize)]
pub struct FaultRule {
    /// Which operation this rule targets.
    pub op: FsOp,
    /// The errno to inject.
    pub errno: i32,
    /// Probability of triggering this fault (0.0 to 1.0).
    pub rate: f64,
}

/// Intermediate struct for TOML deserialization of FaultRule.
/// Errno is specified as a string name (e.g. "EIO").
#[derive(Deserialize)]
struct FaultRuleRaw {
    op: FsOp,
    errno: String,
    rate: f64,
}

impl<'de> Deserialize<'de> for FaultRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = FaultRuleRaw::deserialize(deserializer)?;
        let errno = parse_errno(&raw.errno).map_err(serde::de::Error::custom)?;
        Ok(FaultRule {
            op: raw.op,
            errno,
            rate: raw.rate,
        })
    }
}

/// Metrics/telemetry configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Path to write JSON-lines event log.
    #[serde(default)]
    pub jsonl_path: Option<PathBuf>,
}

// --- Defaults ---

fn default_seed() -> u64 {
    42
}

fn default_fs_name() -> String {
    "fusecraft".into()
}

fn default_subtype() -> String {
    "sim".into()
}

fn default_true() -> bool {
    true
}

fn default_inode_count() -> u64 {
    1000
}

fn default_file_size_bytes() -> u64 {
    65536
}

fn default_concurrency_cap() -> usize {
    64
}

fn default_lognormal_median() -> f64 {
    100.0
}

fn default_lognormal_sigma() -> f64 {
    0.5
}

fn default_pareto_xm() -> f64 {
    1000.0
}

fn default_pareto_alpha() -> f64 {
    1.5
}

fn default_max_us() -> u64 {
    1_000_000
}

// --- Default impls ---

impl Default for Config {
    fn default() -> Self {
        Self {
            seed: default_seed(),
            mount: MountConfig::default(),
            files: FilesConfig::default(),
            ops: HashMap::new(),
            metrics: MetricsConfig::default(),
        }
    }
}

impl Default for MountConfig {
    fn default() -> Self {
        Self {
            fs_name: default_fs_name(),
            subtype: default_subtype(),
            auto_unmount: true,
            default_permissions: true,
            read_only: false,
            direct_io: false,
        }
    }
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            inode_count: default_inode_count(),
            file_size_bytes: default_file_size_bytes(),
            root_layout: RootLayout::default(),
            write_mode: WriteMode::default(),
        }
    }
}

impl Default for OpPolicy {
    fn default() -> Self {
        Self {
            concurrency_cap: default_concurrency_cap(),
            queue_cap: 0,
            latency: LatencyProfile::default(),
            bandwidth: None,
            faults: Vec::new(),
            size_tier: None,
        }
    }
}

impl Default for LatencyProfile {
    fn default() -> Self {
        Self {
            base_us: 0,
            lognormal_median_us: default_lognormal_median(),
            lognormal_sigma: default_lognormal_sigma(),
            pareto_weight: 0.0,
            pareto_xm_us: default_pareto_xm(),
            pareto_alpha: default_pareto_alpha(),
            max_us: default_max_us(),
        }
    }
}

// --- Errno parsing ---

/// Parse an errno name string into its numeric value.
///
/// Supported: EIO, ENOENT, ESTALE, ENOSPC, EAGAIN, EINTR.
pub fn parse_errno(name: &str) -> Result<i32, FsError> {
    match name {
        "EIO" => Ok(libc::EIO),
        "ENOENT" => Ok(libc::ENOENT),
        "ESTALE" => Ok(libc::ESTALE),
        "ENOSPC" => Ok(libc::ENOSPC),
        "EAGAIN" => Ok(libc::EAGAIN),
        "EINTR" => Ok(libc::EINTR),
        other => Err(FsError::Config(format!("unknown errno: {other}"))),
    }
}

// --- Validation ---

impl Config {
    /// Validate all configuration invariants.
    pub fn validate(&self) -> Result<(), FsError> {
        if self.files.inode_count == 0 {
            return Err(FsError::Config("files.inode_count must be > 0".into()));
        }
        if self.files.file_size_bytes == 0 {
            return Err(FsError::Config("files.file_size_bytes must be > 0".into()));
        }

        for (op, policy) in &self.ops {
            let prefix = format!("ops.{}", op.as_str());
            validate_op_policy(*op, policy, &prefix)?;
        }

        Ok(())
    }
}

fn validate_op_policy(op: FsOp, policy: &OpPolicy, prefix: &str) -> Result<(), FsError> {
    if policy.concurrency_cap == 0 {
        return Err(FsError::Config(format!(
            "{prefix}.concurrency_cap must be > 0"
        )));
    }

    validate_latency(&policy.latency, &format!("{prefix}.latency"))?;
    validate_faults(&policy.faults, &format!("{prefix}.faults"))?;

    if let Some(tier) = &policy.size_tier {
        if !matches!(op, FsOp::Read | FsOp::Write) {
            return Err(FsError::Config(format!(
                "{prefix}.size_tier is only valid on Read or Write ops, not {}",
                op.as_str()
            )));
        }
        if tier.threshold_bytes == 0 {
            return Err(FsError::Config(format!(
                "{prefix}.size_tier.threshold_bytes must be > 0 (a zero threshold is \
                 equivalent to not setting size_tier at all — configure the base policy \
                 directly instead)"
            )));
        }
        validate_latency(
            &tier.large.latency,
            &format!("{prefix}.size_tier.large.latency"),
        )?;
        validate_faults(
            &tier.large.faults,
            &format!("{prefix}.size_tier.large.faults"),
        )?;
    }

    Ok(())
}

fn validate_faults(faults: &[FaultRule], prefix: &str) -> Result<(), FsError> {
    for (i, fault) in faults.iter().enumerate() {
        if !fault.rate.is_finite() || fault.rate < 0.0 || fault.rate > 1.0 {
            return Err(FsError::Config(format!(
                "{prefix}[{i}].rate must be a finite value in [0.0, 1.0], got {}",
                fault.rate
            )));
        }
    }
    Ok(())
}

fn validate_latency(latency: &LatencyProfile, prefix: &str) -> Result<(), FsError> {
    // Reject non-finite floats (NaN, ±inf) up front — comparison-only checks
    // below would otherwise silently let NaN pass.
    for (name, value) in [
        ("lognormal_median_us", latency.lognormal_median_us),
        ("lognormal_sigma", latency.lognormal_sigma),
        ("pareto_weight", latency.pareto_weight),
        ("pareto_xm_us", latency.pareto_xm_us),
        ("pareto_alpha", latency.pareto_alpha),
    ] {
        if !value.is_finite() {
            return Err(FsError::Config(format!(
                "{prefix}.{name} must be finite, got {value}"
            )));
        }
    }

    if latency.max_us < latency.base_us {
        return Err(FsError::Config(format!(
            "{prefix}.max_us ({}) must be >= base_us ({})",
            latency.max_us, latency.base_us
        )));
    }
    if latency.pareto_weight < 0.0 || latency.pareto_weight > 1.0 {
        return Err(FsError::Config(format!(
            "{prefix}.pareto_weight must be in [0.0, 1.0], got {}",
            latency.pareto_weight
        )));
    }
    if latency.pareto_weight > 0.0 && latency.pareto_alpha <= 0.0 {
        return Err(FsError::Config(format!(
            "{prefix}.pareto_alpha must be > 0.0 when pareto_weight > 0",
        )));
    }
    if latency.lognormal_median_us < 0.0 {
        return Err(FsError::Config(format!(
            "{prefix}.lognormal_median_us must be >= 0.0"
        )));
    }
    if latency.lognormal_sigma < 0.0 {
        return Err(FsError::Config(format!(
            "{prefix}.lognormal_sigma must be >= 0.0"
        )));
    }
    Ok(())
}
