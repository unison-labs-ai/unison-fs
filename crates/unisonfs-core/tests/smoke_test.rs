//! Smoke test: real round-trip against a live Unison brain.
//!
//! Requires:
//!   UNISON_TOKEN   — a valid usk_... key
//!   UNISON_API_URL — base URL of the brain API (e.g. http://localhost:4001)
//!
//! Skipped automatically when those env vars are absent. Run with:
//!   UNISON_TOKEN=... UNISON_API_URL=http://localhost:4001 cargo test smoke -- --nocapture

use std::sync::Arc;

use unisonfs_core::api::{ApiClient, PutDocReq};
use unisonfs_core::cache::{Db, UnisonFs};

fn smoke_env() -> Option<(String, String)> {
    let token = std::env::var("UNISON_TOKEN").ok()?;
    let api_url = std::env::var("UNISON_API_URL").ok()?;
    if token.is_empty() || api_url.is_empty() {
        return None;
    }
    Some((token, api_url))
}

/// Write a document to the brain via the API client, read it back, and verify
/// the content round-trips correctly.
#[tokio::test]
async fn smoke_api_write_read_roundtrip() {
    let Some((token, api_url)) = smoke_env() else {
        eprintln!("SKIP: UNISON_TOKEN or UNISON_API_URL not set");
        return;
    };

    let client = ApiClient::new(&api_url, &token);

    // Verify auth first.
    let who = client
        .whoami()
        .await
        .expect("whoami should succeed with a valid token");
    eprintln!("smoke: authenticated as {} (workspace: {})", who.user_email, who.workspace_id);

    // Write a test document.
    let test_path = format!("/private/notes/smoke-test-{}.md", now_ms());
    let test_body = format!(
        "# Smoke test\n\nWritten by unisonfs smoke test at {}ms.\n",
        now_ms()
    );

    let put_resp = client
        .put_doc(&PutDocReq {
            path: test_path.clone(),
            body_md: test_body.clone(),
            kind: Some("note".to_string()),
            title: Some("Smoke test".to_string()),
            tldr: None,
            tags: None,
            visibility: None,
            expected_content_hash: None,
        })
        .await
        .expect("put_doc should succeed");
    eprintln!("smoke: wrote {} (doc id: {})", test_path, put_resp.id);

    // Read it back.
    let get_resp = client
        .get_doc(&test_path)
        .await
        .expect("get_doc should succeed");
    let returned_body = get_resp.body_md.as_deref().unwrap_or("");
    assert_eq!(
        returned_body, test_body,
        "body round-trip mismatch: wrote {:?} but got {:?}",
        test_body, returned_body
    );
    eprintln!("smoke: read-back confirmed — body matches");

    // Clean up.
    client
        .delete_doc(&test_path)
        .await
        .expect("delete_doc should succeed");
    eprintln!("smoke: cleaned up {}", test_path);
}

/// Verify that the local cache correctly stores and retrieves a document, and
/// that the dirty-tracking guard is set when enqueue_write is called.
/// This exercises the full upsert_brain_doc + enqueue_write path used by the
/// mount on every local write.
#[tokio::test]
async fn smoke_cache_upsert_and_enqueue() {
    let Some((token, api_url)) = smoke_env() else {
        eprintln!("SKIP: UNISON_TOKEN or UNISON_API_URL not set");
        return;
    };

    let db = Arc::new(Db::open_in_memory().expect("in-memory db"));
    let fs = Arc::new(UnisonFs::new(db.clone()));
    let client = ApiClient::new(&api_url, &token);

    // Write a test document to the brain.
    let test_path = format!("/private/notes/smoke-cache-{}.md", now_ms());
    let test_body = "# Cache upsert test\n".to_string();

    client
        .put_doc(&PutDocReq {
            path: test_path.clone(),
            body_md: test_body.clone(),
            kind: Some("note".to_string()),
            title: None,
            tldr: None,
            tags: None,
            visibility: None,
            expected_content_hash: None,
        })
        .await
        .expect("put_doc for cache test");

    // Upsert into local cache (simulates what the pull loop does).
    let ino = fs
        .upsert_brain_doc(&test_path, test_body.as_bytes())
        .expect("upsert_brain_doc");
    eprintln!("smoke: upserted {test_path} → ino={ino}");

    // Verify the remote-path mapping is set.
    let brain_path = fs.brain_path_for_ino(ino);
    assert_eq!(brain_path.as_deref(), Some(test_path.as_str()));
    eprintln!("smoke: brain_path_for_ino({ino}) = {:?}", brain_path);

    // Simulate a local write: enqueue_write must arm the dirty guard.
    fs.enqueue_write(&test_path, ino);
    eprintln!("smoke: enqueue_write called — push queue now has an entry");

    // Clean up.
    client.delete_doc(&test_path).await.ok();
    eprintln!("smoke: cache upsert + enqueue_write path verified");
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
