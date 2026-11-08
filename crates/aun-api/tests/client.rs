//! Client runtime tests against an in-process mock server (never QA/prod).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use aun_api::queue::{FileQueueStore, MemoryQueueStore, OfflineQueue};
use aun_api::{
    ApiClient, ApiConfig, AuthProvider, Error, OPERATIONS, PostLikeCreateParams,
    PostUploadInitiateEndpointBody, PostUploadInitiateEndpointParams, Result, RetryPolicy, requests,
};
use aun_common::platform::ManualClock;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn config(server: &MockServer) -> ApiConfig {
    let mut c = ApiConfig::new(server.uri());
    c.retry = RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    };
    c
}

struct Tokens {
    current: std::sync::Mutex<Option<String>>,
    next: Option<String>,
    refreshes: AtomicUsize,
}

#[async_trait]
impl AuthProvider for Tokens {
    async fn access_token(&self) -> Option<String> {
        self.current.lock().unwrap().clone()
    }
    async fn refresh(&self) -> Result<Option<String>> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        *self.current.lock().unwrap() = self.next.clone();
        Ok(self.next.clone())
    }
}

fn tokens(current: &str, next: Option<&str>) -> Arc<Tokens> {
    Arc::new(Tokens {
        current: std::sync::Mutex::new(Some(current.into())),
        next: next.map(Into::into),
        refreshes: AtomicUsize::new(0),
    })
}

/// Fails `n` times with `status`, then succeeds with `body`.
struct FailThen {
    n: usize,
    status: u16,
    body: serde_json::Value,
    seen: AtomicUsize,
}

impl Respond for FailThen {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.seen.fetch_add(1, Ordering::SeqCst) < self.n {
            ResponseTemplate::new(self.status).set_body_json(serde_json::json!({"error": "try later"}))
        } else {
            ResponseTemplate::new(200).set_body_json(self.body.clone())
        }
    }
}

#[test]
fn spec_paths_are_normalised() {
    assert!(OPERATIONS.len() > 150);
    assert!(OPERATIONS.iter().all(|o| o.path.starts_with("/api/")));
    assert!(
        !OPERATIONS
            .iter()
            .any(|o| o.path.contains("/api/videos/api/") || o.path.starts_with("/api/api/"))
    );
    let init = OPERATIONS
        .iter()
        .find(|o| o.id == "post_UploadInitiateEndpoint")
        .unwrap();
    assert_eq!(
        (init.method, init.path, init.auth_required),
        ("POST", "/api/videos/upload/initiate", true)
    );
}

#[tokio::test]
async fn typed_call_sends_body_headers_and_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/videos/upload/initiate"))
        .and(header("authorization", "Bearer t1"))
        .and(header("channelId", "ch_1"))
        .and(body_partial_json(serde_json::json!({"title": "درس", "fileSize": 1024.0, "mimeType": "video/mp4"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "result": {"videoId": "v1", "tusSessionId": "s1", "chunkSize": 5242880, "resumable": true, "extra": 1}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let client = ApiClient::new(config(&server))
        .unwrap()
        .with_auth(tokens("t1", None));
    let body = PostUploadInitiateEndpointBody {
        title: "درس".into(),
        file_size: 1024.0,
        mime_type: "video/mp4".into(),
        ..Default::default()
    };
    let params = PostUploadInitiateEndpointParams {
        header_channel_id: Some("ch_1".into()),
    };
    let r = client
        .post_upload_initiate_endpoint(&params, &body)
        .await
        .unwrap();
    assert!(r.success);
    assert_eq!(r.result.video_id, "v1");
    assert_eq!(r.result.tus_session_id, "s1");
    assert_eq!(r.result.chunk_size, 5_242_880.0);
}

#[tokio::test]
async fn authenticated_operation_without_token_fails_fast() {
    let server = MockServer::start().await;
    let client = ApiClient::new(config(&server)).unwrap();
    let e = client
        .post_upload_initiate_endpoint(&Default::default(), &Default::default())
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Unauthorized(_)));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn get_retries_on_503_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/locales"))
        .respond_with(FailThen {
            n: 2,
            status: 503,
            body: serde_json::json!({"ok": true}),
            seen: AtomicUsize::new(0),
        })
        .expect(3)
        .mount(&server)
        .await;
    let client = ApiClient::new(config(&server)).unwrap();
    let raw = client
        .execute(aun_api::RequestSpec::new(
            reqwest::Method::GET,
            "/api/i18n/locales".into(),
            "t",
        ))
        .await
        .unwrap();
    assert_eq!(raw.status, 200);
}

#[tokio::test]
async fn post_without_idempotency_key_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let client = ApiClient::new(config(&server)).unwrap();
    let e = client
        .execute(aun_api::RequestSpec::new(
            reqwest::Method::POST,
            "/api/x".into(),
            "t",
        ))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Http { status: 503, .. }));
}

#[tokio::test]
async fn refreshes_once_on_401_and_retries_with_new_token() {
    let server = MockServer::start().await;
    Mock::given(header("authorization", "Bearer old"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error": "expired"})))
        .mount(&server)
        .await;
    Mock::given(header("authorization", "Bearer new"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&server)
        .await;
    let auth = tokens("old", Some("new"));
    let client = ApiClient::new(config(&server)).unwrap().with_auth(auth.clone());
    client
        .execute(aun_api::RequestSpec::new(
            reqwest::Method::GET,
            "/api/profile".into(),
            "t",
        ))
        .await
        .unwrap();
    assert_eq!(auth.refreshes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_refresh_surfaces_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({"error": "expired"})))
        .expect(1)
        .mount(&server)
        .await;
    let auth = tokens("old", None);
    let client = ApiClient::new(config(&server)).unwrap().with_auth(auth.clone());
    let e = client
        .execute(aun_api::RequestSpec::new(
            reqwest::Method::GET,
            "/api/profile".into(),
            "t",
        ))
        .await
        .unwrap_err();
    assert_eq!(e, Error::Unauthorized("expired".into()));
}

#[tokio::test]
async fn long_retry_after_is_returned_not_slept() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "60"))
        .expect(1)
        .mount(&server)
        .await;
    let client = ApiClient::new(config(&server)).unwrap();
    let e = client
        .execute(aun_api::RequestSpec::new(
            reqwest::Method::GET,
            "/api/x".into(),
            "t",
        ))
        .await
        .unwrap_err();
    assert_eq!(
        e,
        Error::RateLimited {
            retry_after: Some(Duration::from_secs(60))
        }
    );
}

#[tokio::test]
async fn offline_connection_error_is_network() {
    let mut c = ApiConfig::new("http://127.0.0.1:9"); // discard port: nothing listens
    c.retry = RetryPolicy::none();
    let client = ApiClient::new(c).unwrap();
    let e = client
        .execute(aun_api::RequestSpec::new(
            reqwest::Method::GET,
            "/api/x".into(),
            "t",
        ))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Network(_)), "{e:?}");
}

fn like(video: &str) -> aun_api::RequestSpec {
    requests::post_like_create(&PostLikeCreateParams {
        id: video.into(),
        header_channel_id: None,
    })
    .unwrap()
}

#[tokio::test]
async fn queue_coalesces_replays_in_order_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FileQueueStore::new(dir.path().join("queue.json")));
    let q = OfflineQueue::open(store.clone()).unwrap();
    q.enqueue(like("v1"), Some("like:v1".into())).unwrap();
    q.enqueue(like("v2"), None).unwrap();
    q.enqueue(like("v1"), Some("like:v1".into())).unwrap(); // replaces the first
    assert_eq!(q.len(), 2);
    drop(q);

    // A new process picks the queue up from disk.
    let q = OfflineQueue::open(store).unwrap();
    let pending = q.pending().unwrap();
    assert_eq!(
        pending.iter().map(|w| w.path.as_str()).collect::<Vec<_>>(),
        ["/api/videos/v2/likes", "/api/videos/v1/likes"]
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(2)
        .mount(&server)
        .await;
    let client = ApiClient::new(config(&server))
        .unwrap()
        .with_auth(tokens("t", None));
    let report = q.flush(&client).await.unwrap();
    assert_eq!(report.sent.len(), 2);
    assert_eq!(report.remaining, 0);
    let reqs = server.received_requests().await.unwrap();
    assert_eq!(reqs[0].url.path(), "/api/videos/v2/likes");
    assert!(reqs.iter().all(|r| r.headers.contains_key("idempotency-key")));
}

#[tokio::test]
async fn queue_stops_on_retryable_and_drops_rejected() {
    let server = MockServer::start().await;
    Mock::given(path("/api/videos/gone/likes"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(serde_json::json!({"error": "Video not found"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/videos/busy/likes"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.retry = RetryPolicy::none();
    let client = ApiClient::new(cfg).unwrap().with_auth(tokens("t", None));
    let clock = Arc::new(ManualClock::new(1_000_000));
    let q = OfflineQueue::open_with_clock(Arc::new(MemoryQueueStore::default()), clock.clone()).unwrap();
    q.enqueue(like("gone"), None).unwrap();
    q.enqueue(like("busy"), None).unwrap();
    q.enqueue(like("later"), None).unwrap();

    let report = q.flush(&client).await.unwrap();
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.rejected[0].1, Error::NotFound("Video not found".into()));
    assert!(matches!(report.stopped_by, Some(Error::Http { status: 503, .. })));
    assert_eq!(report.remaining, 2);
    let busy = &q.pending().unwrap()[0];
    assert_eq!(busy.attempts, 1);
    assert!(busy.next_attempt_at_ms > 1_000_000);

    // Not due yet: nothing is sent.
    let before = server.received_requests().await.unwrap().len();
    let report = q.flush(&client).await.unwrap();
    assert!(report.sent.is_empty() && report.stopped_by.is_none());
    assert_eq!(server.received_requests().await.unwrap().len(), before);

    clock.advance(60 * 60 * 1000);
    let report = q.flush(&client).await.unwrap();
    assert!(report.stopped_by.is_some()); // still 503
}
