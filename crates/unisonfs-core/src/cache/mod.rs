//! SQLite-backed local filesystem cache.
//!
//! The cache stores inode metadata and file content locally, with a push queue
//! for outbound writes and sync metadata for delta pulls.

pub(crate) mod db;
mod file;
mod fs;
pub mod hydration;
pub mod profile;
#[cfg(test)]
mod tests;

pub use db::{is_noise_path, Db, PushJob, PushOp, DEFAULT_CHUNK_SIZE, DENTRY_CACHE_MAX, ROOT_INO};
pub use fs::UnisonFs;
pub use hydration::{HydrationKey, HydrationScheduler};
