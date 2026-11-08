//! Hand-written runtime behind the generated operations: HTTP, retries with backoff, the auth
//! refresh hook and error mapping.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use aun_common::Error;
use reqwest::{Method, StatusCode, header};

use crate::Result;

/// Supplies access tokens and refreshes them. `aun-auth` implements this for the active account.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// The current access token, if signed in.
    async fn access_token(&self) -> Option<String>;
    /// Called once after a 401: refresh and return the new token, or `None` if sign-in is needed.
    async fn refresh(&self) -> Result<Option<String>>;
}

/// Exponential backoff with full jitter.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Attempts including the first one.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: Duration::from_millis(300),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    /// No retries (tests, or callers that queue failures themselves).
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// Delay before retry number `attempt` (1-based), honouring a server `Retry-After`.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(ra) = retry_after {
            return ra;
        }
        let exp = self
            .base_delay
            .saturating_mul(1u32.checked_shl(attempt.saturating_sub(1)).unwrap_or(u32::MAX));
        let cap = exp.min(self.max_delay);
        // Full jitter in [cap/2, cap] keeps retries spread without collapsing to zero.
        let jitter = fastrand_f64();
        cap.mul_f64(0.5 + jitter / 2.0)
    }
}

/// A tiny xorshift so we don't pull in `rand` just for jitter.
fn fastrand_f64() -> f64 {
    use std::cell::Cell;
    use std::time::{SystemTime, UNIX_EPOCH};
    thread_local! {
        static STATE: Cell<u64> = Cell::new(
            SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0x9E37_79B9) | 1
        );
    }
    STATE.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        (x >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// Client configuration.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// e.g. `https://videos.api.qa.alummahnow.net` (no trailing slash needed).
    pub base_url: String,
    /// e.g. `AlUmmahNow-iOS/1.2.0 aun-core/0.1.0`.
    pub user_agent: String,
    pub timeout: Duration,
    pub retry: RetryPolicy,
    /// Sent as `Accept-Language` (e.g. `ar`, `en`).
    pub locale: Option<String>,
}

impl ApiConfig {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            user_agent: format!("aun-core/{}", aun_common::VERSION),
            timeout: Duration::from_secs(30),
            retry: RetryPolicy::default(),
            locale: None,
        }
    }
}

/// Whether an operation needs a bearer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// Send the token if we have one (public endpoints personalise when signed in).
    #[default]
    Optional,
    Required,
}

/// A multipart form (thumbnail/image uploads).
#[derive(Debug, Default)]
pub struct Multipart {
    pub fields: Vec<(String, String)>,
    pub files: Vec<MultipartFile>,
}

#[derive(Debug)]
pub struct MultipartFile {
    pub field: String,
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Default)]
pub enum Body {
    #[default]
    None,
    Json(serde_json::Value),
    Multipart(Multipart),
}

/// A fully described request; generated methods build one and hand it to `execute_*`.
#[derive(Debug)]
pub struct RequestSpec {
    pub method: Method,
    /// Path starting with `/`, already percent-encoded.
    pub path: String,
    pub operation_id: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Body,
    pub auth: AuthMode,
    /// Makes a non-idempotent request safe to retry (the server dedupes on it).
    pub idempotency_key: Option<String>,
}

impl RequestSpec {
    pub fn new(method: Method, path: String, operation_id: impl Into<String>) -> Self {
        Self {
            method,
            path,
            operation_id: operation_id.into(),
            query: vec![],
            headers: vec![],
            body: Body::None,
            auth: AuthMode::Optional,
            idempotency_key: None,
        }
    }

    fn retry_safe(&self) -> bool {
        matches!(
            self.method,
            Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
        ) || self.idempotency_key.is_some()
    }
}

/// Percent-encodes one path segment. Empty and dot-only segments (`.`, `..`, `...`) are
/// rejected: URL parsers normalise `%2E%2E` back to `..`, so encoding them is not enough to stop
/// a parameter value from changing which route is called.
pub fn encode_path(segment: &str) -> Result<String> {
    if segment.is_empty() {
        return Err(Error::InvalidInput("empty path parameter".into()));
    }
    if segment.bytes().all(|b| b == b'.') {
        return Err(Error::InvalidInput(format!("invalid path parameter {segment:?}")));
    }
    let mut out = String::with_capacity(segment.len());
    for b in segment.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    Ok(out)
}

/// The api-videos client. Cheap to clone; share one per account.
#[derive(Clone)]
pub struct ApiClient {
    config: Arc<ApiConfig>,
    http: reqwest::Client,
    auth: Option<Arc<dyn AuthProvider>>,
    /// The account this client acts as, when bound to one.
    account_id: Option<String>,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiClient")
            .field("base_url", &self.config.base_url)
            .field("auth", &self.auth.is_some())
            .field("account_id", &self.account_id)
            .finish()
    }
}

/// A raw response for callers that work with JSON directly (the FFI `request` escape hatch).
#[derive(Debug, Clone, PartialEq)]
pub struct RawResponse {
    pub status: u16,
    pub body: String,
}

impl ApiClient {
    pub fn new(config: ApiConfig) -> Result<Self> {
        let http = crate::tls::http_client(&config)?;
        Ok(Self {
            config: Arc::new(config),
            http,
            auth: None,
            account_id: None,
        })
    }

    /// Attaches the token provider used for authenticated calls and refresh-on-401.
    pub fn with_auth(mut self, auth: Arc<dyn AuthProvider>) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Records which account this client authenticates as. The offline queue refuses to replay
    /// one account's writes through a client bound to another.
    pub fn with_account(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    pub fn account_id(&self) -> Option<&str> {
        self.account_id.as_deref()
    }

    pub fn config(&self) -> &ApiConfig {
        &self.config
    }

    pub async fn execute_json<T: serde::de::DeserializeOwned>(&self, req: RequestSpec) -> Result<T> {
        let raw = self.execute(req).await?;
        let body = if raw.body.trim().is_empty() {
            "null"
        } else {
            &raw.body
        };
        serde_json::from_str(body).map_err(|e| Error::Internal(format!("decoding response: {e}")))
    }

    pub async fn execute_text(&self, req: RequestSpec) -> Result<String> {
        Ok(self.execute(req).await?.body)
    }

    pub async fn execute_empty(&self, req: RequestSpec) -> Result<()> {
        self.execute(req).await.map(|_| ())
    }

    /// Runs a request with retries and the refresh-on-401 hook. Returns the body of a 2xx.
    pub async fn execute(&self, req: RequestSpec) -> Result<RawResponse> {
        let policy = &self.config.retry;
        let mut attempt = 0u32;
        let mut refreshed = false;
        let mut token = match &self.auth {
            Some(a) => a.access_token().await,
            None => None,
        };
        if req.auth == AuthMode::Required && token.is_none() {
            return Err(Error::Unauthorized(format!(
                "{} requires sign-in",
                req.operation_id
            )));
        }
        loop {
            attempt += 1;
            let result = self.send_once(&req, token.as_deref()).await;
            match result {
                Ok(raw) => return Ok(raw),
                Err((Error::Unauthorized(msg), _)) if !refreshed && self.auth.is_some() => {
                    refreshed = true;
                    let Some(auth) = &self.auth else {
                        return Err(Error::Unauthorized(msg));
                    };
                    match auth.refresh().await? {
                        Some(t) => {
                            token = Some(t);
                            attempt -= 1; // the refresh retry doesn't count against the budget
                        }
                        None => return Err(Error::Unauthorized(msg)),
                    }
                }
                Err((e, retry_after)) => {
                    // A server asking us to wait longer than our longest backoff gets the error back
                    // (the offline queue reschedules) instead of a blocked call.
                    let wait_too_long = retry_after.is_some_and(|ra| ra > policy.max_delay);
                    if wait_too_long
                        || !(e.is_retryable() && req.retry_safe() && attempt < policy.max_attempts)
                    {
                        return Err(match e {
                            Error::RateLimited { .. } => Error::RateLimited { retry_after },
                            other => other,
                        });
                    }
                    tracing::debug!(op = %req.operation_id, attempt, error = %e, "retrying");
                    tokio::time::sleep(policy.delay(attempt, retry_after)).await;
                }
            }
        }
    }

    async fn send_once(
        &self,
        req: &RequestSpec,
        token: Option<&str>,
    ) -> std::result::Result<RawResponse, (Error, Option<Duration>)> {
        let url = format!("{}{}", self.config.base_url, req.path);
        let mut rb = self.http.request(req.method.clone(), &url);
        if !req.query.is_empty() {
            rb = rb.query(&req.query);
        }
        for (k, v) in &req.headers {
            rb = rb.header(k, v);
        }
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        if let Some(l) = &self.config.locale {
            rb = rb.header(header::ACCEPT_LANGUAGE, l);
        }
        if let Some(k) = &req.idempotency_key {
            rb = rb.header("Idempotency-Key", k);
        }
        rb = match &req.body {
            Body::None => rb,
            Body::Json(v) => rb.json(v),
            Body::Multipart(m) => {
                let mut form = reqwest::multipart::Form::new();
                for (k, v) in &m.fields {
                    form = form.text(k.clone(), v.clone());
                }
                for f in &m.files {
                    let part = reqwest::multipart::Part::bytes(f.bytes.clone())
                        .file_name(f.file_name.clone())
                        .mime_str(&f.mime_type)
                        .map_err(|e| (Error::InvalidInput(e.to_string()), None))?;
                    form = form.part(f.field.clone(), part);
                }
                rb.multipart(form)
            }
        };
        let resp = rb.send().await.map_err(|e| (map_reqwest(e), None))?;
        let status = resp.status();
        let retry_after = parse_retry_after(resp.headers());
        let body = resp.text().await.map_err(|e| (map_reqwest(e), None))?;
        if status.is_success() {
            return Ok(RawResponse {
                status: status.as_u16(),
                body,
            });
        }
        Err((error_from_response(status, &body, retry_after), retry_after))
    }
}

fn map_reqwest(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Network(format!("timeout: {e}"))
    } else if e.is_connect() {
        Error::Network(format!("connect: {e}"))
    } else if e.is_decode() || e.is_body() {
        Error::Network(format!("body: {e}"))
    } else {
        Error::Network(e.to_string())
    }
}

fn parse_retry_after(h: &header::HeaderMap) -> Option<Duration> {
    let v = h.get(header::RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Maps an error response. api-videos errors look like `{"success":false,"error":"…"}` or
/// `{"error":{"code":"…","message":"…"}}` or chanfana's `{"errors":[{"message":"…"}]}`.
pub(crate) fn error_from_response(status: StatusCode, body: &str, retry_after: Option<Duration>) -> Error {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let (code, message) = match &json {
        Some(v) => {
            let err = v.get("error");
            let code = err
                .and_then(|e| e.get("code"))
                .or_else(|| v.get("code"))
                .and_then(|c| c.as_str())
                .map(str::to_owned);
            let message = err
                .and_then(|e| {
                    e.as_str()
                        .map(str::to_owned)
                        .or_else(|| e.get("message")?.as_str().map(str::to_owned))
                })
                .or_else(|| v.get("message")?.as_str().map(str::to_owned))
                .or_else(|| {
                    v.get("errors")?
                        .get(0)?
                        .get("message")?
                        .as_str()
                        .map(str::to_owned)
                });
            (code, message)
        }
        None => (None, None),
    };
    let message = message.unwrap_or_else(|| {
        let b = body.trim();
        if b.is_empty() || b.len() > 300 {
            status.canonical_reason().unwrap_or("error").to_owned()
        } else {
            b.to_owned()
        }
    });
    match Error::from_status(status.as_u16(), code, message) {
        Error::RateLimited { .. } => Error::RateLimited { retry_after },
        e => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_path_segments() {
        assert_eq!(encode_path("abc-123").unwrap(), "abc-123");
        assert_eq!(encode_path("a b/c").unwrap(), "a%20b%2Fc");
        assert_eq!(encode_path("سلام").unwrap(), "%D8%B3%D9%84%D8%A7%D9%85");
        assert_eq!(encode_path("v1.mp4").unwrap(), "v1.mp4");
        for bad in ["", ".", "..", "..."] {
            assert!(matches!(encode_path(bad), Err(Error::InvalidInput(_))), "{bad:?}");
        }
        // Encoded separators stay inside the segment once parsed as a URL.
        let url = reqwest::Url::parse(&format!(
            "https://h/api/videos/{}/likes",
            encode_path("../x").unwrap()
        ))
        .unwrap();
        assert_eq!(url.path(), "/api/videos/..%2Fx/likes");
    }

    #[test]
    fn backoff_grows_and_caps() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(1),
        };
        for _ in 0..50 {
            let d1 = p.delay(1, None);
            assert!(d1 >= Duration::from_millis(50) && d1 <= Duration::from_millis(100));
            let d8 = p.delay(8, None);
            assert!(d8 >= Duration::from_millis(500) && d8 <= Duration::from_secs(1));
        }
        assert_eq!(p.delay(1, Some(Duration::from_secs(3))), Duration::from_secs(3));
    }

    #[test]
    fn maps_error_bodies() {
        let e = error_from_response(
            StatusCode::NOT_FOUND,
            r#"{"success":false,"error":"Video not found"}"#,
            None,
        );
        assert_eq!(e, Error::NotFound("Video not found".into()));
        let e = error_from_response(
            StatusCode::BAD_GATEWAY,
            r#"{"error":{"code":"upstream","message":"Bunny down"}}"#,
            None,
        );
        assert_eq!(
            e,
            Error::Http {
                status: 502,
                code: Some("upstream".into()),
                message: "Bunny down".into()
            }
        );
        let e = error_from_response(
            StatusCode::BAD_REQUEST,
            r#"{"errors":[{"message":"title required"}]}"#,
            None,
        );
        assert_eq!(e, Error::InvalidInput("title required".into()));
        let e = error_from_response(StatusCode::TOO_MANY_REQUESTS, "", Some(Duration::from_secs(7)));
        assert_eq!(
            e,
            Error::RateLimited {
                retry_after: Some(Duration::from_secs(7))
            }
        );
    }
}
