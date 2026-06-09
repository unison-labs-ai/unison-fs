//! `.unisonfs` marker file: tag + path auto-detection for a directory.
#![allow(dead_code)]
//!
//! Placing a `.unisonfs` file inside a directory lets `unisonfs init` and
//! `unisonfs mount` auto-detect the brain tag and memory path scope. The file
//! is a simple TOML-flavoured key = "value" text file:
//!
//! ```text
//! tag = "my-workspace"
//! path = "/private/notes/my-workspace"
//! ```
//!
//! `parse_all_markers` walks up from the current directory looking for the
//! nearest `.unisonfs` and returns the parsed contents.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Contents of a `.unisonfs` marker file.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnisonMarker {
    /// Brain tag (corresponds to the daemon's `--tag` flag).
    pub tag: Option<String>,
    /// Brain memory path scope (corresponds to `--memory-paths`).
    pub path: Option<String>,
    /// Free-form description for human readers.
    pub description: Option<String>,
}

/// Filename used for the marker.
pub const MARKER_FILENAME: &str = ".unisonfs";

/// Format a marker struct into the canonical marker file text.
pub fn format_marker(m: &UnisonMarker) -> String {
    let mut out = String::new();
    if let Some(tag) = &m.tag {
        out.push_str(&format!("tag = \"{}\"\n", tag.replace('"', "\\\"")));
    }
    if let Some(path) = &m.path {
        out.push_str(&format!("path = \"{}\"\n", path.replace('"', "\\\"")));
    }
    if let Some(desc) = &m.description {
        out.push_str(&format!(
            "description = \"{}\"\n",
            desc.replace('"', "\\\"")
        ));
    }
    out
}

/// Parse a `.unisonfs` marker file at `path`.
pub fn parse_marker(path: &Path) -> Result<UnisonMarker> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_marker_text(&text)
}

fn parse_marker_text(text: &str) -> Result<UnisonMarker> {
    let mut m = UnisonMarker::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim().trim_matches('"').replace("\\\"", "\"");
        match key {
            "tag" => m.tag = Some(val),
            "path" => m.path = Some(val),
            "description" => m.description = Some(val),
            _ => {}
        }
    }
    Ok(m)
}

/// Walk up the directory tree from `start` and return the marker + its
/// directory when found, or `None` if no `.unisonfs` exists in the ancestry.
pub fn find_marker(start: &Path) -> Option<(PathBuf, UnisonMarker)> {
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };

    loop {
        let candidate = current.join(MARKER_FILENAME);
        if candidate.exists() {
            let m = parse_marker(&candidate).ok()?;
            return Some((current, m));
        }
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => return None,
        }
    }
}

/// Parse all `.unisonfs` markers visible from the filesystem root downward to
/// `target` (ancestors first). Used to check for conflicting mounts.
pub fn parse_all_markers(target: &Path) -> Vec<(PathBuf, UnisonMarker)> {
    let mut results = Vec::new();
    let mut current = if target.is_dir() {
        target.to_path_buf()
    } else if let Some(p) = target.parent() {
        p.to_path_buf()
    } else {
        return results;
    };

    // Collect ancestors (root → target order).
    let mut ancestors = Vec::new();
    loop {
        ancestors.push(current.clone());
        match current.parent() {
            Some(p) => current = p.to_path_buf(),
            None => break,
        }
    }
    ancestors.reverse();

    for dir in ancestors {
        let candidate = dir.join(MARKER_FILENAME);
        if candidate.exists() {
            if let Ok(m) = parse_marker(&candidate) {
                results.push((dir, m));
            }
        }
    }
    results
}

/// Read the `.unisonfs` marker from the given directory (not walking).
pub fn read_unisonfs_marker(dir: &Path) -> Option<UnisonMarker> {
    let path = dir.join(MARKER_FILENAME);
    if path.exists() {
        parse_marker(&path).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn round_trip_format_parse() {
        let m = UnisonMarker {
            tag: Some("my-tag".to_string()),
            path: Some("/private/notes".to_string()),
            description: Some("test workspace".to_string()),
        };
        let text = format_marker(&m);
        let parsed = parse_marker_text(&text).unwrap();
        assert_eq!(parsed.tag.as_deref(), Some("my-tag"));
        assert_eq!(parsed.path.as_deref(), Some("/private/notes"));
        assert_eq!(parsed.description.as_deref(), Some("test workspace"));
    }

    #[test]
    fn find_marker_walks_up() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("a").join("b");
        fs::create_dir_all(&subdir).unwrap();

        // No marker yet
        assert!(find_marker(&subdir).is_none());

        // Place marker at tmp root
        let marker_path = tmp.path().join(MARKER_FILENAME);
        fs::write(&marker_path, "tag = \"root\"\n").unwrap();
        let (found_dir, m) = find_marker(&subdir).unwrap();
        // The nearest marker going up from subdir is at tmp root
        assert_eq!(m.tag.as_deref(), Some("root"));
        assert_eq!(found_dir, tmp.path());
    }

    #[test]
    fn find_marker_nearest_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let subdir = tmp.path().join("inner");
        fs::create_dir_all(&subdir).unwrap();

        fs::write(tmp.path().join(MARKER_FILENAME), "tag = \"outer\"\n").unwrap();
        fs::write(subdir.join(MARKER_FILENAME), "tag = \"inner\"\n").unwrap();

        let (_, m) = find_marker(&subdir).unwrap();
        assert_eq!(m.tag.as_deref(), Some("inner"), "nearest (inner) should win");
    }

    #[test]
    fn read_unisonfs_marker_direct() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join(MARKER_FILENAME), "tag = \"direct\"\n").unwrap();
        let m = read_unisonfs_marker(tmp.path()).unwrap();
        assert_eq!(m.tag.as_deref(), Some("direct"));
    }

    #[test]
    fn ignores_unknown_keys() {
        let text = "tag = \"x\"\nunknown_key = \"y\"\n";
        let m = parse_marker_text(text).unwrap();
        assert_eq!(m.tag.as_deref(), Some("x"));
    }
}
