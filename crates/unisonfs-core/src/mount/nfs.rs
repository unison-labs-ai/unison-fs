//! NFS mount adapter (macOS and Linux).
//!
//! Runs an embedded NFSv3 server on localhost and mounts it via the OS
//! NFS client. This avoids the macFUSE kernel extension requirement on macOS.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::vfs::traits::FileSystem;

/// Start an embedded NFS server and mount it at `mount_path`.
///
/// The server listens on an automatically-assigned localhost port. The mount
/// command is run via the OS NFS client (`mount_nfs` on macOS or `mount -t nfs`
/// on Linux).
///
/// This function blocks until the mount is unmounted.
#[cfg(unix)]
pub async fn mount(
    fs: Arc<dyn FileSystem>,
    mount_path: &Path,
    _rt: tokio::runtime::Handle,
) -> Result<()> {
    use nfsserve::tcp::{NFSTcp, NFSTcpListener};

    let listener = NFSTcpListener::bind("127.0.0.1:0", NfsAdapter::new(fs)).await?;
    let port = listener.get_listen_port();

    tracing::info!("NFS server listening on 127.0.0.1:{port}");

    if !mount_path.exists() {
        std::fs::create_dir_all(mount_path)?;
    }

    let mount_path_str = mount_path.to_string_lossy();

    // Serve BEFORE mounting. `mount_nfs` (macOS) and `mount -t nfs` (Linux)
    // perform the MOUNT/NULL RPC handshake *during* the mount call, so the
    // accept loop must already be running. Running it afterwards left the
    // server unable to answer the handshake: on macOS mount_nfs returned 0 but
    // never attached; on Linux it only worked by lazy first-access luck. The
    // socket is already bound, so any connection mount_nfs opens queues in the
    // listen backlog until handle_forever accepts it.
    let serve = tokio::spawn(async move { listener.handle_forever().await });

    #[cfg(target_os = "macos")]
    {
        let status = tokio::process::Command::new("mount_nfs")
            .args([
                // Mirror the Linux branch: pin v3 + TCP and pass the server's
                // port explicitly (it serves mount + nfs on the same localhost
                // port). `soft` avoids kernel hangs if the daemon dies. No
                // `resvport`: a loopback mount needs no reserved (<1024) source
                // port, and requiring one forced sudo.
                "-o",
                &format!(
                    "vers=3,tcp,port={port},mountport={port},soft,nolocks,locallocks,nfc"
                ),
                "127.0.0.1:/",
                &*mount_path_str,
            ])
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("mount_nfs failed with status: {}", status);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let status = tokio::process::Command::new("mount")
            .args([
                "-t",
                "nfs",
                "-o",
                &format!("port={port},mountport={port},proto=tcp,nfsvers=3,nolock,soft"),
                "127.0.0.1:/",
                &*mount_path_str,
            ])
            .status()
            .await?;
        if !status.success() {
            anyhow::bail!("mount -t nfs failed with status: {}", status);
        }
    }

    tracing::info!("Mounted NFS at {}", mount_path.display());

    // Block until the server stops (i.e. the filesystem is unmounted).
    serve
        .await
        .map_err(|e| anyhow::anyhow!("nfs serve task panicked: {e}"))??;

    Ok(())
}

#[cfg(not(unix))]
pub async fn mount(
    _fs: Arc<dyn FileSystem>,
    _mount_path: &Path,
    _rt: tokio::runtime::Handle,
) -> Result<()> {
    anyhow::bail!("NFS backend is not supported on this platform")
}

// ── NFS adapter ───────────────────────────────────────────────────────────────

/// Bridge from our `FileSystem` trait to `nfsserve::vfs::NFSFileSystem`.
#[cfg(unix)]
struct NfsAdapter {
    fs: Arc<dyn FileSystem>,
}

#[cfg(unix)]
impl NfsAdapter {
    fn new(fs: Arc<dyn FileSystem>) -> Self {
        Self { fs }
    }
}

#[cfg(unix)]
#[async_trait::async_trait]
impl nfsserve::vfs::NFSFileSystem for NfsAdapter {
    fn root_dir(&self) -> nfsserve::nfs::fileid3 {
        1
    }

    fn capabilities(&self) -> nfsserve::vfs::VFSCapabilities {
        nfsserve::vfs::VFSCapabilities::ReadWrite
    }

    async fn lookup(
        &self,
        dirid: nfsserve::nfs::fileid3,
        filename: &nfsserve::nfs::filename3,
    ) -> Result<nfsserve::nfs::fileid3, nfsserve::nfs::nfsstat3> {
        let name =
            std::str::from_utf8(filename).map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        let attr = self
            .fs
            .lookup(dirid, name)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?
            .ok_or(nfsserve::nfs::nfsstat3::NFS3ERR_NOENT)?;
        Ok(attr.ino)
    }

    async fn getattr(
        &self,
        id: nfsserve::nfs::fileid3,
    ) -> Result<nfsserve::nfs::fattr3, nfsserve::nfs::nfsstat3> {
        let attr = self
            .fs
            .getattr(id)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?
            .ok_or(nfsserve::nfs::nfsstat3::NFS3ERR_NOENT)?;
        Ok(vfs_attr_to_nfs(&attr))
    }

    async fn setattr(
        &self,
        id: nfsserve::nfs::fileid3,
        setattr: nfsserve::nfs::sattr3,
    ) -> Result<nfsserve::nfs::fattr3, nfsserve::nfs::nfsstat3> {
        use crate::vfs::{SetAttr, TimeOrNow, Timestamp};
        use nfsserve::nfs::{set_atime, set_gid3, set_mtime, set_size3, set_uid3, set_mode3};

        let mode = match setattr.mode {
            set_mode3::mode(m) => Some(m),
            _ => None,
        };
        let uid = match setattr.uid {
            set_uid3::uid(u) => Some(u),
            _ => None,
        };
        let gid = match setattr.gid {
            set_gid3::gid(g) => Some(g),
            _ => None,
        };
        let size = match setattr.size {
            set_size3::size(s) => Some(s),
            _ => None,
        };
        let atime = match setattr.atime {
            set_atime::DONT_CHANGE => None,
            set_atime::SET_TO_SERVER_TIME => Some(TimeOrNow::Now),
            set_atime::SET_TO_CLIENT_TIME(ts) => Some(TimeOrNow::Time(Timestamp {
                sec: ts.seconds as i64,
                nsec: ts.nseconds,
            })),
        };
        let mtime = match setattr.mtime {
            set_mtime::DONT_CHANGE => None,
            set_mtime::SET_TO_SERVER_TIME => Some(TimeOrNow::Now),
            set_mtime::SET_TO_CLIENT_TIME(ts) => Some(TimeOrNow::Time(Timestamp {
                sec: ts.seconds as i64,
                nsec: ts.nseconds,
            })),
        };
        let sa = SetAttr { mode, uid, gid, size, atime, mtime };
        let attr = self
            .fs
            .setattr(id, sa)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        Ok(vfs_attr_to_nfs(&attr))
    }

    async fn read(
        &self,
        id: nfsserve::nfs::fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsserve::nfs::nfsstat3> {
        let handle = self
            .fs
            .open(id, libc::O_RDONLY)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        let attr = handle
            .getattr()
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        let data = handle
            .read(offset, count as usize)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        let eof = offset + data.len() as u64 >= attr.size;
        Ok((data, eof))
    }

    async fn write(
        &self,
        id: nfsserve::nfs::fileid3,
        offset: u64,
        data: &[u8],
    ) -> Result<nfsserve::nfs::fattr3, nfsserve::nfs::nfsstat3> {
        let handle = self
            .fs
            .open(id, libc::O_WRONLY)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        handle
            .write(offset, data)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        let attr = handle
            .getattr()
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        Ok(vfs_attr_to_nfs(&attr))
    }

    async fn create(
        &self,
        dirid: nfsserve::nfs::fileid3,
        filename: &nfsserve::nfs::filename3,
        _attr: nfsserve::nfs::sattr3,
    ) -> Result<(nfsserve::nfs::fileid3, nfsserve::nfs::fattr3), nfsserve::nfs::nfsstat3> {
        let name =
            std::str::from_utf8(filename).map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        let (file_attr, _handle) = self
            .fs
            .create_file(dirid, name, 0o644, 0, 0)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        Ok((file_attr.ino, vfs_attr_to_nfs(&file_attr)))
    }

    async fn create_exclusive(
        &self,
        dirid: nfsserve::nfs::fileid3,
        filename: &nfsserve::nfs::filename3,
    ) -> Result<nfsserve::nfs::fileid3, nfsserve::nfs::nfsstat3> {
        let name =
            std::str::from_utf8(filename).map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        let (attr, _) = self
            .fs
            .create_file(dirid, name, 0o644, 0, 0)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        Ok(attr.ino)
    }

    async fn mkdir(
        &self,
        dirid: nfsserve::nfs::fileid3,
        dirname: &nfsserve::nfs::filename3,
    ) -> Result<(nfsserve::nfs::fileid3, nfsserve::nfs::fattr3), nfsserve::nfs::nfsstat3> {
        let name =
            std::str::from_utf8(dirname).map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        let attr = self
            .fs
            .mkdir(dirid, name, 0o755, 0, 0)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        Ok((attr.ino, vfs_attr_to_nfs(&attr)))
    }

    async fn remove(
        &self,
        dirid: nfsserve::nfs::fileid3,
        filename: &nfsserve::nfs::filename3,
    ) -> Result<(), nfsserve::nfs::nfsstat3> {
        let name =
            std::str::from_utf8(filename).map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        self.fs
            .unlink(dirid, name)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)
    }

    async fn rename(
        &self,
        from_dirid: nfsserve::nfs::fileid3,
        from_filename: &nfsserve::nfs::filename3,
        to_dirid: nfsserve::nfs::fileid3,
        to_filename: &nfsserve::nfs::filename3,
    ) -> Result<(), nfsserve::nfs::nfsstat3> {
        let from = std::str::from_utf8(from_filename)
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        let to = std::str::from_utf8(to_filename)
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        self.fs
            .rename(from_dirid, from, to_dirid, to)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)
    }

    async fn symlink(
        &self,
        dirid: nfsserve::nfs::fileid3,
        linkname: &nfsserve::nfs::filename3,
        symlink: &nfsserve::nfs::nfspath3,
        _attr: &nfsserve::nfs::sattr3,
    ) -> Result<(nfsserve::nfs::fileid3, nfsserve::nfs::fattr3), nfsserve::nfs::nfsstat3> {
        let name =
            std::str::from_utf8(linkname).map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        let target =
            std::str::from_utf8(symlink).map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        let attr = self
            .fs
            .symlink(dirid, name, target, 0, 0)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?;
        Ok((attr.ino, vfs_attr_to_nfs(&attr)))
    }

    async fn readlink(
        &self,
        id: nfsserve::nfs::fileid3,
    ) -> Result<nfsserve::nfs::nfspath3, nfsserve::nfs::nfsstat3> {
        let target = self
            .fs
            .readlink(id)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?
            .ok_or(nfsserve::nfs::nfsstat3::NFS3ERR_INVAL)?;
        Ok(nfsserve::nfs::nfspath3::from(target.into_bytes()))
    }

    async fn readdir(
        &self,
        dirid: nfsserve::nfs::fileid3,
        start_after: nfsserve::nfs::fileid3,
        max_entries: usize,
    ) -> Result<nfsserve::vfs::ReadDirResult, nfsserve::nfs::nfsstat3> {
        let entries = self
            .fs
            .readdir_plus(dirid)
            .await
            .map_err(|_| nfsserve::nfs::nfsstat3::NFS3ERR_IO)?
            .ok_or(nfsserve::nfs::nfsstat3::NFS3ERR_NOTDIR)?;

        let mut result = nfsserve::vfs::ReadDirResult {
            entries: Vec::new(),
            end: false,
        };

        let mut skipping = start_after != 0;
        for entry in &entries {
            if skipping {
                if entry.attr.ino == start_after {
                    skipping = false;
                }
                continue;
            }
            if result.entries.len() >= max_entries {
                break;
            }
            result.entries.push(nfsserve::vfs::DirEntry {
                fileid: entry.attr.ino,
                name: nfsserve::nfs::filename3::from(entry.name.as_bytes()),
                attr: vfs_attr_to_nfs(&entry.attr),
            });
        }

        result.end = result.entries.len() < max_entries;
        Ok(result)
    }
}

// ── attribute conversion ──────────────────────────────────────────────────────

#[cfg(unix)]
fn vfs_attr_to_nfs(attr: &crate::vfs::FileAttr) -> nfsserve::nfs::fattr3 {
    use crate::vfs::FileType;
    use nfsserve::nfs::{fattr3, ftype3, nfstime3, specdata3};

    let ftype = match attr.file_type() {
        FileType::Regular => ftype3::NF3REG,
        FileType::Directory => ftype3::NF3DIR,
        FileType::Symlink => ftype3::NF3LNK,
    };

    fattr3 {
        ftype,
        mode: attr.mode & 0o7777,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        size: attr.size,
        used: attr.blocks * 512,
        rdev: specdata3 {
            specdata1: 0,
            specdata2: 0,
        },
        fsid: 0,
        fileid: attr.ino,
        atime: nfstime3 {
            seconds: attr.atime.sec as u32,
            nseconds: attr.atime.nsec,
        },
        mtime: nfstime3 {
            seconds: attr.mtime.sec as u32,
            nseconds: attr.mtime.nsec,
        },
        ctime: nfstime3 {
            seconds: attr.ctime.sec as u32,
            nseconds: attr.ctime.nsec,
        },
    }
}
