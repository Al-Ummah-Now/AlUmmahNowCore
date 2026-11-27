//! FFI facade for `aun-telemetry`: consent-gated analytics, playback QoE, handled errors,
//! breadcrumbs and crash reports (Epic 11.7 / 9.8 / 13.10).
//!
//! Nothing is recorded, persisted or sent until the user grants consent via
//! [`TelemetryClient::set_consent`]. Revoking consent immediately purges memory and disk.

use std::collections::HashMap;
use std::sync::Arc;

use aun_telemetry::{
    AppInfo, Breadcrumb, BreadcrumbLevel, ConsentState, CrashReport, FlushReport, HttpSink, NoopSink,
    PlaybackErrorCode, PropValue, QoeSummary, RecordOutcome, Telemetry, TelemetryConfig, TelemetrySink,
};

use crate::AunError;

/// The user's telemetry consent choice. Defaults to [`TelemetryConsentState::Unknown`] (off).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TelemetryConsentState {
    /// Not asked yet: nothing is recorded, stored or sent.
    Unknown,
    /// Declined: nothing is recorded, stored or sent.
    Denied,
    /// Crash reports and their breadcrumbs only.
    CrashOnly,
    /// Product analytics, QoE and handled errors, plus crash reports.
    Analytics,
}

impl From<TelemetryConsentState> for ConsentState {
    fn from(v: TelemetryConsentState) -> Self {
        match v {
            TelemetryConsentState::Unknown => ConsentState::Unknown,
            TelemetryConsentState::Denied => ConsentState::Denied,
            TelemetryConsentState::CrashOnly => ConsentState::CrashOnly,
            TelemetryConsentState::Analytics => ConsentState::Analytics,
        }
    }
}

impl From<ConsentState> for TelemetryConsentState {
    fn from(v: ConsentState) -> Self {
        match v {
            ConsentState::Unknown => TelemetryConsentState::Unknown,
            ConsentState::Denied => TelemetryConsentState::Denied,
            ConsentState::CrashOnly => TelemetryConsentState::CrashOnly,
            ConsentState::Analytics => TelemetryConsentState::Analytics,
        }
    }
}

/// Breadcrumb severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TelemetryBreadcrumbLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl From<TelemetryBreadcrumbLevel> for BreadcrumbLevel {
    fn from(v: TelemetryBreadcrumbLevel) -> Self {
        match v {
            TelemetryBreadcrumbLevel::Debug => BreadcrumbLevel::Debug,
            TelemetryBreadcrumbLevel::Info => BreadcrumbLevel::Info,
            TelemetryBreadcrumbLevel::Warning => BreadcrumbLevel::Warning,
            TelemetryBreadcrumbLevel::Error => BreadcrumbLevel::Error,
        }
    }
}

impl From<BreadcrumbLevel> for TelemetryBreadcrumbLevel {
    fn from(v: BreadcrumbLevel) -> Self {
        match v {
            BreadcrumbLevel::Debug => TelemetryBreadcrumbLevel::Debug,
            BreadcrumbLevel::Info => TelemetryBreadcrumbLevel::Info,
            BreadcrumbLevel::Warning => TelemetryBreadcrumbLevel::Warning,
            BreadcrumbLevel::Error => TelemetryBreadcrumbLevel::Error,
        }
    }
}

/// Outcome of recording a telemetry event or crash report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TelemetryRecordOutcome {
    Recorded,
    NoConsent,
    SampledOut,
    TooLarge,
    InvalidName,
}

impl From<RecordOutcome> for TelemetryRecordOutcome {
    fn from(v: RecordOutcome) -> Self {
        match v {
            RecordOutcome::Recorded => TelemetryRecordOutcome::Recorded,
            RecordOutcome::NoConsent => TelemetryRecordOutcome::NoConsent,
            RecordOutcome::SampledOut => TelemetryRecordOutcome::SampledOut,
            RecordOutcome::TooLarge => TelemetryRecordOutcome::TooLarge,
            RecordOutcome::InvalidName => TelemetryRecordOutcome::InvalidName,
        }
    }
}

/// Stable playback error codes (`playback_error.code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TelemetryPlaybackErrorCode {
    Network,
    Timeout,
    SourceNotFound,
    Forbidden,
    Decode,
    Drm,
    UnsupportedFormat,
    Renderer,
    Unknown,
}

impl From<TelemetryPlaybackErrorCode> for PlaybackErrorCode {
    fn from(v: TelemetryPlaybackErrorCode) -> Self {
        match v {
            TelemetryPlaybackErrorCode::Network => PlaybackErrorCode::Network,
            TelemetryPlaybackErrorCode::Timeout => PlaybackErrorCode::Timeout,
            TelemetryPlaybackErrorCode::SourceNotFound => PlaybackErrorCode::SourceNotFound,
            TelemetryPlaybackErrorCode::Forbidden => PlaybackErrorCode::Forbidden,
            TelemetryPlaybackErrorCode::Decode => PlaybackErrorCode::Decode,
            TelemetryPlaybackErrorCode::Drm => PlaybackErrorCode::Drm,
            TelemetryPlaybackErrorCode::UnsupportedFormat => PlaybackErrorCode::UnsupportedFormat,
            TelemetryPlaybackErrorCode::Renderer => PlaybackErrorCode::Renderer,
            TelemetryPlaybackErrorCode::Unknown => PlaybackErrorCode::Unknown,
        }
    }
}

/// Static app and device context attached to every batch (never per-user identifiers).
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct TelemetryAppInfo {
    /// e.g. `ios`, `macos`, `tvos`, `android`, `windows`, `linux`.
    pub platform: String,
    pub app_version: String,
    pub os_version: String,
    /// `phone`, `tablet`, `tv`, `desktop`, `car`.
    pub device_class: String,
    pub locale: String,
}

/// Configuration for [`TelemetryClient`].
#[derive(Debug, Clone, uniffi::Record)]
pub struct TelemetryClientConfig {
    pub app: TelemetryAppInfo,
    /// HTTPS ingest endpoint (e.g. `https://videos.api.alummahnow.net/api/telemetry/batches`).
    /// When `None`, events are discarded by a no-op sink.
    #[uniffi(default = None)]
    pub ingest_url: Option<String>,
    /// Optional User-Agent header for HTTP delivery.
    #[uniffi(default = None)]
    pub user_agent: Option<String>,
    /// Optional `X-AUN-Client` header value (e.g. `aun-ios/1.2.0 (iOS 18.2)`).
    #[uniffi(default = None)]
    pub client_header: Option<String>,
    /// Path to the persistent JSON buffer file (e.g. inside the app's cache/support directory).
    #[uniffi(default = None)]
    pub buffer_path: Option<String>,
    /// Fraction of sessions whose analytics/QoE/error events are kept (0.0–1.0, finite). Default `1.0`.
    /// Crash reports are never sampled out.
    #[uniffi(default = None)]
    pub sample_rate: Option<f64>,
    /// Maximum JSON batch size in bytes (32 768–240 000). Defaults to 200 000 when `ingest_url` is set.
    #[uniffi(default = None)]
    pub max_batch_bytes: Option<u32>,
}

/// A crash report captured by the native platform layer.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TelemetryCrashReport {
    /// Exception or signal type, e.g. `SIGSEGV`, `NSInvalidArgumentException`.
    pub kind: String,
    pub message: String,
    /// Symbolicated or raw stack trace, if available (scrubbed and truncated in core).
    #[uniffi(default = None)]
    pub stack: Option<String>,
    /// Unix timestamp in milliseconds when the crash occurred; `0` if unknown.
    #[uniffi(default = 0)]
    pub occurred_at_ms: i64,
    /// True when reported on the next launch: attaches the previous session's persisted breadcrumbs.
    #[uniffi(default = false)]
    pub from_previous_session: bool,
    /// Optional JSON object of flat primitive properties (`string`, `number`, `bool`).
    #[uniffi(default = None)]
    pub properties_json: Option<String>,
}

/// End-of-session playback QoE metrics.
#[derive(Debug, Clone, uniffi::Record)]
pub struct TelemetryQoeSummary {
    pub video_id: String,
    /// `vod`, `live`, or `offline`.
    pub playback_type: String,
    #[uniffi(default = None)]
    pub startup_time_ms: Option<u64>,
    pub played_ms: u64,
    pub rebuffer_count: u32,
    pub rebuffer_ms: u64,
    pub rebuffer_ratio: f64,
    pub seek_count: u32,
    pub seek_buffer_ms: u64,
    pub bitrate_switches: u32,
    #[uniffi(default = None)]
    pub avg_bitrate_bps: Option<u64>,
    #[uniffi(default = None)]
    pub max_bitrate_bps: Option<u64>,
    pub error_count: u32,
    #[uniffi(default = None)]
    pub fatal_error: Option<TelemetryPlaybackErrorCode>,
    pub exited_before_start: bool,
}

/// One breadcrumb entry in the ring buffer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TelemetryBreadcrumb {
    pub timestamp_ms: i64,
    pub category: String,
    pub message: String,
    pub level: TelemetryBreadcrumbLevel,
}

impl From<Breadcrumb> for TelemetryBreadcrumb {
    fn from(b: Breadcrumb) -> Self {
        Self {
            timestamp_ms: b.timestamp_ms,
            category: b.category,
            message: b.message,
            level: b.level.into(),
        }
    }
}

/// Result of flushing buffered telemetry batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TelemetryFlushReport {
    pub sent: u32,
    pub dropped: u32,
    pub unsupported: u32,
    pub remaining: u32,
}

impl From<FlushReport> for TelemetryFlushReport {
    fn from(r: FlushReport) -> Self {
        Self {
            sent: r.sent,
            dropped: r.dropped,
            unsupported: r.unsupported,
            remaining: r.remaining,
        }
    }
}

fn parse_properties(json: Option<String>) -> Result<HashMap<String, PropValue>, AunError> {
    let Some(raw) = json else {
        return Ok(HashMap::new());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(HashMap::new());
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| AunError::InvalidInput {
        code: None,
        detail: format!("invalid properties_json: {e}"),
    })?;
    let serde_json::Value::Object(map) = parsed else {
        return Err(AunError::InvalidInput {
            code: None,
            detail: "properties_json must be a JSON object".into(),
        });
    };
    let mut out = HashMap::with_capacity(map.len());
    for (k, v) in map {
        match v {
            serde_json::Value::Bool(b) => {
                out.insert(k, PropValue::Bool(b));
            }
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    out.insert(k, PropValue::Int(i));
                } else if let Some(u) = n.as_u64() {
                    out.insert(k, PropValue::from(u));
                } else if let Some(f) = n.as_f64() {
                    out.insert(k, PropValue::Float(f));
                }
            }
            serde_json::Value::String(s) => {
                out.insert(k, PropValue::Text(s));
            }
            serde_json::Value::Null => {}
            _ => {
                return Err(AunError::InvalidInput {
                    code: None,
                    detail: format!("property '{k}' must be a flat primitive (string, number, or bool)"),
                });
            }
        }
    }
    Ok(out)
}

fn sanitize_header(name: &str, raw: Option<&str>) -> Result<Option<String>, AunError> {
    let Some(value) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let scrubbed = aun_telemetry::scrub::truncate(&aun_telemetry::scrub::scrub_text(value), 160);
    reqwest::header::HeaderValue::from_str(&scrubbed).map_err(|_| AunError::InvalidInput {
        code: None,
        detail: format!("invalid {name} header value"),
    })?;
    Ok(Some(scrubbed))
}

/// Consent-gated telemetry and crash reporter. Create one per app and share it.
#[derive(Debug, uniffi::Object)]
pub struct TelemetryClient {
    inner: Arc<Telemetry>,
}

#[uniffi::export(async_runtime = "tokio")]
impl TelemetryClient {
    #[uniffi::constructor]
    pub fn new(config: TelemetryClientConfig) -> Result<Arc<Self>, AunError> {
        let ingest_url = config
            .ingest_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let has_ingest = ingest_url.is_some();
        let user_agent = sanitize_header("user_agent", config.user_agent.as_deref())?;
        let client_header = sanitize_header("client_header", config.client_header.as_deref())?;
        let sink: Arc<dyn TelemetrySink> = match ingest_url {
            Some(url) => {
                let mut http = HttpSink::new(url, user_agent.as_deref())?;
                if let Some(client) = client_header.as_deref() {
                    http = http.with_header("X-AUN-Client", client);
                }
                Arc::new(http)
            }
            None => Arc::new(NoopSink),
        };
        let mut cfg = TelemetryConfig {
            app: AppInfo {
                platform: config.app.platform,
                app_version: config.app.app_version,
                os_version: config.app.os_version,
                device_class: config.app.device_class,
                locale: config.app.locale,
                core_version: String::new(),
            },
            buffer_path: config.buffer_path,
            ..TelemetryConfig::default()
        };
        if let Some(rate) = config.sample_rate {
            if !rate.is_finite() || !(0.0..=1.0).contains(&rate) {
                return Err(AunError::InvalidInput {
                    code: None,
                    detail: "sample_rate must be a finite number between 0.0 and 1.0".into(),
                });
            }
            cfg.sample_rate = rate;
        }
        cfg.max_batch_bytes = match config.max_batch_bytes {
            Some(b) => b.clamp(32_768, 240_000),
            None if has_ingest => 200_000,
            None => cfg.max_batch_bytes,
        };
        cfg.max_event_bytes = cfg.max_event_bytes.min(cfg.max_batch_bytes.saturating_sub(1_024));
        Ok(Arc::new(Self {
            inner: Telemetry::new(cfg, sink),
        }))
    }

    pub fn consent(&self) -> TelemetryConsentState {
        self.inner.consent().into()
    }

    pub fn set_consent(&self, consent: TelemetryConsentState) -> Result<(), AunError> {
        self.inner.set_consent(consent.into())?;
        Ok(())
    }

    pub fn session_id(&self) -> String {
        self.inner.session_id()
    }

    pub fn new_session(&self) -> String {
        self.inner.new_session()
    }

    pub fn track(
        &self,
        name: String,
        properties_json: Option<String>,
    ) -> Result<TelemetryRecordOutcome, AunError> {
        let props = parse_properties(properties_json)?;
        Ok(self.inner.track(&name, props).into())
    }

    pub fn track_error(
        &self,
        code: String,
        properties_json: Option<String>,
    ) -> Result<TelemetryRecordOutcome, AunError> {
        let props = parse_properties(properties_json)?;
        Ok(self.inner.track_error(&code, props).into())
    }

    pub fn track_playback_error(
        &self,
        video_id: String,
        code: TelemetryPlaybackErrorCode,
        fatal: bool,
        detail: String,
    ) -> TelemetryRecordOutcome {
        self.inner
            .track_playback_error(&video_id, code.into(), fatal, &detail)
            .into()
    }

    pub fn track_qoe(&self, summary: TelemetryQoeSummary) -> TelemetryRecordOutcome {
        let s = QoeSummary {
            video_id: summary.video_id,
            playback_type: summary.playback_type,
            startup_time_ms: summary.startup_time_ms,
            played_ms: summary.played_ms,
            rebuffer_count: summary.rebuffer_count,
            rebuffer_ms: summary.rebuffer_ms,
            rebuffer_ratio: summary.rebuffer_ratio,
            seek_count: summary.seek_count,
            seek_buffer_ms: summary.seek_buffer_ms,
            bitrate_switches: summary.bitrate_switches,
            avg_bitrate_bps: summary.avg_bitrate_bps,
            max_bitrate_bps: summary.max_bitrate_bps,
            error_count: summary.error_count,
            fatal_error: summary.fatal_error.map(Into::into),
            exited_before_start: summary.exited_before_start,
        };
        self.inner.track_qoe(&s).into()
    }

    pub fn add_breadcrumb(&self, category: String, message: String, level: TelemetryBreadcrumbLevel) {
        self.inner.add_breadcrumb(&category, &message, level.into());
    }

    pub fn breadcrumbs(&self) -> Vec<TelemetryBreadcrumb> {
        self.inner.breadcrumbs().into_iter().map(Into::into).collect()
    }

    pub fn report_crash(&self, report: TelemetryCrashReport) -> Result<TelemetryRecordOutcome, AunError> {
        let properties = parse_properties(report.properties_json)?;
        let crash = CrashReport {
            kind: report.kind,
            message: report.message,
            stack: report.stack,
            occurred_at_ms: report.occurred_at_ms,
            from_previous_session: report.from_previous_session,
            properties,
        };
        Ok(self.inner.report_crash(crash).into())
    }

    pub fn persist(&self) -> Result<(), AunError> {
        self.inner.persist()?;
        Ok(())
    }

    pub fn pending_count(&self) -> u32 {
        self.inner.pending_count()
    }

    pub async fn flush(&self) -> Result<TelemetryFlushReport, AunError> {
        let r = self.inner.flush().await?;
        Ok(r.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(buffer_path: Option<String>, ingest_url: Option<String>) -> TelemetryClientConfig {
        TelemetryClientConfig {
            app: TelemetryAppInfo {
                platform: "ios".into(),
                app_version: "1.0.0".into(),
                os_version: "18.2".into(),
                device_class: "phone".into(),
                locale: "ar".into(),
            },
            ingest_url,
            user_agent: Some("AlUmmahNow-iOS/1.0.0".into()),
            client_header: Some("aun-ios/1.0.0 (iOS 18.2)".into()),
            buffer_path,
            sample_rate: Some(1.0),
            max_batch_bytes: None,
        }
    }

    #[test]
    fn enforces_opt_in_consent_and_purges_on_revoke() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("telemetry.json");
        let client = TelemetryClient::new(test_config(Some(path.to_string_lossy().into_owned()), None))?;

        assert_eq!(client.consent(), TelemetryConsentState::Unknown);
        assert_eq!(
            client.track("screen_view".into(), Some(r#"{"screen":"home"}"#.into()))?,
            TelemetryRecordOutcome::NoConsent
        );
        assert_eq!(client.pending_count(), 0);
        assert!(!path.exists());

        client.set_consent(TelemetryConsentState::CrashOnly)?;
        assert_eq!(
            client.track("screen_view".into(), None)?,
            TelemetryRecordOutcome::NoConsent
        );
        client.add_breadcrumb(
            "nav".into(),
            "opened video".into(),
            TelemetryBreadcrumbLevel::Info,
        );
        assert_eq!(
            client.report_crash(TelemetryCrashReport {
                kind: "SIGSEGV".into(),
                message: "segfault".into(),
                stack: Some("frame 0".into()),
                occurred_at_ms: 1_700_000_000_000,
                from_previous_session: false,
                properties_json: Some(r#"{"code":11,"fatal":true,"ignored":null}"#.into()),
            })?,
            TelemetryRecordOutcome::Recorded
        );
        assert_eq!(client.pending_count(), 1);
        assert!(path.exists());

        client.set_consent(TelemetryConsentState::Denied)?;
        assert_eq!(client.pending_count(), 0);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn rejects_invalid_inputs_headers_and_sample_rates() {
        let client = match TelemetryClient::new(test_config(None, None)) {
            Ok(c) => c,
            Err(e) => panic!("unexpected init error: {e:?}"),
        };
        assert!(matches!(
            client.track("screen_view".into(), Some("[1,2,3]".into())),
            Err(AunError::InvalidInput { .. })
        ));
        assert!(matches!(
            client.track("screen_view".into(), Some(r#"{"nested":{"a":1}}"#.into())),
            Err(AunError::InvalidInput { .. })
        ));
        assert!(
            TelemetryClient::new(test_config(
                None,
                Some("http://videos.api.alummahnow.net/api/telemetry/batches".into()),
            ))
            .is_err()
        );

        for bad_rate in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.5] {
            let mut cfg = test_config(None, None);
            cfg.sample_rate = Some(bad_rate);
            assert!(matches!(
                TelemetryClient::new(cfg),
                Err(AunError::InvalidInput { .. })
            ));
        }

        let mut bad_hdr = test_config(
            None,
            Some("https://videos.api.alummahnow.net/api/telemetry/batches".into()),
        );
        bad_hdr.client_header = Some("aun-ios/1.0\r\nX-Injected: 1".into());
        assert!(matches!(
            TelemetryClient::new(bad_hdr),
            Err(AunError::InvalidInput { .. })
        ));

        let scrubbed = match sanitize_header(
            "client_header",
            Some("aun-ios/1.0 user@example.com Bearer secret_token_123456"),
        ) {
            Ok(Some(v)) => v,
            other => panic!("expected scrubbed header, got {other:?}"),
        };
        assert!(!scrubbed.contains("user@example.com"));
        assert!(!scrubbed.contains("secret_token_123456"));
    }

    #[tokio::test]
    async fn flush_with_noop_sink_drains_buffered_events() -> Result<(), Box<dyn std::error::Error>> {
        let client = TelemetryClient::new(test_config(None, None))?;
        client.set_consent(TelemetryConsentState::Analytics)?;
        assert_eq!(
            client.track_error("upload.timeout".into(), Some(r#"{"attempt":2}"#.into()))?,
            TelemetryRecordOutcome::Recorded
        );
        assert_eq!(client.pending_count(), 1);
        let report = client.flush().await?;
        assert_eq!(report.sent, 1);
        assert_eq!(report.remaining, 0);
        Ok(())
    }
}
