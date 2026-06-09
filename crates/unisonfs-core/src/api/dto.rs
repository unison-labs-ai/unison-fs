//! Data transfer objects for the Unison brain REST API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Metadata map for source attribution.
pub type MetadataMap = HashMap<String, serde_json::Value>;

// ─── Auth DTOs ────────────────────────────────────────────────────────────────

/// Response from POST /v1/auth/provision
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionResp {
    pub api_key: String,
    pub tenant_id: String,
    pub status: String,
    pub email_sent: bool,
    pub message: Option<String>,
}

/// Response from POST /v1/auth/verify
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResp {
    pub verified: bool,
    pub tenant_id: Option<String>,
    /// Only present on key recovery (already-verified account).
    pub api_key: Option<String>,
}

// ─── Brain document DTOs ─────────────────────────────────────────────────────

/// A Unison brain document.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainDocument {
    pub id: String,
    pub path: String,
    pub title: Option<String>,
    pub tldr: Option<String>,
    pub kind: Option<String>,
    pub body_md: Option<String>,
    pub tags: Option<Vec<String>>,
    pub visibility: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub content_hash: Option<String>,
}

/// PUT /v1/brain/doc — write/create document.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PutDocReq {
    pub path: String,
    pub body_md: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tldr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    /// Hex-16 content hash for optimistic concurrency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_content_hash: Option<String>,
}

/// PATCH /v1/brain/doc — surgical in-place edit.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchDocReq {
    pub path: String,
    pub old_str: String,
    pub new_str: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_content_hash: Option<String>,
}

/// Response from DELETE /v1/brain/doc
#[derive(Debug, Deserialize)]
pub struct DeleteDocResp {
    pub deleted: bool,
}

/// GET /v1/brain/list parameters.
#[derive(Debug, Default)]
pub struct ListDocsReq {
    pub prefix: Option<String>,
    pub kind: Vec<String>,
    pub tag: Vec<String>,
    pub limit: Option<u32>,
}

/// Response from GET /v1/brain/list
#[derive(Debug, Deserialize)]
pub struct ListDocsResp {
    pub documents: Vec<BrainDocument>,
}

/// GET /v1/brain/fs response
#[derive(Debug, Deserialize)]
pub struct FsListResp {
    pub entries: Vec<FsEntry>,
}

/// A single entry from GET /v1/brain/fs
#[derive(Debug, Deserialize)]
pub struct FsEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub entry_type: String, // "dir" | "file"
    pub mtime: Option<String>,
    pub path: Option<String>,
}

/// Response from GET /v1/brain/fs/read
#[derive(Debug, Deserialize)]
pub struct FsReadResp {
    pub content: Option<String>,
    pub path: String,
}

/// GET /v1/brain/search parameters.
#[derive(Debug, Default)]
pub struct SearchReq {
    pub q: String,
    pub k: Option<u32>,
    pub kind: Vec<String>,
    pub memory_type: Option<String>,
    pub as_of: Option<String>,
}

/// Response from GET /v1/brain/search
#[derive(Debug, Deserialize)]
pub struct SearchResp {
    pub results: Vec<SearchResult>,
}

/// A single search result.
#[derive(Debug, Deserialize)]
pub struct SearchResult {
    pub doc: BrainDocument,
    pub score: f64,
    pub highlight: Option<String>,
}

/// Response from GET /v1/brain/grep
#[derive(Debug, Deserialize)]
pub struct GrepResp {
    pub results: Vec<BrainDocument>,
}

/// POST /v1/brain/doc/tag
#[derive(Debug, Serialize)]
pub struct TagDocReq {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>,
}

/// GET /v1/brain/status
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainStatus {
    pub doc_count: Option<u64>,
    pub doc_with_embedding: Option<u64>,
    pub entity_count: Option<u64>,
    pub fact_count: Option<u64>,
    pub last_ingest_at: Option<String>,
    pub pending_jobs: Option<u64>,
    pub stale_wiki_page_count: Option<u64>,
}

/// GET /v1/brain/neighbors response
#[derive(Debug, Deserialize)]
pub struct NeighborsResp {
    pub documents: Vec<BrainDocument>,
}

// ─── Entity DTOs ─────────────────────────────────────────────────────────────

/// A Unison brain entity.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainEntity {
    pub id: String,
    pub kind: String,
    pub display_name: String,
    pub slug: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub status: Option<String>,
    pub props: Option<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

/// GET /v1/brain/entities/resolve response
#[derive(Debug, Deserialize)]
pub struct ResolveEntityResp {
    pub entity: Option<BrainEntity>,
}

/// POST /v1/brain/entities
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertEntityReq {
    pub kind: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

// ─── Fact DTOs ────────────────────────────────────────────────────────────────

/// A Unison brain fact.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainFact {
    pub id: String,
    pub subject_id: String,
    pub predicate: String,
    pub fact_text: String,
    pub confidence: f64,
    pub valid_from: Option<String>,
    pub valid_to: Option<String>,
    pub object_json: Option<serde_json::Value>,
    pub object_entity_id: Option<String>,
    pub supersedes_id: Option<String>,
    pub created_at: String,
}

/// GET /v1/brain/entities/:id/facts response
#[derive(Debug, Deserialize)]
pub struct FactsResp {
    pub facts: Vec<BrainFact>,
}

/// GET /v1/brain/profile — summary of the user's brain contents.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileResp {
    pub profile: BrainProfile,
}

/// Nested profile data.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainProfile {
    /// Long-lived core facts about the user.
    pub static_memories: Option<Vec<String>>,
    /// Recent contextual signals.
    pub dynamic: Option<Vec<String>>,
}

/// POST /v1/brain/facts
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordFactReq {
    pub subject_id: String,
    pub predicate: String,
    pub fact_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_json: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_entity_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supersedes_id: Option<String>,
}
