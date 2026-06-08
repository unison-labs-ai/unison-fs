//! VFS error type.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("entry not found")]
    NotFound,
    #[error("entry already exists")]
    AlreadyExists,
    #[error("not a directory")]
    NotDirectory,
    #[error("is a directory")]
    IsDirectory,
    #[error("directory not empty")]
    NotEmpty,
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("permission denied")]
    PermissionDenied,
    #[error("name too long")]
    NameTooLong,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("other: {0}")]
    Other(#[from] anyhow::Error),
}

impl VfsError {
    /// Convert to a POSIX errno value for FUSE responses.
    pub fn to_errno(&self) -> i32 {
        match self {
            VfsError::NotFound => libc::ENOENT,
            VfsError::AlreadyExists => libc::EEXIST,
            VfsError::NotDirectory => libc::ENOTDIR,
            VfsError::IsDirectory => libc::EISDIR,
            VfsError::NotEmpty => libc::ENOTEMPTY,
            VfsError::InvalidArgument(_) => libc::EINVAL,
            VfsError::PermissionDenied => libc::EACCES,
            VfsError::NameTooLong => libc::ENAMETOOLONG,
            VfsError::Io(e) => e.raw_os_error().unwrap_or(libc::EIO),
            VfsError::Database(_) => libc::EIO,
            VfsError::Other(_) => libc::EIO,
        }
    }
}

pub type VfsResult<T> = Result<T, VfsError>;
