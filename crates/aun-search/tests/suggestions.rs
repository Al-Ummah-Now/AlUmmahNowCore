//! Suggestions client against an in-process mock server (never QA/prod).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use aun_api::{ApiClient, ApiConfig, Error, RetryPolicy};
use aun_search::{SuggestionKind, SuggestionQuery, fetch_suggestions};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> ApiClient {
    let mut c = ApiConfig::new(server.uri());
    c.retry = RetryPolicy {
        max_attempts: 4,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_secs(30),
    };
    ApiClient::new(c).unwrap()
}

#[tokio::test]
async fn sends_parameters_and_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search/suggestions"))
        .and(query_param("q", "تفسير"))
        .and(query_param("limit", "6"))
        .and(query_param("types", "channel,video"))
        .and(query_param("channelId", "ch_9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"text": "تفسير سورة الكهف", "type": "video", "id": "v1", "subtitle": "قناة", "verified": true},
            {"text": "تفسير", "type": "query"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let got = fetch_suggestions(
        &client(&server),
        &SuggestionQuery {
            text: " تفسير ".into(),
            limit: Some(6),
            kinds: vec![SuggestionKind::Video, SuggestionKind::Channel],
            channel_id: Some("ch_9".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].id.as_deref(), Some("v1"));
    assert_eq!(got[1].kind, SuggestionKind::Query);
}

#[tokio::test]
async fn rate_limit_is_returned_at_once_with_retry_after() {
    let server = MockServer::start().await;
    Mock::given(path("/api/search/suggestions"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "3"))
        .expect(1)
        .mount(&server)
        .await;
    let q = SuggestionQuery {
        text: "q".into(),
        ..SuggestionQuery::default()
    };
    let err = fetch_suggestions(&client(&server), &q).await.unwrap_err();
    assert_eq!(
        err,
        Error::RateLimited {
            retry_after: Some(Duration::from_secs(3))
        }
    );
}

#[tokio::test]
async fn server_errors_are_not_retried() {
    let server = MockServer::start().await;
    Mock::given(path("/api/search/suggestions"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    let q = SuggestionQuery {
        text: "q".into(),
        ..SuggestionQuery::default()
    };
    assert!(fetch_suggestions(&client(&server), &q).await.is_err());
}

#[tokio::test]
async fn blank_text_sends_nothing() {
    let server = MockServer::start().await;
    Mock::given(path("/api/search/suggestions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(0)
        .mount(&server)
        .await;
    let q = SuggestionQuery {
        text: "   ".into(),
        ..SuggestionQuery::default()
    };
    assert!(fetch_suggestions(&client(&server), &q).await.unwrap().is_empty());
}

#[tokio::test]
async fn blank_query_loads_scoped_pages_and_counts_server_rows_before_filtering() {
    let server = MockServer::start().await;
    let scope = || {
        Mock::given(method("GET"))
            .and(path("/api/search/suggestions"))
            .and(query_param("q", ""))
            .and(query_param("types", "playlist"))
            .and(query_param("limit", "2"))
            .and(query_param("channelId", "ch_9"))
    };
    scope()
        .and(query_param("offset", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"text": "Second playlist", "type": "playlist", "id": "p2"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    scope()
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"type": "playlist"},
            {"text": "First playlist", "type": "playlist", "id": "p1"}
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let query = SuggestionQuery {
        text: "  ".into(),
        limit: Some(2),
        kinds: vec![SuggestionKind::Playlist],
        channel_id: Some("ch_9".into()),
    };
    let first = aun_search::fetch_suggestions_page(&client, &query, None)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].id.as_deref(), Some("p1"));
    assert_eq!(first.next_cursor, Some(2));
    assert!(first.has_more);

    let next = aun_search::fetch_suggestions_page(&client, &query, first.next_cursor)
        .await
        .unwrap();
    assert_eq!(next.items[0].id.as_deref(), Some("p2"));
    assert_eq!(next.next_cursor, None);
    assert!(!next.has_more);
}

#[tokio::test]
async fn paging_stops_at_server_offset_cap_and_rejects_invalid_cursors() {
    let server = MockServer::start().await;
    Mock::given(path("/api/search/suggestions"))
        .and(query_param("offset", "500"))
        .and(query_param("q", ""))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"text": "Last reachable result", "type": "video", "id": "last"}
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let client = client(&server);
    let blank = SuggestionQuery {
        limit: Some(1),
        ..Default::default()
    };
    let page = aun_search::fetch_suggestions_page(&client, &blank, Some(500))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next_cursor, None);
    assert!(!page.has_more);
    assert!(matches!(
        aun_search::fetch_suggestions_page(&client, &blank, Some(501)).await,
        Err(Error::InvalidInput { .. })
    ));
    let typed = SuggestionQuery {
        text: "test".into(),
        ..Default::default()
    };
    assert!(matches!(
        aun_search::fetch_suggestions_page(&client, &typed, Some(1)).await,
        Err(Error::InvalidInput { .. })
    ));
}
