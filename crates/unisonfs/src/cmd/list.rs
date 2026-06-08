//! `unisonfs list` — show all running mounts.

use anyhow::Result;

pub async fn run() -> Result<()> {
    let runtime_dir = unisonfs_core::config::runtime_dir();
    if !runtime_dir.exists() {
        println!("No running mounts.");
        return Ok(());
    }

    let mut found = false;
    if let Ok(entries) = std::fs::read_dir(&runtime_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "pid").unwrap_or(false) {
                let tag = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if let Ok(pid_str) = std::fs::read_to_string(&path) {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        if unisonfs_core::daemon::pid_alive(pid) {
                            println!("{tag} (pid {pid})");
                            found = true;
                        }
                    }
                }
            }
        }
    }

    if !found {
        println!("No running mounts.");
    }
    Ok(())
}
