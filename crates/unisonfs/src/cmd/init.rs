//! `unisonfs init` — install the `sgrep` shell wrapper.
//!
//! `sgrep` routes semantic searches through the Unison brain API.
//! Outside a mount, it behaves like `unisonfs grep`.

use anyhow::Result;
use clap::Args as ClapArgs;
use std::io::Write;

#[derive(ClapArgs, Debug)]
pub struct Args {
    /// Shell to install for (bash or zsh). Auto-detected if omitted.
    #[arg(long)]
    pub shell: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let shell = args.shell.unwrap_or_else(detect_shell);

    let wrapper = r#"
# sgrep — semantic grep over the Unison brain
# Installed by `unisonfs init`. Semantic when called without flags.
sgrep() {
    unisonfs grep "$@"
}
"#;

    let rc_path = match shell.as_str() {
        "bash" => {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home).join(".bashrc")
        }
        "zsh" => {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(home).join(".zshrc")
        }
        other => {
            anyhow::bail!("unsupported shell '{}'; run manually:\n{}", other, wrapper.trim());
        }
    };

    // Append only if not already there
    let existing = std::fs::read_to_string(&rc_path).unwrap_or_default();
    if existing.contains("sgrep — semantic grep") {
        eprintln!("sgrep wrapper already installed in {}", rc_path.display());
        return Ok(());
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rc_path)?;
    writeln!(file, "{wrapper}")?;

    eprintln!("Installed sgrep wrapper in {}.", rc_path.display());
    eprintln!("Reload your shell: source {}", rc_path.display());
    Ok(())
}

fn detect_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if shell.contains("zsh") {
            return "zsh".to_string();
        }
        if shell.contains("bash") {
            return "bash".to_string();
        }
    }
    "bash".to_string()
}
