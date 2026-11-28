//! [`Telemetry`]: the consent-gated recorder with a persistent, size-capped buffer, session
//! sampling, breadcrumbs and batched delivery to a [`TelemetrySink`].
//!
//! Consent rules (story 1.3):
//! - `Unknown` / `Denied`: `track*`, breadcrumbs and crash reports are ignored. Nothing is kept in
//!   memory or written to disk, and switching to these states deletes the buffer file.
//! - `CrashOnly`: only crash reports and breadcrumbs. Switching to it purges analytics, QoE and
//!   error events.
//! - `Analytics`: everything.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::task::{Context, Poll};

use aun_common::platform::{Clock, SystemClock};
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::event::{
    AppInfo, Breadcrumb, BreadcrumbLevel, ConsentState, CrashReport, EventKind, PropValue, TelemetryBatch,
    TelemetryEvent,
};
use crate::qoe::{PlaybackErrorCode, QoeSummary};
use crate::scrub::{scrub_properties, scrub_text, truncate};
use crate::sink::TelemetrySink;

/// Limits and settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryConfig {
    pub app: AppInfo,
    /// Buffer file (JSON); `None` keeps the buffer in memory only.
    pub buffer_path: Option<String>,
    /// Fraction of sessions whose analytics/QoE/error events are kept (0.0–1.0). Crash reports are
    /// never sampled out.
    pub sample_rate: f64,
    /// Oldest non-crash events are dropped beyond this.
    pub max_buffered_events: u32,
    pub max_batch_events: u32,
    /// Approximate JSON size cap per batch (api-videos accepts 64 KB).
    pub max_batch_bytes: u32,
    /// Events larger than this after scrubbing are dropped.
    pub max_event_bytes: u32,
    /// Strings are truncated to this many characters.
    pub max_string_chars: u32,
    /// Crash stacks are truncated to this many characters.
    pub max_stack_chars: u32,
    pub max_breadcrumbs: u32,
    pub max_properties: u32,
    /// Persist the buffer after this many new events (and on `persist()` / `flush()`).
    pub persist_every: u32,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            app: AppInfo::default(),
            buffer_path: None,
            sample_rate: 1.0,
            max_buffered_events: 1_000,
            max_batch_events: 100,
            max_batch_bytes: 60_000,
            max_event_bytes: 8_192,
            max_string_chars: 512,
            max_stack_chars: 16_000,
            max_breadcrumbs: 50,
            max_properties: 50,
            persist_every: 20,
        }
    }
}

/// What a flush did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FlushReport {
    pub sent: u32,
    /// Dropped because the sink rejected the batch permanently.
    pub dropped: u32,
    /// Dropped because the sink does not accept their kind.
    pub unsupported: u32,
    pub remaining: u32,
}

/// Why an event was not recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordOutcome {
    Recorded,
    NoConsent,
    SampledOut,
    TooLarge,
    InvalidName,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persisted {
    events: Vec<TelemetryEvent>,
    breadcrumbs: Vec<Breadcrumb>,
}

#[derive(Debug)]
struct State {
    consent: ConsentState,
    session_id: String,
    sampled_in: bool,
    events: VecDeque<TelemetryEvent>,
    breadcrumbs: VecDeque<Breadcrumb>,
    previous_breadcrumbs: Vec<Breadcrumb>,
    unsaved: u32,
}

thread_local! {
    /// Address of the `Telemetry` currently inside `ConsentGated::poll` on this thread, so a
    /// custom `TelemetrySink` that re-entrantly calls `set_consent()` during its own poll does
    /// not deadlock waiting on `send_gate.write()`.
    static ACTIVE_SEND_POLL_OWNER: Cell<usize> = const { Cell::new(0) };
}

struct ActivePollGuard(usize);

impl ActivePollGuard {
    fn enter(owner: usize) -> Self {
        let prev = ACTIVE_SEND_POLL_OWNER.with(|c| c.replace(owner));
        Self(prev)
    }
}

impl Drop for ActivePollGuard {
    fn drop(&mut self) {
        ACTIVE_SEND_POLL_OWNER.with(|c| c.set(self.0));
    }
}

/// Polls `inner` while holding `send_gate.read()` only if `consent_epoch` still matches
/// `expected_epoch`. `set_consent` increments `consent_epoch` and acquires+drops `send_gate.write()`,
/// guaranteeing no poll of a revoked batch can start or still be running once `set_consent` returns.
struct ConsentGated<'a, F> {
    inner: Pin<Box<F>>,
    owner: usize,
    send_gate: &'a RwLock<()>,
    consent_epoch: &'a AtomicU64,
    expected_epoch: u64,
}

impl<F: Future<Output = Result<()>>> Future for ConsentGated<'_, F> {
    type Output = Option<Result<()>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _guard = self.send_gate.read().unwrap_or_else(|p| p.into_inner());
        if self.consent_epoch.load(Ordering::Acquire) != self.expected_epoch {
            return Poll::Ready(None);
        }
        let _active = ActivePollGuard::enter(self.owner);
        match self.inner.as_mut().poll(cx) {
            Poll::Ready(res) => Poll::Ready(Some(res)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// The telemetry recorder. Create one per app and share the `Arc`.
pub struct Telemetry {
    config: TelemetryConfig,
    sink: Arc<dyn TelemetrySink>,
    clock: Arc<dyn Clock>,
    /// Serialises buffer file reads, writes and deletes across `persist` and `set_consent`.
    /// Lock ordering (strict DAG): `send_gate` (read, in poll) → `io_lock` → `state`.
    /// `send_gate.write()` is only ever acquired with no other locks held.
    io_lock: Mutex<()>,
    state: Mutex<State>,
    /// Monotonically incremented under `state` whenever `set_consent` runs.
    consent_epoch: AtomicU64,
    /// Broadcasts `consent_epoch` changes so `flush()` wakes and cancels immediately on revocation.
    consent_tx: tokio::sync::watch::Sender<u64>,
    /// Leaf lock: held for read during each single non-blocking poll of `sink.send()`, and
    /// acquired+dropped for write in `set_consent()` as a barrier against revoked sends.
    send_gate: RwLock<()>,
    flush_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.lock();
        f.debug_struct("Telemetry")
            .field("consent", &st.consent)
            .field("buffered", &st.events.len())
            .finish()
    }
}

/// Random 128-bit hex id from the std hasher's per-process random keys and a counter.
pub(crate) fn random_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut parts = [0u64; 2];
    for (i, p) in parts.iter_mut().enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(n);
        h.write_u128(t);
        h.write_usize(i);
        *p = h.finish();
    }
    format!("{:016x}{:016x}", parts[0], parts[1])
}

/// Deterministic session sampling: FNV-1a of the session id mapped to [0, 1).
pub fn session_in_sample(session_id: &str, rate: f64) -> bool {
    if rate >= 1.0 {
        return true;
    }
    if rate <= 0.0 || rate.is_nan() {
        return false;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_id.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    ((h >> 11) as f64 / (1u64 << 53) as f64) < rate
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

impl Telemetry {
    /// Creates the recorder with consent `Unknown`. The buffer file is only read after consent is
    /// granted (see [`Telemetry::set_consent`]).
    pub fn new(config: TelemetryConfig, sink: Arc<dyn TelemetrySink>) -> Arc<Self> {
        Self::with_clock(config, sink, Arc::new(SystemClock))
    }

    pub fn with_clock(
        mut config: TelemetryConfig,
        sink: Arc<dyn TelemetrySink>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        config.app.core_version = aun_common::VERSION.to_owned();
        config.max_batch_events = config.max_batch_events.clamp(1, 1_000);
        let session_id = random_id();
        let sampled_in = session_in_sample(&session_id, config.sample_rate);
        let (consent_tx, _consent_rx) = tokio::sync::watch::channel(0u64);
        Arc::new(Self {
            config,
            sink,
            clock,
            io_lock: Mutex::new(()),
            state: Mutex::new(State {
                consent: ConsentState::Unknown,
                session_id,
                sampled_in,
                events: VecDeque::new(),
                breadcrumbs: VecDeque::new(),
                previous_breadcrumbs: Vec::new(),
                unsaved: 0,
            }),
            consent_epoch: AtomicU64::new(0),
            consent_tx,
            send_gate: RwLock::new(()),
            flush_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn io_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.io_lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn path(&self) -> Option<PathBuf> {
        self.config.buffer_path.as_ref().map(PathBuf::from)
    }

    pub fn consent(&self) -> ConsentState {
        self.lock().consent
    }

    pub fn session_id(&self) -> String {
        self.lock().session_id.clone()
    }

    /// Starts a new session (e.g. after 30 min in the background); re-draws the sample.
    pub fn new_session(&self) -> String {
        let mut st = self.lock();
        st.session_id = random_id();
        st.sampled_in = session_in_sample(&st.session_id, self.config.sample_rate);
        st.session_id.clone()
    }

    /// Applies the user's consent. Granting consent loads the persisted buffer (the previous
    /// session's breadcrumbs become [`Telemetry::previous_session_breadcrumbs`]). Narrowing it
    /// purges what is no longer allowed, from memory and from disk.
    pub fn set_consent(&self, consent: ConsentState) -> Result<()> {
        let io_result = {
            let _io = self.io_lock();
            let previous = {
                let mut st = self.lock();
                let epoch = self.consent_epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
                let _ = self.consent_tx.send(epoch);
                std::mem::replace(&mut st.consent, consent)
            };
            if !consent.allows_crash() {
                {
                    let mut st = self.lock();
                    st.events.clear();
                    st.breadcrumbs.clear();
                    st.previous_breadcrumbs.clear();
                    st.unsaved = 0;
                }
                if let Some(path) = self.path() {
                    match std::fs::remove_file(&path) {
                        Ok(()) => Ok(()),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(e) => Err(e.into()),
                    }
                } else {
                    Ok(())
                }
            } else {
                let load_res = if !previous.allows_crash() {
                    self.load_persisted_locked()
                } else {
                    Ok(())
                };
                match load_res {
                    Err(e) => Err(e),
                    Ok(()) => {
                        if !consent.allows_analytics() {
                            self.lock().events.retain(|e| e.kind == EventKind::Crash);
                        }
                        self.persist_locked()
                    }
                }
            }
        };
        // Barrier (acquired with ZERO other locks held to preserve strict lock DAG
        // `send_gate` -> `io_lock` -> `state`): wait for any in-progress single poll step of an
        // older-epoch send on another thread to finish before returning. Skip when called
        // re-entrantly from inside our own `ConsentGated::poll` on this thread.
        let self_addr = std::ptr::from_ref(self) as usize;
        let reentrant = ACTIVE_SEND_POLL_OWNER.with(|c| c.get() == self_addr);
        if !reentrant {
            let _barrier = self.send_gate.write().unwrap_or_else(|p| p.into_inner());
            drop(_barrier);
        }
        io_result
    }

    fn load_persisted_locked(&self) -> Result<()> {
        let Some(path) = self.path() else {
            return Ok(());
        };
        let persisted: Persisted = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "discarding unreadable telemetry buffer");
                Persisted::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Persisted::default(),
            Err(e) => return Err(e.into()),
        };
        let mut st = self.lock();
        let mut known: HashSet<String> = st.events.iter().map(|e| e.id.clone()).collect();
        let mut merged: VecDeque<TelemetryEvent> = persisted
            .events
            .into_iter()
            .filter(|e| known.insert(e.id.clone()))
            .collect();
        merged.extend(st.events.drain(..));
        st.events = merged;
        st.previous_breadcrumbs = persisted.breadcrumbs;
        self.enforce_cap(&mut st);
        Ok(())
    }

    /// Writes the buffer and current breadcrumbs (atomic replace). Call when the app goes to the
    /// background so breadcrumbs survive a crash.
    pub fn persist(&self) -> Result<()> {
        let _io = self.io_lock();
        self.persist_locked()
    }

    fn persist_locked(&self) -> Result<()> {
        let Some(path) = self.path() else {
            return Ok(());
        };
        let snapshot = {
            let mut st = self.lock();
            if !st.consent.allows_crash() {
                return Ok(());
            }
            st.unsaved = 0;
            Persisted {
                events: st.events.iter().cloned().collect(),
                breadcrumbs: st.breadcrumbs.iter().cloned().collect(),
            }
        };
        let bytes = serde_json::to_vec(&snapshot)?;
        let parent = path
            .parent()
            .ok_or_else(|| Error::Storage("telemetry buffer path has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        let base_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("telemetry");
        let tmp = write_private_temp_file(parent, base_name, &bytes)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }

    fn enforce_cap(&self, st: &mut State) {
        let cap = self.config.max_buffered_events.max(1) as usize;
        while st.events.len() > cap {
            // Drop the oldest non-crash event first; crash reports are the most valuable.
            match st.events.iter().position(|e| e.kind != EventKind::Crash) {
                Some(i) => {
                    st.events.remove(i);
                }
                None => {
                    st.events.pop_front();
                }
            }
        }
    }

    fn record(
        &self,
        kind: EventKind,
        name: &str,
        mut properties: HashMap<String, PropValue>,
        breadcrumbs: Vec<Breadcrumb>,
        stack: Option<String>,
    ) -> RecordOutcome {
        if !valid_name(name) {
            return RecordOutcome::InvalidName;
        }
        let (consent, sampled_in, session_id) = {
            let st = self.lock();
            (st.consent, st.sampled_in, st.session_id.clone())
        };
        if !consent.allows(kind) {
            return RecordOutcome::NoConsent;
        }
        if kind != EventKind::Crash && !sampled_in {
            return RecordOutcome::SampledOut;
        }
        if properties.len() > self.config.max_properties as usize {
            let mut keys: Vec<String> = properties.keys().cloned().collect();
            keys.sort();
            for k in keys.into_iter().skip(self.config.max_properties as usize) {
                properties.remove(&k);
            }
        }
        scrub_properties(&mut properties, self.config.max_string_chars as usize);
        if let Some(stack) = stack {
            // Stacks get their own (larger) limit; scrubbed like any other text.
            let stack = truncate(&scrub_text(&stack), self.config.max_stack_chars as usize);
            properties.insert("stack".into(), PropValue::Text(stack));
        }
        let event = TelemetryEvent {
            id: random_id(),
            kind,
            name: name.to_owned(),
            timestamp_ms: self.clock.now_ms(),
            session_id,
            properties,
            breadcrumbs,
        };
        let size = serde_json::to_vec(&event).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > self.config.max_event_bytes as usize && kind != EventKind::Crash {
            return RecordOutcome::TooLarge;
        }
        let should_persist = {
            let mut st = self.lock();
            // Consent may have changed while we built the event.
            if !st.consent.allows(kind) {
                return RecordOutcome::NoConsent;
            }
            st.events.push_back(event);
            self.enforce_cap(&mut st);
            st.unsaved += 1;
            kind == EventKind::Crash || st.unsaved >= self.config.persist_every.max(1)
        };
        if should_persist && let Err(e) = self.persist() {
            tracing::warn!(error = %e, "could not persist telemetry buffer");
        }
        RecordOutcome::Recorded
    }

    /// Records a product-analytics event (`snake_case` name, ≤ 64 chars).
    pub fn track(&self, name: &str, properties: HashMap<String, PropValue>) -> RecordOutcome {
        self.record(EventKind::Analytics, name, properties, Vec::new(), None)
    }

    /// Records the end-of-session QoE summary as `qoe_summary`.
    pub fn track_qoe(&self, summary: &QoeSummary) -> RecordOutcome {
        self.record(
            EventKind::Qoe,
            "qoe_summary",
            summary.to_properties(),
            Vec::new(),
            None,
        )
    }

    /// Records a playback error as `playback_error` with its stable code.
    pub fn track_playback_error(
        &self,
        video_id: &str,
        code: PlaybackErrorCode,
        fatal: bool,
        detail: &str,
    ) -> RecordOutcome {
        let mut p = HashMap::new();
        p.insert("video_id".to_owned(), PropValue::from(video_id));
        p.insert("code".to_owned(), PropValue::from(code.as_str()));
        p.insert("fatal".to_owned(), PropValue::Bool(fatal));
        if !detail.is_empty() {
            p.insert("detail".to_owned(), PropValue::from(detail));
        }
        self.record(EventKind::Error, "playback_error", p, Vec::new(), None)
    }

    /// Records a handled error with a stable `code` (e.g. `upload.rate_limited`) as `error`.
    pub fn track_error(&self, code: &str, mut properties: HashMap<String, PropValue>) -> RecordOutcome {
        properties.insert("code".into(), code.into());
        self.record(EventKind::Error, "error", properties, Vec::new(), None)
    }

    /// Adds a breadcrumb to the ring buffer (needs crash consent).
    pub fn add_breadcrumb(&self, category: &str, message: &str, level: BreadcrumbLevel) {
        let mut st = self.lock();
        if !st.consent.allows_crash() {
            return;
        }
        let crumb = Breadcrumb {
            timestamp_ms: self.clock.now_ms(),
            category: truncate(category, 32),
            message: truncate(&scrub_text(message), self.config.max_string_chars as usize),
            level,
        };
        st.breadcrumbs.push_back(crumb);
        while st.breadcrumbs.len() > self.config.max_breadcrumbs as usize {
            st.breadcrumbs.pop_front();
        }
    }

    pub fn breadcrumbs(&self) -> Vec<Breadcrumb> {
        self.lock().breadcrumbs.iter().cloned().collect()
    }

    /// Breadcrumbs persisted by the previous run (attached to a crash reported on this launch).
    pub fn previous_session_breadcrumbs(&self) -> Vec<Breadcrumb> {
        self.lock().previous_breadcrumbs.clone()
    }

    /// Records a crash with the matching breadcrumbs and persists immediately. Previous-session
    /// breadcrumbs are attached once and then cleared.
    pub fn report_crash(&self, report: CrashReport) -> RecordOutcome {
        let crumbs = {
            let mut st = self.lock();
            if !st.consent.allows_crash() {
                return RecordOutcome::NoConsent;
            }
            if report.from_previous_session {
                std::mem::take(&mut st.previous_breadcrumbs)
            } else {
                st.breadcrumbs.iter().cloned().collect()
            }
        };
        let mut p = report.properties;
        p.insert("kind".into(), report.kind.into());
        p.insert("message".into(), report.message.into());
        if report.occurred_at_ms > 0 {
            p.insert("occurred_at_ms".into(), PropValue::Int(report.occurred_at_ms));
        }
        p.insert(
            "from_previous_session".into(),
            report.from_previous_session.into(),
        );
        self.record(EventKind::Crash, "crash", p, crumbs, report.stack)
    }

    /// Number of buffered events.
    pub fn pending_count(&self) -> u32 {
        self.lock().events.len() as u32
    }

    /// Buffered events (for debugging screens and tests).
    pub fn pending_events(&self) -> Vec<TelemetryEvent> {
        self.lock().events.iter().cloned().collect()
    }

    fn envelope_bytes(&self) -> usize {
        let empty = TelemetryBatch {
            batch_id: "0".repeat(32),
            // i64::MIN ("-9223372036854775808") is the widest possible JSON i64 (20 bytes).
            sent_at_ms: i64::MIN,
            app: self.config.app.clone(),
            events: Vec::new(),
        };
        serde_json::to_vec(&empty).map(|v| v.len()).unwrap_or(512)
    }

    fn next_batch(
        &self,
        st: &mut State,
    ) -> (Vec<TelemetryEvent>, u32, u64, tokio::sync::watch::Receiver<u64>) {
        let mut batch = Vec::new();
        let mut dropped = 0u32;
        let max_bytes = self.config.max_batch_bytes as usize;
        let envelope_bytes = self.envelope_bytes();
        let mut bytes = envelope_bytes;
        let mut oversized_ids = HashSet::new();
        for e in st
            .events
            .iter()
            .filter(|e| st.consent.allows(e.kind) && self.sink.accepts(e.kind))
        {
            let size = serde_json::to_vec(e).map(|v| v.len()).unwrap_or(usize::MAX);
            if envelope_bytes.saturating_add(size) > max_bytes {
                // Single event plus serialized envelope exceeds the configured batch byte cap:
                // drop it so it never violates the cap or blocks the rest of the queue.
                oversized_ids.insert(e.id.clone());
                dropped += 1;
                continue;
            }
            let sep = usize::from(!batch.is_empty());
            if !batch.is_empty()
                && (batch.len() >= self.config.max_batch_events as usize
                    || bytes.saturating_add(size + sep) > max_bytes)
            {
                break;
            }
            bytes = bytes.saturating_add(size + sep);
            batch.push(e.clone());
        }
        if !oversized_ids.is_empty() {
            st.events.retain(|e| !oversized_ids.contains(&e.id));
        }
        // Snapshot epoch and subscribe while still holding `state` so any subsequent `set_consent`
        // both bumps `consent_epoch` and marks `consent_rx` changed.
        let epoch = self.consent_epoch.load(Ordering::Acquire);
        let consent_rx = self.consent_tx.subscribe();
        (batch, dropped, epoch, consent_rx)
    }

    /// Sends buffered events in batches until the buffer is empty or a send fails. A retryable
    /// failure keeps the batch and returns the error; a permanent one drops the batch (counted in
    /// `dropped`) and continues. Concurrent calls are serialised.
    pub async fn flush(&self) -> Result<FlushReport> {
        let _guard = self.flush_lock.lock().await;
        let mut report = FlushReport::default();
        {
            let mut st = self.lock();
            if !st.consent.allows_crash() {
                return Ok(report);
            }
            let before = st.events.len();
            let sink = &self.sink;
            st.events.retain(|e| sink.accepts(e.kind));
            report.unsupported = (before - st.events.len()) as u32;
        }
        let mut result = Ok(());
        loop {
            let (batch_events, oversized_dropped, epoch, mut consent_rx) = {
                let mut st = self.lock();
                self.next_batch(&mut st)
            };
            report.dropped += oversized_dropped;
            if batch_events.is_empty() {
                break;
            }
            let ids: HashSet<String> = batch_events.iter().map(|e| e.id.clone()).collect();
            let batch = TelemetryBatch {
                batch_id: random_id(),
                sent_at_ms: self.clock.now_ms(),
                app: self.config.app.clone(),
                events: batch_events,
            };
            let gated_send = ConsentGated {
                inner: Box::pin(self.sink.send(&batch)),
                owner: std::ptr::from_ref(self) as usize,
                send_gate: &self.send_gate,
                consent_epoch: &self.consent_epoch,
                expected_epoch: epoch,
            };
            // `ConsentGated` enforces the `send_gate` read lock around the epoch check + every poll
            // of `sink.send`, while `consent_rx.changed()` wakes and cancels an in-flight Pending send.
            let send_outcome = tokio::select! {
                biased;
                _ = consent_rx.changed() => None,
                res = gated_send => res,
            };
            let Some(send_outcome) = send_outcome else {
                break;
            };
            match send_outcome {
                Ok(()) => {
                    report.sent += ids.len() as u32;
                    self.lock().events.retain(|e| !ids.contains(&e.id));
                }
                Err(e) if e.is_retryable() => {
                    result = Err(e);
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, events = ids.len(), "telemetry batch rejected; dropping it");
                    report.dropped += ids.len() as u32;
                    self.lock().events.retain(|ev| !ids.contains(&ev.id));
                }
            }
        }
        report.remaining = self.pending_count();
        self.persist()?;
        result.map(|()| report)
    }
}

fn write_private_temp_file(
    parent: &std::path::Path,
    base_name: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;
    let pid = std::process::id();
    for _ in 0..8 {
        let suffix = format!(".{pid}.{}.tmp", random_id());
        let file_name = if base_name.len() + 1 + suffix.len() <= 255 {
            format!(".{base_name}{suffix}")
        } else {
            format!(".aun-tel{suffix}")
        };
        let tmp = parent.join(file_name);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&tmp) {
            Ok(mut file) => {
                if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_data()) {
                    drop(file);
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e);
                }
                return Ok(tmp);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate unique telemetry temp file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_is_deterministic_and_proportional() {
        assert!(session_in_sample("x", 1.0));
        assert!(!session_in_sample("x", 0.0));
        assert!(!session_in_sample("x", f64::NAN));
        let kept = (0..10_000)
            .filter(|i| session_in_sample(&format!("session-{i}"), 0.25))
            .count();
        assert!((2_000..3_000).contains(&kept), "{kept}");
        assert_eq!(session_in_sample("abc", 0.5), session_in_sample("abc", 0.5));
    }

    #[test]
    fn ids_are_unique_hex() {
        let a = random_id();
        let b = random_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn names_are_validated() {
        assert!(valid_name("screen_view"));
        assert!(valid_name("player.speed_changed"));
        assert!(!valid_name("Screen View"));
        assert!(!valid_name(""));
        assert!(!valid_name(&"a".repeat(65)));
    }

    #[cfg(unix)]
    #[test]
    fn write_private_temp_file_uses_owner_only_mode_and_unique_names() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aun-tel-mode-{}", random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = write_private_temp_file(&dir, "telemetry-buffer.json", b"{}").unwrap();
        let b = write_private_temp_file(&dir, "telemetry-buffer.json", b"{}").unwrap();
        assert_ne!(a, b);
        assert!(a.exists() && b.exists());
        let mode_a = std::fs::metadata(&a).unwrap().permissions().mode() & 0o777;
        let mode_b = std::fs::metadata(&b).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode_a, 0o600);
        assert_eq!(mode_b, 0o600);
        let long_name = "a".repeat(240);
        let c = write_private_temp_file(&dir, &long_name, b"{}").unwrap();
        assert!(c.exists());
        assert!(c.file_name().unwrap().len() <= 255);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
