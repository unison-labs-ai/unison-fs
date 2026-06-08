//! Shared auth helpers.

use anyhow::{bail, Result};

/// Resolve the API token from: explicit flag > UNISON_TOKEN env > config file.
pub fn resolve_token(explicit: Option<&str>) -> Result<String> {
    let creds = unisonfs_core::config::credentials::resolve(explicit);
    match creds {
        Some(c) => Ok(c.token),
        None => bail!(
            "No Unison token found.\n\
             Set UNISON_TOKEN=usk_live_... or run `unisonfs login` first.\n\
             To provision a new account: `unisonfs provision --email you@example.com`"
        ),
    }
}
