//! FFI facade for `aun-upload`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::AunError;
use crate::api::ApiClient;
use crate::platform::{Connectivity, ForeignConnectivity};

/// The runtime upload workers run on (they outlive any single FFI call).
fn runtime() -> Result<&'static tokio::runtime::Runtime, AunError> {
    static RT: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("aun-upload")
            .enable_all()
            .build()
    })
    .as_ref()
    .map_err(|e| AunError::Internal {
        detail: format!("upload runtime: {e}"),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UploadState {
    Queued,
    /// Waiting for connectivity / Wi-Fi.
    Waiting,
    Hashing,
    Initiating,
    Uploading,
    Completing,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl From<aun_upload::UploadState> for UploadState {
    fn from(s: aun_upload::UploadState) -> Self {
        use aun_upload::UploadState as S;
        match s {
            S::Queued => Self::Queued,
            S::Waiting => Self::Waiting,
            S::Hashing => Self::Hashing,
            S::Initiating => Self::Initiating,
            S::Uploading => Self::Uploading,
            S::Completing => Self::Completing,
            S::Paused => Self::Paused,
            S::Done => Self::Done,
            S::Failed => Self::Failed,
            S::Cancelled => Self::Cancelled,
        }
    }
}

/// A file to upload and its video metadata.
#[derive(Debug, Clone, uniffi::Record)]
pub struct UploadRequest {
    pub file_path: String,
    pub title: String,
    pub description: Option<String>,
    /// `None`: guessed from the file extension.
    pub mime_type: Option<String>,
    pub channel_id: Option<String>,
    pub is_public: Option<bool>,
    /// Comma-separated.
    pub tags: Option<String>,
    pub category: Option<String>,
    /// e.g. `{"ar": "…", "en": "…"}`
    pub title_localized: HashMap<String, String>,
    pub description_localized: HashMap<String, String>,
    /// Higher uploads first.
    pub priority: i32,
}

impl UploadRequest {
    fn into_core(self) -> Result<aun_upload::UploadRequest, AunError> {
        let mime_type = match self.mime_type {
            Some(m) => m,
            None => aun_upload::video_mime_for(std::path::Path::new(&self.file_path))
                .ok_or_else(|| AunError::InvalidInput {
                    detail: format!("{} is not a supported video file", self.file_path),
                })?
                .to_owned(),
        };
        Ok(aun_upload::UploadRequest {
            file_path: self.file_path,
            title: self.title,
            description: self.description,
            mime_type,
            channel_id: self.channel_id,
            is_public: self.is_public,
            tags: self.tags,
            category: self.category,
            title_localized: self.title_localized.into_iter().collect(),
            description_localized: self.description_localized.into_iter().collect(),
            priority: self.priority,
        })
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct UploadConfig {
    /// Uploads at once, 1–5 (the server allows 5 concurrent sessions).
    pub max_parallel: u32,
    /// Bytes/second across all uploads; `None` = unlimited.
    pub bandwidth_bytes_per_sec: Option<u64>,
    pub wifi_only: bool,
    /// Override the server's chunk size (bytes).
    pub chunk_size: Option<u64>,
    pub compute_checksum: bool,
}

impl Default for UploadConfig {
    fn default() -> Self {
        let d = aun_upload::UploadConfig::default();
        Self {
            max_parallel: d.max_parallel as u32,
            bandwidth_bytes_per_sec: d.bandwidth_bytes_per_sec,
            wifi_only: d.wifi_only,
            chunk_size: d.chunk_size,
            compute_checksum: d.compute_checksum,
        }
    }
}

/// Defaults for [`UploadConfig`].
#[uniffi::export]
pub fn upload_config_default() -> UploadConfig {
    UploadConfig::default()
}

/// One upload's status (no secrets).
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct UploadInfo {
    pub id: String,
    pub file_path: String,
    pub title: String,
    pub state: UploadState,
    pub sent_bytes: u64,
    pub total_bytes: u64,
    /// Set once the server created the video.
    pub video_id: Option<String>,
    pub sha256: Option<String>,
    pub last_error: Option<String>,
    pub priority: i32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl From<aun_upload::UploadManifest> for UploadInfo {
    fn from(m: aun_upload::UploadManifest) -> Self {
        Self {
            id: m.id,
            file_path: m.request.file_path,
            title: m.request.title,
            state: m.state.into(),
            sent_bytes: m.offset,
            total_bytes: m.file_size,
            video_id: m.video_id,
            sha256: m.sha256,
            last_error: m.last_error,
            priority: m.request.priority,
            created_at_ms: m.created_at_ms,
            updated_at_ms: m.updated_at_ms,
        }
    }
}

/// Upload events (called on a background thread; hop to the UI thread yourself).
#[uniffi::export(with_foreign)]
pub trait UploadListener: Send + Sync {
    fn on_progress(&self, id: String, sent_bytes: u64, total_bytes: u64);
    fn on_state(&self, id: String, state: UploadState, error: Option<AunError>);
}

struct ForeignUploadListener(Arc<dyn UploadListener>);

impl aun_upload::UploadListener for ForeignUploadListener {
    fn on_progress(&self, id: &str, sent: u64, total: u64) {
        self.0.on_progress(id.to_owned(), sent, total);
    }
    fn on_state(&self, id: &str, state: aun_upload::UploadState, error: Option<&aun_common::Error>) {
        self.0
            .on_state(id.to_owned(), state.into(), error.cloned().map(Into::into));
    }
}

/// The resumable upload queue for one account. Uploads survive app restarts: create the engine
/// again with the same `store_dir` and `account_id` and they continue where they stopped.
#[derive(uniffi::Object)]
pub struct UploadEngine {
    inner: aun_upload::UploadEngine,
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    watchers: Mutex<Vec<aun_upload::watch::FolderWatcher>>,
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    #[allow(dead_code)]
    watchers: Mutex<()>,
}

impl std::fmt::Debug for UploadEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl UploadEngine {
    /// `client` must be bound to `account_id` (use `AuthManager.api_client(config, account_id)`).
    /// `connectivity`: pass the OS adapter so Wi-Fi-only and offline waiting work.
    #[uniffi::constructor]
    pub fn new(
        client: Arc<ApiClient>,
        store_dir: String,
        account_id: String,
        config: UploadConfig,
        connectivity: Option<Arc<dyn Connectivity>>,
    ) -> Result<Arc<Self>, AunError> {
        let cfg = aun_upload::UploadConfig {
            max_parallel: config.max_parallel as usize,
            bandwidth_bytes_per_sec: config.bandwidth_bytes_per_sec,
            wifi_only: config.wifi_only,
            chunk_size: config.chunk_size,
            compute_checksum: config.compute_checksum,
            ..Default::default()
        };
        let connectivity: Arc<dyn aun_common::platform::Connectivity> = match connectivity {
            Some(c) => Arc::new(ForeignConnectivity(c)),
            None => Arc::new(aun_common::platform::AlwaysOnline),
        };
        let inner = aun_upload::UploadEngine::open_with(
            client.inner.clone(),
            store_dir,
            account_id,
            cfg,
            connectivity,
            Arc::new(aun_common::platform::SystemClock),
        )?;
        let _guard = runtime()?.enter();
        inner.start();
        Ok(Arc::new(Self {
            inner,
            watchers: Mutex::default(),
        }))
    }

    pub fn add_listener(&self, listener: Arc<dyn UploadListener>) {
        self.inner.add_listener(Arc::new(ForeignUploadListener(listener)));
    }

    /// Queues a file; returns the upload id.
    pub fn enqueue(&self, request: UploadRequest) -> Result<String, AunError> {
        Ok(self.inner.enqueue(request.into_core()?)?)
    }

    /// Queues many files (bulk import); stops at the first invalid one.
    pub fn enqueue_all(&self, requests: Vec<UploadRequest>) -> Result<Vec<String>, AunError> {
        requests.into_iter().map(|r| self.enqueue(r)).collect()
    }

    /// Every upload, highest priority then oldest first.
    pub fn list(&self) -> Result<Vec<UploadInfo>, AunError> {
        Ok(self.inner.list()?.into_iter().map(Into::into).collect())
    }

    pub fn get(&self, id: String) -> Result<Option<UploadInfo>, AunError> {
        Ok(self.inner.get(&id)?.map(Into::into))
    }

    /// Uploads with this SHA-256 (duplicate detection).
    pub fn find_by_checksum(&self, sha256: String) -> Result<Vec<UploadInfo>, AunError> {
        Ok(self
            .inner
            .find_by_checksum(&sha256)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn pause(&self, id: String) -> Result<(), AunError> {
        Ok(self.inner.pause(&id)?)
    }

    pub fn resume(&self, id: String) -> Result<(), AunError> {
        Ok(self.inner.resume(&id)?)
    }

    pub fn retry(&self, id: String) -> Result<(), AunError> {
        Ok(self.inner.retry(&id)?)
    }

    /// Stops the upload and discards the partial upload on the server.
    pub async fn cancel(&self, id: String) -> Result<(), AunError> {
        Ok(self.inner.cancel(&id).await?)
    }

    /// Forgets a finished, failed or cancelled upload.
    pub fn remove(&self, id: String) -> Result<(), AunError> {
        Ok(self.inner.remove(&id)?)
    }

    pub fn set_bandwidth_limit(&self, bytes_per_sec: Option<u64>) {
        self.inner.set_bandwidth_limit(bytes_per_sec);
    }

    pub fn set_wifi_only(&self, wifi_only: bool) {
        self.inner.set_wifi_only(wifi_only);
    }

    pub fn set_max_parallel(&self, max_parallel: u32) {
        self.inner.set_max_parallel(max_parallel as usize);
    }

    /// Call when connectivity changes or the app returns to the foreground.
    pub fn poke(&self) {
        self.inner.poke();
    }

    /// Stops transfers (they resume next time an engine is created for this store).
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// Desktop only: queue new video files that appear in `dir`, using `template` for the
    /// metadata (its `file_path` is replaced; the title defaults to the file name when empty).
    /// Throws `Unsupported` on mobile and TV.
    pub fn watch_folder(
        &self,
        dir: String,
        recursive: bool,
        template: UploadRequest,
    ) -> Result<(), AunError> {
        self.watch_folder_impl(dir, recursive, template)
    }
}

impl UploadEngine {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    fn watch_folder_impl(
        &self,
        dir: String,
        recursive: bool,
        template: UploadRequest,
    ) -> Result<(), AunError> {
        let engine = self.inner.clone();
        let on_file: aun_upload::watch::OnFile = Arc::new(move |path: std::path::PathBuf| {
            let mut req = template.clone();
            req.file_path = path.to_string_lossy().into_owned();
            req.mime_type = None;
            if req.title.trim().is_empty() {
                req.title = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Video".into());
            }
            let queued = match req.into_core() {
                Ok(r) => engine.enqueue(r).map_err(|e| e.to_string()),
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = queued {
                tracing::warn!(error = %e, "watched file not queued");
            }
        });
        let _guard = runtime()?.enter();
        let w = aun_upload::watch::FolderWatcher::start(
            &dir,
            recursive,
            std::time::Duration::from_secs(3),
            on_file,
        )?;
        self.watchers
            .lock()
            .map_err(|_| AunError::Internal {
                detail: "watcher lock poisoned".into(),
            })?
            .push(w);
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    fn watch_folder_impl(
        &self,
        _dir: String,
        _recursive: bool,
        _template: UploadRequest,
    ) -> Result<(), AunError> {
        Err(AunError::Unsupported {
            detail: "folder watching is a desktop feature".into(),
        })
    }
}
