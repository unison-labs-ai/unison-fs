//! `unisonfs mount` — mount the Unison brain at a local path.
//!
//! With `--daemon` (default on macOS/Linux), forks a child `daemon-inner`
//! process that detaches from the TTY, then watches the startup progress file
//! until the mount is live.  With `--foreground`, runs the full daemon
//! lifecycle in the current process (useful for debugging or containers).

use std::path::PathBuf;
use std::process::Command as StdCommand;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use tokio::sync::watch;
use unisonfs_core::config::credentials::resolve_api_url;
use unisonfs_core::mount::MountBackend;
use unisonfs_core::sync::{InitialPullProgress, SyncEngine, SyncOptions};

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Local path to mount at (created if it doesn't exist).
    pub mount_path: PathBuf,

    /// Mount backend (`fuse` or `nfs`). Defaults to platform-appropriate.
    #[arg(long)]
    pub backend: Option<String>,

    /// Run in the foreground (don't fork a background daemon).
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

    /// Deletion scan interval in seconds (default 300).
    #[arg(long, default_value_t = 300)]
    pub deletion_scan_interval: u64,

    /// Brain path prefixes to sync (comma-separated).
    #[arg(long, default_value = "")]
    pub memory_paths: String,

    /// Skip importing pre-existing local .md files on mount.
    #[arg(long)]
    pub no_import: bool,

    /// Disable injecting semantic-search hints into CLAUDE.md / AGENTS.md.
    #[arg(long)]
    pub no_agent_hint: bool,

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
        // Stale pid — clean up
        unisonfs_core::daemon::cleanup_stale(&tag);
    }

    unisonfs_core::daemon::ensure_dirs()?;

    // Sweep orphaned agent hints from crashed daemons before we start.
    let _ = unisonfs_core::agent_hint::sweep_orphans(Some(&tag));

    if args.foreground {
        run_foreground(args, token, api_url, mount_path, tag).await
    } else {
        run_daemon_fork(args, token, api_url, mount_path, tag).await
    }
}

/// Foreground mode: runs the full daemon lifecycle in the current process.
async fn run_foreground(
    args: Args,
    token: String,
    api_url: String,
    mount_path: PathBuf,
    tag: String,
) -> Result<()> {
    let backend = match &args.backend {
        Some(b) => b.parse::<MountBackend>()?,
        None => MountBackend::default(),
    };

    // Validate token & identify workspace
    let client = unisonfs_core::api::ApiClient::new(&api_url, &token);
    eprint!("Verifying token... ");
    let whoami = client.whoami().await.context("failed to verify Unison token")?;
    eprintln!(
        "ok (workspace: {}, verified: {})",
        whoami.workspace_name, whoami.workspace_verified
    );

    // Open SQLite cache
    let db_path = if args.ephemeral {
        None
    } else {
        if args.clean {
            let p = unisonfs_core::config::cache_db_path_for_tag(&whoami.workspace_id, &tag);
            let _ = std::fs::remove_file(&p);
        }
        let p = unisonfs_core::config::cache_db_path_for_tag(&whoami.workspace_id, &tag);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Some(p)
    };

    let db = Arc::new(match &db_path {
        Some(p) => unisonfs_core::cache::Db::open(p)?,
        None => unisonfs_core::cache::Db::open_in_memory()?,
    });

    let api = Arc::new(
        unisonfs_core::api::ApiClient::new(&api_url, &token)
            .with_user_id(whoami.user_id.clone()),
    );
    let fs = Arc::new(unisonfs_core::cache::UnisonFs::with_api(db, api));

    // Warm profile.md
    let fs_p = fs.clone();
    tokio::spawn(async move { fs_p.warm_profile().await });

    // Memory paths
    if !args.memory_paths.is_empty() {
        let paths: Vec<String> = args.memory_paths
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !paths.is_empty() {
            if let Some(api_ref) = fs.api() {
                let _ = api_ref.update_memory_paths(paths).await;
            }
        }
    }

    // Initial pull
    eprintln!("Running initial pull...");
    let (removed, reconciled) = SyncEngine::initial_pull_with_progress(&fs, |p| {
        match p {
            InitialPullProgress::DeletionScan(sp) => {
                eprint!("\r  Deletion scan: {}/{}", sp.page, sp.total_pages);
            }
            InitialPullProgress::Pull(pp) => {
                eprint!("\r  Pulling: {}/{}", pp.reconciled, pp.total_items);
            }
        }
    })
    .await
    .unwrap_or((0, 0));
    eprintln!("\r  Done: removed={removed}, pulled={reconciled}");

    // Import pre-existing files
    if !args.no_import {
        let mp = mount_path.clone();
        let fs_import = fs.clone();
        tokio::spawn(async move {
            import_existing_files(&mp, &fs_import).await;
        });
    }

    // Create mount point
    if !mount_path.exists() {
        std::fs::create_dir_all(&mount_path)
            .with_context(|| format!("cannot create mount point {}", mount_path.display()))?;
    }

    // Agent hint
    if !args.no_agent_hint {
        if let Err(e) = unisonfs_core::agent_hint::install(&tag, &mount_path) {
            tracing::warn!("agent hint install failed: {e}");
        }
    }

    // Write PID
    unisonfs_core::daemon::write_pid(&tag)?;

    // Start background sync
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let sync_opts = SyncOptions {
        delta_interval: Duration::from_secs(args.sync_interval),
        deletion_scan_interval: Duration::from_secs(args.deletion_scan_interval),
        pull_enabled: !args.no_import,
    };
    let mut task_set = SyncEngine::start(fs.clone(), sync_opts, shutdown_rx);

    eprintln!(
        "Mounted Unison brain at {} (backend: {}, tag: {})",
        mount_path.display(),
        backend,
        tag,
    );

    let rt = tokio::runtime::Handle::current();
    let fs_dyn: Arc<dyn unisonfs_core::vfs::FileSystem> = fs.clone();

    match backend {
        MountBackend::Fuse => {
            let mp = mount_path.clone();
            let join = tokio::task::spawn_blocking(move || {
                unisonfs_core::mount::fuse::mount(fs_dyn, &mp)
            });
            join.await??;
        }
        MountBackend::Nfs => {
            unisonfs_core::mount::nfs::mount(fs_dyn, &mount_path, rt.clone()).await?;
        }
    }

    // Shutdown
    let _ = shutdown_tx.send(true);
    let drain = async {
        while task_set.join_next().await.is_some() {}
    };
    tokio::time::timeout(Duration::from_secs(args.drain_timeout), drain).await.ok();

    SyncEngine::unmount_scan(&fs).await;

    if !args.no_agent_hint {
        let _ = unisonfs_core::agent_hint::uninstall(&tag);
    }
    unisonfs_core::daemon::cleanup_stale(&tag);
    Ok(())
}

/// Daemon mode: fork a `daemon-inner` child, watch the startup file.
async fn run_daemon_fork(
    args: Args,
    token: String,
    api_url: String,
    mount_path: PathBuf,
    tag: String,
) -> Result<()> {
    let exe = std::env::current_exe().context("cannot determine current binary path")?;
    let startup_file = unisonfs_core::config::runtime_dir().join(format!("{tag}.startup"));
    let _ = std::fs::remove_file(&startup_file);

    let mut cmd = StdCommand::new(&exe);
    cmd.arg("daemon-inner")
        .arg("--mountpoint")
        .arg(&mount_path)
        .arg("--tag")
        .arg(&tag)
        .arg("--delta-interval-secs")
        .arg(args.sync_interval.to_string())
        .arg("--deletion-scan-interval-secs")
        .arg(args.deletion_scan_interval.to_string());

    if !args.memory_paths.is_empty() {
        cmd.arg("--memory-paths").arg(&args.memory_paths);
    }
    if args.no_import {
        cmd.arg("--no-import");
    }
    if args.no_agent_hint {
        cmd.arg("--no-agent-hint");
    }

    // Forward credentials via env
    cmd.env("UNISON_TOKEN", &token);
    cmd.env("UNISON_API_URL", &api_url);

    // Detach stdin/stdout/stderr
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn().context("failed to spawn daemon-inner")?;

    // Watch startup file
    eprint!("Starting unisonfs daemon for tag '{tag}'...");
    let timeout = Duration::from_secs(60);
    match super::startup::wait_for_ready(&startup_file, timeout) {
        Some(p) if p.phase == super::startup::Phase::Ready => {
            eprintln!(" {}", p.message);
        }
        Some(p) if p.phase == super::startup::Phase::Failed => {
            anyhow::bail!("daemon startup failed: {}", p.error.unwrap_or_default());
        }
        None => {
            eprintln!(" (timed out waiting for daemon)");
        }
        Some(p) => {
            eprintln!(" {}", p.message);
        }
    }

    Ok(())
}

/// Walk an existing directory and import any `.md` files that don't exist remotely.
async fn import_existing_files(
    dir: &std::path::Path,
    fs: &Arc<unisonfs_core::cache::UnisonFs>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
            let Ok(contents) = std::fs::read(&path) else {
                continue;
            };
            let brain_path = format!("/private/notes/{name}");
            if let Err(e) = fs.import_file(&brain_path, &contents).await {
                tracing::debug!("import {name}: {e}");
            }
        }
    }
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
