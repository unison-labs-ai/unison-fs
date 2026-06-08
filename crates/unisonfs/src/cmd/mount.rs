//! `unisonfs mount` — mount the Unison brain at a local path.
//!
//! The virtual tree exposes the brain namespace:
//!
//!   /private/    — your private notes and files
//!   /tenant/     — shared across your whole tenant/company
//!   /teams/<slug>/ — team-scoped documents
//!   /system/search/semantic/<q>.md — virtual read-only semantic search results

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use unisonfs_core::config::credentials::resolve_api_url;
use unisonfs_core::mount::MountBackend;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Local path to mount at (created if it doesn't exist).
    pub mount_path: PathBuf,

    /// Mount backend (`fuse` or `nfs`). Defaults to `fuse` on Linux and `nfs` on macOS.
    #[arg(long)]
    pub backend: Option<String>,

    /// Run in the foreground instead of daemonizing.
    #[arg(long)]
    pub foreground: bool,

    /// Delete local cache before mounting (pulls fresh from the brain).
    #[arg(long)]
    pub clean: bool,

    /// Use in-memory cache — nothing persists after unmount.
    #[arg(long)]
    pub ephemeral: bool,

    /// Unison API token (usk_live_...).
    #[arg(long, env = "UNISON_TOKEN")]
    pub token: Option<String>,

    /// Override the Unison API base URL.
    #[arg(long, env = "UNISON_API_URL")]
    pub api_url: Option<String>,

    /// Pull interval in seconds (default 30).
    #[arg(long, default_value_t = 30)]
    pub sync_interval: u64,

    /// Disable pulling remote changes. Local writes still push.
    #[arg(long)]
    pub no_sync: bool,

    /// Max seconds to drain the push queue at unmount (default 30).
    #[arg(long, default_value_t = 30)]
    pub drain_timeout: u64,

    /// Mount tag for the daemon (derived from mount_path basename if omitted).
    #[arg(long)]
    pub tag: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let token = super::auth::resolve_token(args.token.as_deref())?;
    let api_url = resolve_api_url(args.api_url.as_deref());

    let backend = match &args.backend {
        Some(b) => b.parse::<MountBackend>()?,
        None => MountBackend::default(),
    };

    let mount_path = normalize_path(&args.mount_path)?;
    let tag = args.tag.clone().unwrap_or_else(|| {
        mount_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "brain".to_string())
    });

    // Guard against double-mount
    if let Some(pid) = unisonfs_core::daemon::read_pid(&tag) {
        if unisonfs_core::daemon::pid_alive(pid) {
            anyhow::bail!(
                "tag '{}' is already mounted (pid {}). Use `unisonfs unmount {}` first.",
                tag,
                pid,
                mount_path.display(),
            );
        }
    }

    unisonfs_core::daemon::ensure_dirs()?;

    // Validate token
    let client = unisonfs_core::api::ApiClient::new(&api_url, &token);
    eprint!("Verifying token... ");
    let whoami = client.whoami().await.context("failed to verify Unison token")?;
    eprintln!(
        "ok (tenant: {}, verified: {})",
        whoami.tenant_name, whoami.tenant_verified
    );

    // Open SQLite cache
    let db_path = if args.ephemeral {
        None
    } else {
        if args.clean {
            let p = unisonfs_core::config::cache_db_path(&whoami.tenant_id);
            let _ = std::fs::remove_file(&p);
        }
        let p = unisonfs_core::config::cache_db_path(&whoami.tenant_id);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Some(p)
    };

    let db = Arc::new(match db_path {
        Some(ref p) => unisonfs_core::cache::Db::open(p)?,
        None => unisonfs_core::cache::Db::open_in_memory()?,
    });

    let fs = Arc::new(unisonfs_core::cache::UnisonFs::new(db.clone()));

    // Create mount point
    if !mount_path.exists() {
        std::fs::create_dir_all(&mount_path)
            .with_context(|| format!("cannot create mount point {}", mount_path.display()))?;
    }

    // Start sync engine
    let api = Arc::new(
        unisonfs_core::api::ApiClient::new(&api_url, &token)
            .with_user_id(whoami.user_id.clone()),
    );
    let shutdown = Arc::new(Notify::new());
    let sync_config = unisonfs_core::sync::SyncConfig {
        pull_interval_secs: args.sync_interval,
        no_pull: args.no_sync,
        drain_timeout_secs: args.drain_timeout,
    };

    let sync_handle = unisonfs_core::sync::start(
        api,
        db.clone(),
        fs.clone(),
        sync_config,
        shutdown.clone(),
    );

    eprintln!(
        "Mounting Unison brain at {} (backend: {}, tag: {})",
        mount_path.display(),
        backend,
        tag,
    );
    eprintln!(
        "Brain namespace: /private/, /tenant/, /teams/<slug>/, /system/search/semantic/<q>.md"
    );

    // Write pid file
    if args.foreground {
        unisonfs_core::daemon::write_pid(&tag)?;
    }

    // Mount
    let rt = tokio::runtime::Handle::current();
    let fs_dyn: Arc<dyn unisonfs_core::vfs::FileSystem> = fs;
    let mount_path_clone = mount_path.clone();

    match backend {
        MountBackend::Fuse => {
            let mount_path_str = mount_path.clone();
            // FUSE blocks the current thread; run in a spawn_blocking
            let join = tokio::task::spawn_blocking(move || {
                unisonfs_core::mount::fuse::mount(fs_dyn, &mount_path_str, rt)
            });
            join.await??;
        }
        MountBackend::Nfs => {
            unisonfs_core::mount::nfs::mount(fs_dyn, &mount_path_clone, rt.clone()).await?;
        }
    }

    // Unmount complete — shut down sync
    shutdown.notify_waiters();
    let _ = sync_handle.await;

    unisonfs_core::daemon::cleanup_stale(&tag);
    Ok(())
}

fn normalize_path(raw: &PathBuf) -> Result<PathBuf> {
    if let Ok(p) = std::fs::canonicalize(raw) {
        return Ok(p);
    }
    let base = if raw.is_absolute() {
        raw.clone()
    } else {
        std::env::current_dir()
            .context("cannot determine current directory")?
            .join(raw)
    };
    let mut normalized = PathBuf::new();
    for component in base.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            c => normalized.push(c),
        }
    }
    Ok(normalized)
}
