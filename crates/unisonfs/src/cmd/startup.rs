//! Startup progress reporter for daemon mode.
#![allow(dead_code)]
//!
//! The daemon child writes atomic JSON snapshots to
//! `<state_dir>/<tag>.startup` while it initialises. The parent (and any
//! CLI progress spinner) reads these snapshots to display human-readable
//! progress before the IPC socket comes up.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Phases that a daemon startup goes through, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Initial startup: process launched, cache opening.
    Init,
    /// Performing the initial deletion scan against the remote.
    DeletionScan,
    /// Pulling all brain documents into the local cache.
    InitialPull,
    /// Mount FUSE/NFS filesystem.
    Mounting,
    /// All done — mount is live.
    Ready,
    /// Non-recoverable error.
    Failed,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Phase::Init => write!(f, "Initialising"),
            Phase::DeletionScan => write!(f, "Scanning for deletions"),
            Phase::InitialPull => write!(f, "Pulling brain documents"),
            Phase::Mounting => write!(f, "Mounting filesystem"),
            Phase::Ready => write!(f, "Ready"),
            Phase::Failed => write!(f, "Failed"),
        }
    }
}

/// A progress snapshot written by the daemon to the startup file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartupProgress {
    pub phase: Phase,
    /// Human-readable description of what's happening now.
    pub message: String,
    /// 0–100 estimate; `None` means indeterminate.
    pub pct: Option<u8>,
    /// Non-zero when `phase == Failed`.
    pub error: Option<String>,
}

impl StartupProgress {
    pub fn init() -> Self {
        Self {
            phase: Phase::Init,
            message: "Starting unisonfs daemon".to_string(),
            pct: Some(0),
            error: None,
        }
    }

    pub fn deletion_scan(page: u32, total: u32) -> Self {
        let pct = if total == 0 {
            None
        } else {
            Some(((page as f32 / total as f32) * 30.0) as u8)
        };
        Self {
            phase: Phase::DeletionScan,
            message: format!("Deletion scan: page {page}/{total}"),
            pct,
            error: None,
        }
    }

    pub fn initial_pull(done: usize, total: usize) -> Self {
        let pct = if total == 0 {
            None
        } else {
            Some(30 + ((done as f32 / total as f32) * 60.0) as u8)
        };
        Self {
            phase: Phase::InitialPull,
            message: format!("Pulling brain documents: {done}/{total}"),
            pct,
            error: None,
        }
    }

    pub fn mounting() -> Self {
        Self {
            phase: Phase::Mounting,
            message: "Mounting filesystem".to_string(),
            pct: Some(90),
            error: None,
        }
    }

    pub fn ready(docs: usize, elapsed: Duration) -> Self {
        Self {
            phase: Phase::Ready,
            message: format!("Mounted ({docs} docs, {:.1}s)", elapsed.as_secs_f64()),
            pct: Some(100),
            error: None,
        }
    }

    pub fn failed(err: &str) -> Self {
        Self {
            phase: Phase::Failed,
            message: format!("Startup failed: {err}"),
            pct: None,
            error: Some(err.to_string()),
        }
    }
}

/// Handles writing startup progress from the daemon side.
pub struct StartupReporter {
    path: PathBuf,
    started: Instant,
}

impl StartupReporter {
    pub fn new(state_dir: &Path, tag: &str) -> Self {
        let path = state_dir.join(format!("{tag}.startup"));
        Self {
            path,
            started: Instant::now(),
        }
    }

    pub fn report(&self, progress: StartupProgress) {
        if let Ok(json) = serde_json::to_string(&progress) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn cleanup(&self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Read the current progress snapshot from a startup file.
pub fn read_progress(path: &Path) -> Option<StartupProgress> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Poll the startup file until `Ready` or `Failed`, with `timeout`.
/// Returns the final snapshot, or `None` on timeout.
pub fn wait_for_ready(path: &Path, timeout: Duration) -> Option<StartupProgress> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(p) = read_progress(path) {
            match p.phase {
                Phase::Ready | Phase::Failed => return Some(p),
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
