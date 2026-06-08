//! Configuration and XDG paths.

pub mod credentials;

use std::path::PathBuf;

/// Platform-appropriate cache directory for unisonfs.
///
/// - Linux: `$XDG_CACHE_HOME/unisonfs` (usually `~/.cache/unisonfs`)
/// - macOS: `~/Library/Caches/unisonfs`
pub fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("ai", "unisonlabs", "unisonfs")
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp/unisonfs"))
}

pub fn cache_db_path(tenant_id: &str) -> PathBuf {
    cache_dir()
        .join(tenant_id)
        .join("brain.db")
}

pub fn daemon_log_path() -> PathBuf {
    cache_dir().join("daemon.log")
}

pub fn runtime_dir() -> PathBuf {
    directories::ProjectDirs::from("ai", "unisonlabs", "unisonfs")
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp/unisonfs-run"))
}
