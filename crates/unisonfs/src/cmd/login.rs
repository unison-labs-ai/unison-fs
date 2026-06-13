//! `unisonfs login` — store Unison brain credentials.

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use std::io::{BufRead, IsTerminal, Write};
use unisonfs_core::config::credentials::{resolve_api_url, save_global, Credentials};

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Unison API token (usk_live_...). If omitted, prompts interactively.
    #[arg(long, env = "UNISON_TOKEN")]
    pub token: Option<String>,

    /// Override the Unison API base URL.
    #[arg(long, env = "UNISON_API_URL")]
    pub api_url: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let token = if let Some(t) = args.token {
        t
    } else {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            bail!("No token provided. Pass --token or set UNISON_TOKEN.");
        }
        eprint!("Enter your Unison API token (usk_live_...): ");
        std::io::stderr().flush()?;
        let mut t = String::new();
        stdin.lock().read_line(&mut t)?;
        let t = t.trim().to_string();
        if t.is_empty() {
            bail!("Token cannot be empty.");
        }
        t
    };

    let api_url = resolve_api_url(args.api_url.as_deref());

    eprint!("Verifying token... ");
    std::io::stderr().flush()?;
    let client = unisonfs_core::api::ApiClient::new(&api_url, &token);
    match client.whoami().await {
        Ok(info) => {
            eprintln!(
                "ok (workspace: {}, email: {})",
                info.workspace_name, info.user_email
            );
        }
        Err(unisonfs_core::api::ApiError::Auth) => {
            bail!("Invalid token — authentication failed.");
        }
        Err(e) => {
            eprintln!("warning: could not verify ({e}). Saving anyway.");
        }
    }

    save_global(&Credentials {
        token,
        api_url: if args.api_url.is_some() {
            args.api_url
        } else {
            None
        },
    })?;

    eprintln!("Credentials saved.");
    Ok(())
}
