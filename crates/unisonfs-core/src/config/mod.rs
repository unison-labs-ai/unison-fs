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

pub fn cache_db_path(workspace_id: &str) -> PathBuf {
    cache_dir()
        .join(workspace_id)
        .join("brain.db")
}

/// Cache DB path scoped by both workspace_id and mount tag.
/// Falls back to `cache_db_path(workspace_id)` when tag is empty.
pub fn cache_db_path_for_tag(workspace_id: &str, tag: &str) -> PathBuf {
    if tag.is_empty() {
        return cache_db_path(workspace_id);
    }
    let safe_tag: String = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    cache_dir().join(format!("{workspace_id}__{safe_tag}")).join("brain.db")
}

/// Legacy path (no tag); use for migration / backward compat only.
pub fn legacy_cache_db_path(workspace_id: &str) -> PathBuf {
    cache_db_path(workspace_id)
}

pub fn daemon_log_path() -> PathBuf {
    cache_dir().join("daemon.log")
}

pub fn runtime_dir() -> PathBuf {
    directories::ProjectDirs::from("ai", "unisonlabs", "unisonfs")
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp/unisonfs-run"))
}
