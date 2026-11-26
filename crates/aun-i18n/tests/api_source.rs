#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `ApiTranslationSource` against a wiremock api-videos.

use aun_api::{ApiClient, ApiConfig, RetryPolicy};
use aun_common::Error;
use aun_i18n::{ApiTranslationSource, TranslationSource};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn source(server: &MockServer) -> ApiTranslationSource {
    let mut cfg = ApiConfig::new(server.uri());
    cfg.retry = RetryPolicy::none();
    ApiTranslationSource::new(ApiClient::new(cfg).unwrap())
}

#[tokio::test]
async fn maps_config_metadata_and_translations() {
    let server = MockServer::start().await;
    let etag = r#"W/"bundle-ar""#;
    Mock::given(method("GET"))
        .and(path("/api/i18n/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "result": {
                "locales": [
                    {"code": "en", "name": "English", "englishName": "English", "isRTL": false, "isDefault": true},
                    {"code": "ar", "name": "العربية", "englishName": "Arabic", "isRTL": true, "isComplete": false}
                ],
                "fallbackLocale": "en", "cacheMaxAge": 3600, "version": "3"
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "result": {"version": "3", "locales": ["en", "ar"], "defaultLocale": "en",
                       "updatedAt": "2026-11-01T00:00:00Z", "checksums": {"en": "e1", "ar": "a1"},
                       "sections": ["common"]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/translations/ar"))
        .and(header("if-none-match", etag))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/translations/ar"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", etag)
                .set_body_json(serde_json::json!({
                    "success": true,
                    "result": {"locale": "ar", "version": "3", "updatedAt": "2026-11-01T00:00:00Z",
                               "translations": {"common": {"save": "حفظ"}}}
                })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/translations/xx"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"success": false, "error": "Invalid locale"})),
        )
        .mount(&server)
        .await;

    let src = source(&server);
    let cfg = src.config().await.unwrap();
    assert_eq!(cfg.fallback_locale, "en");
    assert_eq!(cfg.cache_max_age_s, 3600);
    assert!(cfg.locales[1].is_rtl);
    assert!(!cfg.locales[1].is_complete);
    assert!(cfg.locales[0].is_complete, "defaults to complete");

    let meta = src.metadata().await.unwrap();
    assert_eq!(meta.checksums["ar"], "a1");

    let b = src.translations("ar", None).await.unwrap().unwrap();
    assert_eq!(b.translations["common"]["save"], "حفظ");
    assert_eq!(b.etag.as_deref(), Some(etag));
    assert!(
        src.translations("ar", b.etag.as_deref()).await.unwrap().is_none(),
        "304 → not modified"
    );
    assert_eq!(
        src.translations("xx", None).await.unwrap_err(),
        Error::invalid_input("Invalid locale".into())
    );
}

#[tokio::test]
async fn i18n_sync_over_http() {
    let server = MockServer::start().await;
    let etag = r#""bundle-en""#;
    Mock::given(method("GET"))
        .and(path("/api/i18n/config"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "result": {"locales": [{"code": "en", "name": "English", "englishName": "English", "isRTL": false}],
                       "fallbackLocale": "en", "cacheMaxAge": 60, "version": "1"}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "success": true,
            "result": {"version": "1", "locales": ["en"], "defaultLocale": "en", "updatedAt": "x",
                       "checksums": {"en": "c"}, "sections": []}
        })))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "result": {"version": "2", "locales": ["en"], "checksums": {"en": "c2"}}
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/translations/en"))
        .and(header("if-none-match", etag))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/i18n/translations/en"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", etag).set_body_json(serde_json::json!({
            "success": true,
            "result": {"locale": "en", "version": "1", "updatedAt": "x",
                       "translations": {"videos": {"count_one": "{{count}} video", "count_other": "{{count}} videos"}}}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let cache = tempfile::tempdir().unwrap();
    let options = aun_i18n::I18nOptions {
        cache_dir: Some(cache.path().to_str().unwrap().to_owned()),
        ..Default::default()
    };
    let i18n = aun_i18n::I18n::new(options.clone()).unwrap();
    i18n.set_source(std::sync::Arc::new(source(&server)));
    let r = i18n.sync().await.unwrap();
    assert_eq!(r.updated, vec!["en"]);
    assert_eq!(i18n.sync().await.unwrap().unchanged, vec!["en"]);
    drop(i18n);

    // The checksum changes after restart. A 304 for the persisted ETag keeps the cached bundle.
    let i18n = aun_i18n::I18n::new(options).unwrap();
    i18n.set_source(std::sync::Arc::new(source(&server)));
    assert_eq!(i18n.sync().await.unwrap().unchanged, vec!["en"]);
    let mut args = std::collections::HashMap::new();
    args.insert("count".to_owned(), aun_i18n::FormatArg::Int(3));
    assert_eq!(i18n.format("videos.count", &args), "3 videos");
}
