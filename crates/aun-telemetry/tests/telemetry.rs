#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Consent gating, persistence, caps, breadcrumbs, crash reports and flushing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use aun_common::platform::ManualClock;
use aun_common::{Error, Result};
use aun_telemetry::{
    BreadcrumbLevel, ConsentState, CrashReport, EventKind, NoopSink, PlaybackErrorCode, PropValue,
    QoeTracker, RecordOutcome, Telemetry, TelemetryBatch, TelemetryConfig, TelemetrySink,
};

fn props(pairs: &[(&str, PropValue)]) -> HashMap<String, PropValue> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

/// Records batches; can be told to fail.
#[derive(Default)]
struct Recorder {
    batches: Mutex<Vec<TelemetryBatch>>,
    fail_with: Mutex<Option<Error>>,
    calls: AtomicUsize,
}

#[async_trait]
impl TelemetrySink for Recorder {
    async fn send(&self, batch: &TelemetryBatch) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.fail_with.lock().unwrap().clone() {
            return Err(e);
        }
        self.batches.lock().unwrap().push(batch.clone());
        Ok(())
    }
}

fn config(dir: &tempfile::TempDir) -> TelemetryConfig {
    TelemetryConfig {
        buffer_path: Some(dir.path().join("telemetry.json").to_string_lossy().into_owned()),
        ..TelemetryConfig::default()
    }
}

#[test]
fn nothing_is_recorded_or_stored_without_consent() {
    let dir = tempfile::tempdir().unwrap();
    let t = Telemetry::new(config(&dir), Arc::new(NoopSink));
    assert_eq!(t.consent(), ConsentState::Unknown);
    assert_eq!(t.track("screen_view", HashMap::new()), RecordOutcome::NoConsent);
    t.add_breadcrumb("nav", "home", BreadcrumbLevel::Info);
    assert_eq!(t.report_crash(CrashReport::default()), RecordOutcome::NoConsent);
    assert_eq!(t.pending_count(), 0);
    assert!(t.breadcrumbs().is_empty());
    t.persist().unwrap();
    assert!(
        !dir.path().join("telemetry.json").exists(),
        "nothing written without consent"
    );

    t.set_consent(ConsentState::Denied).unwrap();
    assert_eq!(t.track("screen_view", HashMap::new()), RecordOutcome::NoConsent);
    assert!(!dir.path().join("telemetry.json").exists());
}

#[test]
fn crash_only_consent_keeps_only_crashes() {
    let dir = tempfile::tempdir().unwrap();
    let t = Telemetry::new(config(&dir), Arc::new(NoopSink));
    t.set_consent(ConsentState::CrashOnly).unwrap();
    assert_eq!(t.track("screen_view", HashMap::new()), RecordOutcome::NoConsent);
    assert_eq!(
        t.track_playback_error("v", PlaybackErrorCode::Network, true, ""),
        RecordOutcome::NoConsent
    );
    t.add_breadcrumb("player", "buffering", BreadcrumbLevel::Warning);
    assert_eq!(
        t.report_crash(CrashReport {
            kind: "SIGSEGV".into(),
            message: "boom for sara@example.com".into(),
            ..CrashReport::default()
        }),
        RecordOutcome::Recorded
    );
    let events = t.pending_events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, EventKind::Crash);
    assert_eq!(events[0].breadcrumbs.len(), 1);
    assert_eq!(
        events[0].properties["message"],
        PropValue::Text("boom for [email]".into())
    );
}

#[test]
fn narrowing_consent_purges_memory_and_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.json");
    let t = Telemetry::new(config(&dir), Arc::new(NoopSink));
    t.set_consent(ConsentState::Analytics).unwrap();
    t.track("screen_view", props(&[("screen", "home".into())]));
    t.report_crash(CrashReport {
        kind: "panic".into(),
        ..CrashReport::default()
    });
    assert_eq!(t.pending_count(), 2);
    assert!(path.exists());

    t.set_consent(ConsentState::CrashOnly).unwrap();
    assert_eq!(t.pending_count(), 1, "analytics purged");
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert!(!on_disk.contains("screen_view"), "purged from disk too");

    t.add_breadcrumb("nav", "x", BreadcrumbLevel::Info);
    t.set_consent(ConsentState::Denied).unwrap();
    assert_eq!(t.pending_count(), 0);
    assert!(t.breadcrumbs().is_empty());
    assert!(!path.exists(), "buffer file deleted on revoke");
}

#[test]
fn buffer_survives_restart_and_breadcrumbs_attach_to_next_crash() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(1_000));
    let t = Telemetry::with_clock(config(&dir), Arc::new(NoopSink), clock.clone());
    t.set_consent(ConsentState::Analytics).unwrap();
    t.track("screen_view", HashMap::new());
    t.add_breadcrumb("nav", "opened video", BreadcrumbLevel::Info);
    t.add_breadcrumb(
        "player",
        "token=abc https://x.net/v?sig=1",
        BreadcrumbLevel::Error,
    );
    t.persist().unwrap(); // app went to background, then crashed
    drop(t);

    let t2 = Telemetry::with_clock(config(&dir), Arc::new(NoopSink), clock);
    assert_eq!(t2.pending_count(), 0, "not loaded before consent");
    t2.set_consent(ConsentState::Analytics).unwrap();
    assert_eq!(t2.pending_count(), 1);
    let prev = t2.previous_session_breadcrumbs();
    assert_eq!(prev.len(), 2);
    assert_eq!(prev[1].message, "token=[token] https://x.net/v?[redacted]");
    t2.add_breadcrumb("nav", "new session", BreadcrumbLevel::Info);
    t2.report_crash(CrashReport {
        kind: "SIGABRT".into(),
        message: "abort".into(),
        stack: Some("frame0\nframe1 Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln".into()),
        occurred_at_ms: 900,
        from_previous_session: true,
        ..CrashReport::default()
    });
    let crash = t2
        .pending_events()
        .into_iter()
        .find(|e| e.kind == EventKind::Crash)
        .unwrap();
    assert_eq!(crash.breadcrumbs.len(), 2, "previous session's trail");
    assert_eq!(crash.breadcrumbs[0].message, "opened video");
    let PropValue::Text(stack) = &crash.properties["stack"] else {
        panic!("stack missing")
    };
    assert!(stack.contains("[token]") && !stack.contains("eyJ"));
    assert!(t2.previous_session_breadcrumbs().is_empty(), "attached once");
}

#[test]
fn caps_sampling_and_scrubbing() {
    let cfg = TelemetryConfig {
        max_buffered_events: 3,
        max_event_bytes: 600,
        max_string_chars: 20,
        ..TelemetryConfig::default()
    };
    let t = Telemetry::new(cfg, Arc::new(NoopSink));
    t.set_consent(ConsentState::Analytics).unwrap();
    t.report_crash(CrashReport {
        kind: "first".into(),
        ..CrashReport::default()
    });
    for i in 0..5 {
        t.track("tap", props(&[("i", PropValue::Int(i))]));
    }
    let events = t.pending_events();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].kind, EventKind::Crash, "crashes survive the cap");
    assert_eq!(events[2].properties["i"], PropValue::Int(4));

    assert_eq!(t.track("Bad Name", HashMap::new()), RecordOutcome::InvalidName);
    let big: HashMap<String, PropValue> = (0..40)
        .map(|i| (format!("k{i:02}"), PropValue::Text("x".repeat(100))))
        .collect();
    assert_eq!(t.track("big", big), RecordOutcome::TooLarge);

    t.track(
        "profile",
        props(&[
            ("email", "a@b.co".into()),
            ("bio", "write to a@b.co please, thanks!".into()),
        ]),
    );
    let last = t.pending_events().pop().unwrap();
    assert_eq!(last.properties["email"], PropValue::Text("[redacted]".into()));
    assert_eq!(
        last.properties["bio"],
        PropValue::Text("write to [email] pl…".into())
    );

    let sampled = Telemetry::new(
        TelemetryConfig {
            sample_rate: 0.0,
            ..TelemetryConfig::default()
        },
        Arc::new(NoopSink),
    );
    sampled.set_consent(ConsentState::Analytics).unwrap();
    assert_eq!(sampled.track("x", HashMap::new()), RecordOutcome::SampledOut);
    assert_eq!(
        sampled.report_crash(CrashReport::default()),
        RecordOutcome::Recorded,
        "crashes are never sampled out"
    );
}

#[tokio::test]
async fn flush_batches_retries_and_drops() {
    let sink = Arc::new(Recorder::default());
    let t = Telemetry::new(
        TelemetryConfig {
            max_batch_events: 2,
            ..TelemetryConfig::default()
        },
        sink.clone(),
    );
    t.set_consent(ConsentState::Analytics).unwrap();
    for _ in 0..5 {
        t.track("tap", HashMap::new());
    }

    *sink.fail_with.lock().unwrap() = Some(Error::Network("down".into()));
    let err = t.flush().await.unwrap_err();
    assert!(err.is_retryable());
    assert_eq!(t.pending_count(), 5, "kept for retry");

    *sink.fail_with.lock().unwrap() = None;
    let r = t.flush().await.unwrap();
    assert_eq!((r.sent, r.remaining), (5, 0));
    let batches = sink.batches.lock().unwrap().clone();
    assert_eq!(
        batches.iter().map(|b| b.events.len()).collect::<Vec<_>>(),
        vec![2, 2, 1]
    );
    assert!(!batches[0].app.core_version.is_empty());

    t.track("tap", HashMap::new());
    *sink.fail_with.lock().unwrap() = Some(Error::InvalidInput("bad batch".into()));
    let r = t.flush().await.unwrap();
    assert_eq!(
        (r.sent, r.dropped, r.remaining),
        (0, 1, 0),
        "permanent failure drops the batch"
    );

    // No consent: flush is a no-op that never calls the sink.
    t.set_consent(ConsentState::Denied).unwrap();
    let calls = sink.calls.load(Ordering::SeqCst);
    assert_eq!(t.flush().await.unwrap().sent, 0);
    assert_eq!(sink.calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn qoe_summary_is_recorded() {
    let sink = Arc::new(Recorder::default());
    let t = Telemetry::new(TelemetryConfig::default(), sink.clone());
    t.set_consent(ConsentState::Analytics).unwrap();
    let mut q = QoeTracker::new("vid-1", "vod");
    q.play_requested(0);
    q.first_frame(650);
    q.buffering_started(5_000);
    q.buffering_ended(6_000);
    let summary = q.finish(10_650);
    assert_eq!(t.track_qoe(&summary), RecordOutcome::Recorded);
    t.flush().await.unwrap();
    let batches = sink.batches.lock().unwrap();
    let e = &batches[0].events[0];
    assert_eq!(e.name, "qoe_summary");
    assert_eq!(e.kind, EventKind::Qoe);
    assert_eq!(e.properties["startup_time_ms"], PropValue::Int(650));
    assert_eq!(e.properties["rebuffer_count"], PropValue::Int(1));
    assert_eq!(e.properties["rebuffer_ratio"], PropValue::Float(0.1));
}
