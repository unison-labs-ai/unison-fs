//! `unisonfs unmount` — unmount a running unisonfs mount.

use anyhow::Result;
use clap::Args as ClapArgs;
use std::path::PathBuf;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Mount path or tag to unmount.
    pub path: PathBuf,
}

pub async fn run(args: Args) -> Result<()> {
    use unisonfs_core::daemon::client::send_request;
    use unisonfs_core::daemon::protocol::{Request, Response};

    let tag = args
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| args.path.to_string_lossy().into_owned());

    // Resolve the actual mount path. If the user passed a real path, use it;
    // otherwise (a bare tag like `unmount brain`) find the matching loopback
    // NFS mount in the system mount table — this needs no daemon and survives
    // even if the daemon IPC socket is gone. (Falling back to the daemon's
    // Status if the table scan misses.) Without this the unmount ran
    // `umount <tag>`, which isn't a path, so the kernel mount stayed attached.
    let mut mount_path: Option<PathBuf> = None;
    if args.path.is_absolute() || args.path.exists() {
        mount_path = Some(args.path.clone());
    } else {
        if let Ok(out) = tokio::process::Command::new("mount").output().await {
            let table = String::from_utf8_lossy(&out.stdout);
            for line in table.lines() {
                // macOS:  "127.0.0.1:/ on /path (nfs, ...)"
                // Linux:  "127.0.0.1:/ on /path type nfs (rw,...)"
                if !line.contains("127.0.0.1:/") {
                    continue;
                }
                let Some(rest) = line.split(" on ").nth(1) else {
                    continue;
                };
                let path = rest
                    .split(" type ")
                    .next()
                    .unwrap_or(rest)
                    .split(" (")
                    .next()
                    .unwrap_or("")
                    .trim();
                if !path.is_empty()
                    && std::path::Path::new(path)
                        .file_name()
                        .map(|n| n.to_string_lossy() == tag)
                        .unwrap_or(false)
                {
                    mount_path = Some(PathBuf::from(path));
                    break;
                }
            }
        }
        if mount_path.is_none() {
            if let Ok(Response::Status(s)) = send_request(&tag, Request::Status).await {
                mount_path = Some(PathBuf::from(s.mount_path));
            }
        }
    }

    // Send shutdown request
    match send_request(&tag, Request::Shutdown).await {
        Ok(_) => eprintln!("Shutdown signal sent to daemon (tag: {tag})"),
        Err(e) => eprintln!("Warning: could not contact daemon: {e}"),
    }

    // Platform unmount of the resolved mount path.
    match mount_path {
        Some(mp) => {
            let mp = mp.to_string_lossy().into_owned();
            #[cfg(target_os = "macos")]
            {
                let ok = tokio::process::Command::new("umount")
                    .arg(&mp)
                    .status()
                    .await
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !ok {
                    // Busy / stale handle — force it.
                    let _ = tokio::process::Command::new("diskutil")
                        .args(["unmount", "force", &mp])
                        .status()
                        .await;
                }
            }
            #[cfg(target_os = "linux")]
            {
                let _ = tokio::process::Command::new("fusermount")
                    .args(["-u", &mp])
                    .status()
                    .await;
            }
        }
        None => eprintln!(
            "Could not resolve a mount path for '{tag}'. If it's still mounted, run: umount <path>"
        ),
    }

    unisonfs_core::daemon::cleanup_stale(&tag);
    eprintln!("Unmounted.");
    Ok(())
}
