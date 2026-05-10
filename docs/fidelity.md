# Fidelity model

This document describes what fusecraft does and does not model.

## What fusecraft models

fusecraft simulates **syscall-visible behavior** of filesystem operations:

| Behavior | How |
|----------|-----|
| **Latency** | Per-op latency drawn from a configurable distribution (base + lognormal body + Pareto tail, clamped) |
| **Faults** | Probabilistic errno injection (`EIO`, `ENOENT`, `ESTALE`, `ENOSPC`, `EAGAIN`, `EINTR`) |
| **Queueing** | Per-op concurrency cap with bounded wait queue; overflow returns `EAGAIN` |
| **Bandwidth limits** | Token-bucket throttle on `read`/`write` data transfer |
| **Determinism** | All sampling is a pure function of `(seed, op, ino, offset, len, seq)` — fully reproducible |

These are the behaviors your application observes at the syscall boundary. fusecraft makes them controllable and reproducible.

## What fusecraft does not model

fusecraft does not simulate any storage protocol or filesystem implementation:

- **No NFS semantics** — no lock manager, no delegations, no stale filehandle lifecycle
- **No S3 semantics** — no eventual consistency, no multipart upload, no conditional writes
- **No SMB semantics** — no oplocks, no share modes, no DFS referrals
- **No EBS semantics** — no volume attachment, no snapshot consistency, no IOPS credits
- **No ext4/xfs semantics** — no journaling, no extent allocation, no fsck behavior
- **No page cache modeling** — `direct_io` bypasses the kernel cache, but fusecraft does not simulate cache hits/misses
- **No network partitions** — faults are probabilistic, not correlated across operations or time windows
- **No data corruption** — read content is deterministic; fusecraft never returns garbled bytes
- **No metadata consistency** — the namespace is static; no rename races, no directory ordering guarantees beyond what the flat layout provides

## Why this boundary exists

Protocol-fidelity simulators are expensive to build, impossible to validate without access to the real system, and misleading when they inevitably diverge from production behavior.

fusecraft takes a different approach: it models only what your application can observe through POSIX file operations — timing, errors, and throughput. This is sufficient to answer:

- Does my application retry correctly when reads fail?
- Does my application degrade gracefully under latency spikes?
- Does my application handle backpressure when writes are throttled?
- Does my application deadlock when concurrent operations queue?

These questions do not require simulating NFS lock reclamation or S3 multipart semantics. They require controllable, reproducible syscall behavior — which is exactly what fusecraft provides.

## Determinism guarantees

Given the same configuration (same `seed`) and the same sequence of operations:

1. Every operation receives the same latency value
2. Every operation receives the same fault/no-fault decision
3. Every read returns the same bytes
4. Event logs are identical

This enables diff-based regression testing: run your workload twice with the same seed, compare the event logs, and any difference is caused by your code change — not by the simulator.

## Extending fidelity

If you need behavior not currently modeled:

- **Correlated faults** — could be added as a new sampler that considers recent history (would require extending `SampleKey`)
- **Time-varying profiles** — could be added by making `OpPolicy` a function of wall-clock time
- **New operations** — add a variant to `FsOp` and wire it through the 7-step lifecycle

All extensions must preserve the determinism contract: same key, same result.
