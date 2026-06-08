//! `unisonfs logs` — tail the daemon log.

use anyhow::Result;
use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Mount tag to show logs for.
    pub tag: String,
    /// Number of recent lines to show (default 50).
    #[arg(long, default_value_t = 50)]
    pub lines: usize,
    /// Follow (tail -f).
    #[arg(long, short = 'f')]
    pub follow: bool,
}

pub async fn run(args: Args) -> Result<()> {
    let log_path = unisonfs_core::daemon::log_path(&args.tag);
    if !log_path.exists() {
        eprintln!("No log file for tag '{}'.", args.tag);
        return Ok(());
    }

    if args.follow {
        let mut child = tokio::process::Command::new("tail")
            .args(["-f", "-n", &args.lines.to_string()])
            .arg(&log_path)
            .spawn()?;
        child.wait().await?;
    } else {
        let mut child = tokio::process::Command::new("tail")
            .args(["-n", &args.lines.to_string()])
            .arg(&log_path)
            .spawn()?;
        child.wait().await?;
    }
    Ok(())
}
