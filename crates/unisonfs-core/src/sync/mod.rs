//! Background sync engine.
//!
//! Four loops:
//!
//! - **Loop A — delta pull.** Every ~15s (or immediately on a wake signal),
//!   drain the server's changes feed from the persisted cursor — updates,
//!   creations, and deletion tombstones in one pass. Falls back to the
//!   legacy `/v1/brain/list` watermark pull on pre-feed servers.
//! - **Loop C — deletion scan.** Every ~6h, diff the full remote doc list
//!   against local `fs_remote` — a reconciliation safety net behind the
//!   feed's tombstones.
//! - **Loop D — push worker.** Claims queued push jobs from `push_queue`.
//! - **Loop F — hydration worker.** Pulls missed/stale files on read misses.

pub mod pull;
pub mod push;
pub mod scan;
pub mod stream;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Notify};
use tokio::task::JoinSet;

use crate::cache::UnisonFs;

/// Poll cadence while the wake stream is healthy — a slow safety net behind
/// push events, not the freshness mechanism.
const STREAM_FALLBACK_INTERVAL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy)]
pub enum InitialPullProgress {
    DeletionScan(scan::DeletionScanProgress),
    Pull(pull::PullProgress),
}

/// Knobs for the sync engine. All optional — defaults are production-sane.
#[derive(Debug, Clone, Copy)]
pub struct SyncOptions {
    pub delta_interval: Duration,
    pub deletion_scan_interval: Duration,
    pub pull_enabled: bool,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            // 15s: with the cursor feed a poll is one indexed query that
            // usually returns zero rows — cheap enough to keep worst-case
            // staleness under ~16s without any push channel.
            delta_interval: Duration::from_secs(15),
            // The feed's tombstones are the primary deletion mechanism now;
            // the full-list diff is a rare reconciliation safety net.
            deletion_scan_interval: Duration::from_secs(6 * 60 * 60),
            pull_enabled: true,
        }
    }
}

/// Orchestrates background sync for a mount.
#[derive(Debug)]
pub struct SyncEngine;

impl SyncEngine {
    /// Synchronous startup sequence: deletion scan then full pull.
    pub async fn initial_pull(fs: &Arc<UnisonFs>) -> anyhow::Result<(usize, usize)> {
        let removed = scan::deletion_scan(fs).await.unwrap_or(0);
        let reconciled = pull::full_pull(fs).await?;
        Ok((removed, reconciled))
    }

    pub async fn initial_pull_with_progress<F>(
        fs: &Arc<UnisonFs>,
        mut on_progress: F,
    ) -> anyhow::Result<(usize, usize)>
    where
        F: FnMut(InitialPullProgress) + Send,
    {
        let removed = if fs.db().remote_count() == 0 {
            0
        } else {
            scan::deletion_scan_with_progress(fs, |p| {
                on_progress(InitialPullProgress::DeletionScan(p));
            })
            .await
            .unwrap_or(0)
        };
        let reconciled = pull::full_pull_with_progress(fs, |p| {
            on_progress(InitialPullProgress::Pull(p));
        })
        .await?;
        Ok((removed, reconciled))
    }

    /// Spawn background loops. Returns a `JoinSet` whose tasks exit when
    /// `shutdown.send(true)` is called. `wake` short-circuits the delta
    /// loop's sleep — fired by IPC `Sync` requests (and, later, the server's
    /// change-stream doorbell) to pull immediately instead of waiting out
    /// the interval.
    pub fn start(
        fs: Arc<UnisonFs>,
        opts: SyncOptions,
        shutdown: watch::Receiver<bool>,
        wake: Arc<Notify>,
    ) -> JoinSet<()> {
        let mut set = JoinSet::new();

        if opts.pull_enabled {
            let stream_healthy = Arc::new(AtomicBool::new(false));

            let fs_a = fs.clone();
            let mut sd_a = shutdown.clone();
            let wake_a = wake.clone();
            let healthy_a = stream_healthy.clone();
            set.spawn(async move {
                run_delta_loop(fs_a, opts.delta_interval, &mut sd_a, wake_a, healthy_a).await;
            });

            // Wake stream (Layer 2): push doorbell that fires `wake`; the
            // delta loop demotes itself to a slow fallback while it's up.
            if let Some(api) = fs.api() {
                let api_s = api.clone();
                let sd_s = shutdown.clone();
                set.spawn(async move {
                    stream::run_stream_loop(api_s, wake, stream_healthy, sd_s).await;
                });
            }

            let fs_c = fs.clone();
            let mut sd_c = shutdown.clone();
            set.spawn(async move {
                run_deletion_loop(fs_c, opts.deletion_scan_interval, &mut sd_c).await;
            });

            // Hydration worker.
            let fs_f = fs.clone();
            let sd_f = shutdown.clone();
            set.spawn(async move {
                crate::cache::hydration::run_hydration_worker(fs_f, sd_f).await;
            });
        }

        let fs_d = fs.clone();
        let sd_d = shutdown.clone();
        set.spawn(async move {
            push::run_push_worker(fs_d, sd_d).await;
        });

        set
    }

    /// Final deletion scan before the mount releases (best-effort).
    pub async fn unmount_scan(fs: &Arc<UnisonFs>) {
        match scan::deletion_scan(fs).await {
            Ok(n) if n > 0 => tracing::info!(removed = n, "final deletion scan"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "final deletion scan failed"),
        }
    }
}

async fn run_delta_loop(
    fs: Arc<UnisonFs>,
    base_interval: Duration,
    shutdown: &mut watch::Receiver<bool>,
    wake: Arc<Notify>,
    stream_healthy: Arc<AtomicBool>,
) {
    // Fixed cadence, not adaptive: a cursor-feed poll with nothing new is a
    // single indexed query, so stretching the interval when idle would trade
    // real staleness for a negligible saving. ±2s jitter avoids lockstep
    // across mounts. While the wake stream is connected, events drive the
    // syncs and the interval demotes to a slow missed-event safety net.
    loop {
        let interval = if stream_healthy.load(Ordering::Relaxed) {
            STREAM_FALLBACK_INTERVAL.max(base_interval)
        } else {
            base_interval
        };
        tokio::select! {
            _ = tokio::time::sleep(jittered(interval, 2)) => {}
            _ = wake.notified() => {
                tracing::debug!("delta pull woken early");
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }

        match pull::delta_pull(&fs).await {
            Ok(n) if n > 0 => {
                tracing::debug!(reconciled = n, "delta pull");
                // The profile derives from the memory graph; changed docs can
                // shift it. Debounced to at most one warm per 30s.
                fs.rewarm_profile_debounced().await;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "delta pull failed"),
        }
    }
}

async fn run_deletion_loop(
    fs: Arc<UnisonFs>,
    base_interval: Duration,
    shutdown: &mut watch::Receiver<bool>,
) {
    loop {
        let interval = jittered(base_interval, 30);
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }

        match scan::deletion_scan(&fs).await {
            Ok(n) if n > 0 => tracing::info!(removed = n, "deletion scan"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "deletion scan failed"),
        }
    }
}

/// Add uniform ±`max_jitter_secs` jitter to an interval (never below 1s).
fn jittered(base: Duration, max_jitter_secs: i64) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as i64)
        .unwrap_or(0);
    let jitter = (nanos % (2 * max_jitter_secs + 1)) - max_jitter_secs;
    let secs = (base.as_secs() as i64 + jitter).max(1);
    Duration::from_secs(secs as u64)
}
