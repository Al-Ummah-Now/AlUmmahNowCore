//! Upload engine tests against an in-process mock of api-videos + its TUS proxy.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aun_api::{ApiClient, ApiConfig, RetryPolicy};
use aun_common::Error;
use aun_common::platform::{Connectivity, NetworkState, SystemClock};
use aun_upload::{UploadConfig, UploadEngine, UploadListener, UploadRequest, UploadState};
use serde_json::json;
use wiremock::matchers::{header, header_exists, method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const KIB: usize = 1024;

fn api(server: &MockServer) -> ApiClient {
    let mut c = ApiConfig::new(server.uri());
    c.retry = RetryPolicy {
        max_attempts: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    };
    ApiClient::new(c).unwrap().with_auth(Arc::new(StaticToken))
}

struct StaticToken;
#[async_trait::async_trait]
impl aun_api::AuthProvider for StaticToken {
    async fn access_token(&self) -> Option<String> {
        Some("tok".into())
    }
    async fn refresh(&self) -> aun_common::Result<Option<String>> {
        Ok(Some("tok".into()))
    }
}

fn cfg() -> UploadConfig {
    UploadConfig {
        chunk_size: Some(256 * KIB as u64),
        retry_base: Duration::from_millis(10),
        ..Default::default()
    }
}

fn video(dir: &std::path::Path, name: &str, len: usize) -> String {
    let p = dir.join(name);
    std::fs::write(&p, (0..len).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
    p.to_string_lossy().into_owned()
}

fn request(file: &str) -> UploadRequest {
    UploadRequest {
        file_path: file.into(),
        title: "درس".into(),
        description: None,
        mime_type: "video/mp4".into(),
        channel_id: Some("ch1".into()),
        is_public: Some(true),
        tags: None,
        category: None,
        title_localized: Default::default(),
        description_localized: Default::default(),
        priority: 0,
    }
}

/// A TUS server that tracks the received offset and checks PATCHes arrive in order.
#[derive(Default)]
struct TusState {
    offset: AtomicU64,
    bytes: Mutex<Vec<u8>>,
    patches: AtomicUsize,
}

struct Patch(Arc<TusState>);
impl Respond for Patch {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let off: u64 = req
            .headers
            .get("Upload-Offset")
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        if off != self.0.offset.load(Ordering::SeqCst) {
            return ResponseTemplate::new(409);
        }
        self.0.patches.fetch_add(1, Ordering::SeqCst);
        self.0.bytes.lock().unwrap().extend_from_slice(&req.body);
        let new = off + req.body.len() as u64;
        self.0.offset.store(new, Ordering::SeqCst);
        ResponseTemplate::new(204).insert_header("Upload-Offset", new.to_string().as_str())
    }
}

struct Head(Arc<TusState>);
impl Respond for Head {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).insert_header(
            "Upload-Offset",
            self.0.offset.load(Ordering::SeqCst).to_string().as_str(),
        )
    }
}

async fn mount_api(server: &MockServer, initiate_expect: u64) {
    Mock::given(method("POST"))
        .and(path("/api/videos/upload/initiate"))
        .and(header("channelId", "ch1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "result": {"videoId": "v1", "tusSessionId": "s1", "chunkSize": 1}
        })))
        .expect(initiate_expect)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/videos/upload/complete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true, "result": {"videoId": "v1", "status": "processing", "message": "ok"}
        })))
        .mount(server)
        .await;
}

async fn mount_proxy(server: &MockServer, tus: Arc<TusState>) {
    Mock::given(method("POST"))
        .and(path("/api/videos/tus"))
        .and(header("Tus-Resumable", "1.0.0"))
        .and(header_exists("Upload-Metadata"))
        .respond_with(ResponseTemplate::new(201).insert_header("Location", "/api/videos/tus/s1"))
        .mount(server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/videos/tus/s1"))
        .and(header("Authorization", "Bearer tok"))
        .and(header("Content-Type", "application/offset+octet-stream"))
        .respond_with(Patch(tus.clone()))
        .mount(server)
        .await;
    Mock::given(method("HEAD"))
        .and(path("/api/videos/tus/s1"))
        .respond_with(Head(tus))
        .mount(server)
        .await;
}

async fn wait_for(engine: &UploadEngine, id: &str, want: UploadState) -> aun_upload::UploadManifest {
    for _ in 0..400 {
        let m = engine.get(id).unwrap().unwrap();
        if m.state == want {
            return m;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("upload never reached {want:?}: {:?}", engine.get(id).unwrap());
}

#[derive(Default)]
struct Events {
    progress: Mutex<Vec<(u64, u64)>>,
    states: Mutex<Vec<UploadState>>,
}
impl UploadListener for Events {
    fn on_progress(&self, _: &str, sent: u64, total: u64) {
        self.progress.lock().unwrap().push((sent, total));
    }
    fn on_state(&self, _: &str, state: UploadState, _: Option<&Error>) {
        self.states.lock().unwrap().push(state);
    }
}

#[tokio::test]
async fn uploads_in_chunks_and_completes() {
    let server = MockServer::start().await;
    let tus = Arc::new(TusState::default());
    mount_api(&server, 1).await;
    mount_proxy(&server, tus.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "درس.mp4", 600 * KIB);

    let engine = UploadEngine::open(api(&server), dir.path().join("q"), "u1", cfg()).unwrap();
    let events = Arc::new(Events::default());
    engine.add_listener(events.clone());
    let id = engine.enqueue(request(&file)).unwrap();
    engine.start();
    let m = wait_for(&engine, &id, UploadState::Done).await;

    assert_eq!(tus.patches.load(Ordering::SeqCst), 3); // 256 + 256 + 88 KiB
    assert_eq!(*tus.bytes.lock().unwrap(), std::fs::read(&file).unwrap());
    assert_eq!(m.video_id.as_deref(), Some("v1"));
    assert_eq!(m.offset, 600 * KIB as u64);
    assert_eq!(m.sha256.as_ref().map(String::len), Some(64));
    assert_eq!(
        engine
            .find_by_checksum(m.sha256.as_deref().unwrap())
            .unwrap()
            .len(),
        1
    );
    let progress = events.progress.lock().unwrap().clone();
    assert_eq!(progress.last(), Some(&(600 * KIB as u64, 600 * KIB as u64)));
    assert!(
        progress.windows(2).all(|w| w[0].0 <= w[1].0),
        "progress went backwards"
    );
    let states = events.states.lock().unwrap().clone();
    for s in [
        UploadState::Hashing,
        UploadState::Initiating,
        UploadState::Uploading,
        UploadState::Completing,
        UploadState::Done,
    ] {
        assert!(states.contains(&s), "{s:?} missing from {states:?}");
    }

    // The create request carried the session metadata the proxy needs.
    let reqs = server.received_requests().await.unwrap();
    let create = reqs
        .iter()
        .find(|r| r.method.as_str() == "POST" && r.url.path() == "/api/videos/tus")
        .unwrap();
    let meta = create.headers.get("Upload-Metadata").unwrap().to_str().unwrap();
    assert!(meta.contains("tusSessionId czE="), "{meta}");
    assert!(meta.contains("videoId djE="), "{meta}");
    let complete = reqs
        .iter()
        .find(|r| r.url.path() == "/api/videos/upload/complete")
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&complete.body).unwrap();
    assert_eq!(body["videoId"], "v1");
    assert_eq!(body["fileSize"], 600.0 * KIB as f64);

    engine.remove(&id).unwrap();
    assert!(engine.list().unwrap().is_empty());
    engine.shutdown();
}

#[tokio::test]
async fn resumes_after_restart_without_new_session() {
    let server = MockServer::start().await;
    let tus = Arc::new(TusState::default());
    mount_api(&server, 1).await; // one session in total, across both "app launches"
    mount_proxy(&server, tus.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", 700 * KIB);
    let store = dir.path().join("q");

    // First launch (throttled so it can't finish): quit after the first chunk lands.
    let id = {
        let mut slow = cfg();
        slow.bandwidth_bytes_per_sec = Some(400 * KIB as u64);
        let engine = UploadEngine::open(api(&server), &store, "u1", slow).unwrap();
        let id = engine.enqueue(request(&file)).unwrap();
        engine.start();
        for _ in 0..400 {
            if tus.patches.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        engine.shutdown();
        tokio::time::sleep(Duration::from_millis(100)).await;
        id
    };
    let sent_before = tus.offset.load(Ordering::SeqCst);
    assert!(sent_before > 0 && sent_before < 700 * KIB as u64);

    // Second launch: re-queued, resyncs with HEAD and sends only the rest.
    let engine = UploadEngine::open(api(&server), &store, "u1", cfg()).unwrap();
    assert_eq!(engine.get(&id).unwrap().unwrap().state, UploadState::Queued);
    engine.start();
    wait_for(&engine, &id, UploadState::Done).await;
    assert_eq!(*tus.bytes.lock().unwrap(), std::fs::read(&file).unwrap());
    let heads = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "HEAD")
        .count();
    assert!(heads >= 1);
    engine.shutdown();
}

#[tokio::test]
async fn other_accounts_uploads_are_not_loaded() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", KIB);
    let store = dir.path().join("q");
    let e1 = UploadEngine::open(api(&server), &store, "u1", cfg()).unwrap();
    e1.enqueue(request(&file)).unwrap();
    let e2 = UploadEngine::open(api(&server), &store, "u2", cfg()).unwrap();
    assert!(e2.list().unwrap().is_empty());
    assert_eq!(
        UploadEngine::open(api(&server), &store, "u1", cfg())
            .unwrap()
            .list()
            .unwrap()
            .len(),
        1
    );
    assert!(UploadEngine::open(api(&server).with_account("u3"), &store, "u1", cfg()).is_err());
}

#[tokio::test]
async fn expired_proxy_session_falls_back_to_direct_upload() {
    let server = MockServer::start().await;
    let direct = Arc::new(TusState::default());
    mount_api(&server, 1).await;
    // The proxy forgot the session (KV TTL): create returns 404.
    Mock::given(method("POST"))
        .and(path("/api/videos/tus"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/videos/upload/resume/v1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "result": {
                "videoId": "v1",
                "tusEndpoint": format!("{}/direct/tusupload", server.uri()),
                "tusHeaders": {"AuthorizationSignature": "sig", "AuthorizationExpire": "9999999999",
                               "LibraryId": "lib", "VideoId": "v1"},
                "expiresAt": "2099-01-01T00:00:00Z",
                "chunkSize": 1
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/direct/tusupload"))
        .and(header("AuthorizationSignature", "sig"))
        .and(header("LibraryId", "lib"))
        .respond_with(ResponseTemplate::new(201).insert_header("Location", "/direct/tusupload/xyz"))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/direct/tusupload/xyz"))
        .and(header("AuthorizationSignature", "sig"))
        .respond_with(Patch(direct.clone()))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", 300 * KIB);
    let engine = UploadEngine::open(api(&server), dir.path().join("q"), "u1", cfg()).unwrap();
    let id = engine.enqueue(request(&file)).unwrap();
    engine.start();
    let m = wait_for(&engine, &id, UploadState::Done).await;
    assert!(matches!(m.target, Some(aun_upload::TusTarget::Direct { .. })));
    assert_eq!(*direct.bytes.lock().unwrap(), std::fs::read(&file).unwrap());
    engine.shutdown();
}

#[tokio::test]
async fn rate_limited_initiate_backs_off_instead_of_hammering() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/videos/upload/initiate"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "120"))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", KIB);
    let engine = UploadEngine::open(api(&server), dir.path().join("q"), "u1", cfg()).unwrap();
    let id = engine.enqueue(request(&file)).unwrap();
    engine.start();
    for _ in 0..200 {
        if engine.get(&id).unwrap().unwrap().last_error.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(700)).await; // a scheduler tick passes
    let m = engine.get(&id).unwrap().unwrap();
    assert_eq!(m.state, UploadState::Queued);
    assert_eq!(m.attempts, 0, "rate limiting is not a failure");
    let wait = m.not_before_ms - SystemClockNow::now();
    assert!(wait > 100_000, "should wait out Retry-After, waits {wait} ms");
    engine.shutdown();
}

struct SystemClockNow;
impl SystemClockNow {
    fn now() -> i64 {
        use aun_common::platform::Clock;
        SystemClock.now_ms()
    }
}

struct Net {
    metered: AtomicBool,
}
impl Connectivity for Net {
    fn current(&self) -> NetworkState {
        NetworkState {
            online: true,
            metered: self.metered.load(Ordering::SeqCst),
        }
    }
}

#[tokio::test]
async fn wifi_only_waits_on_metered_network() {
    let server = MockServer::start().await;
    let tus = Arc::new(TusState::default());
    mount_api(&server, 1).await;
    mount_proxy(&server, tus.clone()).await;
    let net = Arc::new(Net {
        metered: AtomicBool::new(true),
    });
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", 100 * KIB);
    let mut c = cfg();
    c.wifi_only = true;
    let engine = UploadEngine::open_with(
        api(&server),
        dir.path().join("q"),
        "u1",
        c,
        net.clone(),
        Arc::new(SystemClock),
    )
    .unwrap();
    let id = engine.enqueue(request(&file)).unwrap();
    engine.start();
    wait_for(&engine, &id, UploadState::Waiting).await;
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "nothing sent on cellular"
    );

    net.metered.store(false, Ordering::SeqCst);
    engine.poke();
    wait_for(&engine, &id, UploadState::Done).await;
    engine.shutdown();
}

#[tokio::test]
async fn cancel_terminates_and_changed_file_fails() {
    let server = MockServer::start().await;
    let tus = Arc::new(TusState::default());
    mount_api(&server, 1).await;
    mount_proxy(&server, tus.clone()).await;
    Mock::given(method("DELETE"))
        .and(path_regex("^/api/videos/tus/s1$"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", 2048 * KIB);
    let mut c = cfg();
    c.bandwidth_bytes_per_sec = Some(512 * KIB as u64); // slow enough to cancel mid-way
    let engine = UploadEngine::open(api(&server), dir.path().join("q"), "u1", c).unwrap();
    let id = engine.enqueue(request(&file)).unwrap();
    engine.start();
    wait_for(&engine, &id, UploadState::Uploading).await;
    for _ in 0..200 {
        if engine.get(&id).unwrap().unwrap().upload_url.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine.cancel(&id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(engine.get(&id).unwrap().unwrap().state, UploadState::Cancelled);
    assert!(engine.cancel(&id).await.is_ok());

    // A file that changes after queueing is not resumed.
    let file2 = video(dir.path(), "b.mp4", 10 * KIB);
    let id2 = engine.enqueue(request(&file2)).unwrap();
    engine.pause(&id2).unwrap();
    std::fs::write(&file2, b"different").unwrap();
    engine.resume(&id2).unwrap();
    let m = wait_for(&engine, &id2, UploadState::Failed).await;
    assert!(m.last_error.unwrap().contains("changed"));
    engine.shutdown();
}

#[tokio::test]
async fn rejects_bad_input() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = UploadEngine::open(api(&server), dir.path().join("q"), "u1", cfg()).unwrap();
    assert!(matches!(
        engine.enqueue(request("/nonexistent/x.mp4")),
        Err(Error::InvalidInput(_))
    ));
    let empty = video(dir.path(), "e.mp4", 0);
    assert!(matches!(
        engine.enqueue(request(&empty)),
        Err(Error::InvalidInput(_))
    ));
    let mut r = request(&video(dir.path(), "a.mp4", 10));
    r.title = " ".into();
    assert!(matches!(engine.enqueue(r), Err(Error::InvalidInput(_))));
    assert!(UploadEngine::open(api(&server), dir.path().join("q"), "", cfg()).is_err());
}

#[tokio::test]
async fn foreign_upload_location_fails_without_sending_the_token() {
    let server = MockServer::start().await;
    mount_api(&server, 1).await;
    // A hostile proxy response pointing the upload (and our bearer token) at another origin.
    Mock::given(method("POST"))
        .and(path("/api/videos/tus"))
        .respond_with(ResponseTemplate::new(201).insert_header("Location", "https://evil.example/tus/s1"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", 10 * KIB);
    let engine = UploadEngine::open(api(&server), dir.path().join("q"), "u1", cfg()).unwrap();
    let id = engine.enqueue(request(&file)).unwrap();
    engine.start();
    let m = wait_for(&engine, &id, UploadState::Failed).await;
    assert_eq!(m.upload_url, None);
    let reqs = server.received_requests().await.unwrap();
    assert!(!reqs.iter().any(|r| r.method.as_str() == "PATCH"));
    engine.shutdown();
}

#[tokio::test]
async fn insecure_tus_endpoint_override_is_refused() {
    let server = MockServer::start().await;
    mount_api(&server, 1).await;
    let dir = tempfile::tempdir().unwrap();
    let file = video(dir.path(), "a.mp4", 10 * KIB);
    let mut c = cfg();
    c.tus_endpoint = Some("http://uploads.example.com/api/videos/tus".into());
    let engine = UploadEngine::open(api(&server), dir.path().join("q"), "u1", c).unwrap();
    let id = engine.enqueue(request(&file)).unwrap();
    engine.start();
    wait_for(&engine, &id, UploadState::Failed).await;
    engine.shutdown();
}
