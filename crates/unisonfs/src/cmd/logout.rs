//! `unisonfs logout` — remove stored credentials.

use anyhow::Result;
use clap::Args as ClapArgs;

#[derive(ClapArgs, Debug)]
pub struct Args {}

pub async fn run(_args: Args) -> Result<()> {
    unisonfs_core::config::credentials::remove_global()?;
    eprintln!("Credentials removed.");
    Ok(())
}
