//! The upload engine: a persistent, prioritised queue of resumable TUS uploads with bounded
//! parallelism, a shared bandwidth cap, Wi-Fi-only mode and restart recovery.
//!
//! Rate limits (api-videos, prod): upload *sessions* are limited (15/min, 20/h, 50/day, 5
//! concurrent); TUS create + complete share a looser bucket; HEAD/PATCH are unlimited. So the
//! engine initiates a session only right before uploading it, persists the session immediately
//! and never re-initiates to retry: retries resume the same session with HEAD + PATCH.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aun_api::ApiClient;
use aun_common::platform::{AlwaysOnline, Clock, Connectivity, SystemClock};
use aun_common::{Error, Result};
use tokio_util::sync::CancellationToken;

use crate::store::{ManifestStore, UploadManifest, UploadRequest, UploadState, file_identity, sha256_file};
use crate::throttle::Throttle;
use crate::tus::{TusClient, TusTarget};

/// Prod allows 5 concurrent upload sessions per user.
pub const MAX_PARALLEL: usize = 5;
const PIECE: usize = 64 * 1024;
const MIN_CHUNK: u64 = 256 * 1024;
const MAX_CHUNK: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct UploadConfig {
    /// Uploads running at once (1..=5).
    pub max_parallel: usize,
    /// Overrides the server's recommended chunk size.
    pub chunk_size: Option<u64>,
    pub bandwidth_bytes_per_sec: Option<u64>,
    /// Hold uploads while the network is metered.
    pub wifi_only: bool,
    /// Failed attempts before an upload is marked `Failed` (rate limiting doesn't count).
    pub max_attempts: u32,
    /// Hash files before upload (duplicate detection, integrity).
    pub compute_checksum: bool,
    /// Defaults to `<api base>/api/videos/tus`.
    pub tus_endpoint: Option<String>,
    /// Base retry backoff.
    pub retry_base: Duration,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            max_parallel: 3,
            chunk_size: None,
            bandwidth_bytes_per_sec: None,
            wifi_only: false,
            max_attempts: 8,
            compute_checksum: true,
            tus_endpoint: None,
            retry_base: Duration::from_secs(2),
        }
    }
}

/// Upload events for the UI (called from worker threads).
pub trait UploadListener: Send + Sync {
    fn on_progress(&self, id: &str, sent: u64, total: u64);
    fn on_state(&self, id: &str, state: UploadState, error: Option<&Error>);
}

struct Inner {
    api: ApiClient,
    tus: TusClient,
    store: ManifestStore,
    account_id: String,
    cfg: Mutex<UploadConfig>,
    throttle: Arc<Throttle>,
    connectivity: Arc<dyn Connectivity>,
    clock: Arc<dyn Clock>,
    uploads: Mutex<BTreeMap<String, UploadManifest>>,
    running: Mutex<HashMap<String, CancellationToken>>,
    listeners: Arc<Mutex<Vec<Arc<dyn UploadListener>>>>,
    wake: tokio::sync::Notify,
    started: AtomicBool,
    shutdown: CancellationToken,
}

/// The engine. Cheap to clone.
#[derive(Clone)]
pub struct UploadEngine {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for UploadEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UploadEngine")
            .field("account_id", &self.inner.account_id)
            .finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    m.lock()
        .map_err(|_| Error::Internal("upload engine lock poisoned".into()))
}

fn new_id(now: i64) -> String {
    use std::sync::atomic::AtomicU64;
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "up-{now:x}-{:x}-{:x}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

impl UploadEngine {
    /// Opens the engine for `account_id`, loading that account's uploads from `store_dir`.
    /// Uploads interrupted mid-flight are re-queued and resume from the server's offset.
    pub fn open(
        api: ApiClient,
        store_dir: impl AsRef<Path>,
        account_id: impl Into<String>,
        cfg: UploadConfig,
    ) -> Result<Self> {
        Self::open_with(
            api,
            store_dir,
            account_id,
            cfg,
            Arc::new(AlwaysOnline),
            Arc::new(SystemClock),
        )
    }

    pub fn open_with(
        api: ApiClient,
        store_dir: impl AsRef<Path>,
        account_id: impl Into<String>,
        mut cfg: UploadConfig,
        connectivity: Arc<dyn Connectivity>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let account_id = account_id.into();
        if account_id.is_empty() {
            return Err(Error::InvalidInput("upload engine needs an account id".into()));
        }
        if api.account_id().is_some_and(|a| a != account_id) {
            return Err(Error::InvalidInput(
                "API client is bound to a different account".into(),
            ));
        }
        cfg.max_parallel = cfg.max_parallel.clamp(1, MAX_PARALLEL);
        let store = ManifestStore::open(store_dir.as_ref())?;
        let mut uploads = BTreeMap::new();
        for mut m in store.load_all()? {
            if m.account_id != account_id {
                continue;
            }
            if matches!(
                m.state,
                UploadState::Hashing
                    | UploadState::Initiating
                    | UploadState::Uploading
                    | UploadState::Completing
                    | UploadState::Waiting
            ) {
                m.state = UploadState::Queued;
                store.save(&m)?;
            }
            uploads.insert(m.id.clone(), m);
        }
        let tus = TusClient::new(api.http().clone());
        let throttle = Arc::new(Throttle::new(cfg.bandwidth_bytes_per_sec));
        Ok(Self {
            inner: Arc::new(Inner {
                api,
                tus,
                store,
                account_id,
                cfg: Mutex::new(cfg),
                throttle,
                connectivity,
                clock,
                uploads: Mutex::new(uploads),
                running: Mutex::new(HashMap::new()),
                listeners: Arc::new(Mutex::new(vec![])),
                wake: tokio::sync::Notify::new(),
                started: AtomicBool::new(false),
                shutdown: CancellationToken::new(),
            }),
        })
    }

    pub fn add_listener(&self, l: Arc<dyn UploadListener>) {
        if let Ok(mut v) = self.inner.listeners.lock() {
            v.push(l);
        }
    }

    /// Queues a file. Returns the upload id.
    pub fn enqueue(&self, request: UploadRequest) -> Result<String> {
        if request.title.trim().is_empty() {
            return Err(Error::InvalidInput("title is required".into()));
        }
        let (size, mtime) = file_identity(Path::new(&request.file_path))?;
        if size == 0 {
            return Err(Error::InvalidInput("file is empty".into()));
        }
        let now = self.inner.clock.now_ms();
        let m = UploadManifest {
            id: new_id(now),
            account_id: self.inner.account_id.clone(),
            request,
            state: UploadState::Queued,
            file_size: size,
            file_mtime_ms: mtime,
            sha256: None,
            video_id: None,
            tus_session_id: None,
            target: None,
            upload_url: None,
            offset: 0,
            chunk_size: 0,
            attempts: 0,
            last_error: None,
            created_at_ms: now,
            updated_at_ms: now,
            not_before_ms: 0,
        };
        self.inner.store.save(&m)?;
        let id = m.id.clone();
        lock(&self.inner.uploads)?.insert(id.clone(), m);
        self.inner.emit_state(&id, UploadState::Queued, None);
        self.inner.wake.notify_one();
        Ok(id)
    }

    /// Every upload of this account, highest priority then oldest first.
    pub fn list(&self) -> Result<Vec<UploadManifest>> {
        let mut v: Vec<_> = lock(&self.inner.uploads)?.values().cloned().collect();
        v.sort_by(|a, b| {
            b.request
                .priority
                .cmp(&a.request.priority)
                .then(a.created_at_ms.cmp(&b.created_at_ms))
        });
        Ok(v)
    }

    pub fn get(&self, id: &str) -> Result<Option<UploadManifest>> {
        Ok(lock(&self.inner.uploads)?.get(id).cloned())
    }

    /// Uploads with the same content hash as `sha256` (duplicate detection for bulk import).
    pub fn find_by_checksum(&self, sha256: &str) -> Result<Vec<UploadManifest>> {
        Ok(lock(&self.inner.uploads)?
            .values()
            .filter(|m| m.sha256.as_deref() == Some(sha256))
            .cloned()
            .collect())
    }

    pub fn pause(&self, id: &str) -> Result<()> {
        self.inner.update(id, |m| {
            if !m.state.is_terminal() {
                m.state = UploadState::Paused;
            }
        })?;
        if let Some(t) = lock(&self.inner.running)?.get(id) {
            t.cancel();
        }
        self.inner.emit_state(id, UploadState::Paused, None);
        Ok(())
    }

    pub fn resume(&self, id: &str) -> Result<()> {
        self.inner.update(id, |m| {
            if m.state == UploadState::Paused {
                m.state = UploadState::Queued;
                m.not_before_ms = 0;
            }
        })?;
        self.inner.wake.notify_one();
        Ok(())
    }

    /// Re-queues a failed upload (keeps its session, so no new upload session is opened).
    pub fn retry(&self, id: &str) -> Result<()> {
        self.inner.update(id, |m| {
            if m.state == UploadState::Failed {
                m.state = UploadState::Queued;
                m.attempts = 0;
                m.not_before_ms = 0;
            }
        })?;
        self.inner.wake.notify_one();
        Ok(())
    }

    /// Cancels: stops the transfer and asks the server to discard the partial upload.
    pub async fn cancel(&self, id: &str) -> Result<()> {
        match self.get(id)?.map(|m| m.state) {
            Some(UploadState::Done) => return Err(Error::InvalidInput("the upload already finished".into())),
            Some(UploadState::Cancelled) => return Ok(()),
            _ => {}
        }
        let m = self.inner.update(id, |m| m.state = UploadState::Cancelled)?;
        if let Some(t) = lock(&self.inner.running)?.get(id) {
            t.cancel();
        }
        if let (Some(target), Some(url)) = (&m.target, &m.upload_url) {
            let bearer = self.inner.api.bearer().await;
            if let Err(e) = self.inner.tus.terminate(target, bearer.as_deref(), url).await {
                tracing::debug!(error = %e, "TUS terminate failed (ignored)");
            }
        }
        self.inner.emit_state(id, UploadState::Cancelled, None);
        Ok(())
    }

    /// Forgets a finished, failed or cancelled upload.
    pub fn remove(&self, id: &str) -> Result<()> {
        let mut uploads = lock(&self.inner.uploads)?;
        match uploads.get(id) {
            Some(m) if m.state.is_terminal() => {
                self.inner.store.delete(id)?;
                uploads.remove(id);
                Ok(())
            }
            Some(_) => Err(Error::InvalidInput("cancel or finish the upload first".into())),
            None => Err(Error::NotFound(format!("upload {id}"))),
        }
    }

    pub fn set_bandwidth_limit(&self, bytes_per_sec: Option<u64>) {
        self.inner.throttle.set_rate(bytes_per_sec);
    }

    pub fn set_wifi_only(&self, wifi_only: bool) {
        if let Ok(mut c) = self.inner.cfg.lock() {
            c.wifi_only = wifi_only;
        }
        self.inner.wake.notify_one();
    }

    pub fn set_max_parallel(&self, n: usize) {
        if let Ok(mut c) = self.inner.cfg.lock() {
            c.max_parallel = n.clamp(1, MAX_PARALLEL);
        }
        self.inner.wake.notify_one();
    }

    /// Call when connectivity changes or the app returns to the foreground.
    pub fn poke(&self) {
        self.inner.wake.notify_one();
    }

    /// Starts the scheduler on the current tokio runtime (idempotent).
    pub fn start(&self) {
        if self.inner.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let inner = self.inner.clone();
        tokio::spawn(async move { inner.schedule_loop().await });
    }

    /// Stops scheduling and interrupts running transfers (they resume on the next start).
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
        if let Ok(r) = self.inner.running.lock() {
            for t in r.values() {
                t.cancel();
            }
        }
    }
}

enum Outcome {
    Done,
    /// Stopped by pause/cancel/shutdown: state already set.
    Interrupted,
    /// Can't run now (offline / metered with Wi-Fi only).
    Waiting,
}

impl Inner {
    fn emit_state(&self, id: &str, state: UploadState, error: Option<&Error>) {
        let ls = self.listeners.lock().map(|v| v.clone()).unwrap_or_default();
        for l in ls {
            l.on_state(id, state, error);
        }
    }

    fn emit_progress(&self, id: &str, sent: u64, total: u64) {
        let ls = self.listeners.lock().map(|v| v.clone()).unwrap_or_default();
        for l in ls {
            l.on_progress(id, sent, total);
        }
    }

    /// Mutates and persists a manifest; returns the updated copy.
    fn update(&self, id: &str, f: impl FnOnce(&mut UploadManifest)) -> Result<UploadManifest> {
        let mut uploads = lock(&self.uploads)?;
        let m = uploads
            .get_mut(id)
            .ok_or_else(|| Error::NotFound(format!("upload {id}")))?;
        f(m);
        m.updated_at_ms = self.clock.now_ms();
        self.store.save(m)?;
        Ok(m.clone())
    }

    fn set_state(&self, id: &str, state: UploadState) -> Result<UploadManifest> {
        let m = self.update(id, |m| m.state = state)?;
        self.emit_state(id, state, None);
        Ok(m)
    }

    fn can_run_now(&self) -> bool {
        let net = self.connectivity.current();
        let wifi_only = self.cfg.lock().map(|c| c.wifi_only).unwrap_or(false);
        net.online && !(wifi_only && net.metered)
    }

    async fn schedule_loop(self: Arc<Self>) {
        loop {
            if self.shutdown.is_cancelled() {
                return;
            }
            if let Err(e) = self.clone().spawn_runnable() {
                tracing::warn!(error = %e, "upload scheduling failed");
            }
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
        }
    }

    fn spawn_runnable(self: Arc<Self>) -> Result<()> {
        let max = self.cfg.lock().map(|c| c.max_parallel).unwrap_or(1);
        let now = self.clock.now_ms();
        let can_run = self.can_run_now();
        let mut running = lock(&self.running)?;
        let candidates: Vec<String> = {
            let mut uploads = lock(&self.uploads)?;
            let mut list: Vec<&mut UploadManifest> = uploads.values_mut().collect();
            list.sort_by(|a, b| {
                b.request
                    .priority
                    .cmp(&a.request.priority)
                    .then(a.created_at_ms.cmp(&b.created_at_ms))
            });
            let mut out = vec![];
            for m in list {
                if !matches!(m.state, UploadState::Queued | UploadState::Waiting)
                    || running.contains_key(&m.id)
                {
                    continue;
                }
                if !can_run {
                    if m.state != UploadState::Waiting {
                        m.state = UploadState::Waiting;
                        self.store.save(m)?;
                    }
                    continue;
                }
                if m.not_before_ms <= now {
                    out.push(m.id.clone());
                }
            }
            out
        };
        for id in candidates {
            if running.len() >= max {
                break;
            }
            let token = self.shutdown.child_token();
            running.insert(id.clone(), token.clone());
            let inner = self.clone();
            tokio::spawn(async move {
                inner.clone().run_one(&id, token).await;
                if let Ok(mut r) = inner.running.lock() {
                    r.remove(&id);
                }
                inner.wake.notify_one();
            });
        }
        Ok(())
    }

    async fn run_one(self: Arc<Self>, id: &str, cancel: CancellationToken) {
        match self.process(id, &cancel).await {
            Ok(Outcome::Done) | Ok(Outcome::Interrupted) => {}
            Ok(Outcome::Waiting) => {
                let _ = self.set_state(id, UploadState::Waiting);
            }
            Err(e) => self.handle_error(id, e),
        }
    }

    fn handle_error(&self, id: &str, e: Error) {
        let (max_attempts, base) = self
            .cfg
            .lock()
            .map(|c| (c.max_attempts, c.retry_base))
            .unwrap_or((8, Duration::from_secs(2)));
        let now = self.clock.now_ms();
        let msg = e.to_string();
        let result = self.update(id, |m| {
            if m.state.is_terminal() || m.state == UploadState::Paused {
                return;
            }
            m.last_error = Some(msg.clone());
            match &e {
                Error::RateLimited { retry_after } => {
                    let wait = retry_after.unwrap_or(Duration::from_secs(60));
                    m.state = UploadState::Queued;
                    m.not_before_ms = now + wait.as_millis() as i64;
                }
                e if e.is_retryable() || matches!(e, Error::Conflict(_)) => {
                    m.attempts += 1;
                    if m.attempts >= max_attempts {
                        m.state = UploadState::Failed;
                    } else {
                        let exp = base
                            .saturating_mul(1u32 << m.attempts.min(10))
                            .min(Duration::from_secs(15 * 60));
                        m.state = UploadState::Queued;
                        m.not_before_ms = now + exp.as_millis() as i64;
                    }
                }
                _ => m.state = UploadState::Failed,
            }
        });
        if let Ok(m) = result {
            self.emit_state(id, m.state, Some(&e));
        }
    }

    fn snapshot(&self, id: &str) -> Result<UploadManifest> {
        lock(&self.uploads)?
            .get(id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("upload {id}")))
    }

    /// Stops if the user paused/cancelled or the engine is shutting down.
    fn interrupted(&self, id: &str, cancel: &CancellationToken) -> bool {
        cancel.is_cancelled()
            || self
                .snapshot(id)
                .map(|m| matches!(m.state, UploadState::Paused | UploadState::Cancelled))
                .unwrap_or(true)
    }

    async fn process(&self, id: &str, cancel: &CancellationToken) -> Result<Outcome> {
        let m = self.snapshot(id)?;
        let path = std::path::PathBuf::from(&m.request.file_path);
        let (size, mtime) = file_identity(&path)?;
        if size != m.file_size || mtime != m.file_mtime_ms {
            return Err(Error::InvalidInput(
                "the file changed since it was queued; add it again".into(),
            ));
        }
        if !self.can_run_now() {
            return Ok(Outcome::Waiting);
        }

        // 1. Checksum
        let compute = self.cfg.lock().map(|c| c.compute_checksum).unwrap_or(true);
        if compute && m.sha256.is_none() {
            self.set_state(id, UploadState::Hashing)?;
            let p = path.clone();
            let hash = tokio::task::spawn_blocking(move || sha256_file(&p))
                .await
                .map_err(|e| Error::Internal(e.to_string()))??;
            self.update(id, |m| m.sha256 = Some(hash))?;
        }
        if self.interrupted(id, cancel) {
            return Ok(Outcome::Interrupted);
        }

        // 2. Upload session (never re-initiated once stored)
        if self.snapshot(id)?.video_id.is_none() {
            self.set_state(id, UploadState::Initiating)?;
            self.initiate(id).await?;
        }

        // 3. TUS transfer
        self.set_state(id, UploadState::Uploading)?;
        match self.transfer(id, &path, cancel).await? {
            Outcome::Done => {}
            other => return Ok(other),
        }

        // 4. Complete
        self.set_state(id, UploadState::Completing)?;
        let m = self.snapshot(id)?;
        let video_id = m.video_id.clone().unwrap_or_default();
        self.api
            .post_upload_complete_endpoint(
                &aun_api::PostUploadCompleteEndpointParams {
                    header_channel_id: m.request.channel_id.clone(),
                },
                &aun_api::PostUploadCompleteEndpointBody {
                    video_id,
                    file_size: Some(m.file_size as f64),
                    duration: None,
                },
            )
            .await?;
        self.update(id, |m| {
            m.state = UploadState::Done;
            m.last_error = None;
        })?;
        self.emit_state(id, UploadState::Done, None);
        Ok(Outcome::Done)
    }

    async fn initiate(&self, id: &str) -> Result<()> {
        let m = self.snapshot(id)?;
        let r = &m.request;
        let opt_map = |b: &BTreeMap<String, String>| (!b.is_empty()).then(|| b.clone());
        let body = aun_api::PostUploadInitiateEndpointBody {
            title: r.title.clone(),
            description: r.description.clone(),
            file_size: m.file_size as f64,
            mime_type: r.mime_type.clone(),
            is_public: r.is_public,
            tags: r.tags.clone(),
            title_localized: opt_map(&r.title_localized),
            description_localized: opt_map(&r.description_localized),
            category: r.category.clone(),
            ..Default::default()
        };
        let resp = self
            .api
            .post_upload_initiate_endpoint(
                &aun_api::PostUploadInitiateEndpointParams {
                    header_channel_id: r.channel_id.clone(),
                },
                &body,
            )
            .await?;
        let res = resp.result;
        if res.video_id.is_empty() || res.tus_session_id.is_empty() {
            return Err(Error::Http {
                status: 200,
                code: None,
                message: "initiate returned no videoId/tusSessionId".into(),
            });
        }
        let endpoint = self.tus_endpoint();
        let chunk = self.chunk_size(res.chunk_size as u64);
        self.update(id, |m| {
            m.video_id = Some(res.video_id);
            m.tus_session_id = Some(res.tus_session_id);
            m.target = Some(TusTarget::Proxy { endpoint });
            m.chunk_size = chunk;
            m.upload_url = None;
            m.offset = 0;
        })?;
        Ok(())
    }

    fn tus_endpoint(&self) -> String {
        self.cfg
            .lock()
            .ok()
            .and_then(|c| c.tus_endpoint.clone())
            .unwrap_or_else(|| format!("{}/api/videos/tus", self.api.config().base_url))
    }

    fn chunk_size(&self, server: u64) -> u64 {
        let over = self.cfg.lock().ok().and_then(|c| c.chunk_size);
        over.unwrap_or(if server > 0 { server } else { 5 * 1024 * 1024 })
            .clamp(MIN_CHUNK, MAX_CHUNK)
    }

    /// The proxy session expired (KV TTL): get fresh direct-upload authorisation for the video.
    async fn reauthorise(&self, id: &str) -> Result<()> {
        let m = self.snapshot(id)?;
        let video_id = m
            .video_id
            .clone()
            .ok_or_else(|| Error::Internal("no video id".into()))?;
        let r = self
            .api
            .get_upload_resume_endpoint(&aun_api::GetUploadResumeEndpointParams {
                video_id,
                header_channel_id: m.request.channel_id.clone(),
            })
            .await?
            .result;
        let mut headers = BTreeMap::new();
        headers.insert(
            "AuthorizationSignature".into(),
            r.tus_headers.authorization_signature,
        );
        headers.insert("AuthorizationExpire".into(), r.tus_headers.authorization_expire);
        headers.insert("LibraryId".into(), r.tus_headers.library_id);
        headers.insert("VideoId".into(), r.tus_headers.video_id);
        let chunk = self.chunk_size(r.chunk_size as u64);
        self.update(id, |m| {
            m.target = Some(TusTarget::Direct {
                endpoint: r.tus_endpoint,
                headers,
            });
            m.upload_url = None;
            m.offset = 0;
            m.chunk_size = chunk;
        })?;
        Ok(())
    }

    fn metadata(m: &UploadManifest) -> BTreeMap<String, String> {
        let mut meta = BTreeMap::new();
        let filename = Path::new(&m.request.file_path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_else(|| "video".into());
        meta.insert("filename".into(), filename);
        meta.insert("filetype".into(), m.request.mime_type.clone());
        meta.insert("title".into(), m.request.title.clone());
        if let Some(s) = &m.tus_session_id {
            meta.insert("tusSessionId".into(), s.clone());
        }
        if let Some(v) = &m.video_id {
            meta.insert("videoId".into(), v.clone());
        }
        meta
    }

    async fn transfer(&self, id: &str, path: &Path, cancel: &CancellationToken) -> Result<Outcome> {
        let mut reauthorised = false;
        let mut refreshed = false;
        loop {
            match self.transfer_once(id, path, cancel).await {
                // 404/410: the proxy session (or direct authorisation) is gone. Once per run.
                Err(Error::NotFound(_)) | Err(Error::Http { status: 410, .. }) if !reauthorised => {
                    reauthorised = true;
                    self.reauthorise(id).await?;
                }
                Err(Error::Unauthorized(_)) | Err(Error::Forbidden(_)) if !refreshed => {
                    refreshed = true;
                    let direct = matches!(self.snapshot(id)?.target, Some(TusTarget::Direct { .. }));
                    if direct {
                        self.reauthorise(id).await?; // signature expired
                    } else {
                        self.api.refresh_bearer().await?;
                    }
                }
                other => return other,
            }
        }
    }

    async fn transfer_once(&self, id: &str, path: &Path, cancel: &CancellationToken) -> Result<Outcome> {
        let m = self.snapshot(id)?;
        let target = m
            .target
            .clone()
            .ok_or_else(|| Error::Internal("no TUS target".into()))?;
        let bearer = self.api.bearer().await;
        let size = m.file_size;

        let upload_url = match &m.upload_url {
            Some(u) => {
                // Resync with the server: it is the source of truth for the offset.
                let off = self.tus.offset(&target, bearer.as_deref(), u).await?;
                self.update(id, |m| m.offset = off.min(size))?;
                u.clone()
            }
            None => {
                let url = self
                    .tus
                    .create(&target, bearer.as_deref(), size, &Self::metadata(&m))
                    .await?;
                self.update(id, |m| {
                    m.upload_url = Some(url.clone());
                    m.offset = 0;
                })?;
                url
            }
        };

        let mut offset = self.snapshot(id)?.offset;
        self.emit_progress(id, offset, size);
        while offset < size {
            if self.interrupted(id, cancel) {
                return Ok(Outcome::Interrupted);
            }
            if !self.can_run_now() {
                return Ok(Outcome::Waiting);
            }
            let chunk = self.snapshot(id)?.chunk_size.max(MIN_CHUNK);
            let len = chunk.min(size - offset);
            let body = self.chunk_body(id, path, offset, len, size, cancel.clone())?;
            let bearer = self.api.bearer().await;
            match self
                .tus
                .patch(&target, bearer.as_deref(), &upload_url, offset, body, len)
                .await
            {
                Ok(new_offset) => {
                    offset = new_offset.min(size);
                    self.update(id, |m| m.offset = offset)?;
                    self.emit_progress(id, offset, size);
                }
                Err(Error::Conflict(_)) => {
                    offset = self
                        .tus
                        .offset(&target, bearer.as_deref(), &upload_url)
                        .await?
                        .min(size);
                    self.update(id, |m| m.offset = offset)?;
                }
                Err(e) if cancel.is_cancelled() => {
                    tracing::debug!(error = %e, "PATCH interrupted");
                    return Ok(Outcome::Interrupted);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(Outcome::Done)
    }

    /// Streams `[offset, offset+len)` of the file in 64 KiB pieces through the bandwidth cap,
    /// reporting progress per piece. Aborts the body when cancelled.
    fn chunk_body(
        &self,
        id: &str,
        path: &Path,
        offset: u64,
        len: u64,
        total: u64,
        cancel: CancellationToken,
    ) -> Result<reqwest::Body> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(offset))?;
        let file = Arc::new(Mutex::new(file.take(len)));
        let id = id.to_owned();
        let state = Arc::new((
            self.throttle.clone(),
            self.listeners.clone(),
            std::sync::atomic::AtomicU64::new(offset),
        ));
        let stream = futures_util::stream::unfold(
            (file, state, id, cancel),
            move |(file, state, id, cancel)| async move {
                if cancel.is_cancelled() {
                    return Some((Err(std::io::Error::other("cancelled")), (file, state, id, cancel)));
                }
                let read = {
                    let f = file.clone();
                    tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                        let mut buf = vec![0u8; PIECE];
                        let mut g = f.lock().map_err(|_| std::io::Error::other("poisoned"))?;
                        let n = g.read(&mut buf)?;
                        buf.truncate(n);
                        Ok(buf)
                    })
                    .await
                };
                let buf = match read {
                    Ok(Ok(b)) => b,
                    Ok(Err(e)) => return Some((Err(e), (file, state, id, cancel))),
                    Err(e) => return Some((Err(std::io::Error::other(e)), (file, state, id, cancel))),
                };
                if buf.is_empty() {
                    return None;
                }
                state.0.acquire(buf.len() as u64).await;
                let sent = state.2.fetch_add(buf.len() as u64, Ordering::Relaxed) + buf.len() as u64;
                for l in state.1.lock().map(|v| v.clone()).unwrap_or_default() {
                    l.on_progress(&id, sent, total);
                }
                Some((Ok(bytes::Bytes::from(buf)), (file, state, id, cancel)))
            },
        );
        Ok(reqwest::Body::wrap_stream(stream))
    }
}
