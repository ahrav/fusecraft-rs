# Configuration reference

fusecraft is configured with a single TOML file. Every key is optional; unspecified fields use the defaults shown below.

## Top-level

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `seed` | `u64` | `42` | RNG seed for reproducible sampling |

## `[mount]`

FUSE mount options passed to the kernel.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `fs_name` | `string` | `"fusecraft"` | Filesystem name reported to the kernel |
| `subtype` | `string` | `"sim"` | Filesystem subtype |
| `auto_unmount` | `bool` | `true` | Auto-unmount on process exit |
| `default_permissions` | `bool` | `true` | Enable kernel permission checking |
| `read_only` | `bool` | `false` | Mount as read-only |
| `direct_io` | `bool` | `false` | Bypass page cache (direct I/O) |

## `[files]`

Virtual filesystem layout.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `inode_count` | `u64` | `1000` | Number of inodes to pre-create (must be > 0) |
| `file_size_bytes` | `u64` | `65536` | Default file size in bytes (must be > 0) |
| `root_layout` | `string` | `"flat"` | Root directory layout strategy |
| `write_mode` | `string` | `"discard"` | How writes are handled |

### `root_layout` values

- `"flat"` — All files in a single `objects/` directory with zero-padded 6-digit names (e.g. `000001`). Maximum 999,999 entries.

### `write_mode` values

- `"discard"` — Written data is accepted (the syscall returns the byte count) but immediately dropped. This is the only supported mode: reads always return bytes derived from `(ino, offset, seed)`, which keeps the determinism contract intact.

## `[ops.<op>]`

Per-operation policy. `<op>` is one of: `lookup`, `getattr`, `open`, `read`, `write`, `flush`, `release`, `fsync`, `readdir`, `statfs`, `access`.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `concurrency_cap` | `usize` | `64` | Maximum concurrent in-flight operations (must be > 0) |
| `queue_cap` | `usize` | `0` | Maximum queued operations waiting for a slot |

Operations exceeding `concurrency_cap + queue_cap` are rejected with `EAGAIN`.

## `[ops.<op>.latency]`

Latency injection model: `base + lognormal body + pareto tail`, clamped to `max_us`.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `base_us` | `u64` | `0` | Fixed base latency in microseconds |
| `lognormal_median_us` | `f64` | `100.0` | Median of the lognormal component (must be >= 0) |
| `lognormal_sigma` | `f64` | `0.5` | Sigma (shape) of the lognormal component (must be >= 0) |
| `pareto_weight` | `f64` | `0.0` | Weight of the Pareto tail (0.0 to 1.0) |
| `pareto_xm_us` | `f64` | `1000.0` | Scale (xm) of the Pareto distribution |
| `pareto_alpha` | `f64` | `1.5` | Shape (alpha) of the Pareto distribution (must be > 0 when `pareto_weight` > 0) |
| `max_us` | `u64` | `1000000` | Maximum latency clamp (must be >= `base_us`) |

All `f64` values must be finite (no NaN or infinity).

## `[ops.<op>.bandwidth]`

Bandwidth throttling. Only applies to `read` and `write` operations.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `mib_per_sec` | `f64` | *(required)* | Sustained bandwidth in MiB/s |
| `burst_bytes` | `u64` | *(required)* | Burst allowance in bytes |

## `[[ops.<op>.faults]]`

Fault injection rules. Multiple rules can target the same operation.

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `op` | `string` | *(required)* | Target operation (must match the enclosing `<op>`) |
| `errno` | `string` | *(required)* | Error to inject: `EIO`, `ENOENT`, `ESTALE`, `ENOSPC`, `EAGAIN`, or `EINTR` |
| `rate` | `f64` | *(required)* | Probability of triggering (0.0 to 1.0, must be finite) |

## `[ops.<op>.size_tier]`

Optional size-keyed alternate policy. Only valid on `read` and `write` — validation rejects `size_tier` on metadata ops. When an op's requested length exceeds `threshold_bytes`, the engine swaps in the large-tier latency, bandwidth, and fault rules in place of the base-policy values. Requests at or below the threshold use the base policy unchanged (the comparison is strict `>`).

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `threshold_bytes` | `u64` | *(required)* | Length in bytes above which requests route to the large tier. Must be > 0 — a zero threshold is equivalent to setting the base policy directly. |
| `large` | table | *(required)* | The alternate latency / bandwidth / faults triple applied to large requests. |

### `[ops.<op>.size_tier.large]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `latency` | table | *(see `[ops.<op>.latency]` above)* | Latency profile for large requests. Same shape as the base latency table. |
| `bandwidth` | table | *(none)* | Optional bandwidth profile for large requests. Same shape as the base bandwidth table. |
| `faults` | array of tables | *(empty)* | Fault rules for large requests. Same shape as the base faults array. |

### Concurrency and queueing are shared, not tiered

`concurrency_cap` and `queue_cap` live only on the base policy — they are intentionally absent from `size_tier.large`. The engine runs a single `BlockingLimiter` queue per op, and the tier split happens *after* limiter admission. Splitting concurrency across tiers would require two queues per op and would break the single-queue invariant the limiter is built on. Both small and large requests therefore share the same admission limit.

### Worked example: 128 KiB split

The following config models a backend where small reads (<= 128 KiB) are served from a fast local cache and large reads spill to a slow remote tier with a ~850 MiB/s ceiling:

```toml
[ops.read]
concurrency_cap = 64
queue_cap = 128

[ops.read.latency]
base_us = 2000          # 2 ms cache-hit latency

[ops.read.size_tier]
threshold_bytes = 131072   # 128 KiB

[ops.read.size_tier.large.latency]
base_us = 50000         # 50 ms remote-tier base latency
pareto_weight = 0.02
pareto_xm_us = 50000.0
pareto_alpha = 1.3
max_us = 2000000

[ops.read.size_tier.large.bandwidth]
mib_per_sec = 850.0
burst_bytes = 1048576
```

A full working example lives at [`examples/size_tiered.toml`](../examples/size_tiered.toml).

## `[metrics]`

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `jsonl_path` | `string` | *(none)* | Path to write JSON-lines event log. Omit to disable event logging. |

## Validation rules

`Config::validate()` enforces:

- `files.inode_count` > 0
- `files.file_size_bytes` > 0
- `concurrency_cap` > 0 for every configured op
- `max_us` >= `base_us`
- `pareto_weight` in [0.0, 1.0]
- `pareto_alpha` > 0 when `pareto_weight` > 0
- `lognormal_median_us` >= 0
- `lognormal_sigma` >= 0
- All float fields must be finite
- `rate` in [0.0, 1.0] and finite for every fault rule
- `size_tier` is only permitted on `read` and `write` ops
- `size_tier.threshold_bytes` > 0
- All latency and fault invariants above apply to `size_tier.large.latency` and `size_tier.large.faults` as well

## Examples

See the [`examples/`](../examples/) directory for complete, valid configuration files.
