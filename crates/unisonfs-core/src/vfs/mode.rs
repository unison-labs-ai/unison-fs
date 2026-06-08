//! POSIX mode constants used throughout unisonfs.

/// File type mask (upper 4 bits of the mode word).
pub const S_IFMT: u32 = 0o170000;
/// Regular file.
pub const S_IFREG: u32 = 0o100000;
/// Directory.
pub const S_IFDIR: u32 = 0o040000;
/// Symbolic link.
pub const S_IFLNK: u32 = 0o120000;

/// Default mode for a new regular file: `-rw-r--r--` (0644).
pub const DEFAULT_FILE_MODE: u32 = S_IFREG | 0o644;
/// Default mode for a new directory: `drwxr-xr-x` (0755).
pub const DEFAULT_DIR_MODE: u32 = S_IFDIR | 0o755;
/// Default mode for a new symbolic link: `lrwxrwxrwx` (0777).
pub const DEFAULT_SYMLINK_MODE: u32 = S_IFLNK | 0o777;

/// Maximum length of a single filename component (bytes).
pub const MAX_NAME_LEN: u32 = 255;

/// Preferred I/O block size returned in `FileAttr::blksize`.
///
/// 4 KiB matches most modern Linux kernels' page size and is a reasonable
/// default for virtual filesystems.
pub const PREFERRED_BLOCK_SIZE: u32 = 4096;
