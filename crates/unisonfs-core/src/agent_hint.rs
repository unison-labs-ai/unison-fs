//! Inject and remove path-scoped semantic-search hints in agent instruction
//! files (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`, `~/.gemini/GEMINI.md`).
//!
//! Each `unisonfs mount` writes a delimited block scoped to the absolute mount
//! path, telling Claude Code / Codex / Gemini CLI to use `unisonfs grep` when
//! searching inside that path. `unisonfs unmount` removes the block. Multiple
//! mounts coexist via per-tag delimiters; mount opportunistically sweeps
//! orphan blocks left by daemons that crashed without unmounting.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::daemon;

const BEGIN_PREFIX: &str = "<!-- >>> unisonfs:";
const BEGIN_SUFFIX: &str = ":begin >>> -->";
const END_PREFIX: &str = "<!-- <<< unisonfs:";
const END_SUFFIX: &str = ":end <<< -->";

/// One target instruction file we may write to.
#[derive(Debug, Clone)]
pub struct Target {
    pub path: PathBuf,
    pub agent: &'static str,
}

/// Sanitize a tag for use inside an HTML comment delimiter (no `--`).
fn sanitize_for_delim(tag: &str) -> String {
    tag.replace("--", "__")
}

fn begin_marker(tag: &str) -> String {
    format!("{BEGIN_PREFIX}{}{BEGIN_SUFFIX}", sanitize_for_delim(tag))
}

fn end_marker(tag: &str) -> String {
    format!("{END_PREFIX}{}{END_SUFFIX}", sanitize_for_delim(tag))
}

/// Compute the agent instruction files we might write to.
/// Only touches each file if its parent directory already exists.
pub fn discover_targets() -> Vec<Target> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    vec![
        Target {
            path: claude_dir.join("CLAUDE.md"),
            agent: "Claude Code",
        },
        Target {
            path: home.join(".codex").join("AGENTS.md"),
            agent: "Codex",
        },
        Target {
            path: home.join(".gemini").join("GEMINI.md"),
            agent: "Gemini CLI",
        },
    ]
}

fn home_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf())
}

fn render_block(tag: &str, mount_path: &Path) -> String {
    let begin = begin_marker(tag);
    let end = end_marker(tag);
    let path_str = mount_path.display();
    format!(
        "{begin}\n\
         <!-- managed by `unisonfs mount`; auto-removed on `unisonfs unmount` -->\n\
         The directory `{path_str}/` is a Unison brain mount with semantic search.\n\
         When searching inside this directory, use:\n\
         \n\
         \u{0020}   unisonfs grep \"<natural language query>\" {path_str}/\n\
         \n\
         instead of grep, rg, find, or your built-in search tool. It returns\n\
         semantically relevant excerpts via a vector index. For a quick high-\n\
         level overview before searching, read `{path_str}/profile.md` — it's\n\
         a reserved virtual file at the mount root with a summary of what\n\
         this brain holds. Files outside this directory behave normally —\n\
         this rule is scoped to that path only.\n\
         {end}\n"
    )
}

/// Install the hint into every detected agent file. Idempotent.
pub fn install(tag: &str, mount_path: &Path) -> Result<Vec<PathBuf>> {
    let block = render_block(tag, mount_path);
    let mut written = Vec::new();
    for target in discover_targets() {
        let Some(parent) = target.path.parent() else {
            continue;
        };
        if !parent.exists() {
            continue;
        }
        match write_block(&target.path, tag, &block) {
            Ok(true) => written.push(target.path),
            Ok(false) => {}
            Err(e) => tracing::warn!(path=?target.path, error=%e, "failed to install hint"),
        }
    }
    Ok(written)
}

/// Remove the hint for `tag` from every detected agent file.
pub fn uninstall(tag: &str) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for target in discover_targets() {
        if !target.path.exists() {
            continue;
        }
        match remove_block(&target.path, tag) {
            Ok(true) => written.push(target.path),
            Ok(false) => {}
            Err(e) => tracing::warn!(path=?target.path, error=%e, "failed to remove hint"),
        }
    }
    Ok(written)
}

/// Scan every agent file for `unisonfs:*:` blocks; remove any whose tag has
/// no live daemon. `keep_tag` prevents sweeping an in-flight remount.
pub type SweepReport = Vec<(PathBuf, Vec<String>)>;

pub fn sweep_orphans(keep_tag: Option<&str>) -> Result<SweepReport> {
    let mut report = Vec::new();
    for target in discover_targets() {
        if !target.path.exists() {
            continue;
        }
        match sweep_one(&target.path, keep_tag) {
            Ok(cleaned) if !cleaned.is_empty() => report.push((target.path, cleaned)),
            Ok(_) => {}
            Err(e) => tracing::warn!(path=?target.path, error=%e, "failed to sweep orphans"),
        }
    }
    Ok(report)
}

fn sweep_one(path: &Path, keep_tag: Option<&str>) -> Result<Vec<String>> {
    let mut text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let blocks = find_all_blocks(&text);
    let mut cleaned = Vec::new();
    for tag in blocks {
        if Some(tag.as_str()) == keep_tag {
            continue;
        }
        let alive = daemon::read_pid(&tag).is_some_and(daemon::pid_alive);
        if alive {
            continue;
        }
        text = strip_block(&text, &tag);
        cleaned.push(tag);
    }
    if cleaned.is_empty() {
        return Ok(cleaned);
    }
    let final_text = trim_trailing_blank_lines(&text);
    atomic_write(path, &final_text)?;
    Ok(cleaned)
}

fn write_block(path: &Path, tag: &str, new_block: &str) -> Result<bool> {
    let original = if path.exists() {
        fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?
    } else {
        String::new()
    };

    let stripped = strip_block(&original, tag);
    let mut updated = stripped.trim_end_matches('\n').to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(new_block);

    if updated == original {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    with_file_lock(path, || atomic_write(path, &updated))?;
    Ok(true)
}

fn remove_block(path: &Path, tag: &str) -> Result<bool> {
    let original = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let stripped = strip_block(&original, tag);
    let final_text = trim_trailing_blank_lines(&stripped);
    if final_text == original {
        return Ok(false);
    }
    with_file_lock(path, || atomic_write(path, &final_text))?;
    Ok(true)
}

fn strip_block(text: &str, tag: &str) -> String {
    let begin = begin_marker(tag);
    let end = end_marker(tag);

    let Some(begin_idx) = text.find(&begin) else {
        return text.to_string();
    };
    let after_begin = begin_idx + begin.len();
    let Some(end_rel) = text[after_begin..].find(&end) else {
        return text.to_string();
    };
    let end_idx = after_begin + end_rel + end.len();

    let mut tail_start = end_idx;
    if text.as_bytes().get(tail_start) == Some(&b'\n') {
        tail_start += 1;
    }
    while text.as_bytes().get(tail_start) == Some(&b'\n') {
        tail_start += 1;
    }

    let mut head_end = begin_idx;
    while head_end > 0 && text.as_bytes()[head_end - 1] == b'\n' {
        head_end -= 1;
    }
    if head_end > 0 {
        head_end += 1;
    }

    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..head_end]);
    out.push_str(&text[tail_start..]);
    out
}

fn find_all_blocks(text: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find(BEGIN_PREFIX) {
        let abs = cursor + rel;
        let tag_start = abs + BEGIN_PREFIX.len();
        let Some(suffix_rel) = text[tag_start..].find(BEGIN_SUFFIX) else {
            break;
        };
        let tag = &text[tag_start..tag_start + suffix_rel];
        tags.push(tag.to_string());
        cursor = tag_start + suffix_rel + BEGIN_SUFFIX.len();
    }
    tags
}

fn trim_trailing_blank_lines(s: &str) -> String {
    let trimmed = s.trim_end_matches('\n').to_string();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

fn atomic_write(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.ufs.tmp"),
        None => "ufs.tmp".to_string(),
    });
    fs::write(&tmp, content).with_context(|| format!("write tmp {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("rename {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

fn with_file_lock<F, T>(path: &Path, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let lock_path = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.ufs.lock"),
        None => "ufs.lock".to_string(),
    });
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))?;

    fs2::FileExt::lock_exclusive(&lock_file)
        .map_err(|e: io::Error| anyhow::anyhow!("lock {}: {e}", lock_path.display()))?;
    let result = f();
    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fake_home(tmp: &Path) {
        env::set_var("HOME", tmp);
        env::remove_var("CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn round_trip_install_then_uninstall() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fake_home(tmp.path());
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();

        let written = install("test_tag", Path::new("/Users/x/mem")).unwrap();
        assert_eq!(written.len(), 1);
        let file = tmp.path().join(".claude/CLAUDE.md");
        let content = fs::read_to_string(&file).unwrap();
        assert!(content.contains(">>> unisonfs:test_tag:begin >>>"));
        assert!(content.contains("<<< unisonfs:test_tag:end <<<"));
        assert!(content.contains("/Users/x/mem"));

        let removed = uninstall("test_tag").unwrap();
        assert_eq!(removed.len(), 1);
        let after = fs::read_to_string(&file).unwrap();
        assert!(!after.contains("unisonfs:test_tag"));
    }

    #[test]
    fn install_is_idempotent() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fake_home(tmp.path());
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();

        let first = install("t", Path::new("/m")).unwrap();
        assert_eq!(first.len(), 1);
        let second = install("t", Path::new("/m")).unwrap();
        assert!(second.is_empty(), "second install should be a no-op");

        let content = fs::read_to_string(tmp.path().join(".claude/CLAUDE.md")).unwrap();
        assert_eq!(content.matches(">>> unisonfs:t:begin >>>").count(), 1);
    }

    #[test]
    fn install_preserves_user_content() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fake_home(tmp.path());
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let user_text = "# My CLAUDE.md\n\nDon't touch this.\n";
        let file = tmp.path().join(".claude/CLAUDE.md");
        fs::write(&file, user_text).unwrap();

        install("c", Path::new("/m")).unwrap();
        let after = fs::read_to_string(&file).unwrap();
        assert!(after.starts_with(user_text));
        assert!(after.contains(">>> unisonfs:c:begin >>>"));

        uninstall("c").unwrap();
        let cleaned = fs::read_to_string(&file).unwrap();
        assert_eq!(cleaned, user_text);
    }

    #[test]
    fn multiple_tags_coexist() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fake_home(tmp.path());
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();

        install("a", Path::new("/m/a")).unwrap();
        install("b", Path::new("/m/b")).unwrap();

        let content = fs::read_to_string(tmp.path().join(".claude/CLAUDE.md")).unwrap();
        assert!(content.contains(">>> unisonfs:a:begin >>>"));
        assert!(content.contains(">>> unisonfs:b:begin >>>"));

        uninstall("a").unwrap();
        let after = fs::read_to_string(tmp.path().join(".claude/CLAUDE.md")).unwrap();
        assert!(!after.contains("unisonfs:a"));
        assert!(after.contains(">>> unisonfs:b:begin >>>"));
    }

    #[test]
    fn skips_uninstalled_agents() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fake_home(tmp.path());
        let written = install("t", Path::new("/m")).unwrap();
        assert!(written.is_empty());
    }

    #[test]
    fn uninstall_when_block_absent_is_no_op() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fake_home(tmp.path());
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();
        let user_text = "user content\n";
        let file = tmp.path().join(".claude/CLAUDE.md");
        fs::write(&file, user_text).unwrap();

        let written = uninstall("nonexistent").unwrap();
        assert!(written.is_empty());
        assert_eq!(fs::read_to_string(&file).unwrap(), user_text);
    }

    #[test]
    fn sanitizes_double_hyphen_in_delimiter() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        fake_home(tmp.path());
        fs::create_dir_all(tmp.path().join(".claude")).unwrap();

        install("my--ctr", Path::new("/m")).unwrap();
        let content = fs::read_to_string(tmp.path().join(".claude/CLAUDE.md")).unwrap();
        assert!(content.contains(">>> unisonfs:my__ctr:begin >>>"));
        uninstall("my--ctr").unwrap();
        let after = fs::read_to_string(tmp.path().join(".claude/CLAUDE.md")).unwrap();
        assert!(!after.contains("my__ctr"));
    }

    #[test]
    fn find_all_blocks_lists_tags() {
        let text = "\
            <!-- >>> unisonfs:a:begin >>> -->\nx\n<!-- <<< unisonfs:a:end <<< -->\n\
            \n\
            <!-- >>> unisonfs:b:begin >>> -->\ny\n<!-- <<< unisonfs:b:end <<< -->\n";
        let tags = find_all_blocks(text);
        assert_eq!(tags, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn strip_block_tolerates_malformed() {
        let text = "<!-- >>> unisonfs:a:begin >>> -->\nno end marker\n";
        assert_eq!(strip_block(text, "a"), text);
    }
}
