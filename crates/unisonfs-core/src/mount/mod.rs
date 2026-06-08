//! Mount backends — FUSE (Linux) and NFS (macOS/Linux).

use std::str::FromStr;

/// Available mount backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountBackend {
    Fuse,
    Nfs,
}

impl MountBackend {
    /// Platform default: FUSE on Linux, NFS on macOS.
    pub fn default_for_platform() -> Self {
        #[cfg(target_os = "linux")]
        return MountBackend::Fuse;
        #[cfg(not(target_os = "linux"))]
        return MountBackend::Nfs;
    }
}

impl Default for MountBackend {
    fn default() -> Self {
        Self::default_for_platform()
    }
}

impl FromStr for MountBackend {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "fuse" => Ok(MountBackend::Fuse),
            "nfs" => Ok(MountBackend::Nfs),
            other => anyhow::bail!("unknown mount backend '{}'; choose 'fuse' or 'nfs'", other),
        }
    }
}

impl std::fmt::Display for MountBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountBackend::Fuse => write!(f, "fuse"),
            MountBackend::Nfs => write!(f, "nfs"),
        }
    }
}

pub mod fuse;
pub mod nfs;
