//! `unisonfs whoami` — show the current user, tenant, and API endpoint.

use anyhow::Result;
use clap::Args as ClapArgs;
use unisonfs_core::config::credentials::resolve_api_url;

#[derive(ClapArgs, Debug)]
pub struct Args {
    #[arg(long, env = "UNISON_TOKEN")]
    pub token: Option<String>,
    #[arg(long, env = "UNISON_API_URL")]
    pub api_url: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let token = super::auth::resolve_token(args.token.as_deref())?;
    let api_url = resolve_api_url(args.api_url.as_deref());
    let client = unisonfs_core::api::ApiClient::new(&api_url, &token);
    let info = client.whoami().await?;

    println!("User:    {} ({})", info.user_email, info.user_id);
    println!(
        "Tenant:  {} ({}) [verified: {}]",
        info.tenant_name, info.tenant_id, info.tenant_verified
    );
    println!("Scopes:  {}", info.scopes.join(", "));
    println!("API URL: {api_url}");
    Ok(())
}
