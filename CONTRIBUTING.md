# Contributing to unison-fs

Thanks for helping improve the Rust filesystem mount for the Unison brain.

## Repo layout

```
crates/unisonfs-core/   — library: API client, SQLite cache, VFS, sync engine, mount adapters
crates/unisonfs/        — CLI binary: thin dispatch over unisonfs-core
```

## Development

Prerequisites: Rust 1.80 or newer (`rustup update stable`).

```bash
cargo build            # debug build
cargo build --release  # release build
cargo test             # unit tests
cargo clippy -- -D warnings  # lints (must be clean)
```

## Before opening a PR

1. `cargo build --release` must succeed with zero errors.
2. `cargo test` must pass.
3. `cargo clippy -- -D warnings` must be clean.
4. Keep changes scoped — one logical change per PR.
5. Update `CHANGELOG.md` under "Unreleased" with a one-line summary.
6. Never push directly to `main` (protected); open a PR.

## Conventions

- Rust stable, `edition = "2021"`.
- `dead_code` warnings are treated as errors — every method must have a real call site
  or be removed. Do not suppress with `#[allow(dead_code)]`.
- No unsafe code outside of existing `libc` call sites (uid/gid from process).
- The push queue and dirty-tracking guard are the correctness boundary for sync
  safety — do not bypass them. See `docs/SYNC_INVARIANTS.md`.
- Platform gates: FUSE is Linux-only; NFS server is cross-platform (used on macOS).
  New mount code must compile on both targets.

## Reporting bugs / proposing features

Use the issue templates. For security issues, see [`SECURITY.md`](./SECURITY.md) —
do not open a public issue.
