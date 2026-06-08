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
    let tag = args
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| args.path.to_string_lossy().into_owned());

    // Send shutdown request
    match unisonfs_core::daemon::client::send_request(
        &tag,
        unisonfs_core::daemon::protocol::Request::Shutdown,
    )
    .await
    {
        Ok(_) => eprintln!("Shutdown signal sent to daemon (tag: {tag})"),
        Err(e) => eprintln!("Warning: could not contact daemon: {e}"),
    }

    // Platform unmount
    #[cfg(target_os = "macos")]
    {
        let _ = tokio::process::Command::new("umount")
            .arg(args.path.to_string_lossy().as_ref())
            .status()
            .await;
    }
    #[cfg(target_os = "linux")]
    {
        let _ = tokio::process::Command::new("fusermount")
            .args(["-u", &args.path.to_string_lossy()])
            .status()
            .await;
    }

    unisonfs_core::daemon::cleanup_stale(&tag);
    eprintln!("Unmounted.");
    Ok(())
}
