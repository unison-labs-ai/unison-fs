//! `unisonfs provision` — headless machine-auth flow.
//!
//! Three-step:
//!   1. POST /v1/auth/provision → get unverified usk_ key + email OTP
//!   2. User enters OTP
//!   3. POST /v1/auth/verify → make workspace durable
//!
//! For existing accounts use `unisonfs provision --request-key` to trigger
//! the recovery flow, then enter the OTP.

use anyhow::{bail, Result};
use clap::Args as ClapArgs;
use std::io::{BufRead, IsTerminal, Write};
use unisonfs_core::api::ApiClient;
use unisonfs_core::config::credentials::{resolve_api_url, save_global, Credentials};

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Email address for the new (or existing) account.
    #[arg(long)]
    pub email: Option<String>,

    /// Request a recovery key for an existing account instead of provisioning new.
    #[arg(long)]
    pub request_key: bool,

    /// OTP code (from the emailed verification). If omitted, prompts interactively.
    #[arg(long)]
    pub code: Option<String>,

    /// Override the Unison API base URL.
    #[arg(long, env = "UNISON_API_URL")]
    pub api_url: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let api_url = resolve_api_url(args.api_url.as_deref());

    let email = match args.email {
        Some(e) => e,
        None => {
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                bail!("No email provided. Pass --email.");
            }
            eprint!("Email: ");
            std::io::stderr().flush()?;
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            line.trim().to_string()
        }
    };

    if args.request_key {
        eprintln!("Requesting recovery key for {email}...");
        ApiClient::request_key(&api_url, &email).await?;
        eprintln!("Check your email for the OTP code.");
    } else {
        eprintln!("Provisioning new Unison account for {email}...");
        match ApiClient::provision(&api_url, &email).await {
            Ok(resp) => {
                eprintln!(
                    "Provisioned (status: {}). Check your email for the OTP verification code.",
                    resp.status
                );
                // Save the unverified key immediately — it's usable right away
                // with brain:read + brain:write scopes.
                save_global(&Credentials {
                    token: resp.api_key.clone(),
                    api_url: None,
                })?;
                eprintln!("Unverified token saved (expires in 72h if not verified).");
            }
            Err(unisonfs_core::api::ApiError::Conflict(_)) => {
                eprintln!(
                    "Account already exists for {email}. Use --request-key to recover your token."
                );
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    }

    // Get OTP and verify
    let code = match args.code {
        Some(c) => c,
        None => {
            let stdin = std::io::stdin();
            if !stdin.is_terminal() {
                eprintln!("Pass --code <OTP> to complete verification.");
                return Ok(());
            }
            eprint!("OTP code from email: ");
            std::io::stderr().flush()?;
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            line.trim().to_string()
        }
    };

    if code.is_empty() {
        eprintln!("No code entered. Run `unisonfs provision --email {email} --code <OTP>` to verify later.");
        return Ok(());
    }

    eprintln!("Verifying OTP...");
    let verify_resp = ApiClient::verify(&api_url, &email, &code).await?;

    if verify_resp.verified {
        if let Some(new_key) = verify_resp.api_key {
            save_global(&Credentials {
                token: new_key,
                api_url: None,
            })?;
            eprintln!("Verified. New token saved.");
        } else {
            eprintln!("Verified. Existing token is now durable.");
        }
    } else {
        bail!("Verification failed — check your OTP code.");
    }

    Ok(())
}
