//! SQLite-backed `FileSystem` implementation for the Unison brain.
//!
//! `UnisonFs` is the core VFS backend. It stores the brain's virtual tree in a
//! local SQLite cache, routes writes to the push queue for background sync, and
//! exposes the brain namespace (/private/..., /tenant/..., /teams/<slug>/...,
//! /system/search/...) as a real directory tree.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::vfs::{
    error::{VfsError, VfsResult},
    traits::{BoxedFile, File, FileSystem},
    types::{DirEntry, FileAttr, FilesystemStats, SetAttr, TimeOrNow, Timestamp},
    mode::{DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, MAX_NAME_LEN, S_IFMT},
};

use super::db::{Db, PushOp, ROOT_INO};
use super::file::DbFile;

/// SQLite-backed filesystem that fronts the Unison brain.
pub struct UnisonFs {
    pub(crate) db: Arc<Db>,
    /// Owning UID/GID for new inodes (from the process at mount time).
    uid: u32,
    gid: u32,
    /// Shared mutex around the brain_path mapping so push_queue upserts are
    /// serialized with dentry creation.
    _write_lock: Mutex<()>,
}

impl std::fmt::Debug for UnisonFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnisonFs")
            .field("uid", &self.uid)
            .field("gid", &self.gid)
            .finish_non_exhaustive()
    }
}

impl UnisonFs {
    pub fn new(db: Arc<Db>) -> Self {
        // Use the current process's uid/gid so files appear owned by the user.
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        Self {
            db,
            uid,
            gid,
            _write_lock: Mutex::new(()),
        }
    }

    /// Get the brain path (under /private/..., /tenant/..., etc.) for an inode.
    pub fn brain_path_for_ino(&self, ino: u64) -> Option<String> {
        self.db.get_remote_path(ino)
    }

    /// Enqueue a write to the push queue. Called from `flush` / `create_file`.
    pub fn enqueue_write(&self, brain_path: &str, content_ino: u64) {
        let now_ms = now_ms();
        self.db
            .push_queue_upsert(brain_path, PushOp::Write, Some(content_ino), None, now_ms);
    }

    /// Enqueue a delete.
    pub fn enqueue_delete(&self, brain_path: &str) {
        let now_ms = now_ms();
        self.db
            .push_queue_upsert(brain_path, PushOp::Delete, None, None, now_ms);
    }

    // ─── Low-level SQLite helpers ──────────────────────────────────────────

    fn lookup_ino(&self, parent_ino: u64, name: &str) -> Option<u64> {
        let conn = self.db.conn.lock();
        conn.query_row(
            "SELECT ino FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
            rusqlite::params![parent_ino as i64, name],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .map(|n| n as u64)
    }

    fn get_attr_by_ino(&self, ino: u64) -> Option<FileAttr> {
        let conn = self.db.conn.lock();
        conn.query_row(
            "SELECT ino, mode, nlink, uid, gid, size, atime, mtime, ctime, rdev,
                    atime_nsec, mtime_nsec, ctime_nsec
               FROM fs_inode WHERE ino = ?1",
            [ino as i64],
            Db::row_to_attr,
        )
        .ok()
    }

    fn create_dir_inode(&self, uid: u32, gid: u32) -> VfsResult<u64> {
        let now = Timestamp::now();
        let conn = self.db.conn.lock();
        conn.execute(
            "INSERT INTO fs_inode (mode, nlink, uid, gid, size, atime, mtime, ctime, rdev, atime_nsec, mtime_nsec, ctime_nsec)
             VALUES (?1, 2, ?2, ?3, 0, ?4, ?5, ?6, 0, ?7, ?8, ?9)",
            rusqlite::params![
                DEFAULT_DIR_MODE as i64,
                uid as i64,
                gid as i64,
                now.sec,
                now.sec,
                now.sec,
                now.nsec,
                now.nsec,
                now.nsec,
            ],
        )
        .map_err(VfsError::Database)?;
        Ok(conn.last_insert_rowid() as u64)
    }

    fn create_file_inode(&self, uid: u32, gid: u32) -> VfsResult<u64> {
        let now = Timestamp::now();
        let conn = self.db.conn.lock();
        conn.execute(
            "INSERT INTO fs_inode (mode, nlink, uid, gid, size, atime, mtime, ctime, rdev, atime_nsec, mtime_nsec, ctime_nsec)
             VALUES (?1, 1, ?2, ?3, 0, ?4, ?5, ?6, 0, ?7, ?8, ?9)",
            rusqlite::params![
                DEFAULT_FILE_MODE as i64,
                uid as i64,
                gid as i64,
                now.sec,
                now.sec,
                now.sec,
                now.nsec,
                now.nsec,
                now.nsec,
            ],
        )
        .map_err(VfsError::Database)?;
        Ok(conn.last_insert_rowid() as u64)
    }

    fn insert_dentry(&self, parent_ino: u64, name: &str, child_ino: u64) -> VfsResult<()> {
        let conn = self.db.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO fs_dentry (parent_ino, name, ino) VALUES (?1, ?2, ?3)",
            rusqlite::params![parent_ino as i64, name, child_ino as i64],
        )
        .map_err(VfsError::Database)?;
        Ok(())
    }

    fn remove_dentry(&self, parent_ino: u64, name: &str) -> VfsResult<()> {
        let conn = self.db.conn.lock();
        conn.execute(
            "DELETE FROM fs_dentry WHERE parent_ino = ?1 AND name = ?2",
            rusqlite::params![parent_ino as i64, name],
        )
        .map_err(VfsError::Database)?;
        Ok(())
    }

    fn children(&self, ino: u64) -> Vec<(String, u64)> {
        let conn = self.db.conn.lock();
        let mut stmt = match conn.prepare(
            "SELECT name, ino FROM fs_dentry WHERE parent_ino = ?1 ORDER BY name ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = match stmt.query_map([ino as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
        }) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    fn is_dir(&self, ino: u64) -> bool {
        self.get_attr_by_ino(ino)
            .map(|a| a.is_directory())
            .unwrap_or(false)
    }

    fn decr_nlink_and_maybe_remove(&self, ino: u64) {
        let conn = self.db.conn.lock();
        let nlink: i64 = conn
            .query_row(
                "SELECT nlink FROM fs_inode WHERE ino = ?1",
                [ino as i64],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if nlink <= 1 {
            let _ = conn.execute("DELETE FROM fs_inode WHERE ino = ?1", [ino as i64]);
            let _ = conn.execute("DELETE FROM fs_data WHERE ino = ?1", [ino as i64]);
            let _ = conn.execute("DELETE FROM fs_symlink WHERE ino = ?1", [ino as i64]);
            let _ = conn.execute("DELETE FROM fs_remote WHERE ino = ?1", [ino as i64]);
        } else {
            let _ = conn.execute(
                "UPDATE fs_inode SET nlink = nlink - 1 WHERE ino = ?1",
                [ino as i64],
            );
        }
    }

    /// Ensure intermediate directories exist for a brain path like /private/notes.
    /// Returns the inode of the deepest created directory.
    pub fn ensure_dirs_for_path(&self, brain_path: &str) -> VfsResult<()> {
        let components: Vec<&str> = brain_path
            .trim_start_matches('/')
            .split('/')
            .collect();
        // The last component is the filename, skip it.
        if components.len() <= 1 {
            return Ok(());
        }

        let mut parent_ino = ROOT_INO;
        for component in &components[..components.len() - 1] {
            if component.is_empty() {
                continue;
            }
            match self.lookup_ino(parent_ino, component) {
                Some(ino) => {
                    parent_ino = ino;
                }
                None => {
                    let new_ino = self.create_dir_inode(self.uid, self.gid)?;
                    self.insert_dentry(parent_ino, component, new_ino)?;
                    parent_ino = new_ino;
                }
            }
        }
        Ok(())
    }

    /// Write a document body into the SQLite cache at the given inode,
    /// replacing any existing content.
    pub fn write_content_to_ino(&self, ino: u64, content: &[u8]) -> VfsResult<()> {
        let conn = self.db.conn.lock();
        // Delete old chunks
        conn.execute("DELETE FROM fs_data WHERE ino = ?1", [ino as i64])
            .map_err(VfsError::Database)?;

        let chunk_size = self.db.chunk_size;
        for (i, chunk) in content.chunks(chunk_size).enumerate() {
            conn.execute(
                "INSERT INTO fs_data (ino, chunk_index, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![ino as i64, i as i64, chunk],
            )
            .map_err(VfsError::Database)?;
        }

        let now = Timestamp::now();
        conn.execute(
            "UPDATE fs_inode SET size = ?2, mtime = ?3, ctime = ?4, mtime_nsec = ?5, ctime_nsec = ?6 WHERE ino = ?1",
            rusqlite::params![
                ino as i64,
                content.len() as i64,
                now.sec,
                now.sec,
                now.nsec,
                now.nsec,
            ],
        )
        .map_err(VfsError::Database)?;

        Ok(())
    }

    /// Create or update a file at the given brain path in the local cache.
    /// Returns the inode number.
    pub fn upsert_brain_doc(&self, brain_path: &str, content: &[u8]) -> VfsResult<u64> {
        self.ensure_dirs_for_path(brain_path)?;

        let filename = brain_path
            .rsplit('/')
            .next()
            .unwrap_or(brain_path);

        // Find parent directory ino
        let parent_path: String = brain_path
            .rsplit_once('/')
            .map(|(p, _)| p)
            .unwrap_or("")
            .to_string();

        let parent_ino = if parent_path.is_empty() {
            ROOT_INO
        } else {
            self.resolve_path_to_ino(&parent_path)
                .unwrap_or(ROOT_INO)
        };

        let ino = match self.lookup_ino(parent_ino, filename) {
            Some(ino) => ino,
            None => {
                let ino = self.create_file_inode(self.uid, self.gid)?;
                self.insert_dentry(parent_ino, filename, ino)?;
                self.db.set_remote_path(ino, brain_path);
                ino
            }
        };

        self.write_content_to_ino(ino, content)?;
        Ok(ino)
    }

    /// Resolve a path string like "/private/notes" to an inode.
    fn resolve_path_to_ino(&self, path: &str) -> Option<u64> {
        let mut ino = ROOT_INO;
        for component in path.trim_start_matches('/').split('/') {
            if component.is_empty() {
                continue;
            }
            ino = self.lookup_ino(ino, component)?;
        }
        Some(ino)
    }
}

#[async_trait]
impl FileSystem for UnisonFs {
    async fn lookup(&self, parent_ino: u64, name: &str) -> VfsResult<Option<FileAttr>> {
        let Some(ino) = self.lookup_ino(parent_ino, name) else {
            return Ok(None);
        };
        Ok(self.get_attr_by_ino(ino))
    }

    async fn getattr(&self, ino: u64) -> VfsResult<Option<FileAttr>> {
        Ok(self.get_attr_by_ino(ino))
    }

    async fn setattr(&self, ino: u64, attr: SetAttr) -> VfsResult<FileAttr> {
        let now = Timestamp::now();
        // Determine whether we need a truncate (which requires an await).
        let need_truncate = attr.size;

        // All synchronous DB work happens inside a scoped block so the
        // MutexGuard is dropped before any await point.
        {
            let conn = self.db.conn.lock();
            if let Some(mode) = attr.mode {
                let existing_mode: i64 = conn
                    .query_row(
                        "SELECT mode FROM fs_inode WHERE ino = ?1",
                        [ino as i64],
                        |r| r.get(0),
                    )
                    .map_err(VfsError::Database)?;
                let new_mode = (existing_mode as u32 & S_IFMT) | (mode & !S_IFMT);
                conn.execute(
                    "UPDATE fs_inode SET mode = ?2 WHERE ino = ?1",
                    rusqlite::params![ino as i64, new_mode as i64],
                )
                .map_err(VfsError::Database)?;
            }
            if let Some(uid) = attr.uid {
                conn.execute(
                    "UPDATE fs_inode SET uid = ?2 WHERE ino = ?1",
                    rusqlite::params![ino as i64, uid as i64],
                )
                .map_err(VfsError::Database)?;
            }
            if let Some(gid) = attr.gid {
                conn.execute(
                    "UPDATE fs_inode SET gid = ?2 WHERE ino = ?1",
                    rusqlite::params![ino as i64, gid as i64],
                )
                .map_err(VfsError::Database)?;
            }
            if need_truncate.is_none() {
                // Only update timestamps when not resizing (resize updates them
                // inside truncate).
                let atime_ts = match attr.atime {
                    Some(TimeOrNow::Now) => now,
                    Some(TimeOrNow::Time(t)) => t,
                    None => now,
                };
                let mtime_ts = match attr.mtime {
                    Some(TimeOrNow::Now) => now,
                    Some(TimeOrNow::Time(t)) => t,
                    None => now,
                };
                conn.execute(
                    "UPDATE fs_inode SET atime = ?2, mtime = ?3, ctime = ?4,
                                        atime_nsec = ?5, mtime_nsec = ?6, ctime_nsec = ?7
                       WHERE ino = ?1",
                    rusqlite::params![
                        ino as i64,
                        atime_ts.sec,
                        mtime_ts.sec,
                        now.sec,
                        atime_ts.nsec,
                        mtime_ts.nsec,
                        now.nsec,
                    ],
                )
                .map_err(VfsError::Database)?;
            }
            // MutexGuard drops here — before any await
        }

        // Truncate after releasing the lock (contains an await internally).
        if let Some(size) = need_truncate {
            let handle = DbFile::new(self.db.clone(), ino);
            handle.truncate(size).await?;
        }

        self.get_attr_by_ino(ino).ok_or(VfsError::NotFound)
    }

    async fn readdir(&self, ino: u64) -> VfsResult<Option<Vec<String>>> {
        if !self.is_dir(ino) {
            return Ok(None);
        }
        let children = self.children(ino);
        Ok(Some(children.into_iter().map(|(n, _)| n).collect()))
    }

    async fn readdir_plus(&self, ino: u64) -> VfsResult<Option<Vec<DirEntry>>> {
        if !self.is_dir(ino) {
            return Ok(None);
        }
        let children = self.children(ino);
        let entries: Vec<DirEntry> = children
            .into_iter()
            .filter_map(|(name, child_ino)| {
                self.get_attr_by_ino(child_ino).map(|attr| DirEntry { name, attr })
            })
            .collect();
        Ok(Some(entries))
    }

    async fn mkdir(
        &self,
        parent_ino: u64,
        name: &str,
        _mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileAttr> {
        if name.len() > MAX_NAME_LEN as usize {
            return Err(VfsError::NameTooLong);
        }
        if !self.is_dir(parent_ino) {
            return Err(VfsError::NotDirectory);
        }
        if self.lookup_ino(parent_ino, name).is_some() {
            return Err(VfsError::AlreadyExists);
        }
        let new_ino = self.create_dir_inode(uid, gid)?;
        self.insert_dentry(parent_ino, name, new_ino)?;
        self.get_attr_by_ino(new_ino).ok_or(VfsError::NotFound)
    }

    async fn rmdir(&self, parent_ino: u64, name: &str) -> VfsResult<()> {
        let child_ino = self
            .lookup_ino(parent_ino, name)
            .ok_or(VfsError::NotFound)?;
        if !self.is_dir(child_ino) {
            return Err(VfsError::NotDirectory);
        }
        if !self.children(child_ino).is_empty() {
            return Err(VfsError::NotEmpty);
        }
        self.remove_dentry(parent_ino, name)?;
        self.decr_nlink_and_maybe_remove(child_ino);
        Ok(())
    }

    async fn open(&self, ino: u64, _flags: i32) -> VfsResult<BoxedFile> {
        let attr = self.get_attr_by_ino(ino).ok_or(VfsError::NotFound)?;
        if attr.is_directory() {
            return Err(VfsError::IsDirectory);
        }
        Ok(Arc::new(DbFile::new(self.db.clone(), ino)))
    }

    async fn create_file(
        &self,
        parent_ino: u64,
        name: &str,
        _mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<(FileAttr, BoxedFile)> {
        if name.len() > MAX_NAME_LEN as usize {
            return Err(VfsError::NameTooLong);
        }
        if !self.is_dir(parent_ino) {
            return Err(VfsError::NotDirectory);
        }

        // If existing, just open it
        if let Some(existing_ino) = self.lookup_ino(parent_ino, name) {
            let attr = self.get_attr_by_ino(existing_ino).ok_or(VfsError::NotFound)?;
            let handle = DbFile::new(self.db.clone(), existing_ino);
            return Ok((attr, Arc::new(handle)));
        }

        let new_ino = self.create_file_inode(uid, gid)?;
        self.insert_dentry(parent_ino, name, new_ino)?;

        let attr = self.get_attr_by_ino(new_ino).ok_or(VfsError::NotFound)?;
        let handle = DbFile::new(self.db.clone(), new_ino);
        Ok((attr, Arc::new(handle)))
    }

    async fn unlink(&self, parent_ino: u64, name: &str) -> VfsResult<()> {
        let child_ino = self
            .lookup_ino(parent_ino, name)
            .ok_or(VfsError::NotFound)?;
        let attr = self.get_attr_by_ino(child_ino).ok_or(VfsError::NotFound)?;
        if attr.is_directory() {
            return Err(VfsError::IsDirectory);
        }

        // Enqueue delete if we have a remote path
        if let Some(brain_path) = self.db.get_remote_path(child_ino) {
            self.enqueue_delete(&brain_path);
        }

        self.remove_dentry(parent_ino, name)?;
        self.decr_nlink_and_maybe_remove(child_ino);
        Ok(())
    }

    async fn readlink(&self, ino: u64) -> VfsResult<Option<String>> {
        let conn = self.db.conn.lock();
        Ok(conn
            .query_row(
                "SELECT target FROM fs_symlink WHERE ino = ?1",
                [ino as i64],
                |r| r.get(0),
            )
            .ok())
    }

    async fn symlink(
        &self,
        parent_ino: u64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileAttr> {
        if !self.is_dir(parent_ino) {
            return Err(VfsError::NotDirectory);
        }
        if self.lookup_ino(parent_ino, name).is_some() {
            return Err(VfsError::AlreadyExists);
        }
        let now = Timestamp::now();
        let conn = self.db.conn.lock();
        conn.execute(
            "INSERT INTO fs_inode (mode, nlink, uid, gid, size, atime, mtime, ctime, rdev, atime_nsec, mtime_nsec, ctime_nsec)
             VALUES (0xA1FF, 1, ?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, ?9)",
            rusqlite::params![
                uid as i64,
                gid as i64,
                target.len() as i64,
                now.sec,
                now.sec,
                now.sec,
                now.nsec,
                now.nsec,
                now.nsec,
            ],
        )
        .map_err(VfsError::Database)?;
        let new_ino = conn.last_insert_rowid() as u64;
        conn.execute(
            "INSERT INTO fs_symlink (ino, target) VALUES (?1, ?2)",
            rusqlite::params![new_ino as i64, target],
        )
        .map_err(VfsError::Database)?;
        drop(conn);
        self.insert_dentry(parent_ino, name, new_ino)?;
        self.get_attr_by_ino(new_ino).ok_or(VfsError::NotFound)
    }

    async fn link(&self, ino: u64, new_parent_ino: u64, new_name: &str) -> VfsResult<FileAttr> {
        let attr = self.get_attr_by_ino(ino).ok_or(VfsError::NotFound)?;
        if attr.is_directory() {
            return Err(VfsError::IsDirectory);
        }
        self.insert_dentry(new_parent_ino, new_name, ino)?;
        {
            let conn = self.db.conn.lock();
            conn.execute(
                "UPDATE fs_inode SET nlink = nlink + 1 WHERE ino = ?1",
                [ino as i64],
            )
            .map_err(VfsError::Database)?;
        }
        self.get_attr_by_ino(ino).ok_or(VfsError::NotFound)
    }

    async fn rename(
        &self,
        old_parent_ino: u64,
        old_name: &str,
        new_parent_ino: u64,
        new_name: &str,
    ) -> VfsResult<()> {
        let child_ino = self
            .lookup_ino(old_parent_ino, old_name)
            .ok_or(VfsError::NotFound)?;

        // Remove old dentry
        self.remove_dentry(old_parent_ino, old_name)?;

        // If new destination exists, remove it
        if let Some(old_dst) = self.lookup_ino(new_parent_ino, new_name) {
            self.remove_dentry(new_parent_ino, new_name)?;
            self.decr_nlink_and_maybe_remove(old_dst);
        }

        // Enqueue rename if we have a remote path
        if let Some(old_brain_path) = self.db.get_remote_path(child_ino) {
            let now_ms = now_ms();
            self.db.push_queue_upsert(
                &old_brain_path,
                PushOp::Rename,
                None,
                Some(new_name),
                now_ms,
            );
        }

        self.insert_dentry(new_parent_ino, new_name, child_ino)?;
        Ok(())
    }

    async fn statfs(&self) -> VfsResult<FilesystemStats> {
        let conn = self.db.conn.lock();
        let inodes: i64 = conn
            .query_row("SELECT COUNT(*) FROM fs_inode", [], |r| r.get(0))
            .unwrap_or(0);
        let bytes_used: i64 = conn
            .query_row("SELECT COALESCE(SUM(size), 0) FROM fs_inode", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        Ok(FilesystemStats {
            inodes: inodes as u64,
            bytes_used: bytes_used as u64,
        })
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
