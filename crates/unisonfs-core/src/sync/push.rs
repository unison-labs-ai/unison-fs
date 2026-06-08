//! Push loop — drains the SQLite push queue and sends writes to the brain.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::Notify;

use crate::api::{ApiClient, ApiError, PutDocReq};
use crate::cache::{Db, PushOp};

/// Maximum retries before poisoning a push job.
const MAX_ATTEMPTS: i64 = 10;
/// Base backoff in milliseconds.
const BASE_BACKOFF_MS: i64 = 500;
/// sync_meta key for the last successful push timestamp.
const SYNC_META_LAST_PUSH_AT: &str = "last_push_at";

/// Run the push loop until `shutdown` is notified.
pub async fn run(
    api: Arc<ApiClient>,
    db: Arc<Db>,
    push_notify: Arc<Notify>,
    shutdown: Arc<Notify>,
) {
    loop {
        // Wait for a push notification or shutdown.
        tokio::select! {
            _ = push_notify.notified() => {}
            _ = shutdown.notified() => {
                tracing::debug!("push loop: shutdown requested, draining remaining queue");
                drain_remaining(&api, &db).await;
                return;
            }
        }

        drain_available(&api, &db).await;
    }
}

/// Drain all currently-available (non-inflight, not-debounced) queue items.
async fn drain_available(api: &ApiClient, db: &Db) {
    loop {
        let now_ms = now_ms();
        let Some(job) = db.push_queue_claim_next(now_ms) else {
            break;
        };

        if job.attempt >= MAX_ATTEMPTS {
            tracing::warn!(
                "push: poisoning {} after {} attempts",
                job.brain_path,
                job.attempt
            );
            db.push_queue_poison(&job.brain_path, 0, "max attempts exceeded", now_ms);
            continue;
        }

        let result = execute_push(api, db, &job).await;

        match result {
            Ok(()) => {
                tracing::debug!("push: {} succeeded (op={:?})", job.brain_path, job.op);
                db.push_queue_finalize_success(&job.brain_path, now_ms);

                // Record the successful push in sync_meta and update mirrored
                // state on the inode.
                db.sync_meta_set(SYNC_META_LAST_PUSH_AT, &now_ms.to_string());
                if job.op == PushOp::Write {
                    if let Some(content_ino) = job.content_ino {
                        db.set_mirrored_state(
                            content_ino,
                            Some(now_ms),
                            Some("pushed"),
                            Some(now_ms),
                        );
                        // Clear dirty_since: the local edit is now on the server.
                        db.set_dirty_since(content_ino, None);
                    }
                } else if job.op == PushOp::Delete {
                    // Remote path is gone — clean up any stale mirrored record.
                    // The inode entry itself was already removed by unlink; if
                    // by any chance it still exists, clear its dirty flag.
                    if let Some(ino) = db.ino_by_remote_path(&job.brain_path) {
                        db.set_dirty_since(ino, None);
                        db.delete_remote_path(ino);
                    }
                }
            }
            Err(e) => {
                // Unrecoverable 4xx → poison
                let is_fatal = matches!(
                    e,
                    ApiError::Auth
                        | ApiError::Forbidden(_)
                        | ApiError::FsContract(_)
                        | ApiError::Rejected { .. }
                );
                if is_fatal {
                    let status = match &e {
                        ApiError::Rejected { status, .. } => *status,
                        ApiError::Forbidden(_) => 403,
                        ApiError::Auth => 401,
                        _ => 422,
                    };
                    tracing::warn!(
                        "push: poisoning {} (unrecoverable, status={}): {e}",
                        job.brain_path,
                        status
                    );
                    db.push_queue_poison(&job.brain_path, status, &e.to_string(), now_ms);
                } else {
                    let backoff = BASE_BACKOFF_MS * 2i64.pow(job.attempt as u32).min(60);
                    tracing::warn!(
                        "push: {} failed (attempt {}), retrying in {}ms: {e}",
                        job.brain_path,
                        job.attempt,
                        backoff
                    );
                    db.push_queue_finalize_failure(&job.brain_path, &e.to_string(), now_ms, backoff);
                }
            }
        }
    }
}

/// After shutdown is requested, drain the queue for up to `drain_timeout` seconds.
async fn drain_remaining(api: &ApiClient, db: &Db) {
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(30);

    loop {
        if start.elapsed() >= timeout {
            tracing::info!("push: drain timeout reached, {} items remain", db.push_queue_len());
            break;
        }
        if db.push_queue_len() == 0 {
            tracing::debug!("push: queue drained");
            break;
        }

        let now_ms = now_ms();
        if db.push_queue_claim_next(now_ms).is_none() {
            // Nothing claimable — wait briefly and retry
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }

        // Put the job back and let drain_available handle it
        drain_available(api, db).await;
    }
}

/// Execute a single push job.
async fn execute_push(api: &ApiClient, db: &Db, job: &crate::cache::PushJob) -> Result<(), ApiError> {
    match job.op {
        PushOp::Write => {
            let content = db.read_all_content(job.content_ino.unwrap_or(0));
            let body_md = String::from_utf8_lossy(&content).into_owned();

            let title = extract_title(&body_md);
            let req = PutDocReq {
                path: job.brain_path.clone(),
                body_md,
                kind: Some("note".to_string()),
                title,
                tldr: None,
                tags: None,
                visibility: None,
                expected_content_hash: None,
            };
            api.put_doc(&req).await?;
        }
        PushOp::Delete => {
            match api.delete_doc(&job.brain_path).await {
                Ok(_) => {}
                Err(ApiError::NotFound) => {
                    // Already deleted — treat as success
                    tracing::debug!("push: delete {} — already gone", job.brain_path);
                }
                Err(e) => return Err(e),
            }
        }
        PushOp::Rename => {
            // Rename is implemented as a read-then-write with a new path +
            // delete of the old path. The rename_to field carries the target name.
            if let Some(new_name) = &job.rename_to {
                // Try to read current doc content from brain first; fall back to local
                let body_md = match api.get_doc(&job.brain_path).await {
                    Ok(doc) => doc.body_md.unwrap_or_default(),
                    Err(ApiError::NotFound) => {
                        if let Some(ino) = db.ino_by_remote_path(&job.brain_path) {
                            String::from_utf8_lossy(&db.read_all_content(ino)).into_owned()
                        } else {
                            String::new()
                        }
                    }
                    Err(e) => return Err(e),
                };

                // Determine new path (simple: replace last path component)
                let new_path = job
                    .brain_path
                    .rsplit_once('/')
                    .map(|(parent, _)| format!("{parent}/{new_name}"))
                    .unwrap_or_else(|| format!("/private/notes/{new_name}"));

                api.put_doc(&PutDocReq {
                    path: new_path.clone(),
                    body_md,
                    kind: Some("note".to_string()),
                    title: None,
                    tldr: None,
                    tags: None,
                    visibility: None,
                    expected_content_hash: None,
                })
                .await?;

                // Delete old path
                match api.delete_doc(&job.brain_path).await {
                    Ok(_) | Err(ApiError::NotFound) => {}
                    Err(e) => {
                        tracing::warn!("push: rename — delete old path {} failed: {e}", job.brain_path);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Extract the first H1 heading from markdown as the document title.
fn extract_title(body_md: &str) -> Option<String> {
    for line in body_md.lines() {
        let trimmed = line.trim_start_matches('#').trim();
        if line.starts_with('#') && !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
