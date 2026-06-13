//! `unisonfs` — mount the Unison brain as a local filesystem.
//!
//! This binary is a thin CLI dispatch layer. All real logic lives in the
//! [`unisonfs_core`] library — this file parses arguments, initializes logging,
//! and hands control to the appropriate command handler.

#![deny(unsafe_code)]

use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod cmd;

/// Top-level CLI.
#[derive(Parser)]
#[command(
    name = "unisonfs",
    version,
    about = "Mount the Unison brain as a local filesystem",
    long_about = "unisonfs — exposes the Unison brain (/private/..., /workspace/..., /workspace/teams/<slug>/...) \
                  as a real local directory. Reads hit the local SQLite cache, writes route through \
                  the background push queue to the brain REST API."
)]
struct Cli {
    #[command(subcommand)]
    command: cmd::Command,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    cmd::dispatch(cli.command).await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("unisonfs=info,unisonfs_core=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
