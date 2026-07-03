//! Pull loop — reconciles remote brain documents into the local cache.
//!
//! Primary path: the server's changes feed (`GET /v1/brain/changes`,
//! changes-feed spec) — a cursor over `(updated_at, id)` that includes
//! deletion tombstones, so one endpoint drives updates, creations, and
//! deletions. Bodies are fetched per changed doc via `GET /v1/brain/doc`,
//! skipped when the feed's `contentHash` matches what the cache already
//! holds (own-write echo).
//!
//! Fallback path: servers that predate the feed 404 on it; those get the
//! legacy full-list pull filtered client-side by an `updated_at` watermark.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{ApiClient, ApiError, ChangeEntry, ListDocsReq};
use crate::cache::UnisonFs;

/// Key used in sync_meta to record when the last successful pull completed.
pub const SYNC_META_LAST_PULL_AT: &str = "last_pull_at";
/// Persisted changes-feed cursor (opaque server token).
pub const SYNC_META_CHANGES_CURSOR: &str = "changes_cursor";
/// Legacy fallback watermark for pre-changes-feed servers.
const SYNC_META_LAST_SEEN: &str = "last_seen_updated_at";

const PAGE_LIMIT: u32 = 1000;

/// Progress snapshot from a full or delta pull.
#[derive(Debug, Clone, Copy)]
pub struct PullProgress {
    pub page: u32,
    pub total_pages: u32,
    pub total_items: usize,
    pub reconciled: usize,
}

enum PullError {
    /// Server has no /changes route — fall back to the legacy list pull.
    NoChangesFeed,
    /// Cursor predates the server's tombstone retention — full resync.
    ResyncRequired,
    Other(anyhow::Error),
}

fn map_changes_err(e: ApiError) -> PullError {
    match e {
        ApiError::NotFound => PullError::NoChangesFeed,
        ApiError::Rejected { status: 410, .. } => PullError::ResyncRequired,
        other => PullError::Other(anyhow::anyhow!("changes feed failed: {other}")),
    }
}

/// Run one pass of the delta pull loop. Returns number of docs reconciled.
pub async fn delta_pull(fs: &Arc<UnisonFs>) -> anyhow::Result<usize> {
    let Some(api) = fs.api() else {
        return Ok(0);
    };

    match fs.db().sync_meta_get(SYNC_META_CHANGES_CURSOR) {
        Some(cursor) => match cursor_delta(fs, api, &cursor).await {
            Ok(n) => Ok(n),
            Err(PullError::ResyncRequired) => {
                tracing::warn!("changes cursor expired (410); running full resync");
                cursor_bootstrap(fs, api, None).await.map_err(pull_err)
            }
            Err(PullError::NoChangesFeed) => legacy_delta_pull(fs, api).await,
            Err(PullError::Other(e)) => Err(e),
        },
        // No cursor yet: first run against a feed-capable server (fresh mount
        // or upgraded client). Bootstrap establishes one and prunes stale
        // locals; pre-feed servers fall back to the legacy watermark pull.
        None => match cursor_bootstrap(fs, api, None).await {
            Ok(n) => Ok(n),
            Err(PullError::NoChangesFeed) => legacy_delta_pull(fs, api).await,
            Err(PullError::ResyncRequired) => unreachable!("bootstrap sends no cursor"),
            Err(PullError::Other(e)) => Err(e),
        },
    }
}

/// Full pull — used at mount startup. Establishes the changes cursor (and
/// prunes local docs deleted remotely) or falls back to the legacy full list.
pub async fn full_pull(fs: &Arc<UnisonFs>) -> anyhow::Result<usize> {
    full_pull_inner(fs, None).await
}

pub async fn full_pull_with_progress<F>(
    fs: &Arc<UnisonFs>,
    mut on_progress: F,
) -> anyhow::Result<usize>
where
    F: FnMut(PullProgress) + Send,
{
    full_pull_inner(fs, Some(&mut on_progress)).await
}

async fn full_pull_inner(
    fs: &Arc<UnisonFs>,
    mut on_progress: Option<&mut (dyn FnMut(PullProgress) + Send)>,
) -> anyhow::Result<usize> {
    let Some(api) = fs.api() else {
        return Ok(0);
    };
    let reborrowed: Option<&mut (dyn FnMut(PullProgress) + Send)> = match on_progress {
        Some(ref mut f) => Some(&mut **f),
        None => None,
    };
    match cursor_bootstrap(fs, api, reborrowed).await {
        Ok(n) => Ok(n),
        Err(PullError::NoChangesFeed) => legacy_full_pull(fs, api, on_progress).await,
        Err(PullError::ResyncRequired) => unreachable!("bootstrap sends no cursor"),
        Err(PullError::Other(e)) => Err(e),
    }
}

fn pull_err(e: PullError) -> anyhow::Error {
    match e {
        PullError::Other(e) => e,
        PullError::NoChangesFeed => anyhow::anyhow!("changes feed unavailable"),
        PullError::ResyncRequired => anyhow::anyhow!("changes cursor expired"),
    }
}

/// Incremental pull from a persisted cursor. Pages until the feed drains;
/// the cursor advances only after a page fully applies, so a failed body
/// fetch re-delivers that page next tick instead of silently skipping it.
async fn cursor_delta(
    fs: &Arc<UnisonFs>,
    api: &Arc<ApiClient>,
    start_cursor: &str,
) -> Result<usize, PullError> {
    let mut cursor = start_cursor.to_string();
    let mut reconciled = 0usize;

    loop {
        let resp = api
            .changes(Some(&cursor), Some(PAGE_LIMIT))
            .await
            .map_err(map_changes_err)?;

        for ch in &resp.changes {
            if apply_change(fs, api, ch).await.map_err(PullError::Other)? {
                reconciled += 1;
            }
        }

        let Some(next) = resp.next_cursor else { break };
        fs.db().sync_meta_set(SYNC_META_CHANGES_CURSOR, &next);
        cursor = next;
        if !resp.has_more {
            break;
        }
    }

    mark_pull_success(fs);
    Ok(reconciled)
}

/// Bootstrap from the changes feed with no cursor: pages the full live doc
/// set, prunes local docs whose remote id no longer exists (covers both a
/// 410 resync and an upgraded client with a pre-feed cache), and persists
/// the resulting cursor.
async fn cursor_bootstrap(
    fs: &Arc<UnisonFs>,
    api: &Arc<ApiClient>,
    mut on_progress: Option<&mut (dyn FnMut(PullProgress) + Send)>,
) -> Result<usize, PullError> {
    let mut since: Option<String> = None;
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut reconciled = 0usize;
    let mut page = 0u32;

    loop {
        let resp = api
            .changes(since.as_deref(), Some(PAGE_LIMIT))
            .await
            .map_err(map_changes_err)?;
        page += 1;

        for ch in &resp.changes {
            if !ch.deleted {
                seen_ids.insert(ch.id.clone());
            }
            if apply_change(fs, api, ch).await.map_err(PullError::Other)? {
                reconciled += 1;
            }
            if let Some(cb) = on_progress.as_mut() {
                cb(PullProgress {
                    page,
                    total_pages: if resp.has_more { page + 1 } else { page },
                    total_items: seen_ids.len(),
                    reconciled,
                });
            }
        }

        match resp.next_cursor {
            Some(next) => {
                since = Some(next);
                if !resp.has_more {
                    break;
                }
            }
            None => break,
        }
    }

    // Prune locals the bootstrap never saw — they were deleted remotely
    // while we had no (valid) cursor to deliver their tombstones.
    let local_ids: Vec<String> = fs.db().all_remote_ids();
    for id in local_ids {
        if !seen_ids.contains(&id) {
            if let Ok(true) = fs.apply_deletion(&id) {
                tracing::info!(remote_id = %id, "bootstrap prune: removed remotely-deleted doc");
            }
        }
    }

    if let Some(c) = since {
        fs.db().sync_meta_set(SYNC_META_CHANGES_CURSOR, &c);
    }
    mark_pull_success(fs);
    Ok(reconciled)
}

/// Apply one feed entry. Returns true when it changed local state.
async fn apply_change(
    fs: &Arc<UnisonFs>,
    api: &Arc<ApiClient>,
    ch: &ChangeEntry,
) -> anyhow::Result<bool> {
    if ch.deleted {
        return Ok(fs.apply_deletion(&ch.id).unwrap_or(false));
    }

    if let Some(ino) = fs.db().ino_by_remote_path(&ch.path) {
        // Dirty guard: never clobber a local edit newer than the remote.
        if let Some(dirty_since) = fs.db().get_dirty_since(ino) {
            if let Ok(remote_ts) = parse_iso8601_ms(&ch.updated_at) {
                if dirty_since >= remote_ts {
                    return Ok(false);
                }
            } else {
                // Unparseable remote timestamp: keep the local edit. Losing
                // freshness for one tick beats overwriting user data.
                return Ok(false);
            }
        }
        // Echo skip: the cache already holds this exact version (our own
        // pushed write coming back around, or a resync over unchanged docs).
        if let (Some(remote_hash), Some(local_hash)) =
            (&ch.content_hash, fs.db().remote_content_hash(ino))
        {
            if *remote_hash == local_hash {
                let remote_ms = parse_iso8601_ms(&ch.updated_at).ok();
                fs.db().set_mirrored_state(ino, remote_ms, Some("ok"), Some(now_ms()));
                fs.db().set_remote_id(ino, &ch.id);
                return Ok(false);
            }
        }
    }

    // Body changed (or doc is new): fetch it.
    match api.get_doc(&ch.path).await {
        Ok(doc) => {
            let content = doc.body_md.as_deref().unwrap_or("").as_bytes().to_vec();
            let ino = fs.upsert_brain_doc(&ch.path, &content)?;
            let remote_ms = parse_iso8601_ms(&ch.updated_at).ok();
            fs.db().set_mirrored_state(ino, remote_ms, Some("ok"), Some(now_ms()));
            fs.db().set_dirty_since(ino, None);
            fs.db().set_remote_id(ino, &ch.id);
            fs.db()
                .set_remote_content_hash(ino, doc.content_hash.as_deref().or(ch.content_hash.as_deref()));
            // Push the change into the kernel cache (FUSE): the entry inval
            // also clears a negative dentry if the file is brand new.
            if let Some((parent_ino, name)) = fs.dentry_of(ino) {
                fs.emit_inval(ino, parent_ino, &name);
            }
            Ok(true)
        }
        // Deleted between the feed page and the body fetch — the tombstone
        // arrives on the next tick; drop it now if we hold a copy.
        Err(ApiError::NotFound) => Ok(fs.apply_deletion(&ch.id).unwrap_or(false)),
        Err(e) => Err(anyhow::anyhow!("fetch {} failed: {e}", ch.path)),
    }
}

fn mark_pull_success(fs: &Arc<UnisonFs>) {
    fs.db().sync_meta_set(SYNC_META_LAST_PULL_AT, &now_ms().to_string());
}

// ─── Legacy fallback (servers without /v1/brain/changes) ────────────────────

async fn legacy_delta_pull(fs: &Arc<UnisonFs>, api: &Arc<ApiClient>) -> anyhow::Result<usize> {
    let last_seen = fs.db().sync_meta_get(SYNC_META_LAST_SEEN).unwrap_or_default();
    let mut newest_seen = last_seen.clone();
    let mut reconciled = 0usize;

    let resp = api
        .list_docs(&ListDocsReq {
            prefix: None,
            kind: Vec::new(),
            tag: Vec::new(),
            limit: Some(500),
        })
        .await
        .map_err(|e| anyhow::anyhow!("delta pull failed: {e}"))?;

    for doc in &resp.documents {
        if !last_seen.is_empty() && doc.updated_at.as_str() <= last_seen.as_str() {
            continue;
        }
        if reconcile_listed_doc(fs, doc)? {
            reconciled += 1;
        }
        if doc.updated_at > newest_seen {
            newest_seen = doc.updated_at.clone();
        }
    }

    if !newest_seen.is_empty() && newest_seen != last_seen {
        fs.db().sync_meta_set(SYNC_META_LAST_SEEN, &newest_seen);
    }
    mark_pull_success(fs);
    Ok(reconciled)
}

async fn legacy_full_pull(
    fs: &Arc<UnisonFs>,
    api: &Arc<ApiClient>,
    mut on_progress: Option<&mut (dyn FnMut(PullProgress) + Send)>,
) -> anyhow::Result<usize> {
    let resp = api
        .list_docs(&ListDocsReq {
            prefix: None,
            kind: Vec::new(),
            tag: Vec::new(),
            limit: Some(500),
        })
        .await
        .map_err(|e| anyhow::anyhow!("full pull failed: {e}"))?;

    let total = resp.documents.len();
    let mut reconciled = 0usize;
    let mut newest_seen = String::new();

    for doc in &resp.documents {
        if reconcile_listed_doc(fs, doc)? {
            reconciled += 1;
        }
        if doc.updated_at > newest_seen {
            newest_seen = doc.updated_at.clone();
        }
        if let Some(cb) = on_progress.as_mut() {
            cb(PullProgress {
                page: 1,
                total_pages: 1,
                total_items: total,
                reconciled,
            });
        }
    }

    if !newest_seen.is_empty() {
        fs.db().sync_meta_set(SYNC_META_LAST_SEEN, &newest_seen);
    }
    mark_pull_success(fs);
    Ok(reconciled)
}

/// Shared legacy-path reconcile of one full doc from `GET /v1/brain/list`.
fn reconcile_listed_doc(
    fs: &Arc<UnisonFs>,
    doc: &crate::api::BrainDocument,
) -> anyhow::Result<bool> {
    if let Some(ino) = fs.db().ino_by_remote_path(&doc.path) {
        if let Some(dirty_since) = fs.db().get_dirty_since(ino) {
            if let Ok(remote_ts) = parse_iso8601_ms(&doc.updated_at) {
                if dirty_since >= remote_ts {
                    return Ok(false);
                }
            }
        }
    }
    let content = doc.body_md.as_deref().unwrap_or("").as_bytes().to_vec();
    let ino = fs.upsert_brain_doc(&doc.path, &content)?;
    let remote_ms = parse_iso8601_ms(&doc.updated_at).ok();
    fs.db().set_mirrored_state(ino, remote_ms, Some("ok"), Some(now_ms()));
    fs.db().set_dirty_since(ino, None);
    fs.db().set_remote_id(ino, &doc.id);
    fs.db().set_remote_content_hash(ino, doc.content_hash.as_deref());
    Ok(true)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse an ISO 8601 datetime string to epoch milliseconds. Accepts both
/// `2026-07-03T12:00:00.123Z` (JSON dates) and `2026-07-03 12:00:00.123+00`
/// (Postgres text) shapes; the offset digits are ignored (feed timestamps
/// are rendered UTC server-side).
fn parse_iso8601_ms(s: &str) -> Result<i64, ()> {
    let s = s.trim_end_matches('Z');
    let s = s.replacen(' ', "T", 1);
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
    let sec_str: String = time_parts[2]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let sec: i64 = sec_str.parse().map_err(|_| ())?;

    // Approximate days since epoch (ignoring leap seconds — good enough for
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_iso_shape() {
        let ms = parse_iso8601_ms("2026-07-03T12:00:00.123Z").unwrap();
        assert_eq!(ms % 1000, 0);
        assert!(ms > 1_780_000_000_000);
    }

    #[test]
    fn parses_pg_text_shape() {
        let a = parse_iso8601_ms("2026-07-03 12:00:00.123456+00").unwrap();
        let b = parse_iso8601_ms("2026-07-03T12:00:00Z").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn parses_no_fraction_with_offset() {
        assert!(parse_iso8601_ms("2026-07-03 12:00:00+00").is_ok());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_iso8601_ms("not a date").is_err());
        assert!(parse_iso8601_ms("2026-07-03").is_err());
    }
}
