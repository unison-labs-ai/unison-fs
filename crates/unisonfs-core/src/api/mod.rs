//! Unison brain HTTP API client.
//!
//! Typed wrapper over the Unison brain REST endpoints at /v1/*.
//! One client per mount. Retries network errors and 5xx with
//! exponential backoff; surfaces 4xx as typed errors without retrying.

pub mod dto;
pub mod error;

pub use dto::*;
pub use error::ApiError;

use reqwest::{Client, RequestBuilder, Response, StatusCode};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const MAX_RETRIES: u32 = 5;
const INITIAL_BACKOFF_MS: u64 = 100;

/// Default Unison API base URL.
pub const DEFAULT_API_URL: &str = "https://api.unisonlabs.ai";

/// Unison brain API client.
///
/// Wraps reqwest, injects `Authorization: Bearer <token>` on every request,
/// and provides typed methods for all brain REST endpoints.
pub struct ApiClient {
    http: Client,
    base_url: String,
    token: String,
    /// User info from /v1/auth/whoami, stamped on writes.
    user_id: Option<String>,
    /// Write-side call counter for tests.
    write_calls: AtomicU32,
}

/// Auth session info from GET /v1/auth/whoami.
#[derive(Debug, Clone)]
pub struct WhoamiInfo {
    pub user_id: String,
    pub user_email: String,
    pub tenant_id: String,
    pub tenant_name: String,
    pub tenant_verified: bool,
    pub scopes: Vec<String>,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl ApiClient {
    /// Create a new API client.
    ///
    /// `base_url` — override with `UNISON_API_URL` env var or default to
    /// `https://api.unisonlabs.ai`.
    /// `token` — the `usk_live_...` key from `UNISON_TOKEN`.
    pub fn new(base_url: &str, token: &str) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("failed to build HTTP client");

        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            user_id: None,
            write_calls: AtomicU32::new(0),
        }
    }

    pub fn with_user_id(mut self, user_id: String) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Number of write-side HTTP calls since the counter was last read.
    pub fn write_calls(&self) -> u32 {
        self.write_calls.load(Ordering::Relaxed)
    }

    // ─── Auth ──────────────────────────────────────────────────────────────

    /// GET /v1/auth/whoami — verify the token and return user/tenant info.
    pub async fn whoami(&self) -> Result<WhoamiInfo, ApiError> {
        let resp: serde_json::Value = self
            .get("/v1/auth/whoami")
            .send_with_retry()
            .await?
            .parse_json()
            .await?;

        let user_id = resp["user"]["id"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let user_email = resp["user"]["email"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let tenant_id = resp["tenant"]["id"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let tenant_name = resp["tenant"]["name"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let tenant_verified = resp["tenant"]["verified"].as_bool().unwrap_or(false);
        let scopes: Vec<String> = resp["scopes"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        Ok(WhoamiInfo {
            user_id,
            user_email,
            tenant_id,
            tenant_name,
            tenant_verified,
            scopes,
        })
    }

    /// POST /v1/auth/provision — create a new headless account.
    pub async fn provision(base_url: &str, email: &str) -> Result<ProvisionResp, ApiError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(ApiError::Network)?;

        let url = format!("{}/v1/auth/provision", base_url.trim_end_matches('/'));
        let body = serde_json::json!({ "email": email });
        let resp = http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Network)?;

        let status = resp.status();
        if status == StatusCode::CONFLICT {
            return Err(ApiError::Conflict("email_registered".into()));
        }
        if status == StatusCode::FORBIDDEN {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Rejected { status: 403, body });
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Server { status: status.as_u16(), body });
        }
        resp.json().await.map_err(ApiError::Network)
    }

    /// POST /v1/auth/verify — verify the OTP emailed after provision/request-key.
    pub async fn verify(
        base_url: &str,
        email: &str,
        code: &str,
    ) -> Result<VerifyResp, ApiError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(ApiError::Network)?;

        let url = format!("{}/v1/auth/verify", base_url.trim_end_matches('/'));
        let body = serde_json::json!({ "email": email, "code": code });
        let resp = http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Network)?;

        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Auth);
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Server { status: status.as_u16(), body });
        }
        resp.json().await.map_err(ApiError::Network)
    }

    /// POST /v1/auth/request-key — request a recovery OTP for an existing account.
    pub async fn request_key(base_url: &str, email: &str) -> Result<(), ApiError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(ApiError::Network)?;

        let url = format!("{}/v1/auth/request-key", base_url.trim_end_matches('/'));
        let body = serde_json::json!({ "email": email });
        let resp = http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(ApiError::Network)?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ApiError::Server { status: status.as_u16(), body });
        }
        Ok(())
    }

    // ─── Brain documents ───────────────────────────────────────────────────

    /// GET /v1/brain/doc?path=<path> — read a single document by path.
    pub async fn get_doc(&self, path: &str) -> Result<BrainDocument, ApiError> {
        let encoded = urlencoding(path);
        self.get(&format!("/v1/brain/doc?path={encoded}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// PUT /v1/brain/doc — write/create a document.
    pub async fn put_doc(&self, req: &PutDocReq) -> Result<BrainDocument, ApiError> {
        self.put_write("/v1/brain/doc")
            .json(req)
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// PATCH /v1/brain/doc — surgical in-place edit.
    pub async fn patch_doc(&self, req: &PatchDocReq) -> Result<BrainDocument, ApiError> {
        self.patch_write("/v1/brain/doc")
            .json(req)
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// DELETE /v1/brain/doc?path=<path>
    pub async fn delete_doc(&self, path: &str) -> Result<DeleteDocResp, ApiError> {
        let encoded = urlencoding(path);
        self.delete_write(&format!("/v1/brain/doc?path={encoded}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/list — list documents with optional prefix and filters.
    pub async fn list_docs(&self, req: &ListDocsReq) -> Result<ListDocsResp, ApiError> {
        let mut params = Vec::new();
        if let Some(p) = &req.prefix {
            params.push(format!("prefix={}", urlencoding(p)));
        }
        for k in &req.kind {
            params.push(format!("kind={}", urlencoding(k)));
        }
        for t in &req.tag {
            params.push(format!("tag={}", urlencoding(t)));
        }
        if let Some(l) = req.limit {
            params.push(format!("limit={l}"));
        }
        let qs = if params.is_empty() {
            String::new()
        } else {
            format!("?{}", params.join("&"))
        };
        self.get(&format!("/v1/brain/list{qs}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/fs?path=<path> — directory listing.
    pub async fn fs_list(&self, path: &str) -> Result<FsListResp, ApiError> {
        let encoded = urlencoding(path);
        self.get(&format!("/v1/brain/fs?path={encoded}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/fs/read?path=<path> — raw content of any tier.
    pub async fn fs_read(&self, path: &str) -> Result<FsReadResp, ApiError> {
        let encoded = urlencoding(path);
        self.get(&format!("/v1/brain/fs/read?path={encoded}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/search — hybrid keyword+semantic search.
    pub async fn search(&self, req: &SearchReq) -> Result<SearchResp, ApiError> {
        let mut params = vec![format!("q={}", urlencoding(&req.q))];
        if let Some(k) = req.k {
            params.push(format!("k={k}"));
        }
        for kind in &req.kind {
            params.push(format!("kind={}", urlencoding(kind)));
        }
        if let Some(mt) = &req.memory_type {
            params.push(format!("memoryType={}", urlencoding(mt)));
        }
        if let Some(as_of) = &req.as_of {
            params.push(format!("asOf={}", urlencoding(as_of)));
        }
        let qs = format!("?{}", params.join("&"));
        self.get(&format!("/v1/brain/search{qs}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/grep — regex scan over document bodies.
    pub async fn grep(&self, pattern: &str, case_sensitive: bool, limit: Option<u32>) -> Result<GrepResp, ApiError> {
        let mut params = vec![format!("pattern={}", urlencoding(pattern))];
        params.push(format!("caseSensitive={case_sensitive}"));
        if let Some(l) = limit {
            params.push(format!("limit={l}"));
        }
        let qs = format!("?{}", params.join("&"));
        self.get(&format!("/v1/brain/grep{qs}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// POST /v1/brain/doc/tag — add/remove tags on a document.
    pub async fn tag_doc(&self, req: &TagDocReq) -> Result<BrainDocument, ApiError> {
        self.post_write("/v1/brain/doc/tag")
            .json(req)
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/status — health and counts.
    pub async fn brain_status(&self) -> Result<BrainStatus, ApiError> {
        self.get("/v1/brain/status")
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/neighbors — graph neighbors.
    pub async fn neighbors(&self, id_or_path: &str, limit: Option<u32>) -> Result<NeighborsResp, ApiError> {
        let mut params = vec![format!("idOrPath={}", urlencoding(id_or_path))];
        if let Some(l) = limit {
            params.push(format!("limit={l}"));
        }
        let qs = format!("?{}", params.join("&"));
        self.get(&format!("/v1/brain/neighbors{qs}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    // ─── Entities ─────────────────────────────────────────────────────────

    /// GET /v1/brain/entities/resolve?name=<name>
    pub async fn resolve_entity(&self, name: &str) -> Result<ResolveEntityResp, ApiError> {
        let encoded = urlencoding(name);
        self.get(&format!("/v1/brain/entities/resolve?name={encoded}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// GET /v1/brain/entities/:id
    pub async fn get_entity(&self, id: &str) -> Result<BrainEntity, ApiError> {
        self.get(&format!("/v1/brain/entities/{id}"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// POST /v1/brain/entities — upsert entity.
    pub async fn upsert_entity(&self, req: &UpsertEntityReq) -> Result<BrainEntity, ApiError> {
        self.post_write("/v1/brain/entities")
            .json(req)
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    // ─── Facts ────────────────────────────────────────────────────────────

    /// GET /v1/brain/entities/:id/facts
    pub async fn facts_about(&self, entity_id: &str) -> Result<FactsResp, ApiError> {
        self.get(&format!("/v1/brain/entities/{entity_id}/facts"))
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    /// POST /v1/brain/facts — record a fact.
    pub async fn record_fact(&self, req: &RecordFactReq) -> Result<BrainFact, ApiError> {
        self.post_write("/v1/brain/facts")
            .json(req)
            .send_with_retry()
            .await?
            .parse_json()
            .await
    }

    // ─── Private helpers ──────────────────────────────────────────────────

    fn get(&self, path: &str) -> RetryableRequest {
        RetryableRequest::new(self.authed(self.http.get(self.url(path))))
    }

    fn put_write(&self, path: &str) -> RetryableRequest {
        self.write_calls.fetch_add(1, Ordering::Relaxed);
        RetryableRequest::new(self.authed(self.http.put(self.url(path))))
    }

    fn patch_write(&self, path: &str) -> RetryableRequest {
        self.write_calls.fetch_add(1, Ordering::Relaxed);
        RetryableRequest::new(self.authed(self.http.patch(self.url(path))))
    }

    fn post_write(&self, path: &str) -> RetryableRequest {
        self.write_calls.fetch_add(1, Ordering::Relaxed);
        RetryableRequest::new(self.authed(self.http.post(self.url(path))))
    }

    fn delete_write(&self, path: &str) -> RetryableRequest {
        self.write_calls.fetch_add(1, Ordering::Relaxed);
        RetryableRequest::new(self.authed(self.http.delete(self.url(path))))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authed(&self, req: RequestBuilder) -> RequestBuilder {
        req.header("Authorization", format!("Bearer {}", self.token))
    }
}

/// URL-encode a string value for query parameters.
fn urlencoding(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                vec![c]
            }
            c => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                encoded
                    .bytes()
                    .flat_map(|b| {
                        let hi = "0123456789ABCDEF".as_bytes()[(b >> 4) as usize] as char;
                        let lo = "0123456789ABCDEF".as_bytes()[(b & 0xf) as usize] as char;
                        vec!['%', hi, lo]
                    })
                    .collect::<Vec<_>>()
            }
        })
        .collect()
}

/// Wraps a `RequestBuilder` with retry + JSON body support.
struct RetryableRequest {
    builder: Option<RequestBuilder>,
    json_body: Option<serde_json::Value>,
}

impl RetryableRequest {
    fn new(builder: RequestBuilder) -> Self {
        Self {
            builder: Some(builder),
            json_body: None,
        }
    }

    fn json<T: serde::Serialize>(mut self, body: &T) -> Self {
        self.json_body = Some(serde_json::to_value(body).expect("serialize body"));
        self
    }

    async fn send_with_retry(self) -> Result<ApiResponse, ApiError> {
        let builder = self.builder.expect("builder consumed");
        let json_body = self.json_body;

        let mut backoff = INITIAL_BACKOFF_MS;

        for attempt in 0..MAX_RETRIES {
            let mut req = builder
                .try_clone()
                .expect("request must be cloneable for retry");

            if let Some(ref body) = json_body {
                req = req.json(body);
            }

            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();

                    if status.is_success() {
                        return Ok(ApiResponse(resp));
                    }

                    if status == StatusCode::UNAUTHORIZED {
                        return Err(ApiError::Auth);
                    }
                    if status == StatusCode::FORBIDDEN {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ApiError::Forbidden(body));
                    }
                    if status == StatusCode::NOT_FOUND {
                        return Err(ApiError::NotFound);
                    }
                    if status == StatusCode::CONFLICT {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ApiError::Conflict(body));
                    }
                    if status == StatusCode::UNPROCESSABLE_ENTITY {
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ApiError::FsContract(body));
                    }

                    // Retry 429 and 5xx
                    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                        if attempt < MAX_RETRIES - 1 {
                            tracing::warn!(
                                status = status.as_u16(),
                                attempt = attempt + 1,
                                "retrying after {}ms",
                                backoff,
                            );
                            tokio::time::sleep(Duration::from_millis(backoff)).await;
                            backoff *= 2;
                            continue;
                        }
                        if status == StatusCode::TOO_MANY_REQUESTS {
                            return Err(ApiError::RateLimited);
                        }
                        let body = resp.text().await.unwrap_or_default();
                        return Err(ApiError::Server {
                            status: status.as_u16(),
                            body,
                        });
                    }

                    let body = resp.text().await.unwrap_or_default();
                    return Err(ApiError::Rejected {
                        status: status.as_u16(),
                        body,
                    });
                }
                Err(e) => {
                    if attempt < MAX_RETRIES - 1 {
                        tracing::warn!(
                            error = %e,
                            attempt = attempt + 1,
                            "network error, retrying after {}ms",
                            backoff,
                        );
                        tokio::time::sleep(Duration::from_millis(backoff)).await;
                        backoff *= 2;
                        continue;
                    }
                    return Err(ApiError::Network(e));
                }
            }
        }

        unreachable!("loop should return before exhausting retries")
    }
}

struct ApiResponse(Response);

impl ApiResponse {
    async fn parse_json<T: serde::de::DeserializeOwned>(self) -> Result<T, ApiError> {
        Ok(self.0.json().await?)
    }
}
