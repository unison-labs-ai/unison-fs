//! `unisonfs sync` — force a sync cycle now.

use anyhow::Result;
use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Mount tag to sync.
    pub tag: String,
}

pub async fn run(args: Args) -> Result<()> {
    let resp = unisonfs_core::daemon::client::send_request(
        &args.tag,
        unisonfs_core::daemon::protocol::Request::Sync,
    )
    .await?;

    match resp {
        unisonfs_core::daemon::protocol::Response::Ok => {
            eprintln!("Sync requested for '{}'.", args.tag);
        }
        unisonfs_core::daemon::protocol::Response::Error(e) => {
            eprintln!("Daemon error: {e}");
        }
        _ => {}
    }
    Ok(())
}
