//! Background sync engine.
//!
//! Four loops:
//!
//! - **Loop A — delta pull.** Every ~30s, walk `/v1/brain/list` sorted by
//!   `updatedAt desc` and reconcile anything newer than our watermark.
//! - **Loop C — deletion scan.** Every ~5min, diff the full remote doc list
//!   against local `fs_remote` and unlink anything that disappeared.
//! - **Loop D — push worker.** Claims queued push jobs from `push_queue`.
//! - **Loop F — hydration worker.** Pulls missed/stale files on read misses.

pub mod pull;
pub mod push;
pub mod scan;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::cache::UnisonFs;

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
            delta_interval: Duration::from_secs(30),
            deletion_scan_interval: Duration::from_secs(300),
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
    /// `shutdown.send(true)` is called.
    pub fn start(
        fs: Arc<UnisonFs>,
        opts: SyncOptions,
        shutdown: watch::Receiver<bool>,
    ) -> JoinSet<()> {
        let mut set = JoinSet::new();

        if opts.pull_enabled {
            let fs_a = fs.clone();
            let mut sd_a = shutdown.clone();
            set.spawn(async move {
                run_delta_loop(fs_a, opts.delta_interval, &mut sd_a).await;
            });

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
) {
    let mut empty_streak = 0u32;
    loop {
        let interval = adaptive_interval(base_interval, empty_streak);
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() { return; }
            }
        }

        match pull::delta_pull(&fs).await {
            Ok(n) => {
                if n == 0 {
                    empty_streak = empty_streak.saturating_add(1);
                } else {
                    empty_streak = 0;
                    tracing::debug!(reconciled = n, "delta pull");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "delta pull failed");
            }
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

/// Adaptive cadence: shorter after activity, stretch when idle, add ±jitter.
fn adaptive_interval(base: Duration, empty_streak: u32) -> Duration {
    let secs = base.as_secs_f64();
    let adjusted = if empty_streak == 0 {
        (secs / 3.0).max(10.0)
    } else if empty_streak >= 3 {
        (secs * 2.0).min(60.0)
    } else {
        secs
    };
    jittered(Duration::from_secs_f64(adjusted), 5)
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
