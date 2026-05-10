# Publishing fusecraft to crates.io

This document describes how to cut a release of the three fusecraft crates.

## Publish order

The three crates must be published in this order, and only in this order:

1. `fusecraft-core`
2. `fusecraft-fuser` (depends on `fusecraft-core`)
3. `fusecraft-cli`   (depends on `fusecraft-core` and `fusecraft-fuser`)

Each step must finish — including crates.io index propagation — before the
next one starts. A short wait (typically 30–60s) between publishes is enough
in practice.

## Why only `fusecraft-core` supports `cargo publish --dry-run`

CI dry-runs `cargo publish --dry-run -p fusecraft-core` on every push, but not the other
two crates. The reason: both `fusecraft-fuser` and `fusecraft-cli` depend on
`fusecraft-core` via a combined path+version dependency, for example:

```toml
fusecraft-core = { path = "../fusecraft-core", version = "0.1.0" }
```

When packaging for publish, cargo drops the `path` and resolves the
dependency through the registry. Before `fusecraft-core 0.1.0` has actually
been uploaded to crates.io, the dry-run fails with:

```text
error: failed to prepare local package for uploading

Caused by:
  no matching package named `fusecraft-core` found
  location searched: crates.io index
```

This is expected, not a bug in our manifests — verify locally with
`cargo publish --dry-run -p fusecraft-fuser --allow-dirty`. The practical
consequence is that manifest issues in `fusecraft-fuser` and `fusecraft-cli`
surface only at real publish time, not in CI. Review their Cargo.toml
changes carefully when bumping versions.

## Version bumps

All three crates move in lockstep; they currently share version `0.1.0`.
To bump:

1. Edit `version` in each of the three crate `Cargo.toml` files.
2. Update the `version` field inside the path+version deps in
   `crates/fusecraft-fuser/Cargo.toml` and `crates/fusecraft-cli/Cargo.toml`
   to match.
3. Run the verification sequence below.
4. Commit, tag, and push. Then publish in order, waiting for the index to
   update between crates.

## Local verification before publishing

Run these in order from the workspace root. All must pass:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo publish --dry-run -p fusecraft-core
```

The first three are the gates CI enforces on every push. The fourth matches
the `docs` CI job and ensures docs.rs will build cleanly. The fifth confirms
that `fusecraft-core`'s manifest is publish-ready — the only dry-run that is
meaningful before the first release (see above).

## Actually publishing

Once verification passes, from a clean checkout on `main`:

```bash
cargo publish -p fusecraft-core
# wait for the index to update, then:
cargo publish -p fusecraft-fuser
# wait again, then:
cargo publish -p fusecraft-cli
```

If a publish fails partway through, do not retry out of order. The lockstep
version invariant above (all three crates share the same version) must hold
on crates.io too — once `fusecraft-core 0.1.0` is published, it is
immutable, so a later failure in `fusecraft-fuser` or `fusecraft-cli`
cannot be recovered by re-publishing `fusecraft-core 0.1.0`.

To recover:

1. Fix the failing crate.
2. Bump **all three** crates to the next patch version (e.g., `0.1.0` →
   `0.1.1`) per the "Version bumps" section, including the path+version
   `version` field in `fusecraft-fuser` and `fusecraft-cli`'s dependency
   on `fusecraft-core`.
3. Re-run the local verification sequence.
4. Restart publishing from `fusecraft-core` in the usual order. The
   already-published lower version (e.g., `0.1.0 fusecraft-core`) stays
   on crates.io and becomes a "dead" release; that is fine — crates.io
   intentionally forbids yanking-and-reusing the same version number.
