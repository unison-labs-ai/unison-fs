//! API error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("authentication failed (invalid or expired token)")]
    Auth,

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("not found")]
    NotFound,

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("fs contract violation: {0}")]
    FsContract(String),

    #[error("rate limited — retry after backoff")]
    RateLimited,

    #[error("server error ({status}): {body}")]
    Server { status: u16, body: String },

    #[error("request rejected ({status}): {body}")]
    Rejected { status: u16, body: String },

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
}

impl ApiError {
    /// Whether this error is recoverable with a retry.
    pub fn is_retryable(&self) -> bool {
        matches!(self, ApiError::RateLimited | ApiError::Server { .. })
    }
}
