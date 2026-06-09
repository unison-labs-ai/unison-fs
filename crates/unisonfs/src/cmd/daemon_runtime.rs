//! Full daemon lifecycle for `daemon-inner`.
//!
//! Sequence:
//! 1. Open SQLite cache.
//! 2. Verify token, identify tenant.
//! 3. Write startup progress file.
//! 4. Run initial deletion scan + full pull (with progress updates).
//! 5. Mount filesystem (FUSE/Linux or NFS/macOS).
//! 6. Write startup `Ready`.
//! 7. Spawn background sync loops via `SyncEngine::start`.
//! 8. Wait for IPC shutdown signal or inactivity timeout.
//! 9. Drain push queue, unmount, cleanup.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::watch;
use unisonfs_core::config::credentials::resolve_api_url;
use unisonfs_core::mount::MountBackend;
use unisonfs_core::sync::{InitialPullProgress, SyncEngine, SyncOptions};

use super::daemon_inner::DaemonConfig;
use super::startup::{Phase, StartupProgress, StartupReporter};

pub async fn run_daemon(config: DaemonConfig) -> Result<()> {
    let tag = &config.tag;

    // --- Startup reporter ---
    let state_dir = unisonfs_core::config::runtime_dir();
    std::fs::create_dir_all(&state_dir)?;
    let reporter = StartupReporter::new(&state_dir, tag);
    reporter.report(StartupProgress::init());

    // --- Resolve credentials ---
    let token = std::env::var("UNISON_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| {
            unisonfs_core::config::credentials::resolve(None).map(|c| c.token)
        })
        .ok_or_else(|| anyhow::anyhow!("no UNISON_TOKEN set"))?;

    let api_url = resolve_api_url(None);

    // --- Verify token & identify tenant ---
    let client = unisonfs_core::api::ApiClient::new(&api_url, &token);
    let whoami = client
        .whoami()
        .await
        .context("failed to verify Unison token")?;

    // --- Open SQLite cache ---
    let db_path = unisonfs_core::config::cache_db_path_for_tag(&whoami.tenant_id, tag);
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db = Arc::new(unisonfs_core::cache::Db::open(&db_path)?);

    // --- Build UnisonFs ---
    let api = Arc::new(
        unisonfs_core::api::ApiClient::new(&api_url, &token)
            .with_user_id(whoami.user_id.clone()),
    );
    let fs = Arc::new(unisonfs_core::cache::UnisonFs::with_api(db, api));

    // Warm the virtual profile.md
    let fs_profile = fs.clone();
    tokio::spawn(async move {
        fs_profile.warm_profile().await;
    });

    // --- Memory paths ---
    if !config.memory_paths.is_empty() {
        let paths: Vec<String> = config
            .memory_paths
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !paths.is_empty() {
            if let Some(api_ref) = fs.api() {
                if let Err(e) = api_ref.update_memory_paths(paths).await {
                    tracing::warn!("update_memory_paths: {e}");
                }
            }
        }
    }

    // --- Initial pull with progress ---
    reporter.report(StartupProgress {
        phase: Phase::DeletionScan,
        message: "Starting initial deletion scan".to_string(),
        pct: Some(5),
        error: None,
    });

    let reporter_ref = &reporter;
    let _ = SyncEngine::initial_pull_with_progress(&fs, |p| match p {
        InitialPullProgress::DeletionScan(sp) => {
            reporter_ref.report(StartupProgress::deletion_scan(sp.page, sp.total_pages));
        }
        InitialPullProgress::Pull(pp) => {
            reporter_ref.report(StartupProgress::initial_pull(pp.reconciled, pp.total_items));
        }
    })
    .await;

    // --- Import pre-existing files (--no-import to skip) ---
    if !config.no_import {
        let mp = config.mountpoint.clone();
        let fs_import = fs.clone();
        tokio::spawn(async move {
            import_existing_files(&mp, &fs_import).await;
        });
    }

    // --- Mount filesystem ---
    reporter.report(StartupProgress::mounting());

    let backend = MountBackend::default();
    let rt = tokio::runtime::Handle::current();
    let fs_dyn: Arc<dyn unisonfs_core::vfs::FileSystem> = fs.clone();
    let mount_path = config.mountpoint.clone();
    if !mount_path.exists() {
        std::fs::create_dir_all(&mount_path)
            .with_context(|| format!("create mount point {}", mount_path.display()))?;
    }

    // --- Shutdown channel ---
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // --- Start background sync ---
    let sync_opts = SyncOptions {
        delta_interval: Duration::from_secs(config.delta_interval_secs),
        deletion_scan_interval: Duration::from_secs(config.deletion_scan_interval_secs),
        pull_enabled: true,
    };
    let mut task_set = SyncEngine::start(fs.clone(), sync_opts, shutdown_rx.clone());

    // --- Agent hint injection ---
    if !config.no_agent_hint {
        if let Err(e) = unisonfs_core::agent_hint::install(tag, &mount_path) {
            tracing::warn!("agent hint install failed: {e}");
        } else {
            tracing::info!("agent hint injected for tag={tag}");
        }
    }

    // --- Write .unisonfs marker ---
    let marker = super::marker::UnisonMarker {
        tag: Some(tag.clone()),
        path: None,
        description: Some("managed by unisonfs mount".to_string()),
    };
    let marker_text = super::marker::format_marker(&marker);
    let marker_path = mount_path.join(super::marker::MARKER_FILENAME);

    // Report ready
    let docs_synced = fs.remote_count();
    reporter.report(StartupProgress::ready(docs_synced, reporter.elapsed()));

    // Mount (blocks until unmount signal)
    match backend {
        MountBackend::Fuse => {
            let mp = mount_path.clone();
            let join = tokio::task::spawn_blocking(move || {
                unisonfs_core::mount::fuse::mount(fs_dyn, &mp, rt)
            });
            // Write marker after mount is ready
            let _ = std::fs::write(&marker_path, &marker_text);
            join.await??;
        }
        MountBackend::Nfs => {
            let mp = mount_path.clone();
            unisonfs_core::mount::nfs::mount(fs_dyn, &mp, rt.clone()).await?;
            let _ = std::fs::write(&marker_path, &marker_text);
        }
    }

    // --- Shutdown ---
    let _ = shutdown_tx.send(true);

    // Drain remaining tasks with a timeout
    let drain = async {
        while task_set.join_next().await.is_some() {}
    };
    tokio::time::timeout(Duration::from_secs(30), drain).await.ok();

    SyncEngine::unmount_scan(&fs).await;

    // Agent hint removal
    if !config.no_agent_hint {
        if let Err(e) = unisonfs_core::agent_hint::uninstall(tag) {
            tracing::warn!("agent hint remove failed: {e}");
        }
    }

    // Remove marker
    let _ = std::fs::remove_file(&marker_path);
    reporter.cleanup();
    unisonfs_core::daemon::cleanup_stale(tag);

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

