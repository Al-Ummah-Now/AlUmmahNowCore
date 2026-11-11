//! FFI facade for `aun-api`: an `ApiClient` object, a generic operation call (every operation in
//! the OpenAPI document, by operation id) and the offline write queue.
//!
//! Native apps keep their own view models; the JSON escape hatch keeps the FFI small while the
//! contract settles (Epic 9.3). Typed FFI records for hot paths land with the modules using them.

use std::collections::HashMap;
use std::sync::Arc;

use aun_api::{OPERATIONS, OperationInfo, RequestSpec};
use aun_common::Error;

use crate::AunError;

/// Configuration for an [`ApiClient`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct ApiClientConfig {
    /// e.g. `https://videos.api.qa.alummahnow.net`
    pub base_url: String,
    /// App identifier prefix for the User-Agent, e.g. `AlUmmahNow-iOS/1.2.0`.
    pub user_agent: Option<String>,
    /// `Accept-Language`, e.g. `ar`.
    pub locale: Option<String>,
    /// Request timeout; default 30 000 ms.
    pub timeout_ms: Option<u64>,
    /// Attempts including the first for idempotent requests; default 4.
    pub max_attempts: Option<u32>,
}

/// Supplies tokens to the API client. `aun-auth`'s `AuthManager` provides one; apps can also
/// implement it natively. Called from a background thread.
#[uniffi::export(with_foreign)]
pub trait TokenSource: Send + Sync {
    fn access_token(&self) -> Option<String>;
    /// Refresh after a 401; return the new token or `None` when sign-in is required.
    fn refresh(&self) -> Result<Option<String>, AunError>;
}

struct ForeignTokens(Arc<dyn TokenSource>);

#[async_trait::async_trait]
impl aun_api::AuthProvider for ForeignTokens {
    async fn access_token(&self) -> Option<String> {
        let s = self.0.clone();
        tokio::task::spawn_blocking(move || s.access_token())
            .await
            .ok()
            .flatten()
    }
    async fn refresh(&self) -> aun_common::Result<Option<String>> {
        let s = self.0.clone();
        tokio::task::spawn_blocking(move || s.refresh())
            .await
            .map_err(|e| Error::Internal(e.to_string()))?
            .map_err(Into::into)
    }
}

/// A response body from [`ApiClient::request`].
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct ApiResponse {
    pub status: u16,
    /// Raw body (JSON for almost every operation).
    pub body: String,
}

/// A call to one OpenAPI operation.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct ApiCall {
    /// The OpenAPI operation id, e.g. `get_VideoGet` (see `api_operations()`).
    pub operation_id: String,
    pub path_params: HashMap<String, String>,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    /// JSON request body.
    pub body_json: Option<String>,
    /// Makes a POST/PATCH safe to retry.
    pub idempotency_key: Option<String>,
}

/// Static description of an operation.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ApiOperation {
    pub id: String,
    pub method: String,
    pub path: String,
    pub auth_required: bool,
}

/// Every operation the client knows (from the pinned OpenAPI document).
#[uniffi::export]
pub fn api_operations() -> Vec<ApiOperation> {
    OPERATIONS
        .iter()
        .map(|o| ApiOperation {
            id: o.id.into(),
            method: o.method.into(),
            path: o.path.into(),
            auth_required: o.auth_required,
        })
        .collect()
}

fn find_op(id: &str) -> Result<&'static OperationInfo, AunError> {
    OPERATIONS
        .iter()
        .find(|o| o.id == id)
        .ok_or_else(|| AunError::InvalidInput {
            detail: format!("unknown operation {id}"),
        })
}

/// Fills `{param}` segments of `template` (percent-encoded, `..` refused).
fn fill_path(template: &str, params: &HashMap<String, String>, what: &str) -> Result<String, AunError> {
    let mut path = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|e| start + e)
            .ok_or_else(|| AunError::InvalidInput {
                detail: format!("bad path template {template}"),
            })?;
        path.push_str(&rest[..start]);
        let name = &rest[start + 1..end];
        let value = params.get(name).ok_or_else(|| AunError::InvalidInput {
            detail: format!("{what} needs path parameter {name}"),
        })?;
        path.push_str(&aun_api::client::encode_path(value)?);
        rest = &rest[end + 1..];
    }
    path.push_str(rest);
    Ok(path)
}

fn finish_request(
    mut req: RequestSpec,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body_json: Option<String>,
    idempotency_key: Option<String>,
) -> Result<RequestSpec, AunError> {
    let mut query: Vec<_> = query.into_iter().collect();
    query.sort();
    req.query = query;
    let mut headers: Vec<_> = headers.into_iter().collect();
    headers.sort();
    req.headers = headers;
    if let Some(b) = body_json {
        let v: serde_json::Value = serde_json::from_str(&b).map_err(|e| AunError::InvalidInput {
            detail: format!("body is not JSON: {e}"),
        })?;
        req.body = aun_api::Body::Json(v);
    }
    req.idempotency_key = idempotency_key;
    Ok(req)
}

/// Builds the request for a call: fills `{param}` segments (percent-encoded), sorts query and
/// headers for deterministic output.
pub(crate) fn build_request(call: ApiCall) -> Result<RequestSpec, AunError> {
    let op = find_op(&call.operation_id)?;
    let path = fill_path(op.path, &call.path_params, op.id)?;
    let method = reqwest::Method::from_bytes(op.method.as_bytes()).map_err(|e| AunError::Internal {
        detail: e.to_string(),
    })?;
    let mut req = RequestSpec::new(method, path, op.id);
    if op.auth_required {
        req.auth = aun_api::AuthMode::Required;
    }
    finish_request(
        req,
        call.query,
        call.headers,
        call.body_json,
        call.idempotency_key,
    )
}

/// A call to an api-videos path that is not (yet) in the pinned OpenAPI document, e.g. a Studio
/// endpoint that shipped after the last spec sync. Prefer [`ApiCall`] when the operation exists.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct PathCall {
    /// `GET`, `POST`, `PUT`, `PATCH` or `DELETE`.
    pub method: String,
    /// Path under `/api/`, with `{name}` placeholders, e.g. `/api/videos/{id}/restore`.
    pub path_template: String,
    pub path_params: HashMap<String, String>,
    pub query: HashMap<String, String>,
    /// Extra headers, e.g. `channelId`. `Authorization`, `Cookie` and `Host` are refused.
    pub headers: HashMap<String, String>,
    /// JSON request body.
    pub body_json: Option<String>,
    /// Makes a POST/PATCH safe to retry.
    pub idempotency_key: Option<String>,
    /// Fail with `Unauthorized` instead of calling anonymously when there is no token.
    pub auth_required: bool,
}

pub(crate) fn build_path_request(call: PathCall) -> Result<RequestSpec, AunError> {
    let t = call.path_template.as_str();
    let bad = || AunError::InvalidInput {
        detail: format!("path must be an /api/ path without query or dot segments: {t:?}"),
    };
    if !t.starts_with("/api/") || t.contains(['?', '#', '\\', '%']) || t.contains("//") {
        return Err(bad());
    }
    if t.split('/')
        .any(|seg| !seg.is_empty() && seg.bytes().all(|b| b == b'.'))
    {
        return Err(bad());
    }
    let method = match call.method.to_ascii_uppercase().as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        other => {
            return Err(AunError::InvalidInput {
                detail: format!("unsupported method {other}"),
            });
        }
    };
    let path = fill_path(t, &call.path_params, t)?;
    let mut req = RequestSpec::new(method, path, "path_call");
    if call.auth_required {
        req.auth = aun_api::AuthMode::Required;
    }
    finish_request(
        req,
        call.query,
        HashMap::new(),
        call.body_json,
        call.idempotency_key,
    )
}

/// The api-videos client. Share one per account.
#[derive(Debug, uniffi::Object)]
pub struct ApiClient {
    pub(crate) inner: aun_api::ApiClient,
}

#[uniffi::export(async_runtime = "tokio")]
impl ApiClient {
    #[uniffi::constructor]
    pub fn new(config: ApiClientConfig, tokens: Option<Arc<dyn TokenSource>>) -> Result<Arc<Self>, AunError> {
        let mut c = aun_api::ApiConfig::new(config.base_url);
        if let Some(ua) = config.user_agent {
            c.user_agent = format!("{ua} aun-core/{}", aun_common::VERSION);
        }
        c.locale = config.locale;
        if let Some(t) = config.timeout_ms {
            c.timeout = std::time::Duration::from_millis(t);
        }
        if let Some(n) = config.max_attempts {
            c.retry.max_attempts = n.max(1);
        }
        let mut inner = aun_api::ApiClient::new(c)?;
        if let Some(t) = tokens {
            inner = inner.with_auth(Arc::new(ForeignTokens(t)));
        }
        Ok(Arc::new(Self { inner }))
    }

    /// Calls any operation by id. Throws `AunError` for non-2xx responses.
    pub async fn request(&self, call: ApiCall) -> Result<ApiResponse, AunError> {
        let raw = self.inner.execute(build_request(call)?).await?;
        Ok(ApiResponse {
            status: raw.status,
            body: raw.body,
        })
    }

    /// Calls an `/api/` path that is not in the pinned OpenAPI document (same auth, refresh on
    /// 401, retries and errors as [`Self::request`]). Throws `AunError` for non-2xx responses.
    pub async fn request_path(&self, call: PathCall) -> Result<ApiResponse, AunError> {
        let raw = self.inner.execute(build_path_request(call)?).await?;
        Ok(ApiResponse {
            status: raw.status,
            body: raw.body,
        })
    }
}

/// What a queue flush did.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct QueueFlushReport {
    pub sent_ids: Vec<String>,
    pub rejected: Vec<RejectedWrite>,
    pub remaining: u32,
    /// Why the flush stopped early (offline, rate limited, sign-in needed).
    pub stopped_by: Option<AunError>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct RejectedWrite {
    pub id: String,
    pub operation_id: String,
    pub error: AunError,
}

/// Persistent offline write queue (likes, comments, notes made while offline).
#[derive(Debug, uniffi::Object)]
pub struct OfflineQueue {
    inner: aun_api::queue::OfflineQueue,
}

#[uniffi::export(async_runtime = "tokio")]
impl OfflineQueue {
    /// Opens (or creates) the queue persisted at `file_path` (app support directory) for
    /// `account_id` (the signed-in user id). Only that account's writes are replayed.
    #[uniffi::constructor]
    pub fn open(file_path: String, account_id: String) -> Result<Arc<Self>, AunError> {
        let store = Arc::new(aun_api::queue::FileQueueStore::new(file_path));
        Ok(Arc::new(Self {
            inner: aun_api::queue::OfflineQueue::open(store, account_id)?,
        }))
    }

    /// Deletes every queued write of `account_id` (sign-out / account removal). Returns the count.
    pub fn purge_account(&self, account_id: String) -> Result<u32, AunError> {
        Ok(self.inner.purge_account(&account_id)? as u32)
    }

    /// Queues a write; writes with the same `coalesce_key` replace each other. Returns its id.
    pub fn enqueue(&self, call: ApiCall, coalesce_key: Option<String>) -> Result<String, AunError> {
        Ok(self.inner.enqueue(build_request(call)?, coalesce_key)?)
    }

    pub fn remove(&self, id: String) -> Result<bool, AunError> {
        Ok(self.inner.remove(&id)?)
    }

    pub fn pending_count(&self) -> u32 {
        self.inner.len() as u32
    }

    /// Replays due writes in order. Call on reconnect, foreground and from a periodic task.
    pub async fn flush(&self, client: Arc<ApiClient>) -> Result<QueueFlushReport, AunError> {
        let r = self.inner.flush(&client.inner).await?;
        Ok(QueueFlushReport {
            sent_ids: r.sent,
            rejected: r
                .rejected
                .into_iter()
                .map(|(w, e)| RejectedWrite {
                    id: w.id,
                    operation_id: w.operation_id,
                    error: e.into(),
                })
                .collect(),
            remaining: r.remaining as u32,
            stopped_by: r.stopped_by.map(Into::into),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_paths_from_templates() {
        let mut call = ApiCall {
            operation_id: "post_LikeCreate".into(),
            ..Default::default()
        };
        assert!(matches!(
            build_request(call.clone()),
            Err(AunError::InvalidInput { .. })
        ));
        call.path_params.insert("id".into(), "v 1".into());
        call.query.insert("b".into(), "2".into());
        call.query.insert("a".into(), "1".into());
        let r = build_request(call).unwrap();
        assert_eq!(r.path, "/api/videos/v%201/likes");
        assert_eq!(r.method, reqwest::Method::POST);
        let mut traversal = ApiCall {
            operation_id: "post_LikeCreate".into(),
            ..Default::default()
        };
        traversal.path_params.insert("id".into(), "..".into());
        assert!(matches!(
            build_request(traversal),
            Err(AunError::InvalidInput { .. })
        ));
        assert_eq!(r.query, vec![("a".into(), "1".into()), ("b".into(), "2".into())]);
    }

    #[test]
    fn rejects_unknown_operation_and_bad_json() {
        let call = ApiCall {
            operation_id: "nope".into(),
            ..Default::default()
        };
        assert!(matches!(build_request(call), Err(AunError::InvalidInput { .. })));
        let call = ApiCall {
            operation_id: "post_UploadInitiateEndpoint".into(),
            body_json: Some("{".into()),
            ..Default::default()
        };
        assert!(matches!(build_request(call), Err(AunError::InvalidInput { .. })));
    }

    #[test]
    fn path_calls_fill_templates_and_refuse_escapes() {
        let mut call = PathCall {
            method: "post".into(),
            path_template: "/api/videos/{id}/restore".into(),
            auth_required: true,
            body_json: Some("{}".into()),
            ..Default::default()
        };
        assert!(matches!(
            build_path_request(call.clone()),
            Err(AunError::InvalidInput { .. })
        ));
        call.path_params.insert("id".into(), "v/1".into());
        call.query.insert("z".into(), "1".into());
        let r = build_path_request(call.clone()).unwrap();
        assert_eq!(r.path, "/api/videos/v%2F1/restore");
        assert_eq!(r.method, reqwest::Method::POST);
        assert_eq!(r.auth, aun_api::AuthMode::Required);
        call.path_params.insert("id".into(), "..".into());
        assert!(build_path_request(call.clone()).is_err());
        for t in [
            "/internal/x",
            "https://evil.example/api/x",
            "/api/../internal",
            "/api/x?y=1",
            "/api//x",
            "/api/%2e%2e/x",
            "api/x",
        ] {
            let c = PathCall {
                method: "GET".into(),
                path_template: t.into(),
                ..Default::default()
            };
            assert!(build_path_request(c).is_err(), "{t}");
        }
        let mut c = PathCall {
            method: "POST".into(),
            path_template: "/api/videos/bulk-delete".into(),
            ..Default::default()
        };
        c.headers.insert("channelId".into(), "ch1".into());
        assert_eq!(
            build_path_request(c.clone()).unwrap().headers,
            vec![("channelId".into(), "ch1".into())]
        );
        c.headers.insert("Authorization".into(), "Bearer x".into());
        assert!(build_path_request(c).is_err());
        let c = PathCall {
            method: "TRACE".into(),
            path_template: "/api/x".into(),
            ..Default::default()
        };
        assert!(build_path_request(c).is_err());
    }

    #[test]
    fn lists_operations() {
        assert!(
            api_operations()
                .iter()
                .any(|o| o.path == "/api/videos/upload/initiate")
        );
    }
}
