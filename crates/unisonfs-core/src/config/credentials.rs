//! Credential storage for the Unison brain API token.
//!
//! Reads from / writes to `~/.config/unison/config.json` (matching the
//! @unisonlabs/sdk convention), but also accepts the environment variable
//! `UNISON_TOKEN` which always takes precedence.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Stored credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    /// The usk_live_... token.
    pub token: String,
    /// Override for the API base URL (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_url: Option<String>,
}

fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("ai", "unisonlabs", "unisonfs")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp/unisonfs-config"))
}

fn global_path() -> PathBuf {
    // Mirror @unisonlabs/sdk: ~/.config/unison/config.json
    if let Some(home) = directories::BaseDirs::new() {
        return home
            .config_dir()
            .join("unison")
            .join("config.json");
    }
    config_dir().join("credentials.json")
}

fn write_json(path: &Path, creds: &Credentials) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(creds)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn load_json(path: &Path) -> Option<Credentials> {
    let data = std::fs::read_to_string(path).ok()?;
    // Try parsing as the stored credentials shape
    if let Ok(c) = serde_json::from_str::<Credentials>(&data) {
        return Some(c);
    }
    // Also try the @unisonlabs/sdk config shape: { "token": "usk_..." }
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let token = v.get("token").and_then(|t| t.as_str()).map(String::from)?;
    let api_url = v
        .get("apiUrl")
        .or_else(|| v.get("api_url"))
        .and_then(|u| u.as_str())
        .map(String::from);
    Some(Credentials { token, api_url })
}

/// Save credentials globally.
pub fn save_global(creds: &Credentials) -> Result<()> {
    write_json(&global_path(), creds)
}

/// Load the global credentials file.
pub fn load_global() -> Option<Credentials> {
    load_json(&global_path())
}

/// Remove globally stored credentials.
pub fn remove_global() -> Result<()> {
    match std::fs::remove_file(global_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Resolve credentials: `UNISON_TOKEN` env var always wins,
/// then the global config file.
pub fn resolve(explicit_token: Option<&str>) -> Option<Credentials> {
    // 1. Explicit --token flag
    if let Some(t) = explicit_token {
        if !t.is_empty() {
            return Some(Credentials {
                token: t.to_string(),
                api_url: None,
            });
        }
    }

    // 2. UNISON_TOKEN env var (per brief: always takes precedence over file)
    if let Ok(t) = std::env::var("UNISON_TOKEN") {
        if !t.is_empty() {
            let api_url = std::env::var("UNISON_API_URL").ok();
            return Some(Credentials { token: t, api_url });
        }
    }

    // 3. Config file
    load_global()
}

/// Encode an absolute path into a safe filename component.
/// Replaces `/` and `:` with `__`, keeping ASCII alnum + `-_`.
pub fn encode_path(path: &str) -> String {
    path.chars()
        .map(|c| match c {
            '/' | ':' => '_',
            c if c.is_ascii_alphanumeric() || c == '-' || c == '.' => c,
            _ => '_',
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn projects_dir() -> PathBuf {
    directories::ProjectDirs::from("ai", "unisonlabs", "unisonfs")
        .map(|d| d.config_dir().join("projects"))
        .unwrap_or_else(|| PathBuf::from("/tmp/unisonfs-config/projects"))
}

fn project_path(mount_path: &str) -> PathBuf {
    projects_dir().join(format!("{}.json", encode_path(mount_path)))
}

/// Save project-scoped credentials for `mount_path`.
pub fn save_project(mount_path: &str, creds: &Credentials) -> Result<()> {
    write_json(&project_path(mount_path), creds)
}

/// Load project-scoped credentials for `mount_path`.
pub fn load_project(mount_path: &str) -> Option<Credentials> {
    load_json(&project_path(mount_path))
}

/// Remove project-scoped credentials for `mount_path`.
pub fn remove_project(mount_path: &str) -> Result<()> {
    match std::fs::remove_file(project_path(mount_path)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Resolve credentials with project scope taking precedence over global:
/// explicit --token > UNISON_TOKEN env > project-scoped file > global file.
pub fn resolve_with_project(explicit_token: Option<&str>, mount_path: Option<&str>) -> Option<Credentials> {
    // 1. Explicit --token flag
    if let Some(t) = explicit_token {
        if !t.is_empty() {
            return Some(Credentials {
                token: t.to_string(),
                api_url: None,
            });
        }
    }

    // 2. UNISON_TOKEN env var
    if let Ok(t) = std::env::var("UNISON_TOKEN") {
        if !t.is_empty() {
            let api_url = std::env::var("UNISON_API_URL").ok();
            return Some(Credentials { token: t, api_url });
        }
    }

    // 3. Project-scoped credentials
    if let Some(p) = mount_path {
        if let Some(c) = load_project(p) {
            return Some(c);
        }
    }

    // 4. Global credentials
    load_global()
}

/// Resolve just the API URL with proper precedence:
/// explicit arg > UNISON_API_URL env > config file > default.
pub fn resolve_api_url(explicit: Option<&str>) -> String {
    if let Some(u) = explicit {
        if !u.is_empty() {
            return u.trim_end_matches('/').to_string();
        }
    }
    if let Ok(u) = std::env::var("UNISON_API_URL") {
        if !u.is_empty() {
            return u.trim_end_matches('/').to_string();
        }
    }
    if let Some(creds) = load_global() {
        if let Some(u) = creds.api_url {
            if !u.is_empty() {
                return u.trim_end_matches('/').to_string();
            }
        }
    }
    crate::api::DEFAULT_API_URL.to_string()
}
