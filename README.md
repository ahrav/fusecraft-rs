# fusecraft

fusecraft simulates syscall-visible latency, faults, queueing, and bandwidth limits through FUSE.

fusecraft does not simulate NFS, S3, SMB, EBS, ext4, xfs, or any exact storage protocol.

Use it to test application behavior when file operations block, fail, or queue.

## Quick start

```bash
# Build
cargo build --workspace

# Mount with an example config
fusecraft mount --config examples/read_heavy.toml --mountpoint /mnt/sim

# Run your workload against the mount point
dd if=/mnt/sim/objects/000001 of=/dev/null bs=64k

# Unmount
fusermount -u /mnt/sim
```

## Configuration

fusecraft is driven by a single TOML file. See [`docs/config.md`](docs/config.md) for the full reference.

Example configs in [`examples/`](examples/):

| File | Purpose |
|------|---------|
| [`minimal.toml`](examples/minimal.toml) | Bare minimum — just a seed |
| [`read_heavy.toml`](examples/read_heavy.toml) | Read-optimized with bandwidth throttle |
| [`fault_injection.toml`](examples/fault_injection.toml) | Exercises error-handling paths |
| [`bandwidth_throttle.toml`](examples/bandwidth_throttle.toml) | Throughput-constrained link |
| [`full.toml`](examples/full.toml) | Every supported key demonstrated |

## Architecture

See [`docs/architecture.md`](docs/architecture.md) for internals: the 7-step op lifecycle, determinism contract, and pluggable models.

## Fidelity model

See [`docs/fidelity.md`](docs/fidelity.md) for what fusecraft does and does not model, and why.

## Building

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Toolchain is pinned to Rust 1.87 via `rust-toolchain.toml`.

## License

MIT
