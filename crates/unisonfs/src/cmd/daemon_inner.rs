//! Hidden `daemon-inner` subcommand.
//!
//! This is the child process spawned by `unisonfs mount --daemon`. It:
//! 1. Detaches from the terminal (`setsid`).
//! 2. Redirects stdio to the daemon log file.
//! 3. Writes its PID to the PID file.
//! 4. Calls into [`super::daemon_runtime::run_daemon`].
//!
//! The parent watches `<state_dir>/<tag>.startup` and the IPC socket for a
//! sign of life, then exits once the daemon reports ready.

use std::path::PathBuf;

use anyhow::Result;

/// Configuration forwarded from the parent `mount` invocation.
#[derive(Debug, clap::Args)]
pub struct DaemonConfig {
    /// Mount point directory.
    #[arg(long)]
    pub mountpoint: PathBuf,

    /// Brain tag (also used as the namespace for daemon state files).
    #[arg(long)]
    pub tag: String,

    /// Log verbosity passed through from the parent.
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Pull interval in seconds.
    #[arg(long, default_value = "30")]
    pub delta_interval_secs: u64,

    /// Deletion scan interval in seconds.
    #[arg(long, default_value = "300")]
    pub deletion_scan_interval_secs: u64,

    /// Memory paths (comma-separated brain path prefixes to sync).
    #[arg(long, default_value = "")]
    pub memory_paths: String,

    /// Skip importing pre-existing local files on mount.
    #[arg(long)]
    pub no_import: bool,

    /// Disable agent hint injection.
    #[arg(long)]
    pub no_agent_hint: bool,

    /// Inactivity timeout in seconds (0 = no timeout).
    #[arg(long, default_value = "0")]
    pub inactivity_timeout_secs: u64,
}

/// Entry point called when the binary is invoked as `unisonfs daemon-inner`.
pub async fn run(config: DaemonConfig) -> Result<()> {
    // Detach from controlling terminal.
    detach_tty();

    // Redirect stdio to log.
    redirect_stdio(&config.tag)?;

    // Write PID file.
    write_pid(&config.tag)?;

    // Run the full daemon lifecycle.
    super::daemon_runtime::run_daemon(config).await
}

fn detach_tty() {
    #[cfg(unix)]
    {
        #[allow(unsafe_code)]
        unsafe {
            let _ = libc::setsid();
        }
    }
}

fn redirect_stdio(tag: &str) -> Result<()> {
    use std::fs::OpenOptions;

    let log_path = unisonfs_core::daemon::log_path(tag);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    // On Unix, dup2 stdin → /dev/null, stdout/stderr → log file.
    #[cfg(unix)]
    {
        use std::os::unix::io::IntoRawFd;

        let log_fd = log_file.into_raw_fd();

        let devnull = OpenOptions::new().read(true).open("/dev/null")?;
        let null_fd = devnull.into_raw_fd();

        #[allow(unsafe_code)]
        unsafe {
            libc::dup2(null_fd, libc::STDIN_FILENO);
            libc::dup2(log_fd, libc::STDOUT_FILENO);
            libc::dup2(log_fd, libc::STDERR_FILENO);
            libc::close(null_fd);
            libc::close(log_fd);
        }
    }

    Ok(())
}

fn write_pid(tag: &str) -> Result<()> {
    let pid_path = unisonfs_core::daemon::pid_path(tag);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let pid = std::process::id();
    std::fs::write(&pid_path, pid.to_string())?;
    Ok(())
}
