//! FFI facade for `aun-upload`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::api::ApiClient;
use crate::platform::{Connectivity, ForeignConnectivity};
use crate::{AunError, Visibility};

/// The runtime upload workers run on (they outlive any single FFI call).
fn runtime() -> Result<&'static tokio::runtime::Runtime, AunError> {
    crate::runtime::background()
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
    /// Legacy visibility used when `visibility` is absent.
    pub is_public: Option<bool>,
    /// Comma-separated.
    pub tags: Option<String>,
    pub category: Option<String>,
    /// e.g. `{"ar": "…", "en": "…"}`
    pub title_localized: HashMap<String, String>,
    pub description_localized: HashMap<String, String>,
    /// Higher uploads first.
    pub priority: i32,
    /// Takes precedence over `is_public`.
    #[uniffi(default = None)]
    pub visibility: Option<Visibility>,
}

impl UploadRequest {
    fn into_core(self) -> Result<aun_upload::UploadRequest, AunError> {
        let mime_type = match self.mime_type {
            Some(m) => m,
            None => aun_upload::video_mime_for(std::path::Path::new(&self.file_path))
                .ok_or_else(|| AunError::InvalidInput {
                    code: None,
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
            visibility: self.visibility.map(Into::into),
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
    /// Stable snake_case error kind, such as `forbidden`.
    pub last_error_kind: Option<String>,
    /// Machine-readable server code, such as `terms_required`.
    pub last_error_code: Option<String>,
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
            last_error_kind: m.last_error_kind,
            last_error_code: m.last_error_code,
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

/// A seekable platform stream with known size. Callbacks can run on background threads.
#[uniffi::export(with_foreign)]
pub trait UploadSource: Send + Sync {
    fn size(&self) -> Result<u64, AunError>;
    /// Identity stable across reopens and different whenever the bytes change.
    fn fingerprint(&self) -> Result<String, AunError>;
    /// Reads at most len bytes. Short reads are allowed; empty means EOF.
    fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, AunError>;
}

/// Reopens a persisted stream ID. The app must retain access across restarts.
#[uniffi::export(with_foreign)]
pub trait UploadSourceOpener: Send + Sync {
    fn reopen(&self, source_id: String) -> Result<Arc<dyn UploadSource>, AunError>;
}

struct ForeignUploadSource(Arc<dyn UploadSource>);

impl aun_upload::UploadSource for ForeignUploadSource {
    fn size(&self) -> aun_common::Result<u64> {
        Ok(self.0.size()?)
    }

    fn fingerprint(&self) -> aun_common::Result<String> {
        Ok(self.0.fingerprint()?)
    }

    fn read_at(&self, offset: u64, len: u64) -> aun_common::Result<Vec<u8>> {
        Ok(self.0.read_at(offset, len)?)
    }
}

struct ForeignUploadSourceOpener(Arc<dyn UploadSourceOpener>);

impl aun_upload::UploadSourceOpener for ForeignUploadSourceOpener {
    fn reopen(&self, source_id: String) -> aun_common::Result<Arc<dyn aun_upload::UploadSource>> {
        Ok(Arc::new(ForeignUploadSource(self.0.reopen(source_id)?)))
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
        Self::with_source_opener(client, store_dir, account_id, config, connectivity, None)
    }

    /// Opens the queue and registers the opener before recovering uploads.
    #[uniffi::constructor]
    pub fn with_source_opener(
        client: Arc<ApiClient>,
        store_dir: String,
        account_id: String,
        config: UploadConfig,
        connectivity: Option<Arc<dyn Connectivity>>,
        source_opener: Option<Arc<dyn UploadSourceOpener>>,
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
        if let Some(opener) = source_opener {
            inner.register_source_opener(Arc::new(ForeignUploadSourceOpener(opener)))?;
        }
        let _guard = runtime()?.enter();
        inner.start();
        Ok(Arc::new(Self {
            inner,
            watchers: Mutex::default(),
        }))
    }

    /// Registers an opener. Call retry on any upload that already failed to reopen.
    pub fn register_source_opener(&self, opener: Arc<dyn UploadSourceOpener>) -> Result<(), AunError> {
        Ok(self
            .inner
            .register_source_opener(Arc::new(ForeignUploadSourceOpener(opener)))?)
    }

    /// Queues a platform stream. Set request.file_path to the display filename for MIME
    /// inference and upload metadata. Only the opener uses source_id.
    pub fn enqueue_stream(&self, request: UploadRequest, source_id: String) -> Result<String, AunError> {
        Ok(self.inner.enqueue_stream(request.into_core()?, source_id)?)
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

#[cfg(test)]
mod tests {
    use super::*;
    use uniffi::{Lift, Lower};

    #[test]
    fn upload_visibility_survives_the_ffi_record_and_reaches_core() {
        for value in [
            None,
            Some(Visibility::Public),
            Some(Visibility::Unlisted),
            Some(Visibility::Private),
        ] {
            let request = UploadRequest {
                file_path: "clip.mp4".into(),
                title: "Title".into(),
                description: None,
                mime_type: None,
                channel_id: None,
                is_public: Some(true),
                tags: None,
                category: None,
                title_localized: HashMap::new(),
                description_localized: HashMap::new(),
                priority: 0,
                visibility: value,
            };
            let mut bytes = Vec::new();
            <UploadRequest as Lower<crate::UniFfiTag>>::write(request, &mut bytes);
            let decoded = <UploadRequest as Lift<crate::UniFfiTag>>::try_read(&mut bytes.as_slice());
            let core = decoded.ok().and_then(|request| request.into_core().ok());
            assert_eq!(
                core.as_ref().and_then(|request| request.visibility),
                value.map(Into::into)
            );
            assert_eq!(core.and_then(|request| request.is_public), Some(true));
        }
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;
    use aun_upload::UploadSourceOpener as _;

    struct Source;

    impl UploadSource for Source {
        fn size(&self) -> Result<u64, AunError> {
            Ok(3)
        }

        fn fingerprint(&self) -> Result<String, AunError> {
            Ok("revision-1".into())
        }

        fn read_at(&self, offset: u64, len: u64) -> Result<Vec<u8>, AunError> {
            assert_eq!(len, 2);
            match offset {
                0 => Ok(vec![42]),
                3 => Ok(vec![]),
                _ => Err(AunError::Platform {
                    detail: "read failed".into(),
                }),
            }
        }
    }

    struct Opener;

    impl UploadSourceOpener for Opener {
        fn reopen(&self, source_id: String) -> Result<Arc<dyn UploadSource>, AunError> {
            if source_id == "content://test/video" {
                Ok(Arc::new(Source))
            } else {
                Err(AunError::Platform {
                    detail: "permission revoked".into(),
                })
            }
        }
    }

    #[test]
    fn adapts_source_results_and_errors() {
        let opener = ForeignUploadSourceOpener(Arc::new(Opener));
        let source = opener.reopen("content://test/video".into()).unwrap();
        assert_eq!(source.size().unwrap(), 3);
        assert_eq!(source.fingerprint().unwrap(), "revision-1");
        assert_eq!(source.read_at(0, 2).unwrap(), vec![42]);
        assert!(source.read_at(3, 2).unwrap().is_empty());
        assert_eq!(
            source.read_at(1, 2),
            Err(aun_common::Error::Platform("read failed".into()))
        );
        assert!(matches!(
            opener.reopen("content://test/revoked".into()),
            Err(aun_common::Error::Platform(detail)) if detail == "permission revoked"
        ));
    }
}
