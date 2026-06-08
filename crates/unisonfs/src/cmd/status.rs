//! `unisonfs status` — show daemon health and queue depth.

use anyhow::Result;
use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Mount tag or path to query.
    pub tag: String,
}

pub async fn run(args: Args) -> Result<()> {
    let tag = args.tag;
    let resp = unisonfs_core::daemon::client::send_request(
        &tag,
        unisonfs_core::daemon::protocol::Request::Status,
    )
    .await?;

    match resp {
        unisonfs_core::daemon::protocol::Response::Status(s) => {
            println!("Tag:            {}", s.tag);
            println!("Mount:          {}", s.mount_path);
            println!("PID:            {}", s.pid);
            println!("Push queue:     {} items", s.push_queue_len);
            if let Some(ts) = s.last_pull_at {
                println!("Last pull:      {}ms ago", now_ms() - ts);
            } else {
                println!("Last pull:      never");
            }
        }
        unisonfs_core::daemon::protocol::Response::Error(e) => {
            eprintln!("Daemon error: {e}");
        }
        _ => {
            eprintln!("Unexpected response");
        }
    }
    Ok(())
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
