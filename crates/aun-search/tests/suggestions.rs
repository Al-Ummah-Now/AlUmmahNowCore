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
