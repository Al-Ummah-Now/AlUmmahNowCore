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
    *sink.fail_with.lock().unwrap() = Some(Error::invalid_input("bad batch".into()));
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

#[tokio::test]
async fn drops_single_event_exceeding_max_batch_bytes() {
    let sink = Arc::new(Recorder::default());
    let t = Telemetry::new(
        TelemetryConfig {
            max_batch_bytes: 512,
            ..TelemetryConfig::default()
        },
        sink.clone(),
    );
    t.set_consent(ConsentState::CrashOnly).unwrap();
    t.report_crash(CrashReport {
        kind: "SIGSEGV".into(),
        message: "huge".into(),
        stack: Some("x".repeat(2_000)),
        ..Default::default()
    });
    let r = t.flush().await.unwrap();
    assert_eq!((r.sent, r.dropped, r.remaining), (0, 1, 0));
    assert!(sink.batches.lock().unwrap().is_empty());
}

#[test]
fn concurrent_persist_and_revoke_leaves_no_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("telemetry.json");
    let t = Telemetry::new(config(&dir), Arc::new(NoopSink));
    for _ in 0..20 {
        t.set_consent(ConsentState::Analytics).unwrap();
        t.track("tap", HashMap::new());
        let t1 = t.clone();
        let t2 = t.clone();
        let h1 = std::thread::spawn(move || {
            let _ = t1.persist();
        });
        let h2 = std::thread::spawn(move || {
            t2.set_consent(ConsentState::Denied).unwrap();
        });
        h1.join().unwrap();
        h2.join().unwrap();
        // After Denied finishes (and any in-flight persist completes), final consent is Denied
        // so the buffer file must not exist if Denied ran last, and calling Denied guarantees removal.
        if t.consent() == ConsentState::Denied {
            assert!(!path.exists(), "persist recreated buffer file after Denied");
        }
    }
}

#[tokio::test]
async fn flush_respects_max_batch_bytes_with_large_app_metadata() {
    let sink = Arc::new(Recorder::default());
    let cap = 1_024u32;
    let t = Telemetry::new(
        TelemetryConfig {
            app: aun_telemetry::AppInfo {
                platform: "ios".into(),
                app_version: "1.0.0-".to_owned() + &"v".repeat(200),
                os_version: "18.2-".to_owned() + &"o".repeat(200),
                device_class: "phone".into(),
                locale: "ar-SA".into(),
                core_version: String::new(),
            },
            max_batch_bytes: cap,
            ..TelemetryConfig::default()
        },
        sink.clone(),
    );
    t.set_consent(ConsentState::Analytics).unwrap();
    for i in 0..5 {
        let mut props = HashMap::new();
        props.insert(
            "screen".into(),
            PropValue::Text(format!("screen_{i}_") + &"s".repeat(120)),
        );
        assert_eq!(t.track("screen_view", props), RecordOutcome::Recorded);
    }
    let report = t.flush().await.unwrap();
    assert_eq!(report.sent, 5);
    assert_eq!(report.remaining, 0);
    let batches = sink.batches.lock().unwrap();
    assert!(batches.len() > 1, "expected split across multiple batches");
    for batch in batches.iter() {
        let actual_len = serde_json::to_vec(batch).unwrap().len();
        assert!(
            actual_len <= cap as usize,
            "batch JSON ({actual_len} bytes) exceeded max_batch_bytes ({cap})"
        );
    }
}

#[tokio::test]
async fn flush_cancels_in_flight_send_when_consent_revoked() {
    struct BlockingSink {
        started: tokio::sync::Notify,
        completed: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TelemetrySink for BlockingSink {
        async fn send(&self, _batch: &TelemetryBatch) -> aun_common::Result<()> {
            self.started.notify_one();
            std::future::pending::<()>().await;
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let sink = Arc::new(BlockingSink {
        started: tokio::sync::Notify::new(),
        completed: AtomicUsize::new(0),
    });
    let t = Telemetry::new(TelemetryConfig::default(), sink.clone());
    t.set_consent(ConsentState::Analytics).unwrap();
    t.track("tap", HashMap::new());

    let t_flush = t.clone();
    let handle = tokio::spawn(async move { t_flush.flush().await });
    sink.started.notified().await;
    t.set_consent(ConsentState::Denied).unwrap();
    let report = handle.await.unwrap().unwrap();
    assert_eq!(report.sent, 0);
    assert_eq!(report.remaining, 0);
    assert_eq!(sink.completed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn flush_respects_max_batch_bytes_with_i64_min_timestamp() {
    struct MinClock;
    impl aun_common::platform::Clock for MinClock {
        fn now_ms(&self) -> i64 {
            i64::MIN
        }
    }

    let sink = Arc::new(Recorder::default());
    let cap = 512u32;
    let t = Telemetry::with_clock(
        TelemetryConfig {
            max_batch_bytes: cap,
            ..TelemetryConfig::default()
        },
        sink.clone(),
        Arc::new(MinClock),
    );
    t.set_consent(ConsentState::Analytics).unwrap();
    for i in 0..4 {
        let mut props = HashMap::new();
        props.insert("k".into(), PropValue::Text(format!("val_{i}_") + &"a".repeat(80)));
        assert_eq!(t.track("tap", props), RecordOutcome::Recorded);
    }
    let report = t.flush().await.unwrap();
    assert_eq!(report.sent, 4);
    for batch in sink.batches.lock().unwrap().iter() {
        let len = serde_json::to_vec(batch).unwrap().len();
        assert!(len <= cap as usize, "batch {len} exceeded cap {cap}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flush_never_starts_send_after_consent_denied_returns() {
    use std::sync::atomic::AtomicBool;

    struct FlagSink {
        revoked: Arc<AtomicBool>,
        sent_after_revoke: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl TelemetrySink for FlagSink {
        async fn send(&self, _batch: &TelemetryBatch) -> aun_common::Result<()> {
            if self.revoked.load(Ordering::SeqCst) {
                self.sent_after_revoke.store(true, Ordering::SeqCst);
            }
            tokio::task::yield_now().await;
            Ok(())
        }
    }

    for _ in 0..30 {
        let revoked = Arc::new(AtomicBool::new(false));
        let sent_after_revoke = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(FlagSink {
            revoked: revoked.clone(),
            sent_after_revoke: sent_after_revoke.clone(),
        });
        let t = Telemetry::new(TelemetryConfig::default(), sink);
        t.set_consent(ConsentState::Analytics).unwrap();
        t.track("tap", HashMap::new());
        let t_flush = t.clone();
        let h = tokio::spawn(async move { t_flush.flush().await });
        t.set_consent(ConsentState::Denied).unwrap();
        revoked.store(true, Ordering::SeqCst);
        let _ = h.await.unwrap();
        assert!(
            !sent_after_revoke.load(Ordering::SeqCst),
            "sink.send started after set_consent(Denied) returned"
        );
    }
}

#[test]
fn custom_sink_reentrantly_revoking_consent_with_concurrent_caller_does_not_deadlock() {
    use std::sync::mpsc;

    struct ReentrantSink {
        telemetry: std::sync::Mutex<Option<Arc<Telemetry>>>,
    }

    #[async_trait::async_trait]
    impl TelemetrySink for ReentrantSink {
        async fn send(&self, _batch: &TelemetryBatch) -> aun_common::Result<()> {
            let t = self.telemetry.lock().unwrap().clone().unwrap();
            t.set_consent(ConsentState::Denied).unwrap();
            Ok(())
        }
    }

    // Run on a dedicated OS thread with a channel recv_timeout so a synchronous lock deadlock
    // inside `poll()` fails the test promptly instead of hanging the runtime.
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        for _ in 0..20 {
            let sink = Arc::new(ReentrantSink {
                telemetry: std::sync::Mutex::new(None),
            });
            let t = Telemetry::new(TelemetryConfig::default(), sink.clone());
            *sink.telemetry.lock().unwrap() = Some(t.clone());
            t.set_consent(ConsentState::Analytics).unwrap();
            t.track("tap", HashMap::new());
            t.track("tap", HashMap::new());

            let t_ext = t.clone();
            let ext = std::thread::spawn(move || {
                let _ = t_ext.set_consent(ConsentState::Denied);
            });
            let report = rt.block_on(t.flush()).unwrap();
            ext.join().unwrap();
            assert_eq!(t.consent(), ConsentState::Denied);
            assert_eq!(report.remaining, 0);
        }
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("re-entrant + concurrent set_consent deadlocked");
}
