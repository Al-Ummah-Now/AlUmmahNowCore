//! Where batches go: the [`TelemetrySink`] trait, a no-op sink, an HTTP JSON sink for a
//! configurable ingest URL (the Epic 9.8 endpoint does not exist yet), and a sink for the existing
//! api-videos `POST /api/analytics/events` (performance events only).

use std::time::Duration;

use async_trait::async_trait;
use aun_api::ApiClient;
use aun_common::{Error, Result};

use crate::event::{EventKind, TelemetryBatch};

/// Delivers batches. Return a retryable error (network, 5xx, 429) to keep the batch buffered; a
/// non-retryable error (400, 413) drops it, so a malformed batch cannot block the queue.
#[async_trait]
pub trait TelemetrySink: Send + Sync {
    async fn send(&self, batch: &TelemetryBatch) -> Result<()>;
    /// Event kinds this sink can deliver. Others are dropped at flush time and counted as
    /// `unsupported` instead of piling up in the buffer.
    fn accepts(&self, _kind: EventKind) -> bool {
        true
    }
}

/// Discards everything (tests, builds without an ingest endpoint).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSink;

#[async_trait]
impl TelemetrySink for NoopSink {
    async fn send(&self, _batch: &TelemetryBatch) -> Result<()> {
        Ok(())
    }
}

/// Posts each batch as JSON (`TelemetryBatch`) to `url`.
#[derive(Debug, Clone)]
pub struct HttpSink {
    url: String,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
}

impl HttpSink {
    /// Uses the shared rustls client from `aun-api` (same TLS setup on every target).
    pub fn new(url: &str, user_agent: Option<&str>) -> Result<Self> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(Error::InvalidInput(format!(
                "telemetry URL must be http(s): {url}"
            )));
        }
        let mut cfg = aun_api::ApiConfig::new(url);
        cfg.timeout = Duration::from_secs(20);
        if let Some(ua) = user_agent {
            cfg.user_agent = ua.to_owned();
        }
        Ok(Self {
            url: url.to_owned(),
            client: aun_api::http_client(&cfg)?,
            headers: Vec::new(),
        })
    }

    /// Adds a static header (e.g. a public ingest key). Never put user tokens here.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

#[async_trait]
impl TelemetrySink for HttpSink {
    async fn send(&self, batch: &TelemetryBatch) -> Result<()> {
        let mut rb = self.client.post(&self.url).json(batch);
        for (k, v) in &self.headers {
            rb = rb.header(k, v);
        }
        let resp = rb.send().await.map_err(|e| {
            if e.is_timeout() {
                Error::Network(format!("timeout: {e}"))
            } else {
                Error::Network(e.to_string())
            }
        })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let body = resp.text().await.unwrap_or_default();
        let message = if body.len() > 300 {
            status.to_string()
        } else {
            body
        };
        Err(match Error::from_status(status.as_u16(), None, message) {
            Error::RateLimited { .. } => Error::RateLimited { retry_after },
            e => e,
        })
    }
}

/// Sends QoE and playback-error events to api-videos `POST /api/analytics/events` with
/// `kind: "performance"` (max 100 events / 64 KB per batch, enforced by the server). That endpoint
/// has no crash or product-analytics kind, so this sink accepts only [`EventKind::Qoe`] and
/// [`EventKind::Error`].
#[derive(Debug, Clone)]
pub struct ApiPerformanceSink {
    client: ApiClient,
}

impl ApiPerformanceSink {
    pub fn new(client: ApiClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TelemetrySink for ApiPerformanceSink {
    async fn send(&self, batch: &TelemetryBatch) -> Result<()> {
        let events = batch
            .events
            .iter()
            .filter(|e| self.accepts(e.kind))
            .map(|e| {
                let mut m = std::collections::BTreeMap::new();
                m.insert("id".to_owned(), serde_json::Value::String(e.id.clone()));
                m.insert("name".to_owned(), serde_json::Value::String(e.name.clone()));
                m.insert("timestamp".to_owned(), serde_json::Value::from(e.timestamp_ms));
                m.insert(
                    "sessionId".to_owned(),
                    serde_json::Value::String(e.session_id.clone()),
                );
                m.insert(
                    "platform".to_owned(),
                    serde_json::Value::String(batch.app.platform.clone()),
                );
                m.insert(
                    "appVersion".to_owned(),
                    serde_json::Value::String(batch.app.app_version.clone()),
                );
                m.insert(
                    "properties".to_owned(),
                    serde_json::to_value(&e.properties).unwrap_or_default(),
                );
                m
            })
            .collect::<Vec<_>>();
        if events.is_empty() {
            return Ok(());
        }
        let body = aun_api::PostClientAnalyticsEventsCreateBody {
            kind: "performance".into(),
            events,
            user_agent: None,
            referrer: None,
        };
        self.client
            .post_client_analytics_events_create(&body)
            .await
            .map(|_| ())
    }

    fn accepts(&self, kind: EventKind) -> bool {
        matches!(kind, EventKind::Qoe | EventKind::Error)
    }
}
