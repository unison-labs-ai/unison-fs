//! Cache conformance tests.
//!
//! These tests exercise the full cache stack (Db + UnisonFs) in isolation —
//! no network calls, no FUSE/NFS.

use std::sync::Arc;

use crate::cache::{Db, UnisonFs};

fn open_mem_db() -> Arc<Db> {
    Arc::new(Db::open_in_memory().expect("in-memory db"))
}

fn open_mem_fs() -> Arc<UnisonFs> {
    let db = open_mem_db();
    Arc::new(UnisonFs::new(db))
}

// ── 1. ino_by_remote_path ─────────────────────────────────────────────────────

#[test]
fn ino_by_remote_path_roundtrip() {
    let fs = open_mem_fs();
    let path = "/private/notes/rt.md";
    let ino = fs.upsert_brain_doc(path, b"hi").expect("upsert");
    assert_eq!(fs.db().ino_by_remote_path(path), Some(ino));
    assert_eq!(fs.db().ino_by_remote_path("/nonexistent.md"), None);
}

// ── 2. upsert_brain_doc idempotency ─────────────────────────────────────────

#[test]
fn upsert_brain_doc_idempotent() {
    let fs = open_mem_fs();
    let path = "/private/notes/idem.md";
    let content = b"# Hello\n";

    let ino1 = fs.upsert_brain_doc(path, content).expect("first upsert");
    let ino2 = fs.upsert_brain_doc(path, content).expect("second upsert");
    assert_eq!(ino1, ino2, "same inode on re-upsert");

    let stored = fs.db().read_all_content(ino1);
    assert_eq!(stored, content);
}

#[test]
fn upsert_brain_doc_updates_content() {
    let fs = open_mem_fs();
    let path = "/private/notes/upd.md";
    let ino = fs.upsert_brain_doc(path, b"v1").expect("v1 upsert");
    fs.upsert_brain_doc(path, b"v2").expect("v2 upsert");
    assert_eq!(fs.db().read_all_content(ino), b"v2");
}

#[test]
fn different_paths_get_different_inos() {
    let fs = open_mem_fs();
    let a = fs.upsert_brain_doc("/private/notes/a.md", b"a").unwrap();
    let b = fs.upsert_brain_doc("/private/notes/b.md", b"b").unwrap();
    assert_ne!(a, b);
}

// ── 3. Push queue lifecycle ──────────────────────────────────────────────────

#[test]
fn push_queue_enqueue_claim_finalize_success() {
    let fs = open_mem_fs();
    let ino = fs.upsert_brain_doc("/private/notes/push.md", b"data").unwrap();
    let now = 1_000_000i64;

    let db = fs.db();
    db.push_queue_upsert("/private/notes/push.md", crate::cache::PushOp::Write, Some(ino), None, now);

    let job = db
        .push_queue_claim_next(now + 1)
        .expect("should have a claimable job");
    assert_eq!(job.brain_path, "/private/notes/push.md");

    db.push_queue_finalize_success(&job.brain_path, now + 2);
    assert!(
        db.push_queue_claim_next(now + 3).is_none(),
        "queue should be empty after finalize_success"
    );
}

#[test]
fn push_queue_finalize_failure_increments_attempt() {
    let fs = open_mem_fs();
    let ino = fs.upsert_brain_doc("/private/notes/retry.md", b"data").unwrap();
    let now = 1_000_000i64;

    let db = fs.db();
    db.push_queue_upsert("/private/notes/retry.md", crate::cache::PushOp::Write, Some(ino), None, now);
    let job = db.push_queue_claim_next(now + 1).unwrap();
    assert_eq!(job.attempt, 0);

    db.push_queue_finalize_failure(&job.brain_path, "transient error", now + 2, 500);

    let job2 = db.push_queue_claim_next(now + 3000).unwrap();
    assert_eq!(job2.attempt, 1, "attempt should increment after failure");
}

#[test]
fn push_queue_coalesces_duplicates() {
    let fs = open_mem_fs();
    let ino = fs.upsert_brain_doc("/private/notes/coalesce.md", b"data").unwrap();
    let now = 1_000_000i64;

    let db = fs.db();
    db.push_queue_upsert("/private/notes/coalesce.md", crate::cache::PushOp::Write, Some(ino), None, now);
    db.push_queue_upsert("/private/notes/coalesce.md", crate::cache::PushOp::Write, Some(ino), None, now + 1);

    let _j = db.push_queue_claim_next(now + 2).unwrap();
    assert!(
        db.push_queue_claim_next(now + 3).is_none(),
        "second enqueue should have been coalesced"
    );
}

// ── 4. Deletion ──────────────────────────────────────────────────────────────

#[test]
fn apply_deletion_removes_inode() {
    let fs = open_mem_fs();
    let path = "/private/notes/delete_me.md";
    let ino = fs.upsert_brain_doc(path, b"data").expect("upsert");

    fs.db().set_remote_id(ino, "remote-abc-123");

    fs.apply_deletion("remote-abc-123");
    assert_eq!(
        fs.db().ino_by_remote_path(path),
        None,
        "inode should be removed after deletion"
    );
}

// ── 5. Dirty-since bookkeeping ───────────────────────────────────────────────

#[test]
fn dirty_since_roundtrip() {
    let fs = open_mem_fs();
    let ino = fs.upsert_brain_doc("/private/notes/dirty.md", b"x").unwrap();

    assert!(fs.db().get_dirty_since(ino).is_none());

    fs.db().set_dirty_since(ino, Some(999_000));
    assert_eq!(fs.db().get_dirty_since(ino), Some(999_000));

    fs.db().set_dirty_since(ino, None);
    assert!(fs.db().get_dirty_since(ino).is_none());
}

// ── 6. Mirrored-state bookkeeping ───────────────────────────────────────────

#[test]
fn mirrored_state_set_does_not_panic() {
    let fs = open_mem_fs();
    let ino = fs.upsert_brain_doc("/private/notes/mirror.md", b"x").unwrap();
    // set_mirrored_state should not panic; we verify by just calling it.
    fs.db().set_mirrored_state(ino, Some(12345), Some("ok"), Some(99000));
    // And a second time with NULLs should also be fine.
    fs.db().set_mirrored_state(ino, None, None, None);
}

// ── 7. sync_meta round-trip ──────────────────────────────────────────────────

#[test]
fn sync_meta_roundtrip() {
    let db = open_mem_db();
    assert!(db.sync_meta_get("k1").is_none());

    db.sync_meta_set("k1", "hello world");
    assert_eq!(db.sync_meta_get("k1").as_deref(), Some("hello world"));

    db.sync_meta_set("k1", "updated");
    assert_eq!(db.sync_meta_get("k1").as_deref(), Some("updated"));
}

// ── 8. remote_count ─────────────────────────────────────────────────────────

#[test]
fn remote_count_reflects_upserts() {
    let fs = open_mem_fs();
    assert_eq!(fs.db().remote_count(), 0);

    fs.upsert_brain_doc("/private/notes/c1.md", b"a").unwrap();
    assert_eq!(fs.db().remote_count(), 1);

    fs.upsert_brain_doc("/private/notes/c2.md", b"b").unwrap();
    assert_eq!(fs.db().remote_count(), 2);

    // Re-upsert doesn't increment
    fs.upsert_brain_doc("/private/notes/c1.md", b"updated").unwrap();
    assert_eq!(fs.db().remote_count(), 2);
}

// ── 9. Hydration scheduler ──────────────────────────────────────────────────

#[test]
fn hydration_scheduler_fifo_dedup() {
    use crate::cache::HydrationKey;
    use crate::cache::HydrationScheduler;

    let hs = HydrationScheduler::new();

    hs.enqueue(HydrationKey::Exact("/a.md".to_string()));
    hs.enqueue(HydrationKey::Exact("/b.md".to_string()));
    // Duplicate — should not re-enqueue /a.md
    hs.enqueue(HydrationKey::Exact("/a.md".to_string()));

    let first = hs.claim_next().expect("first claim");
    assert_eq!(first.path(), "/a.md");

    let second = hs.claim_next().expect("second claim");
    assert_eq!(second.path(), "/b.md");

    assert!(hs.claim_next().is_none(), "queue should be empty (dedup)");
}

// ── 10. Large-content round-trip ─────────────────────────────────────────────

#[test]
fn large_content_round_trips() {
    let fs = open_mem_fs();
    let path = "/private/notes/large.md";

    // 3× the default chunk size
    let content: Vec<u8> = (0u8..=255).cycle().take(3 * 256 * 1024).collect();
    let ino = fs.upsert_brain_doc(path, &content).unwrap();
    let read_back = fs.db().read_all_content(ino);
    assert_eq!(read_back, content, "large content should round-trip");
}

// ── 11. Noise path filtering ─────────────────────────────────────────────────

#[test]
fn is_noise_path_filters_correctly() {
    use crate::cache::is_noise_path;
    assert!(is_noise_path(".DS_Store"), ".DS_Store is noise");
    assert!(is_noise_path("._foo"), "dot-underscore is noise");
    assert!(is_noise_path(".Spotlight-V100"), ".Spotlight-V100 is noise");
    assert!(!is_noise_path(".git"), ".git is NOT filtered (not a noise basename)");
    assert!(!is_noise_path("notes.md"), "regular .md is not noise");
    assert!(!is_noise_path("README"), "regular README is not noise");
}

// ── 12. ino_for_remote_id round-trip ────────────────────────────────────────

#[test]
fn ino_for_remote_id_roundtrip() {
    let fs = open_mem_fs();
    let ino = fs.upsert_brain_doc("/private/notes/rid.md", b"x").unwrap();
    fs.db().set_remote_id(ino, "remote-xyz");
    assert_eq!(fs.db().ino_for_remote_id("remote-xyz"), Some(ino));
    assert_eq!(fs.db().ino_for_remote_id("nonexistent"), None);
}

// ── 13. push_queue_len ──────────────────────────────────────────────────────

#[test]
fn push_queue_len_tracks_entries() {
    let fs = open_mem_fs();
    let ino = fs.upsert_brain_doc("/private/notes/len.md", b"x").unwrap();
    let db = fs.db();
    let now = 2_000_000i64;

    assert_eq!(db.push_queue_len(), 0);
    db.push_queue_upsert("/private/notes/len.md", crate::cache::PushOp::Write, Some(ino), None, now);
    assert_eq!(db.push_queue_len(), 1);
}
