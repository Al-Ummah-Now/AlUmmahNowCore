#![allow(clippy::unwrap_used, clippy::expect_used)]
//! HTTP and api-videos sinks against wiremock.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aun_api::{ApiClient, ApiConfig, RetryPolicy};
use aun_common::Error;
use aun_telemetry::{
    ApiPerformanceSink, AppInfo, ConsentState, CrashReport, HttpSink, PlaybackErrorCode, Telemetry,
    TelemetryBatch, TelemetryConfig, TelemetrySink,
};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn app() -> AppInfo {
    AppInfo {
        platform: "ios".into(),
        app_version: "1.2.3".into(),
        os_version: "26.0".into(),
        device_class: "phone".into(),
        locale: "ar".into(),
        core_version: String::new(),
    }
}

#[tokio::test]
async fn http_sink_posts_json_batches() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/ingest"))
        .and(header("x-ingest-key", "public-test-key"))
        .and(body_partial_json(serde_json::json!({"app": {"platform": "ios"}})))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&server)
        .await;

    let sink = HttpSink::new(&format!("{}/ingest", server.uri()), Some("aun-test"))
        .unwrap()
        .with_header("x-ingest-key", "public-test-key");
    let t = Telemetry::new(
        TelemetryConfig {
            app: app(),
            ..TelemetryConfig::default()
        },
        Arc::new(sink),
    );
    t.set_consent(ConsentState::Analytics).unwrap();
    t.track("screen_view", HashMap::new());
    t.report_crash(CrashReport {
        kind: "panic".into(),
        ..CrashReport::default()
    });
    let r = t.flush().await.unwrap();
    assert_eq!(r.sent, 2);

    let received: Vec<Request> = server.received_requests().await.unwrap();
    let batch: TelemetryBatch = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(batch.events.len(), 2);
    assert_eq!(batch.app.app_version, "1.2.3");
    assert_eq!(batch.app.core_version, aun_common::VERSION);
}

#[tokio::test]
async fn http_sink_maps_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/busy"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/down"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/bad"))
        .respond_with(ResponseTemplate::new(400).set_body_string("schema"))
        .mount(&server)
        .await;
    let batch = TelemetryBatch {
        batch_id: "b".into(),
        sent_at_ms: 1,
        app: app(),
        events: vec![],
    };
    let send = |p: &str| {
        let sink = HttpSink::new(&format!("{}{p}", server.uri()), None).unwrap();
        let batch = batch.clone();
        async move { sink.send(&batch).await }
    };
    assert_eq!(
        send("/busy").await.unwrap_err(),
        Error::RateLimited {
            retry_after: Some(Duration::from_secs(30))
        }
    );
    assert!(send("/down").await.unwrap_err().is_retryable());
    assert_eq!(
        send("/bad").await.unwrap_err(),
        Error::InvalidInput("schema".into())
    );
    assert!(HttpSink::new("ftp://x", None).is_err());
    assert!(HttpSink::new("http://telemetry.example.com/ingest", None).is_err());
    assert!(HttpSink::new("https://telemetry.example.com/ingest", None).is_ok());
    let dead = HttpSink::new("http://127.0.0.1:9/x", None).unwrap();
    assert!(matches!(dead.send(&batch).await, Err(Error::Network(_))));
}

#[tokio::test]
async fn api_sink_sends_performance_events_only() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/analytics/events"))
        .and(body_partial_json(serde_json::json!({"kind": "performance"})))
        .respond_with(
            ResponseTemplate::new(202).set_body_json(serde_json::json!({"success": true, "accepted": 1})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = ApiConfig::new(server.uri());
    cfg.retry = RetryPolicy::none();
    let sink = ApiPerformanceSink::new(ApiClient::new(cfg).unwrap());
    let t = Telemetry::new(
        TelemetryConfig {
            app: app(),
            ..TelemetryConfig::default()
        },
        Arc::new(sink),
    );
    t.set_consent(ConsentState::Analytics).unwrap();
    t.track("screen_view", HashMap::new());
    t.track_playback_error("vid", PlaybackErrorCode::Decode, false, "bad frame");
    let r = t.flush().await.unwrap();
    assert_eq!((r.sent, r.unsupported, r.remaining), (1, 1, 0));

    let received = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["events"].as_array().unwrap().len(), 1);
    assert_eq!(body["events"][0]["name"], "playback_error");
    assert_eq!(body["events"][0]["properties"]["code"], "decode");
    assert_eq!(body["events"][0]["platform"], "ios");
}
