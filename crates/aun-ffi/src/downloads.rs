//! UniFFI facade for `aun-downloads` (Epic 14, E14-D1/D2/D5/D8: plain-file `(user_id, video_id)`
//! manifest store with exclusive lock, non-revivable two-phase deletion, revocations feed cursor &
//! purge, quarantine reconciliation, atomic budget reservation, Wi-Fi/charging policy, availability
//! sync cleanup, and per-channel auto-download selection).

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use aun_downloads as core;

use crate::AunError;

fn map_err(err: core::DownloadError) -> AunError {
    match err {
        core::DownloadError::InvalidInput(msg) => AunError::InvalidInput { detail: msg },
        core::DownloadError::Conflict(msg) => AunError::Conflict { detail: msg },
        core::DownloadError::Storage(msg) => AunError::Storage { detail: msg },
        core::DownloadError::Io(e) => AunError::Storage {
            detail: e.to_string(),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OfflineDownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Removed,
    Failed,
}

impl From<OfflineDownloadStatus> for core::DownloadStatus {
    fn from(s: OfflineDownloadStatus) -> Self {
        match s {
            OfflineDownloadStatus::Queued => Self::Queued,
            OfflineDownloadStatus::Downloading => Self::Downloading,
            OfflineDownloadStatus::Paused => Self::Paused,
            OfflineDownloadStatus::Completed => Self::Completed,
            OfflineDownloadStatus::Removed => Self::Removed,
            OfflineDownloadStatus::Failed => Self::Failed,
        }
    }
}

impl From<core::DownloadStatus> for OfflineDownloadStatus {
    fn from(s: core::DownloadStatus) -> Self {
        match s {
            core::DownloadStatus::Queued => Self::Queued,
            core::DownloadStatus::Downloading => Self::Downloading,
            core::DownloadStatus::Paused => Self::Paused,
            core::DownloadStatus::Completed => Self::Completed,
            core::DownloadStatus::Removed => Self::Removed,
            core::DownloadStatus::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OfflineUpdateOutcome {
    Updated,
    RemovedOrMissing,
}

impl From<core::UpdateOutcome> for OfflineUpdateOutcome {
    fn from(o: core::UpdateOutcome) -> Self {
        match o {
            core::UpdateOutcome::Updated => Self::Updated,
            core::UpdateOutcome::RemovedOrMissing => Self::RemovedOrMissing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OfflineNetworkKind {
    Unknown,
    Wifi,
    Ethernet,
    Cellular,
    Offline,
}

impl From<OfflineNetworkKind> for aun_player::NetworkKind {
    fn from(k: OfflineNetworkKind) -> Self {
        match k {
            OfflineNetworkKind::Unknown => Self::Unknown,
            OfflineNetworkKind::Wifi => Self::Wifi,
            OfflineNetworkKind::Ethernet => Self::Ethernet,
            OfflineNetworkKind::Cellular => Self::Cellular,
            OfflineNetworkKind::Offline => Self::Offline,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum OfflineTransferDecision {
    Allow,
    WaitForWifi,
    Offline,
    StorageBudgetExceeded,
}

impl From<core::TransferDecision> for OfflineTransferDecision {
    fn from(d: core::TransferDecision) -> Self {
        match d {
            core::TransferDecision::Allow => Self::Allow,
            core::TransferDecision::WaitForWifi => Self::WaitForWifi,
            core::TransferDecision::Offline => Self::Offline,
            core::TransferDecision::StorageBudgetExceeded => Self::StorageBudgetExceeded,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineDownloadItem {
    pub video_id: String,
    pub user_id: String,
    pub rung: Option<String>,
    pub title: String,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub thumbnail_path: Option<String>,
    pub duration_seconds: u64,
    pub local_path: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub status: OfflineDownloadStatus,
    pub removed_reason: Option<String>,
    pub created_at_ms: i64,
    pub last_accessed_at_ms: i64,
}

impl From<OfflineDownloadItem> for core::DownloadRecord {
    fn from(r: OfflineDownloadItem) -> Self {
        Self {
            video_id: r.video_id,
            user_id: r.user_id,
            rung: r.rung,
            title: r.title,
            channel_id: r.channel_id,
            channel_name: r.channel_name,
            thumbnail_path: r.thumbnail_path,
            duration_seconds: r.duration_seconds,
            local_path: r.local_path,
            bytes_downloaded: r.bytes_downloaded,
            total_bytes: r.total_bytes,
            status: r.status.into(),
            removed_reason: r.removed_reason,
            created_at_ms: r.created_at_ms,
            last_accessed_at_ms: r.last_accessed_at_ms,
            extra: BTreeMap::new(),
        }
    }
}

impl From<core::DownloadRecord> for OfflineDownloadItem {
    fn from(r: core::DownloadRecord) -> Self {
        Self {
            video_id: r.video_id,
            user_id: r.user_id,
            rung: r.rung,
            title: r.title,
            channel_id: r.channel_id,
            channel_name: r.channel_name,
            thumbnail_path: r.thumbnail_path,
            duration_seconds: r.duration_seconds,
            local_path: r.local_path,
            bytes_downloaded: r.bytes_downloaded,
            total_bytes: r.total_bytes,
            status: r.status.into(),
            removed_reason: r.removed_reason,
            created_at_ms: r.created_at_ms,
            last_accessed_at_ms: r.last_accessed_at_ms,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineDownloadKey {
    pub user_id: String,
    pub video_id: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineAvailabilityResult {
    pub video_id: String,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineRevocationEntry {
    pub seq: u64,
    pub video_id: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineDownloadSettings {
    pub wifi_only: bool,
    pub max_storage_bytes: Option<u64>,
    pub min_free_space_bytes: u64,
    pub auto_download_enabled: bool,
    pub auto_download_charging_only: bool,
}

impl From<OfflineDownloadSettings> for core::DownloadSettings {
    fn from(s: OfflineDownloadSettings) -> Self {
        Self {
            wifi_only: s.wifi_only,
            max_storage_bytes: s.max_storage_bytes,
            min_free_space_bytes: s.min_free_space_bytes,
            auto_download_enabled: s.auto_download_enabled,
            auto_download_charging_only: s.auto_download_charging_only,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineAutoDownloadRule {
    pub source_id: String,
    pub max_episodes: u32,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct OfflineEpisodeCandidate {
    pub video_id: String,
    pub source_id: String,
    pub published_at_ms: i64,
    pub allow_downloads: bool,
    pub estimated_bytes: Option<u64>,
}

#[uniffi::export]
pub fn offline_default_download_settings() -> OfflineDownloadSettings {
    let d = core::DownloadSettings::default();
    OfflineDownloadSettings {
        wifi_only: d.wifi_only,
        max_storage_bytes: d.max_storage_bytes,
        min_free_space_bytes: d.min_free_space_bytes,
        auto_download_enabled: d.auto_download_enabled,
        auto_download_charging_only: d.auto_download_charging_only,
    }
}

#[uniffi::export]
pub fn offline_can_start_download(
    settings: OfflineDownloadSettings,
    network: OfflineNetworkKind,
    used_bytes: u64,
    item_bytes: u64,
    available_disk_bytes: Option<u64>,
) -> OfflineTransferDecision {
    core::can_start_download(
        &settings.into(),
        network.into(),
        used_bytes,
        item_bytes,
        available_disk_bytes,
    )
    .into()
}

#[uniffi::export]
#[allow(clippy::too_many_arguments)]
pub fn offline_select_auto_downloads(
    settings: OfflineDownloadSettings,
    network: OfflineNetworkKind,
    is_charging: bool,
    used_bytes: u64,
    available_disk_bytes: Option<u64>,
    rules: Vec<OfflineAutoDownloadRule>,
    candidates: Vec<OfflineEpisodeCandidate>,
    existing_video_ids: Vec<String>,
    dismissed_video_ids: Vec<String>,
) -> Vec<String> {
    let core_settings: core::DownloadSettings = settings.into();
    let core_rules: Vec<core::AutoDownloadRule> = rules
        .into_iter()
        .map(|r| core::AutoDownloadRule {
            source_id: r.source_id,
            max_episodes: r.max_episodes as usize,
        })
        .collect();
    let core_candidates: Vec<core::EpisodeCandidate> = candidates
        .into_iter()
        .map(|c| core::EpisodeCandidate {
            video_id: c.video_id,
            source_id: c.source_id,
            published_at_ms: c.published_at_ms,
            allow_downloads: c.allow_downloads,
            estimated_bytes: c.estimated_bytes,
        })
        .collect();
    let existing: HashSet<String> = existing_video_ids.into_iter().collect();
    let dismissed: HashSet<String> = dismissed_video_ids.into_iter().collect();
    core::select_auto_downloads(
        &core_settings,
        network.into(),
        is_charging,
        used_bytes,
        available_disk_bytes,
        &core_rules,
        &core_candidates,
        &existing,
        &dismissed,
    )
}

/// Thread-safe handle to the persistent plain-file offline download manifest keyed by `(user_id, video_id)`.
#[derive(Debug, uniffi::Object)]
pub struct OfflineDownloadStore {
    inner: Mutex<core::DownloadStore>,
}

#[uniffi::export]
impl OfflineDownloadStore {
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>, AunError> {
        let store = core::DownloadStore::open(path).map_err(map_err)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(store),
        }))
    }

    pub fn revocations_cursor(&self) -> Result<u64, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.revocations_cursor())
    }

    pub fn needs_disk_reconciliation(&self) -> Result<bool, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.needs_disk_reconciliation())
    }

    pub fn reconcile_orphaned_paths(&self, disk_paths: Vec<String>) -> Result<Vec<String>, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.reconcile_orphaned_paths(&disk_paths))
    }

    pub fn mark_reconciled(&self) -> Result<(), AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard.mark_reconciled().map_err(map_err)
    }

    pub fn has_local_downloads(&self) -> Result<bool, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.has_local_downloads())
    }

    pub fn is_playable_offline(&self, user_id: String, video_id: String) -> Result<bool, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.is_playable_offline(&user_id, &video_id))
    }

    /// Atomically checks network + storage cap / free-space reserve (excluding this item's own
    /// reservation) and persists the record on `Allow`. Refuses to create over a `Removed` record.
    pub fn try_reserve(
        &self,
        item: OfflineDownloadItem,
        settings: OfflineDownloadSettings,
        network: OfflineNetworkKind,
        available_disk_bytes: Option<u64>,
    ) -> Result<OfflineTransferDecision, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard
            .try_reserve(
                item.into(),
                &settings.into(),
                network.into(),
                available_disk_bytes,
            )
            .map(Into::into)
            .map_err(map_err)
    }

    pub fn update_progress(
        &self,
        user_id: String,
        video_id: String,
        bytes_downloaded: u64,
        total_bytes: u64,
        now_ms: i64,
    ) -> Result<OfflineUpdateOutcome, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard
            .update_progress(&user_id, &video_id, bytes_downloaded, total_bytes, now_ms)
            .map(Into::into)
            .map_err(map_err)
    }

    pub fn mark_completed(
        &self,
        user_id: String,
        video_id: String,
        final_bytes: u64,
        now_ms: i64,
    ) -> Result<OfflineUpdateOutcome, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard
            .mark_completed(&user_id, &video_id, final_bytes, now_ms)
            .map(Into::into)
            .map_err(map_err)
    }

    pub fn mark_downloading(
        &self,
        user_id: String,
        video_id: String,
        now_ms: i64,
    ) -> Result<OfflineUpdateOutcome, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard
            .mark_downloading(&user_id, &video_id, now_ms)
            .map(Into::into)
            .map_err(map_err)
    }

    pub fn mark_paused(
        &self,
        user_id: String,
        video_id: String,
        now_ms: i64,
    ) -> Result<OfflineUpdateOutcome, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard
            .mark_paused(&user_id, &video_id, now_ms)
            .map(Into::into)
            .map_err(map_err)
    }

    pub fn mark_failed(
        &self,
        user_id: String,
        video_id: String,
        now_ms: i64,
    ) -> Result<OfflineUpdateOutcome, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard
            .mark_failed(&user_id, &video_id, now_ms)
            .map(Into::into)
            .map_err(map_err)
    }

    pub fn get(&self, user_id: String, video_id: String) -> Result<Option<OfflineDownloadItem>, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.get(&user_id, &video_id).cloned().map(Into::into))
    }

    pub fn list(&self) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.list().into_iter().map(Into::into).collect())
    }

    pub fn list_for_user(&self, user_id: String) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard
            .list_for_user(&user_id)
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn pending_deletions(&self) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.pending_deletions().into_iter().map(Into::into).collect())
    }

    pub fn active_count_for_user(&self, user_id: String) -> Result<u32, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.active_count_for_user(&user_id) as u32)
    }

    pub fn remove(&self, user_id: String, video_id: String) -> Result<Option<OfflineDownloadItem>, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard
            .remove(&user_id, &video_id)
            .map_err(map_err)?
            .map(Into::into))
    }

    pub fn clear_for_user(&self, user_id: String) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard
            .clear_for_user(&user_id)
            .map_err(map_err)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn confirm_removed(&self, user_id: String, video_id: String) -> Result<bool, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard.confirm_removed(&user_id, &video_id).map_err(map_err)
    }

    pub fn confirm_removed_batch(&self, keys: Vec<OfflineDownloadKey>) -> Result<u32, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        let pairs: Vec<(String, String)> = keys.into_iter().map(|k| (k.user_id, k.video_id)).collect();
        Ok(guard.confirm_removed_batch(&pairs).map_err(map_err)? as u32)
    }

    pub fn apply_availability_sync(
        &self,
        results: Vec<OfflineAvailabilityResult>,
    ) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        let core_results: Vec<core::AvailabilitySyncResult> = results
            .into_iter()
            .map(|r| core::AvailabilitySyncResult {
                video_id: r.video_id,
                available: r.available,
                reason: r.reason,
            })
            .collect();
        Ok(guard
            .apply_availability_sync(&core_results)
            .map_err(map_err)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn apply_revocations(
        &self,
        entries: Vec<OfflineRevocationEntry>,
    ) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        let core_entries: Vec<core::RevocationEntry> = entries
            .into_iter()
            .map(|e| core::RevocationEntry {
                seq: e.seq,
                video_id: e.video_id,
            })
            .collect();
        Ok(guard
            .apply_revocations(&core_entries)
            .map_err(map_err)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn commit_revocations_cursor(&self, new_seq: u64) -> Result<(), AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        guard.commit_revocations_cursor(new_seq).map_err(map_err)
    }

    pub fn total_bytes(&self) -> Result<u64, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard.total_bytes())
    }

    pub fn purge_inactive(&self) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard
            .purge_inactive()
            .map_err(map_err)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn evict_to_budget(&self, max_bytes: u64) -> Result<Vec<OfflineDownloadItem>, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Storage {
            detail: "download store lock poisoned".into(),
        })?;
        Ok(guard
            .evict_to_budget(max_bytes)
            .map_err(map_err)?
            .into_iter()
            .map(Into::into)
            .collect())
    }
}
