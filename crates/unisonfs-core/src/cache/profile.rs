//! Virtual read-only `profile.md` — surfaces the Unison brain entity/fact
//! graph as a human-readable summary at the mount root.
//!
//! Backed by `GET /v1/brain/profile`. Populated at mount startup via
//! [`ProfileFile::warm`] and remains static for the lifetime of the mount
//! (restart to refresh). Read-only: writes return `PermissionDenied`.

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;

use crate::api::{ApiClient, ProfileResp};
use crate::cache::Db;
use crate::vfs::error::{VfsError, VfsResult};
use crate::vfs::mode::S_IFREG;
use crate::vfs::types::{FileAttr, Timestamp};

pub const PROFILE_INO: u64 = u64::MAX - 1;
pub const PROFILE_NAME: &str = "profile.md";

/// Pull silence after which profile.md starts carrying a staleness banner.
/// Sync failures are otherwise WARN-only in a detached daemon's log — this is
/// the one place a reader of the mount actually sees that the mirror stopped.
const STALE_AFTER_MS: i64 = 10 * 60 * 1000;

#[derive(Debug)]
pub struct ProfileFile {
    api: Arc<ApiClient>,
    db: Option<Arc<Db>>,
    cache: RwLock<Option<Vec<u8>>>,
}

impl ProfileFile {
    pub fn new(api: Arc<ApiClient>, db: Option<Arc<Db>>) -> Self {
        Self {
            api,
            db,
            cache: RwLock::new(None),
        }
    }

    /// Banner prepended to profile.md while the sync loop is silently failing.
    fn staleness_banner(&self) -> Option<Vec<u8>> {
        let db = self.db.as_ref()?;
        let last: i64 = db
            .sync_meta_get(crate::sync::pull::SYNC_META_LAST_PULL_AT)?
            .parse()
            .ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let silent_ms = now - last;
        if silent_ms < STALE_AFTER_MS {
            return None;
        }
        Some(
            format!(
                "⚠ SYNC STALE: last successful brain sync was {} minutes ago — \
                 mounted files may not reflect the brain's current state.\n\n",
                silent_ms / 60_000
            )
            .into_bytes(),
        )
    }

    /// The bytes read() serves: staleness banner (if any) + cached profile.
    fn content_bytes(&self) -> Vec<u8> {
        let cache = self.cache.read();
        let body = cache.as_deref().unwrap_or(&[]);
        match self.staleness_banner() {
            Some(mut banner) => {
                banner.extend_from_slice(body);
                banner
            }
            None => body.to_vec(),
        }
    }

    /// Fetch the profile from the API and populate the in-memory cache.
    pub async fn warm(&self) {
        match self.api.get_profile().await {
            Ok(resp) => {
                *self.cache.write() = Some(format_profile(&resp).into_bytes());
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "profile warm failed; profile.md will be empty until next mount"
                );
            }
        }
    }

    pub fn profile_attr(&self) -> FileAttr {
        let now = Timestamp::now();
        // Sized over the same view read() serves (banner included) so a
        // reader never gets a short read or trailing garbage.
        let size = self.content_bytes().len() as u64;
        FileAttr {
            ino: PROFILE_INO,
            mode: S_IFREG | 0o444,
            nlink: 1,
            uid: 0,
            gid: 0,
            size,
            blocks: size.div_ceil(512),
            atime: now,
            mtime: now,
            ctime: now,
            rdev: 0,
            blksize: 4096,
        }
    }
}

#[async_trait]
impl crate::vfs::traits::File for ProfileFile {
    async fn read(&self, offset: u64, size: usize) -> VfsResult<Vec<u8>> {
        let content = self.content_bytes();
        let offset = offset as usize;
        if offset >= content.len() {
            return Ok(Vec::new());
        }
        let end = (offset + size).min(content.len());
        Ok(content[offset..end].to_vec())
    }

    async fn write(&self, _offset: u64, _data: &[u8]) -> VfsResult<u32> {
        Err(VfsError::PermissionDenied)
    }

    async fn truncate(&self, _size: u64) -> VfsResult<()> {
        Err(VfsError::PermissionDenied)
    }

    async fn flush(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn fsync(&self) -> VfsResult<()> {
        Ok(())
    }

    async fn getattr(&self) -> VfsResult<FileAttr> {
        Ok(self.profile_attr())
    }
}

fn format_profile(resp: &ProfileResp) -> String {
    let mut out = String::new();
    out.push_str("# Brain Profile\n");
    out.push_str("# This file is auto-generated from your Unison brain.\n");
    out.push_str("# It is read-only. Edit source files to update.\n\n");

    if let Some(statics) = &resp.profile.static_memories {
        if !statics.is_empty() {
            out.push_str("## Core Knowledge\n");
            for mem in statics {
                out.push_str(&format!("- {mem}\n"));
            }
            out.push('\n');
        }
    }

    if let Some(dynamics) = &resp.profile.dynamic {
        if !dynamics.is_empty() {
            out.push_str("## Recent Context\n");
            for mem in dynamics {
                out.push_str(&format!("- {mem}\n"));
            }
        }
    }

    if out.lines().count() <= 4 {
        out.push_str("(No memories yet. Write files to the brain to generate memories.)\n");
    }

    out
}
