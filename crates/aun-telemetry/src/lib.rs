//! Consent-gated analytics, QoE events and crash breadcrumbs (Epic 11.7, 9.8, 13.10).
//!
//! - [`Telemetry`]: the recorder. It respects [`ConsentState`] (nothing is stored or sent without
//!   consent, and revoking consent purges the buffer), scrubs PII ([`scrub`]), samples by session,
//!   caps sizes, persists the buffer atomically and delivers batches through a [`TelemetrySink`].
//! - [`QoeTracker`]: per-playback startup time, rebuffers, bitrate switches and error codes.
//! - Sinks: [`HttpSink`] (JSON batches to a configurable URL, for the future Epic 9.8 ingest
//!   endpoint), [`ApiPerformanceSink`] (QoE/errors to api-videos `POST /api/analytics/events`) and
//!   [`NoopSink`].

mod event;
pub mod qoe;
pub mod scrub;
mod sink;
mod telemetry;

pub use event::{
    AppInfo, Breadcrumb, BreadcrumbLevel, ConsentState, CrashReport, EventKind, PropValue, TelemetryBatch,
    TelemetryEvent,
};
pub use qoe::{PlaybackErrorCode, QoeSummary, QoeTracker};
pub use sink::{ApiPerformanceSink, HttpSink, NoopSink, TelemetrySink};
pub use telemetry::{FlushReport, RecordOutcome, Telemetry, TelemetryConfig, session_in_sample};
