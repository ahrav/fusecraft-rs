# fusecraft

[![CI](https://github.com/ahrav/fusecraft-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/ahrav/fusecraft-rs/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE-MIT)

fusecraft is a deterministic FUSE filesystem simulator. It mounts a virtual
filesystem that behaves like a flaky, latency-prone, bandwidth-limited remote
store — under your full control, with reproducible results.

fusecraft models **syscall-visible behavior**: per-operation latency, fault
injection, concurrency queueing, and bandwidth throttling. Every decision is
a pure function of `(seed, op, ino, offset, len, seq)`, so the same config
and workload yield identical event logs across runs.

## Non-goals

fusecraft does **not** simulate NFS, S3, SMB, EBS, ext4, xfs, or any exact
storage protocol. Specifically, fusecraft does not model:

- NFS lock managers, delegations, or stale filehandle lifecycle.
- S3 eventual consistency, multipart upload, or conditional writes.
- SMB oplocks, share modes, or DFS referrals.
- EBS volume attachment, snapshot consistency, or IOPS credits.
- ext4/xfs journaling, extent allocation, or fsck behavior.
- Kernel page cache hit/miss effects (beyond honoring `direct_io`).
- Network partitions or time-correlated faults.
- Data corruption (read content is deterministic; fusecraft never garbles bytes).
- Metadata consistency beyond the static flat namespace.

Protocol-fidelity simulators are expensive to build, impossible to validate
without the real system, and misleading when they inevitably diverge from
production behavior. fusecraft instead controls only what your application
observes at the syscall boundary: timing, errors, and throughput. That is
sufficient to answer questions like "does my app retry correctly when reads
fail?" or "does it deadlock when writes queue?" — without pretending to be a
particular protocol.

See [`docs/fidelity.md`](docs/fidelity.md) for the full discussion.

## Workspace layout

| Crate | Role | crates.io |
|-------|------|-----------|
| [`fusecraft-core`](crates/fusecraft-core) | Simulator engine, policy/sampling/metrics logic | `fusecraft-core` |
| [`fusecraft-fuser`](crates/fusecraft-fuser) | FUSE kernel adapter built on [fuser](https://crates.io/crates/fuser) | `fusecraft-fuser` |
| [`fusecraft-cli`](crates/fusecraft-cli) | `fusecraft` binary: `mount`, `validate-config`, `print-default-config` | `fusecraft-cli` |

All simulation logic lives in `fusecraft-core`. The FUSE adapter and CLI are
thin wrappers.

## Quick start

```bash
# Build
cargo build --workspace --release

# Validate a config without mounting
cargo run --bin fusecraft -- validate-config --config examples/zero_latency.toml

# Mount with an example config
mkdir -p /tmp/sim
cargo run --release --bin fusecraft -- \
    mount --config examples/read_heavy.toml --mountpoint /tmp/sim

# In another shell: run your workload against the mount
dd if=/tmp/sim/objects/000001 of=/dev/null bs=64k

# Ctrl-C the foreground fusecraft process to unmount, or
fusermount -u /tmp/sim
```

## Configuration

fusecraft is driven by a single TOML file. See
[`docs/config.md`](docs/config.md) for the full reference.

| Example | Purpose |
|---------|---------|
| [`examples/minimal.toml`](examples/minimal.toml) | Bare minimum — just a seed |
| [`examples/zero_latency.toml`](examples/zero_latency.toml) | Every op pinned to 0 µs latency, no faults — useful for smoke tests and overhead benchmarks |
| [`examples/read_heavy.toml`](examples/read_heavy.toml) | Read-optimized with bandwidth throttle and occasional tail-latency spikes |
| [`examples/write_fsync_tail.toml`](examples/write_fsync_tail.toml) | Fast writes, heavy-tailed fsync, occasional fsync EIO — exercises durable-write retry paths |
| [`examples/fault_injection.toml`](examples/fault_injection.toml) | Exercises application error-handling paths |
| [`examples/bandwidth_throttle.toml`](examples/bandwidth_throttle.toml) | Throughput-constrained link |
| [`examples/full.toml`](examples/full.toml) | Every supported key demonstrated |

## Architecture

See [`docs/architecture.md`](docs/architecture.md) for internals: the 7-step
op lifecycle, the determinism contract, and the pluggable content/namespace
models.

## Fidelity model

See [`docs/fidelity.md`](docs/fidelity.md) for what fusecraft does and does
not model, and why.

## Requirements

- Linux host with FUSE support (`/dev/fuse`) for mounting. The core crate is
  portable; the FUSE adapter and CLI require Linux to actually mount.
- Rust 1.87 (pinned via [`rust-toolchain.toml`](rust-toolchain.toml)).

## Building and testing

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI enforces all three gates with `-D warnings`.

## License

MIT. See [`LICENSE-MIT`](LICENSE-MIT).
