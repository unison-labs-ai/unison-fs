//! Self-install: copy the running binary to `~/.local/bin/unisonfs`.

use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Install the binary at `src` to `~/.local/bin/unisonfs` (or `dest_dir` if
/// overridden). Prints a PATH hint if the target directory is not in PATH.
pub fn run_install(dest_dir: Option<PathBuf>) -> Result<()> {
    let src = env::current_exe().context("could not determine current executable path")?;

    let dir = dest_dir.unwrap_or_else(default_install_dir);
    fs::create_dir_all(&dir)
        .with_context(|| format!("create install dir {}", dir.display()))?;

    let dest = dir.join("unisonfs");

    // Remove any existing binary first (overwrite fails on some OSes).
    if dest.exists() {
        fs::remove_file(&dest)
            .with_context(|| format!("remove existing binary at {}", dest.display()))?;
    }

    fs::copy(&src, &dest)
        .with_context(|| format!("copy {} → {}", src.display(), dest.display()))?;

    // On Unix, ensure the destination is executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest)?.permissions();
        let mode = perms.mode() | 0o111;
        perms.set_mode(mode);
        fs::set_permissions(&dest, perms)
            .with_context(|| format!("chmod +x {}", dest.display()))?;
    }

    println!("Installed: {}", dest.display());

    // Check PATH and print a shell-specific hint if needed.
    if !is_in_path(&dir) {
        let dir_str = dir.display().to_string();
        let shell = detect_shell();
        let snippet = match shell.as_deref() {
            Some("fish") => format!("fish_add_path \"{}\"", dir_str),
            Some("nu") => format!("$env.PATH = ($env.PATH | append \"{}\")", dir_str),
            _ => format!("export PATH=\"$PATH:{}\"", dir_str),
        };
        let rc = match shell.as_deref() {
            Some("fish") => "~/.config/fish/config.fish",
            Some("zsh") => "~/.zshrc",
            Some("bash") => "~/.bashrc",
            _ => "your shell's rc file",
        };
        eprintln!(
            "\n{dir_str} is not in your PATH.\nAdd it by running:\n\n    {snippet}\n\nor add to {rc}.\n"
        );
    }

    Ok(())
}

fn default_install_dir() -> PathBuf {
    if let Ok(home) = env::var("HOME") {
        return PathBuf::from(home).join(".local").join("bin");
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().join(".local").join("bin");
    }
    PathBuf::from("/usr/local/bin")
}

fn is_in_path(dir: &std::path::Path) -> bool {
    env::var_os("PATH")
        .map(|p| {
            env::split_paths(&p).any(|entry| {
                entry == dir
                    || fs::canonicalize(&entry)
                        .ok()
                        .zip(fs::canonicalize(dir).ok())
                        .is_some_and(|(a, b)| a == b)
            })
        })
        .unwrap_or(false)
}

fn detect_shell() -> Option<String> {
    // Try $SHELL env var.
    if let Ok(shell) = env::var("SHELL") {
        let name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

pub fn run(dest_dir: Option<PathBuf>) -> Result<()> {
    run_install(dest_dir)
}
