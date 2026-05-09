# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

Toolchain is pinned to Rust 1.87 via `rust-toolchain.toml` (edition 2024, MSRV 1.87). CI runs the three gates below with `-D warnings` — match that locally before pushing.

```bash
cargo fmt --check                                  # format gate
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                             # all unit + integration tests
cargo test -p fusecraft-core                       # tests for one crate
cargo test -p fusecraft-core --test config_toml    # one integration test file
cargo test -p fusecraft-core engine::tests::zero_latency_runs_closure_and_returns_value  # single test
```

`cargo deny check` uses `deny.toml` (MIT / Apache-2.0 / BSD / ISC / Zlib / MPL-2.0 / Unicode-DFS-2016 allowlist) when run — license changes that introduce new crates need to stay inside this set.

## Architecture

### Workspace layout
- `fusecraft-core` — the simulator engine and all policy/sampling/metrics logic.
- `fusecraft-fuser` — FUSE kernel adapter (`FaultFs<N, C>` impl of `fuser::Filesystem`, `mount`/`spawn_mount` helpers).
- `fusecraft-cli` — binary entry point exposing `mount`, `validate-config`, and `print-default-config` subcommands.

### The central abstraction: `SimEngine::run_op`
Everything flows through one function: `crates/fusecraft-core/src/engine/mod.rs::SimEngine::run_op`. Future FUSE handlers call it with an `OpContext` and a closure that produces the "real" reply. The engine wraps that closure with a fixed 7-step lifecycle:

1. Acquire the per-op `BlockingLimiter` slot (rejection → `EAGAIN` event + return).
2. Build a `SampleKey` from `(seed, op, ino, offset, len, seq)` — `seq` comes from an internal `AtomicU64`.
3. Draw a fault errno and latency (µs) via the deterministic samplers, and compute a bandwidth delay (only for `Read`/`Write` with a configured `BandwidthProfile`).
4. `thread::sleep(latency + bandwidth_delay)` — faulted ops still wait, mirroring real filesystems.
5. Run the closure unless a fault was injected.
6. Emit one `Event` to the `EventSink` and one sample to the `HistogramRecorder`.
7. Drop the guard to release the concurrency slot.

When adding a new behavior (e.g. dirty-write throttling), plug it into this lifecycle rather than creating a parallel path. The invariant "every op produces exactly one event and one histogram sample, in µs" must survive your change.

### Determinism contract
Reproducibility is a first-class requirement, not an afterthought:

- `sampler/key.rs` derives per-call RNGs by splitmix64-mixing the full `SampleKey` with a per-stream constant (`LATENCY_STREAM` vs `FAULT_STREAM`). Latency and fault draws are statistically independent for the same key, and any `(seed, op, ino, offset, len, seq)` tuple always produces the same samples.
- Samplers (`sample_latency_us`, `sample_fault`) are pure functions — no shared mutable state, no locks on the sampling path. Don't introduce any.
- If you add a new sampler or a new field that influences sampling, it must feed into `SampleKey` so test replay stays exact.

### Hot-path discipline
The engine is meant to sit on a FUSE handler thread. Two rules follow:

- **`EventSink::emit` must not return errors and must not panic.** `JsonlEventSink` suppresses serialization errors on purpose (see the comment at `events/mod.rs`). Preserve that shape for new sinks.
- **Sync only, no async.** `BlockingLimiter` uses `parking_lot::{Mutex, Condvar}` and blocks `std::thread`. Do not introduce Tokio or `async fn` into the core engine path.

### Pluggable models
The engine is generic over two traits that callers supply:

- `ContentModel` (`content/mod.rs`) — `file_len`, `read_at` (must fill every byte of `dst`), `write_at`. `DeterministicContent` is the reference implementation; bytes are a pure function of `(ino, offset, seed)`.
- `NamespaceModel` (`namespace/mod.rs`) — `lookup`, `attr`, `readdir` with a `DirSink`. `FlatObjectNamespace` is the reference implementation (a single `objects/` dir with 6-digit zero-padded names, capped at 999 999 entries).

When adding a new layout or content strategy, add it as a new impl alongside the existing ones; keep the engine generic.

### Config → TOML boundary
`Config` (and its nested types) derive both `Serialize` and `Deserialize`, but two types use manual `Deserialize` impls intentionally:

- `BandwidthProfile` — TOML writes `mib_per_sec`, Rust stores `bytes_per_sec` (conversion happens in the deserializer).
- `FaultRule` — TOML writes `errno = "EIO"` as a string name; the deserializer calls `parse_errno` which only accepts `EIO|ENOENT|ESTALE|ENOSPC|EAGAIN|EINTR`. Extending the errno set means updating `parse_errno`.

`Config::validate()` is the authoritative gate for invariants (non-finite floats, `max_us < base_us`, fault rate outside `[0,1]`, etc.). Add new invariants there rather than scattering checks.

### Error model
`FsError` (thiserror) has three variants: `Errno(i32)`, `Config(String)`, `Io(io::Error)`. Every variant maps to a raw errno via `FsError::as_errno()` — `Config` becomes `EINVAL`, `Io` prefers `raw_os_error` and falls back to `EIO`. FUSE adapters returning an errno to the kernel should go through `as_errno` rather than matching variants manually.
