//! FUSE mount adapter (Linux only).
//!
//! Bridges the `FileSystem` trait to the `fuser` crate's FUSE kernel interface.

#[cfg(target_os = "linux")]
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use fuser::{
    FileAttr as FuserAttr, FileType as FuserFileType, Filesystem, KernelConfig, MountOption,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request as FuserRequest,
};

#[cfg(target_os = "linux")]
use crate::vfs::{traits::FileSystem, FileType, Timestamp};

#[cfg(target_os = "linux")]
const TTL: Duration = Duration::from_secs(1);

#[cfg(target_os = "linux")]
fn to_fuser_type(ft: FileType) -> FuserFileType {
    match ft {
        FileType::Regular => FuserFileType::RegularFile,
        FileType::Directory => FuserFileType::Directory,
        FileType::Symlink => FuserFileType::Symlink,
    }
}

#[cfg(target_os = "linux")]
fn to_fuser_attr(attr: &crate::vfs::FileAttr) -> FuserAttr {
    FuserAttr {
        ino: attr.ino,
        size: attr.size,
        blocks: attr.blocks,
        atime: std::time::UNIX_EPOCH
            + Duration::new(attr.atime.sec as u64, attr.atime.nsec),
        mtime: std::time::UNIX_EPOCH
            + Duration::new(attr.mtime.sec as u64, attr.mtime.nsec),
        ctime: std::time::UNIX_EPOCH
            + Duration::new(attr.ctime.sec as u64, attr.ctime.nsec),
        crtime: std::time::UNIX_EPOCH,
        kind: to_fuser_type(attr.file_type()),
        perm: (attr.mode & 0o7777) as u16,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        rdev: attr.rdev,
        blksize: attr.blksize,
        flags: 0,
    }
}

/// FUSE adapter wrapping an `Arc<dyn FileSystem>`.
#[cfg(target_os = "linux")]
pub struct FuseAdapter {
    fs: Arc<dyn FileSystem>,
    rt: tokio::runtime::Handle,
    /// Open file table: fh → BoxedFile
    open_files: parking_lot::Mutex<std::collections::HashMap<u64, crate::vfs::BoxedFile>>,
    next_fh: std::sync::atomic::AtomicU64,
}

#[cfg(target_os = "linux")]
impl FuseAdapter {
    pub fn new(fs: Arc<dyn FileSystem>, rt: tokio::runtime::Handle) -> Self {
        Self {
            fs,
            rt,
            open_files: parking_lot::Mutex::new(std::collections::HashMap::new()),
            next_fh: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn alloc_fh(&self, file: crate::vfs::BoxedFile) -> u64 {
        let fh = self
            .next_fh
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.open_files.lock().insert(fh, file);
        fh
    }

    fn get_file(&self, fh: u64) -> Option<crate::vfs::BoxedFile> {
        self.open_files.lock().get(&fh).cloned()
    }

    fn release_fh(&self, fh: u64) {
        self.open_files.lock().remove(&fh);
    }
}

#[cfg(target_os = "linux")]
impl Filesystem for FuseAdapter {
    fn lookup(&mut self, _req: &FuserRequest, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_string_lossy().to_string();
        let fs = self.fs.clone();
        match self.rt.block_on(fs.lookup(parent, &name)) {
            Ok(Some(attr)) => reply.entry(&TTL, &to_fuser_attr(&attr), 0),
            Ok(None) => reply.error(libc::ENOENT),
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn getattr(&mut self, _req: &FuserRequest, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.rt.block_on(self.fs.getattr(ino)) {
            Ok(Some(attr)) => reply.attr(&TTL, &to_fuser_attr(&attr)),
            Ok(None) => reply.error(libc::ENOENT),
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn readdir(
        &mut self,
        _req: &FuserRequest,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        match self.rt.block_on(self.fs.readdir_plus(ino)) {
            Ok(Some(entries)) => {
                // offset 0 = start; . and .. are synthetic
                if offset == 0 && reply.add(ino, 1, FuserFileType::Directory, ".") {
                    reply.ok();
                    return;
                }
                let base = if offset <= 1 { 0 } else { offset as usize - 2 };
                if offset <= 1 {
                    let parent_ino = if ino == 1 { 1 } else { ino }; // parent unknown, use ino
                    if offset == 0 {
                        let _ = reply.add(ino, 1, FuserFileType::Directory, ".");
                    }
                    let _ = reply.add(parent_ino, 2, FuserFileType::Directory, "..");
                }
                for (i, entry) in entries.iter().enumerate().skip(base) {
                    let fuse_type = to_fuser_type(entry.attr.file_type());
                    let done = reply.add(
                        entry.attr.ino,
                        (i + 3) as i64,
                        fuse_type,
                        &entry.name,
                    );
                    if done {
                        break;
                    }
                }
                reply.ok();
            }
            Ok(None) => reply.error(libc::ENOTDIR),
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn open(&mut self, _req: &FuserRequest, ino: u64, flags: i32, reply: ReplyOpen) {
        match self.rt.block_on(self.fs.open(ino, flags)) {
            Ok(file) => {
                let fh = self.alloc_fh(file);
                reply.opened(fh, 0);
            }
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn read(
        &mut self,
        _req: &FuserRequest,
        _ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        if let Some(file) = self.get_file(fh) {
            match self.rt.block_on(file.read(offset as u64, size as usize)) {
                Ok(data) => reply.data(&data),
                Err(e) => reply.error(e.to_errno()),
            }
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn write(
        &mut self,
        _req: &FuserRequest,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if let Some(file) = self.get_file(fh) {
            match self.rt.block_on(file.write(offset as u64, data)) {
                Ok(written) => reply.written(written),
                Err(e) => reply.error(e.to_errno()),
            }
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn release(
        &mut self,
        _req: &FuserRequest,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if let Some(file) = self.get_file(fh) {
            let _ = self.rt.block_on(file.flush());
        }
        self.release_fh(fh);
        reply.ok();
    }

    fn flush(&mut self, _req: &FuserRequest, _ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        if let Some(file) = self.get_file(fh) {
            match self.rt.block_on(file.flush()) {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(e.to_errno()),
            }
        } else {
            reply.ok();
        }
    }

    fn fsync(&mut self, _req: &FuserRequest, _ino: u64, fh: u64, _datasync: bool, reply: ReplyEmpty) {
        if let Some(file) = self.get_file(fh) {
            match self.rt.block_on(file.fsync()) {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(e.to_errno()),
            }
        } else {
            reply.ok();
        }
    }

    fn mkdir(
        &mut self,
        req: &FuserRequest,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name = name.to_string_lossy().to_string();
        match self
            .rt
            .block_on(self.fs.mkdir(parent, &name, mode, req.uid(), req.gid()))
        {
            Ok(attr) => reply.entry(&TTL, &to_fuser_attr(&attr), 0),
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn rmdir(&mut self, _req: &FuserRequest, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_string_lossy().to_string();
        match self.rt.block_on(self.fs.rmdir(parent, &name)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn create(
        &mut self,
        req: &FuserRequest,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let name = name.to_string_lossy().to_string();
        match self
            .rt
            .block_on(self.fs.create_file(parent, &name, mode, req.uid(), req.gid()))
        {
            Ok((attr, file)) => {
                let fh = self.alloc_fh(file);
                reply.created(&TTL, &to_fuser_attr(&attr), 0, fh, flags as u32);
            }
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn unlink(&mut self, _req: &FuserRequest, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_string_lossy().to_string();
        match self.rt.block_on(self.fs.unlink(parent, &name)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn rename(
        &mut self,
        _req: &FuserRequest,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let name = name.to_string_lossy().to_string();
        let new_name = new_name.to_string_lossy().to_string();
        match self
            .rt
            .block_on(self.fs.rename(parent, &name, new_parent, &new_name))
        {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn statfs(&mut self, _req: &FuserRequest, _ino: u64, reply: ReplyStatfs) {
        match self.rt.block_on(self.fs.statfs()) {
            Ok(stats) => {
                reply.statfs(
                    u64::MAX / 512,          // total blocks
                    u64::MAX / 512,          // free blocks
                    u64::MAX / 512,          // available blocks
                    stats.inodes,            // total inodes
                    u64::MAX - stats.inodes, // free inodes
                    512,                     // block size
                    255,                     // max name len
                    0,                       // fragment size
                );
            }
            Err(e) => reply.error(e.to_errno()),
        }
    }

    fn setattr(
        &mut self,
        _req: &FuserRequest,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        use crate::vfs::{SetAttr, TimeOrNow};

        let to_ts = |t: fuser::TimeOrNow| match t {
            fuser::TimeOrNow::SpecificTime(st) => {
                let d = st.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                TimeOrNow::Time(Timestamp {
                    sec: d.as_secs() as i64,
                    nsec: d.subsec_nanos(),
                })
            }
            fuser::TimeOrNow::Now => TimeOrNow::Now,
        };

        let sa = SetAttr {
            mode,
            uid,
            gid,
            size,
            atime: atime.map(to_ts),
            mtime: mtime.map(to_ts),
        };
        match self.rt.block_on(self.fs.setattr(ino, sa)) {
            Ok(attr) => reply.attr(&TTL, &to_fuser_attr(&attr)),
            Err(e) => reply.error(e.to_errno()),
        }
    }
}

/// Mount the filesystem at `mount_path` using FUSE.
///
/// Blocks until unmount. Call from a background thread.
#[cfg(target_os = "linux")]
pub fn mount(
    fs: Arc<dyn FileSystem>,
    mount_path: &Path,
    rt: tokio::runtime::Handle,
) -> anyhow::Result<()> {
    let options = vec![
        MountOption::FSName("unisonfs".to_string()),
        MountOption::AutoUnmount,
        MountOption::AllowRoot,
        MountOption::DefaultPermissions,
    ];
    let adapter = FuseAdapter::new(fs, rt);
    fuser::mount2(adapter, mount_path, &options)?;
    Ok(())
}

/// Stub for non-Linux targets.
#[cfg(not(target_os = "linux"))]
pub fn mount(
    _fs: std::sync::Arc<dyn crate::vfs::FileSystem>,
    _mount_path: &std::path::Path,
    _rt: tokio::runtime::Handle,
) -> anyhow::Result<()> {
    anyhow::bail!("FUSE backend is only supported on Linux; use --backend nfs on macOS")
}
