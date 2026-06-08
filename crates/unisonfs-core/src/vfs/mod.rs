//! Virtual filesystem trait and in-memory reference implementation.
//!
//! This is the single source of truth for what a filesystem operation looks
//! like in unisonfs. The [`FileSystem`] trait is implemented by every backend
//! and called by every frontend (FUSE and NFS mount adapters).

pub mod error;
pub mod mem;
pub mod mode;
pub mod path;
pub mod traits;
pub mod types;

pub use error::{VfsError, VfsResult};
pub use mem::MemFs;
pub use mode::{
    DEFAULT_DIR_MODE, DEFAULT_FILE_MODE, DEFAULT_SYMLINK_MODE, MAX_NAME_LEN, PREFERRED_BLOCK_SIZE,
    S_IFDIR, S_IFLNK, S_IFMT, S_IFREG,
};
pub use traits::{BoxedFile, File, FileSystem};
pub use types::{DirEntry, FileAttr, FileType, FilesystemStats, SetAttr, TimeOrNow, Timestamp};
