#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aun_common::Error;
use aun_media::CancellationToken;
use aun_media::transcribe::{DownloadListener, HttpModelFetcher, ModelDescriptor, ModelManager, ModelTier};
use sha2::{Digest, Sha256};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn descriptor(url: String, payload: &[u8]) -> ModelDescriptor {
    ModelDescriptor {
        id: "test".into(),
        tier: ModelTier::Tiny,
        quantization: None,
        file_name: "ggml-test.bin".into(),
        url,
        size_bytes: payload.len() as u64,
        sha256: sha256(payload),
    }
}

fn manager(dir: &std::path::Path, d: ModelDescriptor) -> ModelManager {
    ModelManager::with_catalog(dir, vec![d], Arc::new(HttpModelFetcher::new()))
}

#[derive(Default)]
struct Progress(AtomicU64);

impl DownloadListener for Progress {
    fn on_progress(&self, downloaded_bytes: u64, _total: u64) {
        self.0.store(downloaded_bytes, Ordering::SeqCst);
    }
}

/// Serves `payload`, honouring `Range`, and records which ranges were requested.
struct RangeServer {
    payload: Vec<u8>,
    ranges: Arc<Mutex<Vec<Option<String>>>>,
}

impl Respond for RangeServer {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let range = request
            .headers
            .get("range")
            .map(|v| v.to_str().unwrap().to_owned());
        self.ranges.lock().unwrap().push(range.clone());
        match range {
            Some(value) => {
                let offset: usize = value
                    .trim_start_matches("bytes=")
                    .trim_end_matches('-')
                    .parse()
                    .unwrap();
                ResponseTemplate::new(206).set_body_bytes(self.payload[offset..].to_vec())
            }
            None => ResponseTemplate::new(200).set_body_bytes(self.payload.clone()),
        }
    }
}

fn payload(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

async fn serve(payload: &[u8], ranges: Arc<Mutex<Vec<Option<String>>>>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/m.bin"))
        .respond_with(RangeServer {
            payload: payload.to_vec(),
            ranges,
        })
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn downloads_verifies_and_installs() {
    let data = payload(4096);
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let server = serve(&data, ranges.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), descriptor(format!("{}/m.bin", server.uri()), &data));

    assert!(!m.list().unwrap()[0].installed);
    assert_eq!(m.model_path("test").unwrap(), None);

    let progress = Arc::new(Progress::default());
    let path = m
        .download("test", progress.clone(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(progress.0.load(Ordering::SeqCst), 4096);
    assert!(m.list().unwrap()[0].installed);
    assert_eq!(m.model_path("test").unwrap(), Some(path));
    assert_eq!(m.disk_usage_bytes().unwrap(), 4096);
    assert!(m.verify("test").unwrap());
    assert_eq!(*ranges.lock().unwrap(), vec![None]);
}

#[tokio::test]
async fn resumes_from_a_partial_file_with_a_range_request() {
    let data = payload(10_000);
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let server = serve(&data, ranges.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let d = descriptor(format!("{}/m.bin", server.uri()), &data);
    std::fs::write(dir.path().join("ggml-test.bin.part"), &data[..6000]).unwrap();

    let m = manager(dir.path(), d);
    assert_eq!(m.list().unwrap()[0].partial_bytes, 6000);
    let path = m
        .download("test", Arc::new(Progress::default()), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), data);
    assert_eq!(*ranges.lock().unwrap(), vec![Some("bytes=6000-".to_owned())]);
    assert_eq!(m.list().unwrap()[0].partial_bytes, 0);
}

#[tokio::test]
async fn server_ignoring_range_restarts_the_file() {
    let data = payload(2000);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/m.bin"))
        .and(header("range", "bytes=500-"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(data.clone()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ggml-test.bin.part"), vec![0xAA; 500]).unwrap();
    let m = manager(dir.path(), descriptor(format!("{}/m.bin", server.uri()), &data));
    let path = m
        .download("test", Arc::new(Progress::default()), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), data);
}

#[tokio::test]
async fn bad_checksum_discards_the_download() {
    let data = payload(1000);
    let server = serve(&data, Arc::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let mut d = descriptor(format!("{}/m.bin", server.uri()), &data);
    d.sha256 = "0".repeat(64);
    let m = manager(dir.path(), d);
    let err = m
        .download("test", Arc::new(Progress::default()), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err}");
    assert!(err.is_retryable());
    assert!(!m.list().unwrap()[0].installed);
    assert_eq!(m.list().unwrap()[0].partial_bytes, 0);
    assert_eq!(m.disk_usage_bytes().unwrap(), 0);
}

#[tokio::test]
async fn truncated_transfer_keeps_the_part_for_resume() {
    let data = payload(1000);
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/m.bin"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(data[..600].to_vec()))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), descriptor(format!("{}/m.bin", server.uri()), &data));
    let err = m
        .download("test", Arc::new(Progress::default()), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Network(_)), "{err}");
    assert_eq!(m.list().unwrap()[0].partial_bytes, 600);
}

#[tokio::test]
async fn oversized_part_restarts_from_zero() {
    let data = payload(100);
    let ranges = Arc::new(Mutex::new(Vec::new()));
    let server = serve(&data, ranges.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ggml-test.bin.part"), vec![1; 500]).unwrap();
    let m = manager(dir.path(), descriptor(format!("{}/m.bin", server.uri()), &data));
    m.download("test", Arc::new(Progress::default()), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*ranges.lock().unwrap(), vec![None]);
}

#[tokio::test]
async fn http_errors_map_to_the_error_model() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/m.bin"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), descriptor(format!("{}/m.bin", server.uri()), b"x"));
    let err = m
        .download("test", Arc::new(Progress::default()), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
}

#[tokio::test]
async fn cancellation_stops_the_download() {
    let data = payload(100);
    let server = serve(&data, Arc::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), descriptor(format!("{}/m.bin", server.uri()), &data));
    let token = CancellationToken::new();
    token.cancel();
    let err = m
        .download("test", Arc::new(Progress::default()), &token)
        .await
        .unwrap_err();
    assert_eq!(err, Error::Cancelled);
    assert!(!m.list().unwrap()[0].installed);
}

#[tokio::test]
async fn delete_removes_files_and_verify_detects_damage() {
    let data = payload(64);
    let server = serve(&data, Arc::default()).await;
    let dir = tempfile::tempdir().unwrap();
    let m = manager(dir.path(), descriptor(format!("{}/m.bin", server.uri()), &data));
    let path = m
        .download("test", Arc::new(Progress::default()), &CancellationToken::new())
        .await
        .unwrap();

    let mut damaged = data.clone();
    damaged[0] ^= 0xFF;
    std::fs::write(&path, damaged).unwrap();
    assert!(!m.verify("test").unwrap(), "same size, different bytes");

    m.delete("test").unwrap();
    assert!(!std::path::Path::new(&path).exists());
    assert_eq!(m.disk_usage_bytes().unwrap(), 0);
    m.delete("test").unwrap();
}

#[tokio::test]
async fn unknown_model_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let m = ModelManager::new(dir.path(), Arc::new(HttpModelFetcher::new()));
    assert_eq!(m.list().unwrap().len(), 15);
    assert!(matches!(m.delete("nope"), Err(Error::NotFound(_))));
    assert!(matches!(
        m.download("nope", Arc::new(Progress::default()), &CancellationToken::new())
            .await,
        Err(Error::NotFound(_))
    ));
}
