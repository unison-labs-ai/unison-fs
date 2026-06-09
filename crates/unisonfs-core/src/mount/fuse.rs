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
    Filesystem, FopenFlags, Generation, INodeNo, InitFlags, KernelConfig, LockOwner,
    MountOption, OpenFlags, RenameFlags, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, SessionACL, WriteFlags,
};

#[cfg(target_os = "linux")]
use crate::vfs::{traits::FileSystem, FileType, Timestamp};

/// Attribute cache TTL.
///
/// We use a long TTL because the daemon is the only writer — there is no
/// outside process that can change the namespace without going through us, so
/// cached dentries and attributes never go stale on their own.
#[cfg(target_os = "linux")]
const TTL: Duration = Duration::from_secs(60 * 60 * 24 * 365);

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
///
/// Each handler callback runs on the thread that `fuser` uses to dispatch FUSE
/// requests.  That thread is itself a worker of the *outer* multi-threaded
/// tokio runtime, so calling `Handle::block_on` from inside a callback would
/// exhaust the worker pool and deadlock (all workers park waiting for each
/// other).
///
/// The fix: own a *private* `current_thread` tokio runtime.  Its event loop
/// runs entirely on the calling thread (the fuser dispatch thread) and never
/// competes with the outer pool, so `block_on` is always safe.
#[cfg(target_os = "linux")]
pub struct FuseAdapter {
    fs: Arc<dyn FileSystem + 'static>,
    /// Dedicated single-threaded runtime used exclusively for bridging the
    /// synchronous fuser callbacks to async VFS operations.  Must NOT be the
    /// shared multi-threaded runtime — see struct-level doc comment.
    rt: tokio::runtime::Runtime,
    /// Open file table: fh → BoxedFile
    open_files: parking_lot::Mutex<std::collections::HashMap<u64, crate::vfs::BoxedFile>>,
    next_fh: std::sync::atomic::AtomicU64,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for FuseAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuseAdapter")
            .field("open_files_count", &self.open_files.lock().len())
            .field("next_fh", &self.next_fh.load(std::sync::atomic::Ordering::Relaxed))
            .finish()
    }
}

#[cfg(target_os = "linux")]
impl FuseAdapter {
    pub fn new(fs: Arc<dyn FileSystem + 'static>) -> Self {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build FuseAdapter current_thread runtime");
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
    // ── Lifecycle ─────────────────────────────────────────────────────────

    fn init(&mut self, _req: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        // Enable writeback caching and async reads. FUSE_NO_OPENDIR_SUPPORT
        // tells the kernel we don't track directory handles (opendir/releasedir
        // are no-ops), which reduces round-trips for ls/readdir.
        let _ = config.add_capabilities(
            InitFlags::FUSE_ASYNC_READ
                | InitFlags::FUSE_WRITEBACK_CACHE
                | InitFlags::FUSE_PARALLEL_DIROPS
                | InitFlags::FUSE_CACHE_SYMLINKS
                | InitFlags::FUSE_NO_OPENDIR_SUPPORT,
        );
        Ok(())
    }

    fn destroy(&mut self) {
        self.open_files.lock().clear();
    }

    // ── Name resolution + metadata ────────────────────────────────────────

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name = name.to_string_lossy().to_string();
        let fs = self.fs.clone();
        match self.rt.block_on(fs.lookup(parent.0, &name)) {
            Ok(Some(attr)) => reply.entry(&TTL, &to_fuser_attr(&attr), Generation(0)),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn forget(&self, _req: &Request, _ino: INodeNo, _nlookup: u64) {
        // No-op: we don't reference-count kernel inode lookups.
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.rt.block_on(self.fs.getattr(ino.0)) {
            Ok(Some(attr)) => reply.attr(&TTL, &to_fuser_attr(&attr)),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    #[allow(clippy::too_many_arguments)]
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

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let fs = self.fs.clone();
        match self.rt.block_on(async move { fs.readlink(ino.0).await }) {
            Ok(Some(target)) => reply.data(target.as_bytes()),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    // ── Directory operations ──────────────────────────────────────────────

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

    fn opendir(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        // FUSE_NO_OPENDIR_SUPPORT is requested in init(); some older kernels
        // still call opendir. Return fh=0 with no flags — we don't track dir handles.
        reply.opened(FileHandle(0), FopenFlags::empty());
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let fs = self.fs.clone();
        let entries = match self.rt.block_on(async move { fs.readdir_plus(ino.0).await }) {
            Ok(Some(e)) => e,
            Ok(None) => {
                reply.error(Errno::ENOTDIR);
                return;
            }
            Err(e) => {
                reply.error(to_errno(&e));
                return;
            }
        };

        // FUSE readdir is offset-based. `offset` is the cursor the kernel
        // wants us to resume from; we return entries starting at that position.
        // `reply.add` returns `true` when the reply buffer is full.
        for (i, entry) in entries.iter().enumerate().skip(offset as usize) {
            let next_offset = (i + 1) as u64;
            let full = reply.add(
                INodeNo(entry.attr.ino),
                next_offset,
                to_fuser_type(entry.attr.file_type()),
                &entry.name,
            );
            if full {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    // ── File operations (handle-based) ────────────────────────────────────

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.rt.block_on(self.fs.open(ino.0, flags.0)) {
            Ok(file) => {
                let fh = self.alloc_fh(file);
                reply.opened(FileHandle(fh), FopenFlags::empty());
            }
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    #[allow(clippy::too_many_arguments)]
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

    #[allow(clippy::too_many_arguments)]
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

    #[allow(clippy::too_many_arguments)]
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

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        let Some(file) = self.get_file(fh.0) else {
            reply.ok();
            return;
        };
        match self.rt.block_on(file.flush()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(&e)),
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

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        let Some(file) = self.get_file(fh.0) else {
            reply.ok();
            return;
        };
        match self.rt.block_on(file.fsync()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    // ── Remove + rename ──────────────────────────────────────────────────

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

    // ── Symbolic + hard links ────────────────────────────────────────────

    fn symlink(
        &self,
        req: &Request,
        parent: INodeNo,
        link_name: &OsStr,
        target: &std::path::Path,
        reply: ReplyEntry,
    ) {
        let Some(name_str) = link_name.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let Some(target_str) = target.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let name_owned = name_str.to_string();
        let target_owned = target_str.to_string();
        let fs = self.fs.clone();
        let uid = req.uid();
        let gid = req.gid();
        match self.rt.block_on(async move {
            fs.symlink(parent.0, &name_owned, &target_owned, uid, gid).await
        }) {
            Ok(attr) => reply.entry(&TTL, &to_fuser_attr(&attr), Generation(0)),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn link(
        &self,
        _req: &Request,
        ino: INodeNo,
        newparent: INodeNo,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let Some(name_str) = newname.to_str() else {
            reply.error(Errno::EINVAL);
            return;
        };
        let name_owned = name_str.to_string();
        let fs = self.fs.clone();
        match self.rt.block_on(async move {
            fs.link(ino.0, newparent.0, &name_owned).await
        }) {
            Ok(attr) => reply.entry(&TTL, &to_fuser_attr(&attr), Generation(0)),
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    // ── Filesystem-wide ──────────────────────────────────────────────────

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        match self.rt.block_on(self.fs.statfs()) {
            Ok(stats) => {
                reply.statfs(
                    stats.bytes_used / 4096, // blocks
                    u64::MAX / 2,            // bfree
                    u64::MAX / 2,            // bavail
                    stats.inodes,            // files
                    u64::MAX / 2,            // ffree
                    4096,                    // bsize
                    255,                     // namelen
                    4096,                    // frsize
                );
            }
            Err(e) => reply.error(to_errno(&e)),
        }
    }

    fn mknod(
        &self,
        _req: &Request,
        _parent: INodeNo,
        _name: &OsStr,
        _mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        // unisonfs has no FIFOs, character devices, block devices, or sockets.
        reply.error(Errno::ENOSYS);
    }
}

/// Mount the filesystem at `mount_path` using FUSE.
///
/// Blocks until unmount. Intended to be called from a `spawn_blocking` task or
/// a dedicated OS thread — never from an `async` context directly, because
/// `fuser::mount2` drives a synchronous event loop.
///
/// The `rt` parameter has been removed: `FuseAdapter` now owns its own
/// `current_thread` runtime so it never borrows workers from the caller's pool.
#[cfg(target_os = "linux")]
pub fn mount(
    fs: Arc<dyn FileSystem + 'static>,
    mount_path: &Path,
) -> anyhow::Result<()> {
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("unisonfs".to_string()),
        MountOption::AutoUnmount,
        MountOption::DefaultPermissions,
    ];
    config.acl = SessionACL::RootAndOwner;
    let adapter = FuseAdapter::new(fs);
    fuser::mount2(adapter, mount_path, &config)?;
    Ok(())
}

/// Stub for non-Linux targets.
#[cfg(not(target_os = "linux"))]
pub fn mount(
    _fs: std::sync::Arc<dyn crate::vfs::FileSystem>,
    _mount_path: &std::path::Path,
) -> anyhow::Result<()> {
    anyhow::bail!("FUSE backend is only supported on Linux; use --backend nfs on macOS")
}
