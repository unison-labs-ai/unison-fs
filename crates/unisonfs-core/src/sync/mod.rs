//! Background sync engine.
//!
//! Two loops run concurrently:
//!
//! - **Pull loop** — periodically calls `GET /v1/brain/list` (or a smarter
//!   delta endpoint when available) and reconciles new/updated documents
//!   into the local SQLite cache. Respects `dirty_since` to avoid clobbering
//!   locally-written files.
//!
//! - **Push loop** — drains the SQLite push queue, sending each pending write
//!   to the brain via `PUT /v1/brain/doc`. Uses exponential backoff on
//!   failures; poisons items that return 4xx (unrecoverable) to avoid
//!   infinite retries.

pub mod pull;
pub mod push;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::api::ApiClient;
use crate::cache::{Db, UnisonFs};

/// Configuration for the sync engine.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// How often to pull remote changes (seconds).
    pub pull_interval_secs: u64,
    /// When true, only push; never pull.
    pub no_pull: bool,
    /// Max seconds to wait at shutdown for the push queue to drain.
    pub drain_timeout_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            pull_interval_secs: 30,
            no_pull: false,
            drain_timeout_secs: 30,
        }
    }
}

/// Spawn both the pull and push background tasks and return a `Notify`
/// handle that the caller can use to request graceful shutdown.
pub fn start(
    api: Arc<ApiClient>,
    db: Arc<Db>,
    fs: Arc<UnisonFs>,
    config: SyncConfig,
    shutdown: Arc<Notify>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let pull_shutdown = shutdown.clone();
        let push_shutdown = shutdown.clone();
        let api_push = api.clone();
        let api_pull = api.clone();
        let db_push = db.clone();
        let db_pull = db.clone();
        let fs_pull = fs.clone();

        let pull_handle = if !config.no_pull {
            let pull_interval = Duration::from_secs(config.pull_interval_secs);
            Some(tokio::spawn(async move {
                pull::run(api_pull, db_pull, fs_pull, pull_interval, pull_shutdown).await
            }))
        } else {
            None
        };

        let push_notify = db_push.push_notify();
        let push_handle = tokio::spawn(async move {
            push::run(api_push, db_push, push_notify, push_shutdown).await
        });

        if let Some(h) = pull_handle {
            let _ = h.await;
        }
        let _ = push_handle.await;
    })
}
