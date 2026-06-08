//! SQLite database wrapper for the local filesystem cache.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::Connection;
use tokio::sync::Notify;

use crate::vfs::{FileAttr, Timestamp, DEFAULT_DIR_MODE, PREFERRED_BLOCK_SIZE};

pub const DEFAULT_CHUNK_SIZE: usize = 4096;
pub const ROOT_INO: u64 = 1;
pub const DENTRY_CACHE_MAX: usize = 10_000;

/// SQLite-backed persistent store for filesystem metadata and content.
pub struct Db {
    pub(crate) conn: Mutex<Connection>,
    pub(crate) chunk_size: usize,
    pub(crate) push_notify: Arc<Notify>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        Self::configure_and_init(conn)
    }

    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure_and_init(conn)
    }

    fn configure_and_init(conn: Connection) -> anyhow::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = OFF;",
        )?;

        conn.execute_batch(include_str!("schema.sql"))?;

        let migrations = [
            "ALTER TABLE fs_inode  ADD COLUMN dirty_since         INTEGER",
            "ALTER TABLE fs_remote ADD COLUMN mirrored_updated_at INTEGER",
            "ALTER TABLE fs_remote ADD COLUMN last_status         TEXT",
            "ALTER TABLE fs_remote ADD COLUMN last_status_at      INTEGER",
            "ALTER TABLE push_queue ADD COLUMN remote_path TEXT",
            "ALTER TABLE fs_inode   ADD COLUMN derived     INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE push_queue ADD COLUMN poisoned    INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE push_queue ADD COLUMN last_status INTEGER",
        ];
        for sql in migrations {
            if let Err(e) = conn.execute(sql, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column") {
                    return Err(e.into());
                }
            }
        }

        let db = Self {
            conn: Mutex::new(conn),
            chunk_size: DEFAULT_CHUNK_SIZE,
            push_notify: Arc::new(Notify::new()),
        };

        db.ensure_root()?;
        db.ensure_config()?;

        Ok(db)
    }

    pub(crate) fn push_notify(&self) -> Arc<Notify> {
        self.push_notify.clone()
    }

    fn ensure_root(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM fs_inode WHERE ino = ?1",
            [ROOT_INO as i64],
            |row| row.get(0),
        )?;

        if !exists {
            let now = Timestamp::now();
            conn.execute(
                "INSERT INTO fs_inode (ino, mode, nlink, uid, gid, size, atime, mtime, ctime, rdev, atime_nsec, mtime_nsec, ctime_nsec)
                 VALUES (?1, ?2, 2, 0, 0, 0, ?3, ?4, ?5, 0, ?6, ?7, ?8)",
                rusqlite::params![
                    ROOT_INO as i64,
                    DEFAULT_DIR_MODE as i64,
                    now.sec,
                    now.sec,
                    now.sec,
                    now.nsec,
                    now.nsec,
                    now.nsec,
                ],
            )?;
        }
        Ok(())
    }

    fn ensure_config(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR IGNORE INTO fs_config (key, value) VALUES ('chunk_size', ?1)",
            [self.chunk_size.to_string()],
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO fs_config (key, value) VALUES ('schema_version', '1')",
            [],
        )?;
        Ok(())
    }

    pub(crate) fn get_remote_path(&self, ino: u64) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT remote_path FROM fs_remote WHERE ino = ?1",
            [ino as i64],
            |row| row.get(0),
        )
        .ok()
    }

    pub(crate) fn set_remote_path(&self, ino: u64, remote_path: &str) {
        let conn = self.conn.lock();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO fs_remote (ino, remote_path) VALUES (?1, ?2)",
            rusqlite::params![ino as i64, remote_path],
        );
    }

    pub(crate) fn delete_remote_path(&self, ino: u64) {
        let conn = self.conn.lock();
        let _ = conn.execute("DELETE FROM fs_remote WHERE ino = ?1", [ino as i64]);
    }

    pub(crate) fn set_dirty_since(&self, ino: u64, epoch_ms: Option<i64>) {
        let conn = self.conn.lock();
        let _ = conn.execute(
            "UPDATE fs_inode SET dirty_since = ?2 WHERE ino = ?1",
            rusqlite::params![ino as i64, epoch_ms],
        );
    }

    pub(crate) fn get_dirty_since(&self, ino: u64) -> Option<i64> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT dirty_since FROM fs_inode WHERE ino = ?1",
            [ino as i64],
            |row| row.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
    }

    pub(crate) fn set_mirrored_state(
        &self,
        ino: u64,
        mirrored_updated_at: Option<i64>,
        last_status: Option<&str>,
        last_status_at: Option<i64>,
    ) {
        let conn = self.conn.lock();
        let _ = conn.execute(
            "UPDATE fs_remote
                SET mirrored_updated_at = COALESCE(?2, mirrored_updated_at),
                    last_status         = COALESCE(?3, last_status),
                    last_status_at      = COALESCE(?4, last_status_at)
              WHERE ino = ?1",
            rusqlite::params![ino as i64, mirrored_updated_at, last_status, last_status_at],
        );
    }

    pub(crate) fn ino_by_remote_path(&self, remote_path: &str) -> Option<u64> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT ino FROM fs_remote WHERE remote_path = ?1",
            [remote_path],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .map(|n| n as u64)
    }

    pub(crate) fn sync_meta_get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock();
        conn.query_row("SELECT value FROM sync_meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .ok()
    }

    pub(crate) fn sync_meta_set(&self, key: &str, value: &str) {
        let conn = self.conn.lock();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO sync_meta (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        );
    }

    /// Enqueue a push-queue op with latest-wins coalescing semantics.
    pub(crate) fn push_queue_upsert(
        &self,
        brain_path: &str,
        op: PushOp,
        content_ino: Option<u64>,
        rename_to: Option<&str>,
        now_ms: i64,
    ) {
        if is_noise_path(brain_path) {
            return;
        }
        let conn = self.conn.lock();
        let op_str = op.as_str();
        let content_i64 = content_ino.map(|n| n as i64);

        let inflight_started: Option<i64> = conn
            .query_row(
                "SELECT inflight_started_at FROM push_queue WHERE brain_path = ?1",
                [brain_path],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten();

        if inflight_started.is_some() {
            let _ = conn.execute(
                "UPDATE push_queue
                    SET pending_op           = ?2,
                        pending_content_ino  = ?3,
                        pending_rename_to    = ?4,
                        updated_at           = ?5
                  WHERE brain_path = ?1",
                rusqlite::params![brain_path, op_str, content_i64, rename_to, now_ms],
            );
        } else {
            let _ = conn.execute(
                "INSERT INTO push_queue
                    (brain_path, op, content_ino, rename_to, attempt, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5)
                 ON CONFLICT(brain_path) DO UPDATE SET
                    op         = excluded.op,
                    content_ino= excluded.content_ino,
                    rename_to  = excluded.rename_to,
                    attempt    = 0,
                    last_error = NULL,
                    updated_at = excluded.updated_at",
                rusqlite::params![brain_path, op_str, content_i64, rename_to, now_ms],
            );
        }
        drop(conn);
        self.push_notify.notify_one();
    }

    /// Atomically claim the next queued job.
    pub(crate) fn push_queue_claim_next(&self, now_ms: i64) -> Option<PushJob> {
        let conn = self.conn.lock();
        let row: (String, String, Option<i64>, Option<String>, i64) = conn
            .query_row(
                "SELECT brain_path, op, content_ino, rename_to, attempt
                   FROM push_queue
                  WHERE inflight_started_at IS NULL
                    AND poisoned = 0
                    AND updated_at <= ?1
                  ORDER BY updated_at ASC
                  LIMIT 1",
                [now_ms],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .ok()?;

        let (brain_path, op_str, content_ino, rename_to, attempt) = row;
        let op = PushOp::parse(&op_str)?;

        let updated = conn.execute(
            "UPDATE push_queue
                SET inflight_started_at = ?2
              WHERE brain_path = ?1 AND inflight_started_at IS NULL",
            rusqlite::params![brain_path, now_ms],
        );
        if matches!(updated, Ok(0) | Err(_)) {
            return None;
        }

        Some(PushJob {
            brain_path,
            op,
            content_ino: content_ino.map(|n| n as u64),
            rename_to,
            attempt,
        })
    }

    pub(crate) fn push_queue_finalize_success(&self, brain_path: &str, now_ms: i64) {
        let conn = self.conn.lock();
        let pending: Option<(String, Option<i64>, Option<String>)> = conn
            .query_row(
                "SELECT pending_op, pending_content_ino, pending_rename_to
                   FROM push_queue WHERE brain_path = ?1",
                [brain_path],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok()
            .and_then(|(op, c, r)| op.map(|o| (o, c, r)));

        if let Some((op, content_ino, rename_to)) = pending {
            let _ = conn.execute(
                "UPDATE push_queue
                    SET op                  = ?2,
                        content_ino         = ?3,
                        rename_to           = ?4,
                        pending_op          = NULL,
                        pending_content_ino = NULL,
                        pending_rename_to   = NULL,
                        inflight_started_at = NULL,
                        attempt             = 0,
                        last_error          = NULL,
                        updated_at          = ?5
                  WHERE brain_path = ?1",
                rusqlite::params![brain_path, op, content_ino, rename_to, now_ms],
            );
        } else {
            let _ = conn.execute("DELETE FROM push_queue WHERE brain_path = ?1", [brain_path]);
        }
    }

    pub(crate) fn push_queue_finalize_failure(
        &self,
        brain_path: &str,
        error: &str,
        now_ms: i64,
        backoff_ms: i64,
    ) {
        let conn = self.conn.lock();
        let _ = conn.execute(
            "UPDATE push_queue
                SET inflight_started_at = NULL,
                    attempt             = attempt + 1,
                    last_error          = ?2,
                    updated_at          = ?3
              WHERE brain_path = ?1",
            rusqlite::params![brain_path, error, now_ms + backoff_ms],
        );
    }

    pub(crate) fn push_queue_poison(
        &self,
        brain_path: &str,
        http_status: u16,
        reason: &str,
        now_ms: i64,
    ) {
        let conn = self.conn.lock();
        let _ = conn.execute(
            "UPDATE push_queue
                SET poisoned            = 1,
                    inflight_started_at = NULL,
                    last_error          = ?2,
                    last_status         = ?3,
                    updated_at          = ?4
              WHERE brain_path = ?1",
            rusqlite::params![brain_path, reason, http_status as i64, now_ms],
        );
    }

    pub(crate) fn push_queue_len(&self) -> usize {
        let conn = self.conn.lock();
        conn.query_row("SELECT COUNT(*) FROM push_queue", [], |r| {
            r.get::<_, i64>(0)
        })
        .map(|n| n as usize)
        .unwrap_or(0)
    }

    pub(crate) fn read_all_content(&self, ino: u64) -> Vec<u8> {
        let conn = self.conn.lock();
        let size: i64 = match conn.query_row(
            "SELECT size FROM fs_inode WHERE ino = ?1",
            [ino as i64],
            |r| r.get(0),
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        if size <= 0 {
            return Vec::new();
        }
        let mut stmt = match conn
            .prepare("SELECT data FROM fs_data WHERE ino = ?1 ORDER BY chunk_index ASC")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<u8> = Vec::with_capacity(size as usize);
        if let Ok(rows) = stmt.query_map([ino as i64], |r| r.get::<_, Vec<u8>>(0)) {
            for row in rows.flatten() {
                out.extend_from_slice(&row);
            }
        }
        out.truncate(size as usize);
        out
    }

    pub(crate) fn row_to_attr(row: &rusqlite::Row) -> rusqlite::Result<FileAttr> {
        let ino: i64 = row.get("ino")?;
        let mode: i64 = row.get("mode")?;
        let nlink: i64 = row.get("nlink")?;
        let uid: i64 = row.get("uid")?;
        let gid: i64 = row.get("gid")?;
        let size: i64 = row.get("size")?;
        let atime_sec: i64 = row.get("atime")?;
        let mtime_sec: i64 = row.get("mtime")?;
        let ctime_sec: i64 = row.get("ctime")?;
        let rdev: i64 = row.get("rdev")?;
        let atime_nsec: i64 = row.get("atime_nsec")?;
        let mtime_nsec: i64 = row.get("mtime_nsec")?;
        let ctime_nsec: i64 = row.get("ctime_nsec")?;

        Ok(FileAttr {
            ino: ino as u64,
            mode: mode as u32,
            nlink: nlink as u32,
            uid: uid as u32,
            gid: gid as u32,
            size: size as u64,
            blocks: (size as u64).div_ceil(512),
            atime: Timestamp {
                sec: atime_sec,
                nsec: atime_nsec as u32,
            },
            mtime: Timestamp {
                sec: mtime_sec,
                nsec: mtime_nsec as u32,
            },
            ctime: Timestamp {
                sec: ctime_sec,
                nsec: ctime_nsec as u32,
            },
            rdev: rdev as u32,
            blksize: PREFERRED_BLOCK_SIZE,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOp {
    Write,
    Delete,
    Rename,
}

impl PushOp {
    pub fn as_str(self) -> &'static str {
        match self {
            PushOp::Write => "write",
            PushOp::Delete => "delete",
            PushOp::Rename => "rename",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "write" => Some(PushOp::Write),
            "delete" => Some(PushOp::Delete),
            "rename" => Some(PushOp::Rename),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct PushJob {
    pub brain_path: String,
    pub op: PushOp,
    pub content_ino: Option<u64>,
    pub rename_to: Option<String>,
    pub attempt: i64,
}

/// Filter out OS-generated noise paths.
pub fn is_noise_path(path: &str) -> bool {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return false;
    }
    let first = trimmed.split('/').next().unwrap_or("");
    let basename = trimmed.rsplit('/').next().unwrap_or("");

    if first.starts_with(".Spotlight-V100")
        || first.starts_with(".Trashes")
        || first.starts_with(".fseventsd")
        || first.starts_with(".TemporaryItems")
    {
        return true;
    }
    if basename.starts_with("._") {
        return true;
    }
    matches!(
        basename,
        ".DS_Store"
            | ".localized"
            | ".apdisk"
            | ".VolumeIcon.icns"
            | ".metadata_never_index"
            | ".metadata_never_index_unless_rootfs"
            | ".com.apple.timemachine.donotpresent"
            | "Icon\r"
    )
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("chunk_size", &self.chunk_size)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_creates_root() {
        let db = Db::open_in_memory().unwrap();
        let conn = db.conn.lock();
        let ino: i64 = conn
            .query_row("SELECT ino FROM fs_inode WHERE ino = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ino, 1);
    }

    #[test]
    fn set_then_get_remote_path_round_trips() {
        let db = Db::open_in_memory().unwrap();
        db.set_remote_path(42, "/private/notes/test.md");
        assert_eq!(
            db.get_remote_path(42),
            Some("/private/notes/test.md".to_string())
        );
    }

    #[test]
    fn noise_path_skipped() {
        let db = Db::open_in_memory().unwrap();
        db.push_queue_upsert("/foo/._bar.md", PushOp::Write, None, None, 1);
        assert!(db.push_queue_claim_next(10).is_none());
    }

    #[test]
    fn normal_path_enqueues() {
        let db = Db::open_in_memory().unwrap();
        db.push_queue_upsert(
            "/private/notes/foo.md",
            PushOp::Write,
            None,
            None,
            1,
        );
        assert!(db.push_queue_claim_next(10).is_some());
    }
}
