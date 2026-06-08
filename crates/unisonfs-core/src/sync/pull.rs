//! Pull loop — reconciles remote brain documents into the local cache.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::api::{ApiClient, ListDocsReq};
use crate::cache::{Db, UnisonFs};

/// Run the pull loop until `shutdown` is notified.
pub async fn run(
    api: Arc<ApiClient>,
    db: Arc<Db>,
    fs: Arc<UnisonFs>,
    interval: Duration,
    shutdown: Arc<Notify>,
) {
    loop {
        // Wait for the interval or an early wake from shutdown.
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = shutdown.notified() => {
                tracing::debug!("pull loop: shutdown requested");
                return;
            }
        }

        if let Err(e) = pull_once(&api, &db, &fs).await {
            tracing::warn!("pull error: {e}");
        }
    }
}

/// Perform one pull cycle: list all documents and reconcile.
async fn pull_once(api: &ApiClient, _db: &Db, fs: &UnisonFs) -> anyhow::Result<()> {
    tracing::debug!("pull: fetching document list");

    let resp = api
        .list_docs(&ListDocsReq {
            prefix: None,
            kind: vec![],
            tag: vec![],
            limit: Some(500),
        })
        .await?;

    tracing::debug!("pull: received {} documents", resp.documents.len());

    for doc in &resp.documents {
        let brain_path = &doc.path;

        // Check if the local inode is dirty — if so, skip to avoid overwriting
        // user's in-progress edits.
        if let Some(ino) = fs.db.ino_by_remote_path(brain_path) {
            if let Some(dirty_since) = fs.db.get_dirty_since(ino) {
                // Parse the remote updated_at and compare
                if let Ok(remote_ts) = parse_iso8601_ms(&doc.updated_at) {
                    if dirty_since >= remote_ts {
                        tracing::trace!(
                            "pull: skipping {} (dirty_since={dirty_since} >= remote={remote_ts})",
                            brain_path
                        );
                        continue;
                    }
                }
            }
        }

        // Upsert the document into the local cache
        let content = doc.body_md.as_deref().unwrap_or("").as_bytes().to_vec();
        if let Err(e) = fs.upsert_brain_doc(brain_path, &content) {
            tracing::warn!("pull: failed to upsert {brain_path}: {e}");
        } else {
            tracing::trace!("pull: upserted {brain_path}");
        }
    }

    Ok(())
}

/// Parse an ISO 8601 datetime string to epoch milliseconds.
fn parse_iso8601_ms(s: &str) -> Result<i64, ()> {
    // Very simple parser: look for the epoch-second part and convert.
    // Format: "2024-01-15T12:34:56.789Z" or "2024-01-15T12:34:56Z"
    let s = s.trim_end_matches('Z');
    let parts: Vec<&str> = s.splitn(2, 'T').collect();
    if parts.len() != 2 {
        return Err(());
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();

    if date_parts.len() < 3 || time_parts.len() < 3 {
        return Err(());
    }

    let year: i64 = date_parts[0].parse().map_err(|_| ())?;
    let month: i64 = date_parts[1].parse().map_err(|_| ())?;
    let day: i64 = date_parts[2].parse().map_err(|_| ())?;
    let hour: i64 = time_parts[0].parse().map_err(|_| ())?;
    let min: i64 = time_parts[1].parse().map_err(|_| ())?;
    let sec_str = time_parts[2].split('.').next().unwrap_or("0");
    let sec: i64 = sec_str.parse().map_err(|_| ())?;

    // Approximate days since epoch (ignoring leap years for now — good enough for
    // dirty-since comparison)
    let days_from_epoch = days_from_epoch(year, month, day);
    let ts_secs = days_from_epoch * 86400 + hour * 3600 + min * 60 + sec;
    Ok(ts_secs * 1000)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn days_from_epoch(year: i64, month: i64, day: i64) -> i64 {
    let mut days = 0i64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..month {
        days += days_in_month(year, m);
    }
    days + day - 1
}
