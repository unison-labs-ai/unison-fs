//! SQLite-backed file handle.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use crate::vfs::{
    error::{VfsError, VfsResult},
    types::{FileAttr, Timestamp},
    traits::File,
};

use super::db::Db;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A handle to an open file in the SQLite cache.
#[derive(Debug)]
pub struct DbFile {
    pub(crate) db: Arc<Db>,
    pub(crate) ino: u64,
}

impl DbFile {
    pub fn new(db: Arc<Db>, ino: u64) -> Self {
        Self { db, ino }
    }
}

#[async_trait]
impl File for DbFile {
    async fn read(&self, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        let conn = self.db.conn.lock();
        let total_size: i64 = conn
            .query_row(
                "SELECT size FROM fs_inode WHERE ino = ?1",
                [self.ino as i64],
                |r| r.get(0),
            )
            .map_err(VfsError::Database)?;

        if total_size <= 0 || offset >= total_size as u64 {
            return Ok(Vec::new());
        }

        let start_chunk = (offset / self.db.chunk_size as u64) as usize;
        let end_byte = (offset as usize + size).min(total_size as usize);
        let end_chunk = (end_byte.saturating_sub(1) / self.db.chunk_size) as i64;

        let mut stmt = conn
            .prepare(
                "SELECT data FROM fs_data WHERE ino = ?1 AND chunk_index >= ?2 AND chunk_index <= ?3
                 ORDER BY chunk_index ASC",
            )
            .map_err(VfsError::Database)?;

        let mut assembled: Vec<u8> = Vec::new();
        let rows = stmt
            .query_map(
                rusqlite::params![self.ino as i64, start_chunk as i64, end_chunk],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .map_err(VfsError::Database)?;

        for row in rows {
            assembled.extend_from_slice(&row.map_err(VfsError::Database)?);
        }

        let local_offset = offset as usize - start_chunk * self.db.chunk_size;
        let available = assembled.len().saturating_sub(local_offset);
        let take = size.min(available).min((total_size as usize).saturating_sub(offset as usize));
        Ok(assembled[local_offset..local_offset + take].to_vec())
    }

    async fn write(&self, offset: u64, data: &[u8]) -> VfsResult<u32> {
        if data.is_empty() {
            return Ok(0);
        }

        let chunk_size = self.db.chunk_size;
        let first_chunk = (offset / chunk_size as u64) as usize;
        let last_byte = offset as usize + data.len() - 1;
        let last_chunk = last_byte / chunk_size;

        let conn = self.db.conn.lock();

        // Fetch all affected chunks to merge
        let mut chunks: std::collections::HashMap<usize, Vec<u8>> = std::collections::HashMap::new();
        let mut stmt = conn
            .prepare(
                "SELECT chunk_index, data FROM fs_data WHERE ino = ?1 AND chunk_index >= ?2 AND chunk_index <= ?3",
            )
            .map_err(VfsError::Database)?;

        let rows = stmt
            .query_map(
                rusqlite::params![self.ino as i64, first_chunk as i64, last_chunk as i64],
                |r| {
                    let idx: i64 = r.get(0)?;
                    let d: Vec<u8> = r.get(1)?;
                    Ok((idx as usize, d))
                },
            )
            .map_err(VfsError::Database)?;

        for row in rows {
            let (idx, d) = row.map_err(VfsError::Database)?;
            chunks.insert(idx, d);
        }

        // Get current file size for extending
        let current_size: i64 = conn
            .query_row(
                "SELECT size FROM fs_inode WHERE ino = ?1",
                [self.ino as i64],
                |r| r.get(0),
            )
            .unwrap_or(0);

        // Merge data into affected chunks
        for chunk_idx in first_chunk..=last_chunk {
            let chunk_start = chunk_idx * chunk_size;
            let chunk = chunks.entry(chunk_idx).or_default();
            if chunk.len() < chunk_size {
                // Extend if this chunk was shorter (at end of file)
                let needed = if chunk_idx == last_chunk {
                    ((last_byte % chunk_size) + 1).min(chunk_size)
                } else {
                    chunk_size
                };
                if chunk.len() < needed {
                    chunk.resize(needed, 0);
                }
            }

            let write_start = (offset as usize).saturating_sub(chunk_start);
            let data_start = chunk_start.saturating_sub(offset as usize);
            let write_end = ((chunk_idx + 1) * chunk_size - chunk_start)
                .min(data.len() - data_start + write_start)
                .min(chunk_size);

            if write_start <= write_end && data_start < data.len() {
                let to_write = write_end - write_start;
                let to_write = to_write.min(data.len() - data_start);
                if chunk.len() < write_start + to_write {
                    chunk.resize(write_start + to_write, 0);
                }
                chunk[write_start..write_start + to_write]
                    .copy_from_slice(&data[data_start..data_start + to_write]);
            }
        }

        // Write chunks back
        let new_size = (offset as usize + data.len()).max(current_size as usize) as i64;

        for (idx, chunk_data) in &chunks {
            conn.execute(
                "INSERT OR REPLACE INTO fs_data (ino, chunk_index, data) VALUES (?1, ?2, ?3)",
                rusqlite::params![self.ino as i64, *idx as i64, chunk_data],
            )
            .map_err(VfsError::Database)?;
        }

        let now = Timestamp::now();
        conn.execute(
            "UPDATE fs_inode SET size = ?2, mtime = ?3, ctime = ?4, mtime_nsec = ?5, ctime_nsec = ?6 WHERE ino = ?1",
            rusqlite::params![
                self.ino as i64,
                new_size,
                now.sec,
                now.sec,
                now.nsec,
                now.nsec,
            ],
        )
        .map_err(VfsError::Database)?;

        // Stamp dirty_since so the pull loop won't overwrite an inode that has
        // in-progress local edits before they are pushed.
        self.db.set_dirty_since(self.ino, Some(now_ms()));

        Ok(data.len() as u32)
    }

    async fn truncate(&self, size: u64) -> VfsResult<()> {
        let conn = self.db.conn.lock();
        let chunk_size = self.db.chunk_size;
        let last_needed_chunk = if size == 0 {
            // Delete all chunks
            conn.execute(
                "DELETE FROM fs_data WHERE ino = ?1",
                [self.ino as i64],
            )
            .map_err(VfsError::Database)?;
            conn.execute(
                "UPDATE fs_inode SET size = 0 WHERE ino = ?1",
                [self.ino as i64],
            )
            .map_err(VfsError::Database)?;
            return Ok(());
        } else {
            (size as usize - 1) / chunk_size
        };

        // Delete chunks beyond last needed
        conn.execute(
            "DELETE FROM fs_data WHERE ino = ?1 AND chunk_index > ?2",
            rusqlite::params![self.ino as i64, last_needed_chunk as i64],
        )
        .map_err(VfsError::Database)?;

        // Trim the last chunk if needed
        let last_chunk_bytes = size as usize % chunk_size;
        if last_chunk_bytes > 0 {
            let chunk: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT data FROM fs_data WHERE ino = ?1 AND chunk_index = ?2",
                    rusqlite::params![self.ino as i64, last_needed_chunk as i64],
                    |r| r.get(0),
                )
                .ok();

            if let Some(mut chunk) = chunk {
                chunk.truncate(last_chunk_bytes);
                conn.execute(
                    "INSERT OR REPLACE INTO fs_data (ino, chunk_index, data) VALUES (?1, ?2, ?3)",
                    rusqlite::params![self.ino as i64, last_needed_chunk as i64, chunk],
                )
                .map_err(VfsError::Database)?;
            }
        }

        conn.execute(
            "UPDATE fs_inode SET size = ?2 WHERE ino = ?1",
            rusqlite::params![self.ino as i64, size as i64],
        )
        .map_err(VfsError::Database)?;

        Ok(())
    }

    async fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn fsync(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn getattr(&self) -> VfsResult<FileAttr> {
        let conn = self.db.conn.lock();
        let attr = conn
            .query_row(
                "SELECT ino, mode, nlink, uid, gid, size, atime, mtime, ctime, rdev, atime_nsec, mtime_nsec, ctime_nsec
                   FROM fs_inode WHERE ino = ?1",
                [self.ino as i64],
                Db::row_to_attr,
            )
            .map_err(VfsError::Database)?;
        Ok(attr)
    }
}
