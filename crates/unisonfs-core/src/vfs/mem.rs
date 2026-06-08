//! In-memory reference implementation of the [`FileSystem`] trait.
//!
//! `MemFs` is used in tests and as a fallback when the SQLite cache is unavailable.
//! It stores everything in a `parking_lot::Mutex`-protected `HashMap` behind an `Arc`
//! so that open file handles can share the state safely without any `unsafe` code.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::error::{VfsError, VfsResult};
use super::mode::{DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, MAX_NAME_LEN};
use super::traits::{BoxedFile, File, FileSystem};
use super::types::{DirEntry, FileAttr, FilesystemStats, SetAttr, TimeOrNow, Timestamp};

// ── internal allocator ──────────────────────────────────────────────────────

struct Allocator(u64);

impl Allocator {
    fn next(&mut self) -> u64 {
        let n = self.0;
        self.0 += 1;
        n
    }
}

// ── inode ───────────────────────────────────────────────────────────────────

struct Inode {
    attr: FileAttr,
    children: Option<HashMap<String, u64>>,
    data: Option<Vec<u8>>,
    target: Option<String>,
}

impl Inode {
    fn new_dir(ino: u64, uid: u32, gid: u32) -> Self {
        Self {
            attr: FileAttr::new_dir(ino, uid, gid),
            children: Some(HashMap::new()),
            data: None,
            target: None,
        }
    }

    fn new_file(ino: u64, uid: u32, gid: u32) -> Self {
        Self {
            attr: FileAttr::new_file(ino, uid, gid),
            children: None,
            data: Some(Vec::new()),
            target: None,
        }
    }

    fn new_symlink(ino: u64, target: &str, uid: u32, gid: u32) -> Self {
        Self {
            attr: FileAttr::new_symlink(ino, target.len() as u64, uid, gid),
            children: None,
            data: None,
            target: Some(target.to_string()),
        }
    }
}

// ── shared state ─────────────────────────────────────────────────────────────

struct State {
    inodes: HashMap<u64, Inode>,
    alloc: Allocator,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("inode_count", &self.inodes.len())
            .finish()
    }
}

impl State {
    fn new() -> Self {
        let root = Inode::new_dir(1, 0, 0);
        let mut inodes = HashMap::new();
        inodes.insert(1, root);
        Self {
            inodes,
            alloc: Allocator(2),
        }
    }

    fn next_ino(&mut self) -> u64 {
        self.alloc.next()
    }
}

// ── MemFs ─────────────────────────────────────────────────────────────────────

/// In-memory filesystem. Thread-safe; file handles hold an `Arc` to the shared state.
#[derive(Debug, Clone)]
pub struct MemFs {
    inner: Arc<Mutex<State>>,
}

impl MemFs {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State::new())),
        }
    }
}

impl Default for MemFs {
    fn default() -> Self {
        Self::new()
    }
}

// ── FileSystem impl ──────────────────────────────────────────────────────────

#[async_trait]
impl FileSystem for MemFs {
    async fn lookup(&self, parent_ino: u64, name: &str) -> VfsResult<Option<FileAttr>> {
        let state = self.inner.lock();
        let parent = state.inodes.get(&parent_ino);
        let Some(parent) = parent else {
            return Ok(None);
        };
        let Some(children) = &parent.children else {
            return Ok(None);
        };
        let Some(&child_ino) = children.get(name) else {
            return Ok(None);
        };
        Ok(state.inodes.get(&child_ino).map(|n| n.attr.clone()))
    }

    async fn getattr(&self, ino: u64) -> VfsResult<Option<FileAttr>> {
        let state = self.inner.lock();
        Ok(state.inodes.get(&ino).map(|n| n.attr.clone()))
    }

    async fn setattr(&self, ino: u64, attr: SetAttr) -> VfsResult<FileAttr> {
        let mut state = self.inner.lock();
        let inode = state.inodes.get_mut(&ino).ok_or(VfsError::NotFound)?;
        let now = Timestamp::now();
        if let Some(mode) = attr.mode {
            let old_type_bits = inode.attr.mode & super::mode::S_IFMT;
            inode.attr.mode = old_type_bits | (mode & !super::mode::S_IFMT);
        }
        if let Some(uid) = attr.uid {
            inode.attr.uid = uid;
        }
        if let Some(gid) = attr.gid {
            inode.attr.gid = gid;
        }
        if let Some(size) = attr.size {
            if let Some(data) = &mut inode.data {
                data.resize(size as usize, 0);
            }
            inode.attr.size = size;
            inode.attr.blocks = size.div_ceil(512);
        }
        if let Some(atime) = attr.atime {
            inode.attr.atime = match atime {
                TimeOrNow::Now => now,
                TimeOrNow::Time(t) => t,
            };
        }
        if let Some(mtime) = attr.mtime {
            inode.attr.mtime = match mtime {
                TimeOrNow::Now => now,
                TimeOrNow::Time(t) => t,
            };
        }
        inode.attr.ctime = now;
        Ok(inode.attr.clone())
    }

    async fn readdir(&self, ino: u64) -> VfsResult<Option<Vec<String>>> {
        let state = self.inner.lock();
        let inode = state.inodes.get(&ino);
        let Some(inode) = inode else {
            return Ok(None);
        };
        let Some(children) = &inode.children else {
            return Ok(None);
        };
        let mut names: Vec<String> = children.keys().cloned().collect();
        names.sort();
        Ok(Some(names))
    }

    async fn readdir_plus(&self, ino: u64) -> VfsResult<Option<Vec<DirEntry>>> {
        let state = self.inner.lock();
        let inode = state.inodes.get(&ino);
        let Some(inode) = inode else {
            return Ok(None);
        };
        let Some(children) = &inode.children else {
            return Ok(None);
        };
        let mut entries: Vec<DirEntry> = children
            .iter()
            .filter_map(|(name, &child_ino)| {
                state.inodes.get(&child_ino).map(|n| DirEntry {
                    name: name.clone(),
                    attr: n.attr.clone(),
                })
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
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
        let mut state = self.inner.lock();
        {
            let parent = state.inodes.get(&parent_ino).ok_or(VfsError::NotFound)?;
            if !parent.attr.is_directory() {
                return Err(VfsError::NotDirectory);
            }
            let children = parent.children.as_ref().unwrap();
            if children.contains_key(name) {
                return Err(VfsError::AlreadyExists);
            }
        }
        let new_ino = state.next_ino();
        let mut new_dir = Inode::new_dir(new_ino, uid, gid);
        new_dir.attr.mode = DEFAULT_DIR_MODE;
        let attr = new_dir.attr.clone();
        state.inodes.insert(new_ino, new_dir);
        let parent = state.inodes.get_mut(&parent_ino).unwrap();
        parent.children.as_mut().unwrap().insert(name.to_string(), new_ino);
        parent.attr.nlink += 1;
        Ok(attr)
    }

    async fn rmdir(&self, parent_ino: u64, name: &str) -> VfsResult<()> {
        let mut state = self.inner.lock();
        let child_ino = {
            let parent = state.inodes.get(&parent_ino).ok_or(VfsError::NotFound)?;
            let children = parent.children.as_ref().ok_or(VfsError::NotDirectory)?;
            *children.get(name).ok_or(VfsError::NotFound)?
        };
        {
            let child = state.inodes.get(&child_ino).ok_or(VfsError::NotFound)?;
            if !child.attr.is_directory() {
                return Err(VfsError::NotDirectory);
            }
            if let Some(ch) = &child.children {
                if !ch.is_empty() {
                    return Err(VfsError::NotEmpty);
                }
            }
        }
        state.inodes.remove(&child_ino);
        let parent = state.inodes.get_mut(&parent_ino).unwrap();
        parent.children.as_mut().unwrap().remove(name);
        parent.attr.nlink = parent.attr.nlink.saturating_sub(1);
        Ok(())
    }

    async fn open(&self, ino: u64, _flags: i32) -> VfsResult<BoxedFile> {
        let state = self.inner.lock();
        let inode = state.inodes.get(&ino).ok_or(VfsError::NotFound)?;
        if inode.attr.is_directory() {
            return Err(VfsError::IsDirectory);
        }
        drop(state);
        Ok(Arc::new(MemFileHandle {
            inner: Arc::clone(&self.inner),
            ino,
        }))
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

        // Check if file already exists — if so, open it
        {
            let state = self.inner.lock();
            let parent = state.inodes.get(&parent_ino).ok_or(VfsError::NotFound)?;
            if !parent.attr.is_directory() {
                return Err(VfsError::NotDirectory);
            }
            if let Some(&existing_ino) = parent.children.as_ref().unwrap().get(name) {
                let attr = state.inodes.get(&existing_ino).unwrap().attr.clone();
                let ino = existing_ino;
                drop(state);
                let handle = Arc::new(MemFileHandle {
                    inner: Arc::clone(&self.inner),
                    ino,
                });
                return Ok((attr, handle));
            }
        }

        // Create new file
        let new_ino = {
            let mut state = self.inner.lock();
            let new_ino = state.next_ino();
            let mut new_file = Inode::new_file(new_ino, uid, gid);
            new_file.attr.mode = DEFAULT_FILE_MODE;
            state.inodes.insert(new_ino, new_file);
            let parent = state.inodes.get_mut(&parent_ino).unwrap();
            parent.children.as_mut().unwrap().insert(name.to_string(), new_ino);
            new_ino
        };

        let attr = self.inner.lock().inodes.get(&new_ino).unwrap().attr.clone();
        let handle = Arc::new(MemFileHandle {
            inner: Arc::clone(&self.inner),
            ino: new_ino,
        });
        Ok((attr, handle))
    }

    async fn unlink(&self, parent_ino: u64, name: &str) -> VfsResult<()> {
        let mut state = self.inner.lock();
        let child_ino = {
            let parent = state.inodes.get(&parent_ino).ok_or(VfsError::NotFound)?;
            let children = parent.children.as_ref().ok_or(VfsError::NotDirectory)?;
            *children.get(name).ok_or(VfsError::NotFound)?
        };
        {
            let child = state.inodes.get(&child_ino).ok_or(VfsError::NotFound)?;
            if child.attr.is_directory() {
                return Err(VfsError::IsDirectory);
            }
        }
        let parent = state.inodes.get_mut(&parent_ino).unwrap();
        parent.children.as_mut().unwrap().remove(name);
        let inode = state.inodes.get_mut(&child_ino).unwrap();
        inode.attr.nlink = inode.attr.nlink.saturating_sub(1);
        if inode.attr.nlink == 0 {
            state.inodes.remove(&child_ino);
        }
        Ok(())
    }

    async fn readlink(&self, ino: u64) -> VfsResult<Option<String>> {
        let state = self.inner.lock();
        let inode = state.inodes.get(&ino);
        let Some(inode) = inode else {
            return Ok(None);
        };
        Ok(inode.target.clone())
    }

    async fn symlink(
        &self,
        parent_ino: u64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> VfsResult<FileAttr> {
        let mut state = self.inner.lock();
        {
            let parent = state.inodes.get(&parent_ino).ok_or(VfsError::NotFound)?;
            if !parent.attr.is_directory() {
                return Err(VfsError::NotDirectory);
            }
            if parent.children.as_ref().unwrap().contains_key(name) {
                return Err(VfsError::AlreadyExists);
            }
        }
        let new_ino = state.next_ino();
        let link = Inode::new_symlink(new_ino, target, uid, gid);
        let attr = link.attr.clone();
        state.inodes.insert(new_ino, link);
        let parent = state.inodes.get_mut(&parent_ino).unwrap();
        parent.children.as_mut().unwrap().insert(name.to_string(), new_ino);
        Ok(attr)
    }

    async fn link(&self, ino: u64, new_parent_ino: u64, new_name: &str) -> VfsResult<FileAttr> {
        let mut state = self.inner.lock();
        {
            let inode = state.inodes.get(&ino).ok_or(VfsError::NotFound)?;
            if inode.attr.is_directory() {
                return Err(VfsError::IsDirectory);
            }
        }
        {
            let new_parent = state.inodes.get(&new_parent_ino).ok_or(VfsError::NotFound)?;
            if !new_parent.attr.is_directory() {
                return Err(VfsError::NotDirectory);
            }
            if new_parent.children.as_ref().unwrap().contains_key(new_name) {
                return Err(VfsError::AlreadyExists);
            }
        }
        let inode = state.inodes.get_mut(&ino).unwrap();
        inode.attr.nlink += 1;
        let attr = inode.attr.clone();
        let new_parent = state.inodes.get_mut(&new_parent_ino).unwrap();
        new_parent.children.as_mut().unwrap().insert(new_name.to_string(), ino);
        Ok(attr)
    }

    async fn rename(
        &self,
        old_parent_ino: u64,
        old_name: &str,
        new_parent_ino: u64,
        new_name: &str,
    ) -> VfsResult<()> {
        let mut state = self.inner.lock();
        let child_ino = {
            let parent = state.inodes.get(&old_parent_ino).ok_or(VfsError::NotFound)?;
            *parent
                .children
                .as_ref()
                .ok_or(VfsError::NotDirectory)?
                .get(old_name)
                .ok_or(VfsError::NotFound)?
        };
        state
            .inodes
            .get_mut(&old_parent_ino)
            .unwrap()
            .children
            .as_mut()
            .unwrap()
            .remove(old_name);
        let maybe_old_dst = state
            .inodes
            .get(&new_parent_ino)
            .and_then(|p| p.children.as_ref())
            .and_then(|c| c.get(new_name))
            .copied();
        if let Some(old_dst) = maybe_old_dst {
            let dst = state.inodes.get_mut(&old_dst).unwrap();
            dst.attr.nlink = dst.attr.nlink.saturating_sub(1);
            if dst.attr.nlink == 0 {
                state.inodes.remove(&old_dst);
            }
        }
        state
            .inodes
            .get_mut(&new_parent_ino)
            .ok_or(VfsError::NotFound)?
            .children
            .as_mut()
            .ok_or(VfsError::NotDirectory)?
            .insert(new_name.to_string(), child_ino);
        Ok(())
    }

    async fn statfs(&self) -> VfsResult<FilesystemStats> {
        let state = self.inner.lock();
        let inodes = state.inodes.len() as u64;
        let bytes_used: u64 = state
            .inodes
            .values()
            .filter_map(|n| n.data.as_ref())
            .map(|d| d.len() as u64)
            .sum();
        Ok(FilesystemStats { inodes, bytes_used })
    }
}

// ── file handle ───────────────────────────────────────────────────────────────

#[derive(Debug)]
struct MemFileHandle {
    inner: Arc<Mutex<State>>,
    ino: u64,
}

#[async_trait]
impl File for MemFileHandle {
    async fn read(&self, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        let state = self.inner.lock();
        let inode = state.inodes.get(&self.ino).ok_or(VfsError::NotFound)?;
        let data = inode.data.as_ref().ok_or(VfsError::InvalidArgument("not a file".into()))?;
        let start = (offset as usize).min(data.len());
        let end = (start + size).min(data.len());
        Ok(data[start..end].to_vec())
    }

    async fn write(&self, offset: u64, data: &[u8]) -> VfsResult<u32> {
        let mut state = self.inner.lock();
        let inode = state.inodes.get_mut(&self.ino).ok_or(VfsError::NotFound)?;
        let buf = inode.data.as_mut().ok_or(VfsError::InvalidArgument("not a file".into()))?;
        let end = offset as usize + data.len();
        if end > buf.len() {
            buf.resize(end, 0);
        }
        buf[offset as usize..end].copy_from_slice(data);
        let size = buf.len() as u64;
        inode.attr.size = size;
        inode.attr.blocks = size.div_ceil(512);
        let now = Timestamp::now();
        inode.attr.mtime = now;
        inode.attr.ctime = now;
        Ok(data.len() as u32)
    }

    async fn truncate(&self, size: u64) -> VfsResult<()> {
        let mut state = self.inner.lock();
        let inode = state.inodes.get_mut(&self.ino).ok_or(VfsError::NotFound)?;
        let buf = inode.data.as_mut().ok_or(VfsError::InvalidArgument("not a file".into()))?;
        buf.resize(size as usize, 0);
        inode.attr.size = size;
        inode.attr.blocks = size.div_ceil(512);
        Ok(())
    }

    async fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn fsync(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn getattr(&self) -> VfsResult<FileAttr> {
        let state = self.inner.lock();
        let inode = state.inodes.get(&self.ino).ok_or(VfsError::NotFound)?;
        Ok(inode.attr.clone())
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn root_exists_as_directory() {
        let fs = MemFs::new();
        let attr = fs.getattr(1).await.unwrap();
        assert!(attr.is_some());
        assert!(attr.unwrap().is_directory());
    }

    #[tokio::test]
    async fn create_and_lookup_file() {
        let fs = MemFs::new();
        let (attr, _) = fs.create_file(1, "hello.md", DEFAULT_FILE_MODE, 0, 0).await.unwrap();
        assert!(attr.is_file());
        let found = fs.lookup(1, "hello.md").await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn readdir_returns_created_files() {
        let fs = MemFs::new();
        fs.create_file(1, "a.md", DEFAULT_FILE_MODE, 0, 0).await.unwrap();
        fs.create_file(1, "b.md", DEFAULT_FILE_MODE, 0, 0).await.unwrap();
        let names = fs.readdir(1).await.unwrap().unwrap();
        assert!(names.contains(&"a.md".to_string()));
        assert!(names.contains(&"b.md".to_string()));
    }

    #[tokio::test]
    async fn write_and_read_round_trips() {
        let fs = MemFs::new();
        let (_, handle) = fs.create_file(1, "test.md", DEFAULT_FILE_MODE, 0, 0).await.unwrap();
        let written = handle.write(0, b"hello world").await.unwrap();
        assert_eq!(written, 11);
        let read = handle.read(0, 11).await.unwrap();
        assert_eq!(read, b"hello world");
    }
}
