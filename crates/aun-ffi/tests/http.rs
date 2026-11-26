#![allow(clippy::unwrap_used)]

use aun_core::{ApiCall, ApiClient, ApiClientConfig, SuggestionKind, SuggestionQuery};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> std::sync::Arc<ApiClient> {
    ApiClient::new(
        ApiClientConfig {
            base_url: server.uri(),
            user_agent: None,
            locale: None,
            timeout_ms: None,
            max_attempts: Some(1),
            client: None,
        },
        None,
    )
    .unwrap()
}

#[tokio::test]
async fn raw_request_preserves_conditional_status_and_headers() {
    let server = MockServer::start().await;
    let etag = r#""cached""#;
    Mock::given(method("GET"))
        .and(path("/api/i18n/translations/en"))
        .and(header("if-none-match", etag))
        .respond_with(ResponseTemplate::new(304).insert_header("ETag", etag))
        .expect(1)
        .mount(&server)
        .await;

    let result = client(&server)
        .request(ApiCall {
            operation_id: "get_GetTranslationsEndpoint".into(),
            path_params: [("locale".into(), "en".into())].into(),
            headers: [("If-None-Match".into(), etag.into())].into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(result.status, 304);
    assert!(result.body.is_empty());
    assert!(result.headers.iter().any(|h| h.name == "etag" && h.value == etag));
}

#[tokio::test]
async fn picker_page_exposes_the_cursor_and_end_of_results() {
    let server = MockServer::start().await;
    let scope = || {
        Mock::given(method("GET"))
            .and(path("/api/search/suggestions"))
            .and(query_param("q", ""))
            .and(query_param("types", "video"))
            .and(query_param("channelId", "ch1"))
            .and(query_param("limit", "1"))
    };
    scope()
        .and(query_param("offset", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    scope()
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"text": "First video", "type": "video", "id": "v1"}
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let client = client(&server);
    let query = SuggestionQuery {
        limit: Some(1),
        kinds: vec![SuggestionKind::Video],
        channel_id: Some("ch1".into()),
        ..Default::default()
    };
    let first = client.search_suggestions_page(query.clone(), None).await.unwrap();
    assert_eq!(first.items[0].id.as_deref(), Some("v1"));
    assert_eq!(first.next_cursor, Some(1));
    assert!(first.has_more);
    let last = client
        .search_suggestions_page(query, first.next_cursor)
        .await
        .unwrap();
    assert!(last.items.is_empty());
    assert_eq!(last.next_cursor, None);
    assert!(!last.has_more);
}
