//! Deletion reconciliation — periodic remote→local cleanup.
//!
//! Walks the full remote document list and removes local inodes whose
//! `remote_id` (brain doc id) has disappeared from the server.

use std::collections::HashSet;
use std::sync::Arc;

use crate::api::ListDocsReq;
use crate::cache::UnisonFs;

const PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone, Copy)]
pub struct DeletionScanProgress {
    pub page: u32,
    pub total_pages: u32,
    pub total_items: usize,
    pub remote_seen: usize,
}

/// Run one deletion-scan pass. Returns `Ok(removed)` — the count of local
/// inodes that were unlinked because their remote doc id disappeared.
pub async fn deletion_scan(fs: &Arc<UnisonFs>) -> anyhow::Result<usize> {
    deletion_scan_inner(fs, None).await
}

pub async fn deletion_scan_with_progress<F>(
    fs: &Arc<UnisonFs>,
    mut on_progress: F,
) -> anyhow::Result<usize>
where
    F: FnMut(DeletionScanProgress) + Send,
{
    deletion_scan_inner(fs, Some(&mut on_progress)).await
}

async fn deletion_scan_inner(
    fs: &Arc<UnisonFs>,
    mut on_progress: Option<&mut (dyn FnMut(DeletionScanProgress) + Send)>,
) -> anyhow::Result<usize> {
    let Some(api) = fs.api() else {
        return Ok(0);
    };

    // Page through all remote docs and collect their ids.
    let mut remote_ids: HashSet<String> = HashSet::new();
    let mut page = 1u32;
    let total_pages = 1u32;

    loop {
        let resp = api
            .list_docs(&ListDocsReq {
                prefix: None,
                kind: Vec::new(),
                tag: Vec::new(),
                limit: Some(PAGE_SIZE),
            })
            .await
            .map_err(|e| anyhow::anyhow!("deletion scan list failed: {e}"))?;

        for doc in &resp.documents {
            remote_ids.insert(doc.id.clone());
        }
        // The Unison API returns all docs in one call (no paging yet);
        // treat as a single page so we don't loop forever.
        let total_items = remote_ids.len();
        if let Some(cb) = on_progress.as_mut() {
            cb(DeletionScanProgress {
                page,
                total_pages,
                total_items,
                remote_seen: remote_ids.len(),
            });
        }
        if page >= total_pages || resp.documents.is_empty() {
            break;
        }
        page += 1;
    }

    // Read local ids and remove anything absent from remote.
    let local_ids: Vec<String> = {
        let conn = fs.db().conn.lock();
        let mut stmt = conn
            .prepare("SELECT remote_id FROM fs_remote")
            .map_err(|e| anyhow::anyhow!(e))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| anyhow::anyhow!(e))?;
        rows.filter_map(|r| r.ok()).collect()
    };

    let mut removed = 0usize;
    for id in local_ids {
        if !remote_ids.contains(&id) {
            if let Ok(true) = fs.apply_deletion(&id) {
                removed += 1;
            }
        }
    }

    Ok(removed)
}
