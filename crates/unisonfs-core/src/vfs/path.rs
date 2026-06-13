//! Path normalization helpers for brain document paths.
//!
//! The Unison brain enforces a filesystem contract:
//! - Writable roots: /private/..., /workspace/..., /workspace/teams/<slug>/...
//! - Read-only roots: /system/..., /sources/...
//! - All paths must end in .md
//!
//! Unqualified paths are rewritten to /private/notes/<slug>.md

/// Normalize a brain document path.
///
/// - If the path is already under a valid writable root, return it unchanged.
/// - If it's a bare filename without a leading slash, rewrite to /private/notes/<slug>.md
/// - If it's a root-level .md file (one path segment), rewrite to /private/notes/<name>.md
pub fn normalize_brain_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim_start_matches('/');

    // Already under a writable root — pass through
    if raw.starts_with("/private/")
        || raw.starts_with("/workspace/")
        || raw.starts_with("/wiki/")
        || raw.starts_with("/skills/")
    {
        return Some(raw.to_string());
    }

    // Under a read-only root — reject
    if raw.starts_with("/system/") || raw.starts_with("/sources/") {
        return None;
    }

    // Bare filename or relative path — rewrite to /private/notes/<slug>.md
    let basename = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let slug = slugify(basename.trim_end_matches(".md"));
    Some(format!("/private/notes/{slug}.md"))
}

/// Convert a display name to a URL-safe slug.
fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Return true if the path is under a writable brain root.
pub fn is_writable_path(path: &str) -> bool {
    path.starts_with("/private/")
        || path.starts_with("/workspace/")
        || path.starts_with("/wiki/")
        || path.starts_with("/skills/")
}

/// Return true if the path is under a read-only brain root.
pub fn is_readonly_path(path: &str) -> bool {
    path.starts_with("/system/") || path.starts_with("/sources/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_path_passes_through() {
        assert_eq!(
            normalize_brain_path("/private/notes/foo.md"),
            Some("/private/notes/foo.md".to_string())
        );
    }

    #[test]
    fn workspace_path_passes_through() {
        assert_eq!(
            normalize_brain_path("/workspace/people/daniel.md"),
            Some("/workspace/people/daniel.md".to_string())
        );
    }

    #[test]
    fn workspace_teams_path_passes_through() {
        assert_eq!(
            normalize_brain_path("/workspace/teams/eng/docs/arch.md"),
            Some("/workspace/teams/eng/docs/arch.md".to_string())
        );
    }

    #[test]
    fn system_path_rejected() {
        assert_eq!(normalize_brain_path("/system/search/semantic/foo.md"), None);
    }

    #[test]
    fn bare_name_rewrites_to_private_notes() {
        assert_eq!(
            normalize_brain_path("my note"),
            Some("/private/notes/my-note.md".to_string())
        );
    }
}
