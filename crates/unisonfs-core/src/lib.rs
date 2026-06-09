//! unisonfs core library.
//!
//! - [`vfs`] — the `FileSystem` trait, `MemFs` reference implementation, and
//!   supporting types (`FileAttr`, `VfsError`, path helpers, POSIX mode constants).
//! - [`mount`] — FUSE (Linux) and NFS (macOS) mount adapters.
//! - [`sync`] — background sync engine that reconciles the local cache with the Unison brain API.
//! - [`api`] — typed HTTP client over the Unison brain REST API.
//! - [`daemon`] — long-running daemon lifecycle, fork dance, and unix-socket IPC control channel.
//! - [`config`] — XDG paths and runtime configuration.
//! - [`cache`] — SQLite-backed local filesystem cache.
//! - [`agent_hint`] — inject/remove path-scoped semantic-search hints in agent instruction files.

#![warn(missing_debug_implementations)]

pub mod agent_hint;
pub mod api;
pub mod cache;
pub mod config;
pub mod daemon;
pub mod mount;
pub mod sync;
pub mod vfs;

/// Crate version, exposed for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
