#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ApiSyncTransport` against a wiremock api-videos.

use std::sync::Arc;

use async_trait::async_trait;
use aun_api::{ApiClient, ApiConfig, AuthProvider, RetryPolicy};
use aun_common::Result;
use aun_sync::{ApiSyncTransport, SyncConfig, SyncEngine, SyncTransport};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct StaticToken;
#[async_trait]
impl AuthProvider for StaticToken {
    async fn access_token(&self) -> Option<String> {
        Some("test-token".into())
    }
    async fn refresh(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

fn transport(server: &MockServer) -> ApiSyncTransport {
    let mut cfg = ApiConfig::new(server.uri());
    cfg.retry = RetryPolicy::none();
    ApiSyncTransport::new(ApiClient::new(cfg).unwrap().with_auth(Arc::new(StaticToken)))
}

fn entry(id: &str, progress: f64, duration: f64, at: &str) -> serde_json::Value {
    serde_json::json!({
        "id": format!("h-{id}"), "userId": "u", "videoId": id, "videoTitle": format!("Video {id}"),
        "videoThumbnail": "https://img/x.jpg", "videoDuration": duration, "channelId": "c",
        "channelName": "Channel", "firstWatchedAt": at, "lastWatchedAt": at, "watchCount": 1,
        "progress": progress, "completed": progress >= duration * 0.95, "percentageWatched": 0
    })
}

#[tokio::test]
async fn history_paging_and_units() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/watch-history"))
        .and(query_param("cursor", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "entries": [entry("b", 30.0, 60.0, "2026-11-01T00:00:00Z")], "total": 2, "hasMore": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/watch-history"))
        .and(query_param("sort", "newest"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "entries": [entry("a", 125.5, 600.0, "2026-11-02T10:00:00.000Z")], "total": 2, "hasMore": true, "cursor": "p2"
        })))
        .mount(&server)
        .await;
    let t = transport(&server);
    let p1 = t.fetch_history(None, 50).await.unwrap();
    assert_eq!(p1.entries[0].position_ms, 125_500);
    assert_eq!(p1.entries[0].duration_ms, 600_000);
    assert_eq!(p1.entries[0].last_watched_at_ms, 1_793_613_600_000);
    assert_eq!(p1.entries[0].meta.as_ref().unwrap().title, "Video a");
    assert_eq!(p1.next_cursor.as_deref(), Some("p2"));
    let p2 = t.fetch_history(p1.next_cursor, 50).await.unwrap();
    assert_eq!(p2.next_cursor, None);
}

#[tokio::test]
async fn progress_falls_back_to_record_view() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/watch-history/v1/progress"))
        .and(body_json(serde_json::json!({"progress": 42.5, "duration": 600.0})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "videoId": "v1", "progress": 42.5, "duration": 600, "percentageWatched": 7, "completed": false, "timestamp": "x"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/watch-history/new/progress"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({"error": "no entry"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/watch-history"))
        .and(body_json(
            serde_json::json!({"videoId": "new", "progress": 1.0, "duration": 10.0}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(1)
        .mount(&server)
        .await;
    let t = transport(&server);
    t.push_progress("v1", 42_500, 600_000).await.unwrap();
    t.push_progress("new", 1_000, 10_000).await.unwrap();
}

#[tokio::test]
async fn library_and_likes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/library/watch-later"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"videoId": "s1", "title": "Saved one", "addedAt": "2026-11-01T00:00:00Z"}], "hasMore": false
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/library/liked"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"videos": [{"id": "l1"}]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/library/watch-later"))
        .and(body_json(serde_json::json!({"videoId": "s2"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"success": true})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/library/watch-later/s1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/library/watch-later/order"))
        .and(body_json(serde_json::json!({"videoIds": ["s2", "s1"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/videos/l2/likes"))
        .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({"error": "already liked"})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/videos/l1/likes"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({"error": "not liked"})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/watch-history/gone"))
        .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({"error": "nope"})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/watch-history"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"deleted": 3, "message": "ok"})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let t = transport(&server);
    let saved = t.fetch_saved(None, 50).await.unwrap();
    assert_eq!(saved.items[0].video_id, "s1");
    assert_eq!(saved.items[0].meta.as_ref().unwrap().title, "Saved one");
    assert_eq!(t.fetch_liked(None, 50).await.unwrap().items[0].video_id, "l1");
    t.set_saved("s2", true).await.unwrap();
    t.set_saved("s1", false).await.unwrap();
    t.reorder_saved(&["s2".into(), "s1".into()]).await.unwrap();
    t.set_liked("l2", true).await.unwrap(); // 409 = already liked
    t.set_liked("l1", false).await.unwrap(); // 404 on unlike = not liked
    t.remove_history_entry("gone").await.unwrap(); // already gone
    t.clear_history().await.unwrap();
}

#[tokio::test]
async fn engine_end_to_end_over_http() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/watch-history/v9/progress"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/watch-history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "entries": [entry("v9", 90.0, 600.0, "2026-11-08T00:00:00Z"), entry("v8", 200.0, 300.0, "2026-11-07T00:00:00Z")],
            "total": 2, "hasMore": false
        })))
        .mount(&server)
        .await;
    for p in ["/api/library/watch-later", "/api/library/liked"] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})))
            .mount(&server)
            .await;
    }
    let engine = SyncEngine::open(
        SyncConfig {
            device_id: "phone".into(),
            ..SyncConfig::default()
        },
        Arc::new(transport(&server)),
    )
    .unwrap();
    engine.record_position("v9", 90_000, 600_000).unwrap();
    let r = engine.sync().await.unwrap();
    assert_eq!((r.pushed, r.history_entries_pulled), (1, 2));
    let cw = engine.continue_watching(10);
    assert_eq!(
        cw.iter().map(|c| c.video_id.as_str()).collect::<Vec<_>>(),
        vec!["v9", "v8"]
    );
    assert_eq!(cw[1].title.as_deref(), Some("Video v8"));
}
