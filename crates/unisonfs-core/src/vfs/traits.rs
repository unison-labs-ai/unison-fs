//! The [`FileSystem`] and [`File`] traits — the core filesystem abstraction.

use std::sync::Arc;

use async_trait::async_trait;

use super::error::VfsResult;
use super::types::{DirEntry, FileAttr, FilesystemStats, SetAttr};

#[async_trait]
pub trait FileSystem: Send + Sync {
    async fn lookup(&self, parent_ino: u64, name: &str) -> VfsResult<Option<FileAttr>>;
    async fn getattr(&self, ino: u64) -> VfsResult<Option<FileAttr>>;
    async fn setattr(&self, ino: u64, attr: SetAttr) -> VfsResult<FileAttr>;
    async fn readdir(&self, ino: u64) -> VfsResult<Option<Vec<String>>>;
    async fn readdir_plus(&self, ino: u64) -> VfsResult<Option<Vec<DirEntry>>>;
    async fn mkdir(
        &self,
        parent_ino: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileAttr>;
    async fn rmdir(&self, parent_ino: u64, name: &str) -> VfsResult<()>;
    async fn open(&self, ino: u64, flags: i32) -> VfsResult<BoxedFile>;
    async fn create_file(
        &self,
        parent_ino: u64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> VfsResult<(FileAttr, BoxedFile)>;
    async fn unlink(&self, parent_ino: u64, name: &str) -> VfsResult<()>;
    async fn readlink(&self, ino: u64) -> VfsResult<Option<String>>;
    async fn symlink(
        &self,
        parent_ino: u64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileAttr>;
    async fn link(&self, ino: u64, new_parent_ino: u64, new_name: &str) -> VfsResult<FileAttr>;
    async fn rename(
        &self,
        old_parent_ino: u64,
        old_name: &str,
        new_parent_ino: u64,
        new_name: &str,
    ) -> VfsResult<()>;
    async fn statfs(&self) -> VfsResult<FilesystemStats>;
    async fn forget(&self, _ino: u64, _nlookup: u64) {}
}

#[async_trait]
pub trait File: Send + Sync + std::fmt::Debug {
    async fn read(&self, offset: u64, size: usize) -> VfsResult<Vec<u8>>;
    async fn write(&self, offset: u64, data: &[u8]) -> VfsResult<u32>;
    async fn truncate(&self, size: u64) -> VfsResult<()>;
    async fn flush(&self) -> VfsResult<()>;
    async fn fsync(&self) -> VfsResult<()>;
    async fn getattr(&self) -> VfsResult<FileAttr>;
}

/// A shareable, reference-counted file handle.
pub type BoxedFile = Arc<dyn File + Send + Sync>;
