//! IPC protocol between the parent CLI and the daemon.

use serde::{Deserialize, Serialize};

/// Requests from the parent CLI to the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Liveness check.
    Ping,
    /// Request a graceful shutdown.
    Shutdown,
    /// Force a sync cycle.
    Sync,
    /// Query daemon status.
    Status,
}

/// Responses from the daemon.
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Pong,
    Ok,
    Error(String),
    Status(DaemonStatus),
}

/// Daemon status snapshot.
#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub tag: String,
    pub mount_path: String,
    pub push_queue_len: usize,
    pub last_pull_at: Option<i64>,
    pub pid: u32,
}
