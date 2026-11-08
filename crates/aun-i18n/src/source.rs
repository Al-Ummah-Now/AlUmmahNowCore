//! Where bundles come from: the [`TranslationSource`] trait and its api-videos implementation.
//!
//! Endpoints used (api-videos, public):
//! - `GET /api/i18n/config`: available locales, RTL flags, fallback locale, cache max-age.
//! - `GET /api/i18n/metadata`: version and a checksum per locale, used for cache invalidation.
//! - `GET /api/i18n/translations/{locale}`: the full bundle (`If-None-Match` supported; 304 when
//!   unchanged).
//!
//! `POST /api/i18n/sync` seeds the server's R2 store (development/admin) and is never called by apps.

use async_trait::async_trait;
use aun_api::ApiClient;
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// A locale the server offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleInfo {
    pub code: String,
    /// Native name (`العربية`).
    pub name: String,
    pub english_name: String,
    pub is_rtl: bool,
    #[serde(default)]
    pub is_default: bool,
    /// `false` while a locale is partially translated.
    #[serde(default = "default_true")]
    pub is_complete: bool,
}

fn default_true() -> bool {
    true
}

/// `GET /api/i18n/config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    pub locales: Vec<LocaleInfo>,
    pub fallback_locale: String,
    /// Seconds the server suggests caching bundles for.
    pub cache_max_age_s: u64,
    pub version: String,
}

/// `GET /api/i18n/metadata`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteMetadata {
    pub version: String,
    pub locales: Vec<String>,
    pub default_locale: String,
    pub updated_at: String,
    /// Checksum per locale; a changed checksum means the bundle must be refetched.
    pub checksums: std::collections::BTreeMap<String, String>,
}

/// `GET /api/i18n/translations/{locale}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteBundle {
    pub locale: String,
    pub version: String,
    pub updated_at: String,
    pub translations: serde_json::Value,
}

/// A source of translation bundles. Implemented over api-videos by [`ApiTranslationSource`];
/// tests use in-memory fakes.
#[async_trait]
pub trait TranslationSource: Send + Sync {
    async fn config(&self) -> Result<RemoteConfig>;
    async fn metadata(&self) -> Result<RemoteMetadata>;
    /// `Ok(None)` means "not modified" for `if_none_match`.
    async fn translations(&self, locale: &str, if_none_match: Option<&str>) -> Result<Option<RemoteBundle>>;
}

/// [`TranslationSource`] backed by the generated api-videos client.
#[derive(Debug, Clone)]
pub struct ApiTranslationSource {
    client: ApiClient,
}

impl ApiTranslationSource {
    pub fn new(client: ApiClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TranslationSource for ApiTranslationSource {
    async fn config(&self) -> Result<RemoteConfig> {
        let r = self.client.get_get_i18n_config_endpoint().await?.result;
        Ok(RemoteConfig {
            locales: r
                .locales
                .into_iter()
                .map(|l| LocaleInfo {
                    code: l.code,
                    name: l.name,
                    english_name: l.english_name,
                    is_rtl: l.is_rtl,
                    is_default: l.is_default.unwrap_or(false),
                    is_complete: l.is_complete.unwrap_or(true),
                })
                .collect(),
            fallback_locale: r.fallback_locale,
            cache_max_age_s: if r.cache_max_age.is_finite() && r.cache_max_age > 0.0 {
                r.cache_max_age as u64
            } else {
                0
            },
            version: r.version,
        })
    }

    async fn metadata(&self) -> Result<RemoteMetadata> {
        let r = self.client.get_get_i18n_metadata_endpoint().await?.result;
        Ok(RemoteMetadata {
            version: r.version,
            locales: r.locales,
            default_locale: r.default_locale,
            updated_at: r.updated_at,
            checksums: r.checksums,
        })
    }

    async fn translations(&self, locale: &str, if_none_match: Option<&str>) -> Result<Option<RemoteBundle>> {
        let params = aun_api::GetGetTranslationsEndpointParams {
            locale: locale.to_owned(),
            header_if_none_match: if_none_match.map(|e| format!("\"{}\"", e.trim_matches('"'))),
        };
        match self.client.get_get_translations_endpoint(&params).await {
            Ok(resp) => Ok(Some(RemoteBundle {
                locale: resp.result.locale,
                version: resp.result.version,
                updated_at: resp.result.updated_at,
                translations: resp.result.translations,
            })),
            // The client treats non-2xx as errors; 304 is our cache hit.
            Err(Error::Http { status: 304, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
