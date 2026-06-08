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
    BsdFileFlags, Config, Errno, FileAttr as FuserAttr, FileHandle, FileType as FuserFileType,
    Filesystem, FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenFlags, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request, SessionACL, WriteFlags,
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
        ino: INodeNo(attr.ino),
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

#[cfg(target_os = "linux")]
fn to_errno(e: &crate::vfs::error::VfsError) -> Errno {
    Errno::from_i32(e.to_errno())
}

/// FUSE adapter wrapping an `Arc<dyn FileSystem>`.
#[cfg(target_os = "linux")]
pub struct FuseAdapter {
    fs: Arc<dyn FileSystem + 'static>,
    rt: tokio::runtime::Handle,
    /// Open file table: fh → BoxedFile
    open_files: parking_lot::Mutex<std::collections::HashMap<u64, crate::vfs::BoxedFile>>,
    next_fh: std::sync::atomic::AtomicU64,
}

#[cfg(target_os = "linux")]
impl FuseAdapter {
    pub fn new(fs: Arc<dyn FileSystem + 'static>, rt: tokio::runtime::Handle) -> Self {
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
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_string_lossy().to_string();
        let fs = self.fs.clone();
        match self.rt.block_on(fs.lookup(parent.0, &name)) {
            Ok(Some(attr)) => reply.entry(&TTL, &to_fuser_attr(&attr), Generation(0)),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.rt.block_on(self.fs.getattr(ino.0)) {
            Ok(Some(attr)) => reply.attr(&TTL, &to_fuser_attr(&attr)),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        match self.rt.block_on(self.fs.readdir_plus(ino.0)) {
            Ok(Some(entries)) => {
                // offset 0 = start; . and .. are synthetic
                if offset == 0 && reply.add(ino, 1, FuserFileType::Directory, ".") {
                    reply.ok();
                    return;
                }
                let base = if offset <= 1 { 0 } else { offset as usize - 2 };
                if offset <= 1 {
                    let parent_ino = ino; // parent unknown, use ino
                    if offset == 0 {
                        let _ = reply.add(ino, 1, FuserFileType::Directory, ".");
                    }
                    let _ = reply.add(parent_ino, 2, FuserFileType::Directory, "..");
                }
                for (i, entry) in entries.iter().enumerate().skip(base) {
                    let fuse_type = to_fuser_type(entry.attr.file_type());
                    let done = reply.add(
                        INodeNo(entry.attr.ino),
                        (i + 3) as u64,
                        fuse_type,
                        &entry.name,
                    );
                    if done {
                        break;
                    }
                }
                reply.ok();
            }
            Ok(None) => reply.error(Errno::ENOTDIR),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.rt.block_on(self.fs.open(ino.0, flags.0)) {
            Ok(file) => {
                let fh = self.alloc_fh(file);
                reply.opened(FileHandle(fh), FopenFlags::empty());
            }
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        if let Some(file) = self.get_file(fh.0) {
            match self.rt.block_on(file.read(offset, size as usize)) {
                Ok(data) => reply.data(&data),
                Err(e) => reply.error(to_errno(&e)),
            }
        } else {
            reply.error(Errno::EBADF);
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        if let Some(file) = self.get_file(fh.0) {
            match self.rt.block_on(file.write(offset, data)) {
                Ok(written) => reply.written(written),
                Err(e) => reply.error(to_errno(&e)),
            }
        } else {
            reply.error(Errno::EBADF);
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        if let Some(file) = self.get_file(fh.0) {
            let _ = self.rt.block_on(file.flush());
        }
        self.release_fh(fh.0);
        reply.ok();
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        if let Some(file) = self.get_file(fh.0) {
            match self.rt.block_on(file.flush()) {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(to_errno(&e)),
            }
        } else {
            reply.ok();
        }
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        if let Some(file) = self.get_file(fh.0) {
            match self.rt.block_on(file.fsync()) {
                Ok(()) => reply.ok(),
                Err(e) => reply.error(to_errno(&e)),
            }
        } else {
            reply.ok();
        }
    }

    fn mkdir(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name = name.to_string_lossy().to_string();
        match self
            .rt
            .block_on(self.fs.mkdir(parent.0, &name, mode, req.uid(), req.gid()))
        {
            Ok(attr) => reply.entry(&TTL, &to_fuser_attr(&attr), Generation(0)),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_string_lossy().to_string();
        match self.rt.block_on(self.fs.rmdir(parent.0, &name)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn create(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let name = name.to_string_lossy().to_string();
        match self
            .rt
            .block_on(self.fs.create_file(parent.0, &name, mode, req.uid(), req.gid()))
        {
            Ok((attr, file)) => {
                let fh = self.alloc_fh(file);
                reply.created(
                    &TTL,
                    &to_fuser_attr(&attr),
                    Generation(0),
                    FileHandle(fh),
                    FopenFlags::from_bits_retain(flags as u32),
                );
            }
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_string_lossy().to_string();
        match self.rt.block_on(self.fs.unlink(parent.0, &name)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        new_parent: INodeNo,
        new_name: &OsStr,
        _flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let name = name.to_string_lossy().to_string();
        let new_name = new_name.to_string_lossy().to_string();
        match self
            .rt
            .block_on(self.fs.rename(parent.0, &name, new_parent.0, &new_name))
        {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
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
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<BsdFileFlags>,
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
        match self.rt.block_on(self.fs.setattr(ino.0, sa)) {
            Ok(attr) => reply.attr(&TTL, &to_fuser_attr(&attr)),
            Err(e) => reply.error(to_errno(&e)),
        }
    }
}

/// Mount the filesystem at `mount_path` using FUSE.
///
/// Blocks until unmount. Call from a background thread.
#[cfg(target_os = "linux")]
pub fn mount(
    fs: Arc<dyn FileSystem + 'static>,
    mount_path: &Path,
    rt: tokio::runtime::Handle,
) -> anyhow::Result<()> {
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("unisonfs".to_string()),
        MountOption::AutoUnmount,
        MountOption::DefaultPermissions,
    ];
    config.acl = SessionACL::RootAndOwner;
    let adapter = FuseAdapter::new(fs, rt);
    fuser::mount2(adapter, mount_path, &config)?;
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
