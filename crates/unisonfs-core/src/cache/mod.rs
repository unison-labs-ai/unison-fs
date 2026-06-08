//! SQLite-backed local filesystem cache.
//!
//! The cache stores inode metadata and file content locally, with a push queue
//! for outbound writes and sync metadata for delta pulls.

pub mod db;
pub mod file;
pub mod fs;

pub use db::{Db, PushJob, PushOp};
pub use fs::UnisonFs;
