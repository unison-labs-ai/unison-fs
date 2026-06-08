//! Daemon lifecycle — pid files, socket paths, and IPC.

pub mod client;
pub mod ipc;
pub mod protocol;

use std::path::PathBuf;

use crate::config::runtime_dir;

/// Path to the daemon's pid file for a given mount tag.
pub fn pid_path(tag: &str) -> PathBuf {
    runtime_dir().join(format!("{tag}.pid"))
}

/// Path to the daemon's unix socket for a given mount tag.
pub fn socket_path(tag: &str) -> PathBuf {
    runtime_dir().join(format!("{tag}.sock"))
}

/// Path to the per-mount daemon log file.
pub fn log_path(tag: &str) -> PathBuf {
    crate::config::cache_dir().join(format!("{tag}.log"))
}

/// Path to the startup progress file.
pub fn startup_path(tag: &str) -> PathBuf {
    runtime_dir().join(format!("{tag}.startup"))
}

/// Ensure runtime directories exist.
pub fn ensure_dirs() -> anyhow::Result<()> {
    std::fs::create_dir_all(runtime_dir())?;
    std::fs::create_dir_all(crate::config::cache_dir())?;
    Ok(())
}

/// Read the PID from the pid file, if it exists.
pub fn read_pid(tag: &str) -> Option<u32> {
    let data = std::fs::read_to_string(pid_path(tag)).ok()?;
    data.trim().parse().ok()
}

/// Write the current process's PID.
pub fn write_pid(tag: &str) -> anyhow::Result<()> {
    let path = pid_path(tag);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, std::process::id().to_string())?;
    Ok(())
}

/// Remove pid and socket files for a tag.
pub fn cleanup_stale(tag: &str) {
    let _ = std::fs::remove_file(pid_path(tag));
    let _ = std::fs::remove_file(socket_path(tag));
    let _ = std::fs::remove_file(startup_path(tag));
}

/// Check if a process with the given PID is alive.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}
