#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use aun_core::{
    ApiCall, ApiClient, ApiClientConfig, AunError, PlaylistMetadata, PlaylistQuery, SuggestionKind,
    SuggestionQuery, TokenSource, Visibility,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, header_exists, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> Arc<ApiClient> {
    client_with_tokens(server, None)
}

fn client_with_tokens(server: &MockServer, tokens: Option<Arc<dyn TokenSource>>) -> Arc<ApiClient> {
    ApiClient::new(
        ApiClientConfig {
            base_url: server.uri(),
            user_agent: None,
            locale: None,
            timeout_ms: None,
            max_attempts: Some(1),
            client: None,
        },
        tokens,
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

struct Tokens;

impl TokenSource for Tokens {
    fn access_token(&self) -> Option<String> {
        Some("test-token".into())
    }

    fn refresh(&self) -> Result<Option<String>, AunError> {
        Ok(self.access_token())
    }
}

#[tokio::test]
async fn personal_playlists_use_mine_and_omit_the_active_channel_header() {
    let server = MockServer::start().await;
    let unlisted = json!({
        "id": "p1", "title": "دروس", "userId": "u1", "channelId": "",
        "titleLocalized": {"ar": "دروس"}, "descriptionLocalized": {"en": "Notes"},
        "isPublic": false, "isUnlisted": true, "visibility": "unlisted", "videos": []
    });
    let private = json!({
        "id": "p1", "title": "دروس", "userId": "u1", "channelId": "",
        "isPublic": false
    });
    let mut updated_playlist = private.clone();
    updated_playlist["descriptionLocalized"] = json!({"en": "Notes"});
    let public = json!({
        "id": "p2", "title": "Public", "userId": "u1", "channelId": "ch1"
    });
    let auth = || header("authorization", "Bearer test-token");
    Mock::given(method("POST"))
        .and(path("/api/playlists"))
        .and(auth())
        .and(body_json(json!({
            "titleLocalized": {"ar": "دروس"}, "descriptionLocalized": {"en": "Notes"}, "visibility": "unlisted"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"playlist": unlisted}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/playlists"))
        .and(auth())
        .and(query_param("mine", "true"))
        .and(query_param("cursor", "1"))
        .and(query_param("limit", "2"))
        .and(query_param("q", "دروس"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"cursor": "3", "data": [unlisted, private]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/playlists/p1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(unlisted.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/playlists/p1"))
        .and(auth())
        .and(body_json(json!({"visibility": "private", "titleLocalized": {}})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"result": {"playlist": updated_playlist}})),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut with_video = private.clone();
    with_video["videos"] = json!(["v1"]);
    Mock::given(method("POST"))
        .and(path("/api/playlists/p1/videos"))
        .and(auth())
        .and(body_json(json!({"videoId": "v1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"playlist": with_video}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/playlists/p1/videos/v1"))
        .and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"playlist": private}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/playlists/p1"))
        .and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"success": true})))
        .expect(1)
        .mount(&server)
        .await;

    let anonymous = client(&server);
    assert!(matches!(
        anonymous
            .playlists_list(PlaylistQuery {
                mine: true,
                ..Default::default()
            })
            .await,
        Err(AunError::Unauthorized { .. })
    ));
    assert!(
        anonymous
            .playlist_fetch("p1".into(), None)
            .await
            .unwrap()
            .is_unlisted
    );
    let client = client_with_tokens(&server, Some(Arc::new(Tokens)));
    let created = client
        .playlist_create(
            None,
            PlaylistMetadata {
                title_localized: Some([("ar".into(), "دروس".into())].into()),
                description_localized: Some([("en".into(), "Notes".into())].into()),
                visibility: Some(Visibility::Unlisted),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(created.channel_id, "");
    assert_eq!(created.visibility, Visibility::Unlisted);
    assert_eq!(created.title_localized.as_ref().unwrap()["ar"], "دروس");
    assert_eq!(created.description_localized.as_ref().unwrap()["en"], "Notes");
    assert_eq!(
        aun_core::localized_playlist_text(created.title.into(), created.title_localized, "ar-EG".into()),
        "دروس"
    );

    let page = client
        .playlists_list(PlaylistQuery {
            limit: Some(2),
            cursor: Some("1".into()),
            mine: true,
            q: Some("دروس".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.cursor, "3");
    assert_eq!(
        page.data.iter().map(|p| p.visibility).collect::<Vec<_>>(),
        [Visibility::Unlisted, Visibility::Private]
    );

    let updated = client
        .playlist_update(
            "p1".into(),
            None,
            PlaylistMetadata {
                title_localized: Some(Default::default()),
                visibility: Some(Visibility::Private),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.visibility, Visibility::Private);
    assert!(!updated.is_unlisted);
    assert!(updated.title_localized.is_none());
    assert_eq!(updated.description_localized.unwrap()["en"], "Notes");
    assert_eq!(
        client
            .playlist_add_video("p1".into(), "v1".into(), None)
            .await
            .unwrap()
            .videos,
        ["v1"]
    );
    assert!(
        client
            .playlist_remove_video("p1".into(), "v1".into(), None)
            .await
            .unwrap()
            .videos
            .is_empty()
    );
    assert!(client.playlist_delete("p1".into(), None).await.unwrap());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| !r.headers.contains_key("channelId"))
    );

    Mock::given(method("POST"))
        .and(path("/api/playlists"))
        .and(header_exists("channelId"))
        .and(header("channelId", "ch1"))
        .and(body_json(json!({"title": "Public"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"playlist": public}})))
        .expect(1)
        .mount(&server)
        .await;
    let channel_playlist = client
        .playlist_create(
            Some("ch1".into()),
            PlaylistMetadata {
                title: Some("Public".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(channel_playlist.channel_id, "ch1");
    assert_eq!(channel_playlist.visibility, Visibility::Public);
}

#[tokio::test]
async fn video_visibility_reads_link_only_video_and_writes_exact_values() {
    let server = MockServer::start().await;
    let unlisted = json!({"id": "v1", "isPublic": false, "isUnlisted": true});
    Mock::given(method("GET"))
        .and(path("/api/videos/v1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(unlisted.clone()))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        client(&server).video_visibility("v1".into(), None).await.unwrap(),
        Visibility::Unlisted
    );

    Mock::given(method("PUT"))
        .and(path("/api/videos/v1"))
        .and(header("authorization", "Bearer test-token"))
        .and(header("channelId", "ch1"))
        .and(body_json(json!({"visibility": "unlisted"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {"video": unlisted}})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/videos/bulk-update"))
        .and(header("authorization", "Bearer test-token"))
        .and(body_json(json!({"ids": ["v1", "v2"], "changes": {"visibility": "private"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "success": true,
            "results": [{"videoId": "v1", "success": true}, {"videoId": "v2", "success": false, "code": "not_found"}]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let client = client_with_tokens(&server, Some(Arc::new(Tokens)));
    assert_eq!(
        client
            .video_update_visibility("v1".into(), Some("ch1".into()), Visibility::Unlisted)
            .await
            .unwrap(),
        Visibility::Unlisted
    );
    let bulk = client
        .videos_bulk_update_visibility(vec!["v1".into(), "v2".into()], None, Visibility::Private, None)
        .await
        .unwrap();
    assert!(bulk.results[0].success);
    assert_eq!(bulk.results[1].code.as_deref(), Some("not_found"));
}
