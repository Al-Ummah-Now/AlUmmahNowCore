//! Persistent offline write queue (Epic 11.2, 14.5): likes, comments, notes and other small writes
//! made while offline are stored and replayed in order when connectivity returns.
//!
//! Rules:
//! * FIFO replay. A retryable failure (offline, 5xx, 429) or a 401 stops the flush so later writes
//!   don't overtake earlier ones; the item is rescheduled with backoff.
//! * A permanent failure (4xx other than 401/408/429) drops the item into the report's `rejected`
//!   list so the app can tell the user (e.g. "comment could not be posted").
//! * Writes with the same `coalesce_key` replace each other (like → unlike → like sends one request).
//! * Every write carries an `Idempotency-Key` so a replay after a lost response is safe.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aun_common::platform::{Clock, SystemClock};
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::client::{ApiClient, Body, RequestSpec, RetryPolicy};

/// One queued write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedWrite {
    pub id: String,
    pub operation_id: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: Vec<(String, String)>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub idempotency_key: String,
    pub coalesce_key: Option<String>,
    pub created_at_ms: i64,
    pub attempts: u32,
    pub next_attempt_at_ms: i64,
    pub last_error: Option<String>,
}

/// Where the queue is persisted.
pub trait QueueStore: Send + Sync {
    fn load(&self) -> Result<Vec<QueuedWrite>>;
    fn save(&self, items: &[QueuedWrite]) -> Result<()>;
}

/// JSON file store with atomic replace (write temp file, then rename).
#[derive(Debug, Clone)]
pub struct FileQueueStore {
    path: PathBuf,
}

impl FileQueueStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl QueueStore for FileQueueStore {
    fn load(&self) -> Result<Vec<QueuedWrite>> {
        match std::fs::read(&self.path) {
            Ok(bytes) if bytes.is_empty() => Ok(vec![]),
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| Error::Storage(format!("queue file: {e}")))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, items: &[QueuedWrite]) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(items)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// In-memory store (tests).
#[derive(Debug, Default)]
pub struct MemoryQueueStore(Mutex<Vec<QueuedWrite>>);

impl QueueStore for MemoryQueueStore {
    fn load(&self) -> Result<Vec<QueuedWrite>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| Error::Internal("poisoned".into()))?
            .clone())
    }
    fn save(&self, items: &[QueuedWrite]) -> Result<()> {
        *self.0.lock().map_err(|_| Error::Internal("poisoned".into()))? = items.to_vec();
        Ok(())
    }
}

/// What a flush did.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FlushReport {
    /// Ids sent successfully.
    pub sent: Vec<String>,
    /// Items the server refused permanently, with the error.
    pub rejected: Vec<(QueuedWrite, Error)>,
    /// Items still queued.
    pub remaining: usize,
    /// Why the flush stopped early, if it did (offline, rate limited, sign-in needed).
    pub stopped_by: Option<Error>,
}

/// The queue. Thread-safe; one per account.
pub struct OfflineQueue {
    store: Arc<dyn QueueStore>,
    items: Mutex<Vec<QueuedWrite>>,
    clock: Arc<dyn Clock>,
    backoff: RetryPolicy,
    flushing: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for OfflineQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OfflineQueue").field("len", &self.len()).finish()
    }
}

impl OfflineQueue {
    /// Loads any writes persisted by a previous run.
    pub fn open(store: Arc<dyn QueueStore>) -> Result<Self> {
        Self::open_with_clock(store, Arc::new(SystemClock))
    }

    pub fn open_with_clock(store: Arc<dyn QueueStore>, clock: Arc<dyn Clock>) -> Result<Self> {
        let items = store.load()?;
        Ok(Self {
            store,
            items: Mutex::new(items),
            clock,
            backoff: RetryPolicy {
                max_attempts: u32::MAX,
                base_delay: std::time::Duration::from_secs(2),
                max_delay: std::time::Duration::from_secs(15 * 60),
            },
            flushing: tokio::sync::Mutex::new(()),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Vec<QueuedWrite>>> {
        self.items
            .lock()
            .map_err(|_| Error::Internal("queue lock poisoned".into()))
    }

    pub fn len(&self) -> usize {
        self.items.lock().map(|i| i.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn pending(&self) -> Result<Vec<QueuedWrite>> {
        Ok(self.lock()?.clone())
    }

    /// Queues a write built by a generated operation's request (JSON or empty bodies only).
    /// Returns the queue id.
    pub fn enqueue(&self, req: RequestSpec, coalesce_key: Option<String>) -> Result<String> {
        let body = match req.body {
            Body::None => None,
            Body::Json(v) => Some(v),
            Body::Multipart(_) => return Err(Error::InvalidInput("multipart writes can't be queued".into())),
        };
        let now = self.clock.now_ms();
        let id = new_id(now);
        let item = QueuedWrite {
            id: id.clone(),
            operation_id: req.operation_id,
            method: req.method.as_str().to_owned(),
            path: req.path,
            query: req.query,
            headers: req.headers,
            body,
            idempotency_key: req.idempotency_key.unwrap_or_else(|| id.clone()),
            coalesce_key,
            created_at_ms: now,
            attempts: 0,
            next_attempt_at_ms: now,
            last_error: None,
        };
        let mut items = self.lock()?;
        if let Some(key) = &item.coalesce_key {
            items.retain(|i| i.coalesce_key.as_ref() != Some(key));
        }
        items.push(item);
        self.store.save(&items)?;
        Ok(id)
    }

    /// Removes a queued write (user cancelled it).
    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut items = self.lock()?;
        let before = items.len();
        items.retain(|i| i.id != id);
        let removed = items.len() != before;
        if removed {
            self.store.save(&items)?;
        }
        Ok(removed)
    }

    /// Replays due writes in order. Safe to call often (on reconnect, app foreground, timer):
    /// concurrent calls are serialised.
    pub async fn flush(&self, client: &ApiClient) -> Result<FlushReport> {
        let _guard = self.flushing.lock().await;
        let mut report = FlushReport::default();
        loop {
            let now = self.clock.now_ms();
            let next = {
                let items = self.lock()?;
                match items.first() {
                    Some(i) if i.next_attempt_at_ms <= now => i.clone(),
                    _ => break,
                }
            };
            let method = reqwest::Method::from_bytes(next.method.as_bytes())
                .map_err(|_| Error::Internal(format!("bad method {}", next.method)))?;
            let mut spec = RequestSpec::new(method, next.path.clone(), next.operation_id.clone());
            spec.query = next.query.clone();
            spec.headers = next.headers.clone();
            spec.body = next.body.clone().map(Body::Json).unwrap_or(Body::None);
            spec.idempotency_key = Some(next.idempotency_key.clone());
            spec.auth = crate::client::AuthMode::Required;
            match client.execute(spec).await {
                Ok(_) => {
                    let mut items = self.lock()?;
                    items.retain(|i| i.id != next.id);
                    self.store.save(&items)?;
                    report.sent.push(next.id);
                }
                Err(e) if e.is_retryable() || matches!(e, Error::Unauthorized(_)) => {
                    let mut items = self.lock()?;
                    if let Some(item) = items.iter_mut().find(|i| i.id == next.id) {
                        item.attempts += 1;
                        let retry_after = match &e {
                            Error::RateLimited { retry_after } => *retry_after,
                            _ => None,
                        };
                        let delay = self.backoff.delay(item.attempts, retry_after);
                        item.next_attempt_at_ms = now + delay.as_millis() as i64;
                        item.last_error = Some(e.to_string());
                    }
                    self.store.save(&items)?;
                    report.stopped_by = Some(e);
                    break;
                }
                Err(e) => {
                    let mut items = self.lock()?;
                    items.retain(|i| i.id != next.id);
                    self.store.save(&items)?;
                    report.rejected.push((next, e));
                }
            }
        }
        report.remaining = self.len();
        Ok(report)
    }
}

fn new_id(now_ms: i64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!("q-{now_ms:x}-{pid:x}-{seq:x}")
}
