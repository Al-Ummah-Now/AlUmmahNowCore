//! The FFI facade moves these across threads; a lost `Send` or `Sync` should fail here, not there.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use aun_media::CancellationToken;
use aun_media::job::{ConformanceOptions, JobHandle, JobRunner, SoftwareBackend, run_conformance};
use aun_media::transcribe::{DownloadListener, HttpModelFetcher, ModelManager};

fn assert_send_sync<T: Send + Sync>() {}
fn assert_send<T: Send>(_: T) {}

struct Quiet;
impl DownloadListener for Quiet {
    fn on_progress(&self, _: u64, _: u64) {}
}

#[test]
fn handles_are_shareable() {
    assert_send_sync::<JobRunner>();
    assert_send_sync::<JobHandle>();
    assert_send_sync::<ModelManager>();
    assert_send_sync::<SoftwareBackend>();
}

#[tokio::test]
async fn long_running_futures_can_be_spawned() {
    let dir = tempfile::tempdir().unwrap();
    assert_send(run_conformance(
        Arc::new(SoftwareBackend::new()),
        ConformanceOptions::new(dir.path().to_string_lossy()),
    ));
    let manager = ModelManager::new(dir.path(), Arc::new(HttpModelFetcher::new()));
    let token = CancellationToken::new();
    assert_send(manager.download("tiny", Arc::new(Quiet), &token));
}
