//! Telemetry data types: consent, events, breadcrumbs, crash reports and batches.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The user's choice from the consent banner (story 1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentState {
    /// Not asked yet: nothing is recorded, stored or sent.
    #[default]
    Unknown,
    /// Declined: nothing is recorded, stored or sent.
    Denied,
    /// Crash reports and their breadcrumbs only.
    CrashOnly,
    /// Product analytics, QoE and errors, plus crash reports.
    Analytics,
}

impl ConsentState {
    pub fn allows_analytics(self) -> bool {
        self == ConsentState::Analytics
    }
    pub fn allows_crash(self) -> bool {
        matches!(self, ConsentState::Analytics | ConsentState::CrashOnly)
    }
    pub fn allows(self, kind: EventKind) -> bool {
        match kind {
            EventKind::Crash => self.allows_crash(),
            EventKind::Analytics | EventKind::Qoe | EventKind::Error => self.allows_analytics(),
        }
    }
}

/// What an event is for; decides consent and routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Product analytics (screen views, feature use).
    Analytics,
    /// Playback quality of experience (Epic 13.10).
    Qoe,
    /// Handled errors with a stable code.
    Error,
    /// A crash report (with breadcrumbs).
    Crash,
}

/// A property value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<&str> for PropValue {
    fn from(s: &str) -> Self {
        PropValue::Text(s.to_owned())
    }
}
impl From<String> for PropValue {
    fn from(s: String) -> Self {
        PropValue::Text(s)
    }
}
impl From<i64> for PropValue {
    fn from(v: i64) -> Self {
        PropValue::Int(v)
    }
}
impl From<u64> for PropValue {
    fn from(v: u64) -> Self {
        PropValue::Int(i64::try_from(v).unwrap_or(i64::MAX))
    }
}
impl From<f64> for PropValue {
    fn from(v: f64) -> Self {
        PropValue::Float(v)
    }
}
impl From<bool> for PropValue {
    fn from(v: bool) -> Self {
        PropValue::Bool(v)
    }
}

/// One recorded event, already scrubbed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// Random id (dedupe on the server, removal after a send).
    pub id: String,
    pub kind: EventKind,
    /// `snake_case` event name, e.g. `screen_view`, `qoe_summary`, `playback_error`, `crash`.
    pub name: String,
    pub timestamp_ms: i64,
    pub session_id: String,
    #[serde(default)]
    pub properties: HashMap<String, PropValue>,
    /// Crash reports only: breadcrumbs leading up to the crash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breadcrumbs: Vec<Breadcrumb>,
}

/// Breadcrumb severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreadcrumbLevel {
    Debug,
    #[default]
    Info,
    Warning,
    Error,
}

/// A short trail entry (navigation, network, player state) kept in a ring buffer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub timestamp_ms: i64,
    pub category: String,
    pub message: String,
    #[serde(default)]
    pub level: BreadcrumbLevel,
}

/// A crash reported by the native layer, usually on the next launch (MetricKit,
/// `ApplicationExitInfo`, WER, a minidump handler).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CrashReport {
    /// Exception/signal type, e.g. `SIGSEGV`, `NSInvalidArgumentException`.
    pub kind: String,
    pub message: String,
    /// Symbolicated or raw stack, if available (scrubbed and truncated).
    pub stack: Option<String>,
    /// When the crash happened (Unix ms); 0 = unknown.
    pub occurred_at_ms: i64,
    /// True when reported on the launch after the crash: the previous session's persisted
    /// breadcrumbs are attached instead of the current ones.
    pub from_previous_session: bool,
    #[serde(default)]
    pub properties: HashMap<String, PropValue>,
}

/// Static app/device context sent with every batch (never per-user identifiers).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AppInfo {
    /// e.g. `ios`, `android`, `windows`, `linux`, `tvos`, `tizen`.
    pub platform: String,
    pub app_version: String,
    pub os_version: String,
    /// `phone`, `tablet`, `tv`, `desktop`, `car`.
    pub device_class: String,
    pub locale: String,
    /// Filled in by the library.
    #[serde(default)]
    pub core_version: String,
}

/// The JSON posted to an ingest endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryBatch {
    pub batch_id: String,
    pub sent_at_ms: i64,
    pub app: AppInfo,
    pub events: Vec<TelemetryEvent>,
}
