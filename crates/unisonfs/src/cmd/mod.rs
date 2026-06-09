//! Subcommand dispatch.

use anyhow::Result;
use clap::Subcommand;

pub mod auth;
pub mod daemon_inner;
pub mod daemon_runtime;
pub mod grep;
pub mod init;
pub mod install;
pub mod list;
pub mod login;
pub mod logout;
pub mod logs;
pub mod marker;
pub mod mount;
pub mod provision;
pub mod startup;
pub mod status;
pub mod sync;
pub mod unmount;
pub mod whoami;

#[derive(Subcommand)]
pub enum Command {
    /// Authenticate with the Unison brain (stores UNISON_TOKEN)
    Login(login::Args),

    /// Show the currently-authenticated user and tenant
    Whoami(whoami::Args),

    /// Mount the Unison brain at a local path
    Mount(mount::Args),

    /// Unmount a running unisonfs mount
    Unmount(unmount::Args),

    /// Show status of the running daemon
    Status(status::Args),

    /// List all running unisonfs mounts
    List,

    /// Tail a running daemon's log
    Logs(logs::Args),

    /// Semantic search across the brain
    Grep(grep::Args),

    /// Install the sgrep shell wrapper for transparent semantic search
    Init(init::Args),

    /// Remove stored credentials
    Logout(logout::Args),

    /// Force a sync cycle now
    Sync(sync::Args),

    /// Provision a new headless Unison account (machine-auth flow)
    Provision(provision::Args),

    /// Install the unisonfs binary to ~/.local/bin
    Install(InstallArgs),

    /// [hidden] Inner daemon process (spawned by mount --daemon)
    #[command(hide = true, name = "daemon-inner")]
    DaemonInner(daemon_inner::DaemonConfig),
}

#[derive(clap::Args, Debug)]
pub struct InstallArgs {
    /// Override the install directory (default: ~/.local/bin).
    #[arg(long)]
    pub dir: Option<std::path::PathBuf>,
}

pub async fn dispatch(cmd: Command) -> Result<()> {
    match cmd {
        Command::Login(args) => login::run(args).await,
        Command::Whoami(args) => whoami::run(args).await,
        Command::Mount(args) => mount::run(args).await,
        Command::Grep(args) => grep::run(args).await,
        Command::Init(args) => init::run(args).await,
        Command::Logout(args) => logout::run(args).await,
        Command::Unmount(args) => unmount::run(args).await,
        Command::Status(args) => status::run(args).await,
        Command::List => list::run().await,
        Command::Logs(args) => logs::run(args).await,
        Command::Sync(args) => sync::run(args).await,
        Command::Provision(args) => provision::run(args).await,
        Command::Install(args) => install::run(args.dir),
        Command::DaemonInner(config) => daemon_inner::run(config).await,
    }
}
