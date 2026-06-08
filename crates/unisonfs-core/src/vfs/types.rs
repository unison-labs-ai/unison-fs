//! Data types used by the [`FileSystem`](super::FileSystem) trait.

use std::time::{SystemTime, UNIX_EPOCH};

use super::mode::{
    DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, DEFAULT_SYMLINK_MODE, PREFERRED_BLOCK_SIZE, S_IFMT,
};

/// A Unix-style timestamp with nanosecond precision.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Timestamp {
    pub sec: i64,
    pub nsec: u32,
}

impl Timestamp {
    pub const ZERO: Self = Self { sec: 0, nsec: 0 };

    pub fn now() -> Self {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => Self {
                sec: d.as_secs() as i64,
                nsec: d.subsec_nanos(),
            },
            Err(_) => Self::ZERO,
        }
    }

    pub const fn from_secs(sec: i64) -> Self {
        Self { sec, nsec: 0 }
    }
}

/// File type classification.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileType {
    Regular,
    Directory,
    Symlink,
}

/// Complete metadata for an inode — equivalent of POSIX `struct stat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileAttr {
    pub ino: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub blocks: u64,
    pub atime: Timestamp,
    pub mtime: Timestamp,
    pub ctime: Timestamp,
    pub rdev: u32,
    pub blksize: u32,
}

impl FileAttr {
    pub fn new_file(ino: u64, uid: u32, gid: u32) -> Self {
        let now = Timestamp::now();
        Self {
            ino,
            mode: DEFAULT_FILE_MODE,
            nlink: 1,
            uid,
            gid,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            rdev: 0,
            blksize: PREFERRED_BLOCK_SIZE,
        }
    }

    pub fn new_dir(ino: u64, uid: u32, gid: u32) -> Self {
        let now = Timestamp::now();
        Self {
            ino,
            mode: DEFAULT_DIR_MODE,
            nlink: 2,
            uid,
            gid,
            size: 0,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            rdev: 0,
            blksize: PREFERRED_BLOCK_SIZE,
        }
    }

    pub fn new_symlink(ino: u64, target_len: u64, uid: u32, gid: u32) -> Self {
        let now = Timestamp::now();
        Self {
            ino,
            mode: DEFAULT_SYMLINK_MODE,
            nlink: 1,
            uid,
            gid,
            size: target_len,
            blocks: 0,
            atime: now,
            mtime: now,
            ctime: now,
            rdev: 0,
            blksize: PREFERRED_BLOCK_SIZE,
        }
    }

    pub fn new_file_with(ino: u64, mode: u32, uid: u32, gid: u32, ts: Timestamp) -> Self {
        Self {
            ino,
            mode,
            nlink: 1,
            uid,
            gid,
            size: 0,
            blocks: 0,
            atime: ts,
            mtime: ts,
            ctime: ts,
            rdev: 0,
            blksize: PREFERRED_BLOCK_SIZE,
        }
    }

    pub fn new_dir_with(ino: u64, mode: u32, uid: u32, gid: u32, ts: Timestamp) -> Self {
        Self {
            ino,
            mode,
            nlink: 2,
            uid,
            gid,
            size: 0,
            blocks: 0,
            atime: ts,
            mtime: ts,
            ctime: ts,
            rdev: 0,
            blksize: PREFERRED_BLOCK_SIZE,
        }
    }

    pub fn file_type(&self) -> FileType {
        match self.mode & S_IFMT {
            m if m == super::mode::S_IFREG => FileType::Regular,
            m if m == super::mode::S_IFDIR => FileType::Directory,
            m if m == super::mode::S_IFLNK => FileType::Symlink,
            _ => FileType::Regular,
        }
    }

    pub fn is_file(&self) -> bool {
        matches!(self.file_type(), FileType::Regular)
    }

    pub fn is_directory(&self) -> bool {
        matches!(self.file_type(), FileType::Directory)
    }

    pub fn is_symlink(&self) -> bool {
        matches!(self.file_type(), FileType::Symlink)
    }
}

/// A directory entry with its full attributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub attr: FileAttr,
}

/// Filesystem-wide statistics returned by `statfs`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FilesystemStats {
    pub inodes: u64,
    pub bytes_used: u64,
}

/// Request for a timestamp update in [`SetAttr`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TimeOrNow {
    Now,
    Time(Timestamp),
}

/// Partial attribute update passed to `setattr`.
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct SetAttr {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<TimeOrNow>,
    pub mtime: Option<TimeOrNow>,
}
