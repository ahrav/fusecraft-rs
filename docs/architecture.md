# Architecture

## Workspace layout

| Crate | Role |
|-------|------|
| `fusecraft-core` | Simulation engine, policy/sampling/metrics logic |
| `fusecraft-fuser` | FUSE kernel adapter |
| `fusecraft-cli` | Binary entry point |

All simulation logic lives in `fusecraft-core`. The FUSE adapter and CLI are thin wrappers.

## The central abstraction: `SimEngine::run_op`

Every filesystem operation flows through a single function:

```text
crates/fusecraft-core/src/engine/mod.rs :: SimEngine::run_op
```

FUSE handlers call it with an `OpContext` (operation type, inode, offset, length) and a closure that produces the "real" reply. The engine wraps that closure in a fixed **7-step lifecycle**:

### Step 1: Acquire concurrency slot

The per-op `BlockingLimiter` controls how many operations of each type can be in-flight simultaneously. If the limiter rejects the request (both active slots and queue are full), the engine emits an `EAGAIN` event and returns immediately.

### Step 2: Build SampleKey

A deterministic `SampleKey` is constructed from `(seed, op, ino, offset, len, seq)`. The `seq` field comes from an internal `AtomicU64` counter that increments monotonically.

### Step 3: Sample fault, latency, and bandwidth

Using the `SampleKey`, the engine draws:

- A fault errno via `sample_fault` (probabilistic, per fault rule)
- A latency value in microseconds via `sample_latency_us` (base + lognormal + Pareto tail)
- A bandwidth delay (only for `Read`/`Write` with a configured `BandwidthProfile`)

All sampling is deterministic: the same key always produces the same result.

### Step 4: Sleep

The calling thread sleeps for `latency + bandwidth_delay`. Faulted operations still wait — this mirrors real filesystems, which frequently return errors slowly.

### Step 5: Execute or skip the closure

If a fault was injected, the closure is skipped and an `Errno` error is returned. Otherwise the closure runs and its result is propagated.

### Step 6: Record event and metrics

Exactly one `Event` is emitted to the `EventSink` and one sample is recorded in the `HistogramRecorder`. The invariant **"every op produces exactly one event and one histogram sample (in microseconds)"** is maintained unconditionally.

### Step 7: Release the concurrency slot

The limiter guard is dropped, freeing the slot for the next waiting operation.

## Determinism contract

Reproducibility is a first-class requirement.

### Key derivation (`sampler/key.rs`)

Per-call RNGs are derived by splitmix64-mixing the full `SampleKey` with a per-stream constant:

- `LATENCY_STREAM` (0xA5A5_A5A5_A5A5_A5A5) — for latency draws
- `FAULT_STREAM` (0x5A5A_5A5A_5A5A_5A5A) — for fault draws

This guarantees:

- Latency and fault draws are statistically independent for the same key
- Any `(seed, op, ino, offset, len, seq)` tuple always produces the same samples
- No shared mutable state on the sampling path — samplers are pure functions

### Replay guarantee

Given the same `Config` (specifically the same `seed`) and the same sequence of operations, fusecraft produces identical latency/fault sequences. This enables:

- Deterministic test replay
- Comparing application behavior across code changes
- Bisecting performance regressions

## Pluggable models

The engine is generic over two traits:

### `ContentModel`

Defined in `content/mod.rs`. Methods: `file_len`, `read_at`, `write_at`.

- `DeterministicContent` — reference implementation. File bytes are a pure function of `(ino, offset, seed)`.

### `NamespaceModel`

Defined in `namespace/mod.rs`. Methods: `lookup`, `attr`, `readdir`.

- `FlatObjectNamespace` — reference implementation. A single `objects/` directory with 6-digit zero-padded names, capped at 999,999 entries.

## Hot-path discipline

The engine sits on FUSE handler threads. Two invariants are enforced:

1. **`EventSink::emit` must not return errors and must not panic.** The `JsonlEventSink` suppresses serialization errors on purpose.
2. **Sync only, no async.** `BlockingLimiter` uses `parking_lot::{Mutex, Condvar}` and blocks `std::thread`. No Tokio or `async fn` on the core engine path.

## Error model

`FsError` has three variants:

| Variant | Maps to errno | Use case |
|---------|---------------|----------|
| `Errno(i32)` | The contained value | Injected faults, limiter rejection |
| `Config(String)` | `EINVAL` | Configuration validation failures |
| `Io(io::Error)` | `raw_os_error()` or `EIO` | Underlying I/O errors |

FUSE adapters use `FsError::as_errno()` to convert any error to a kernel errno.
