//! Persistent local manifest of plain progressive `video.mp4` downloads (Epic 14, E14-D1/D5/D8).
//!
//! - Exclusive OS lock (`StoreLock` on `<path>.lock`) prevents two open handles from overwriting
//!   each other's revocations or downloads.
//! - Records are keyed by `(user_id, video_id)` so multiple accounts on one device never clobber
//!   or orphan each other's local media files.
//! - Never persists signed URLs (neither media URLs nor signed thumbnail URLs); artwork is stored
//!   as a local file path in `thumbnail_path`.
//! - No encryption, DRM, quality ladder, audio-only downloads, or license expiry (E14-D1, E14-D5).
//! - Crash-safe two-phase deletion + non-revivable `Removed` records:
//!   - `remove`, `clear_for_user`, `apply_availability_sync`, `apply_revocations`, `purge_inactive`,
//!     and `evict_to_budget` mark records `Removed` and persist first (fsync + atomic rename, with
//!     full rollback on failed save).
//!   - Progress/completion updates (`update_progress`, `mark_downloading`, `mark_paused`,
//!     `mark_completed`, `mark_failed`) ONLY mutate an existing non-`Removed` record and return
//!     [`UpdateOutcome::RemovedOrMissing`] otherwise so a late in-flight transfer can NEVER revive a
//!     revoked or deleted download (even after `confirm_removed` and `commit_revocations_cursor`).
//!   - `try_reserve` refuses to create over an unconfirmed `Removed` record and atomically checks
//!     network + storage budget (excluding the item's own reservation so queued/paused resumes never
//!     double-count).

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aun_common::lock::StoreLock;
use aun_player::NetworkKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::policy::{DownloadSettings, TransferDecision, can_start_download, effective_item_bytes};
use crate::{DownloadError, Result};

const MANIFEST_VERSION: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Paused,
    Completed,
    Removed,
    Failed,
}

/// Result of a progress or lifecycle update on an existing download transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOutcome {
    /// The active record was updated and persisted.
    Updated,
    /// The record was marked `Removed` (revoked/deleted/signed-out) or already confirmed removed.
    /// The caller MUST cancel the transfer immediately and delete any local temp/media file.
    RemovedOrMissing,
}

/// One downloaded (or downloading) plain progressive R2 `video.mp4` file on this device.
/// Never stores signed URLs (neither media URLs nor signed thumbnail URLs); artwork is stored as
/// a local file path in `thumbnail_path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub video_id: String,
    pub user_id: String,
    /// Server-reported progressive `video.mp4` rung (e.g. `"480p"` or `"360p"`), informational only.
    #[serde(default)]
    pub rung: Option<String>,
    pub title: String,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub thumbnail_path: Option<String>,
    pub duration_seconds: u64,
    /// Relative or absolute local path to the plain `video.mp4` file (never empty or a URL).
    pub local_path: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub status: DownloadStatus,
    pub removed_reason: Option<String>,
    pub created_at_ms: i64,
    pub last_accessed_at_ms: i64,
    /// Forward-compatible passthrough for unknown fields written by compatible builds.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl DownloadRecord {
    /// True if the plain local media file finished downloading and has not been marked removed.
    pub fn is_playable_offline(&self) -> bool {
        self.status == DownloadStatus::Completed
    }

    fn is_active(&self) -> bool {
        matches!(
            self.status,
            DownloadStatus::Queued
                | DownloadStatus::Downloading
                | DownloadStatus::Paused
                | DownloadStatus::Completed
        )
    }

    /// Bytes counted against the local storage budget: active/queued downloads reserve
    /// `max(bytes_downloaded, effective_item_bytes(total_bytes))` so concurrent starts and
    /// unknown-size items cannot overshoot the cap.
    pub fn reserved_bytes(&self) -> u64 {
        if self.is_active() {
            let known = self.bytes_downloaded.max(self.total_bytes);
            if known == 0 {
                effective_item_bytes(0)
            } else {
                known
            }
        } else {
            self.bytes_downloaded
        }
    }
}

/// Server answer for one item in `POST /api/v1/downloads/sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilitySyncResult {
    pub video_id: String,
    pub available: bool,
    pub reason: Option<String>,
}

/// One entry from `GET /api/v1/downloads/revocations?since=<seq>` (E14-D8).
/// Carries only the monotonic `seq` and `video_id` — nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationEntry {
    pub seq: u64,
    pub video_id: String,
}

fn record_key(user_id: &str, video_id: &str) -> String {
    format!("{user_id}\u{001f}{video_id}")
}

fn validate_local_only_path(path: &str, field: &str) -> Result<()> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must be a non-empty local file path"
        )));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("//")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("data:")
        || lower.starts_with("file://")
    {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must be a local file path, never a remote or signed URL"
        )));
    }
    if lower.contains('?') {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must not contain URL query parameters"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ManifestFile {
    version: u32,
    #[serde(default)]
    revocations_cursor: u64,
    #[serde(default)]
    needs_reconciliation: bool,
    #[serde(default)]
    records: BTreeMap<String, DownloadRecord>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    extra: BTreeMap<String, Value>,
}

fn lock_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lock", path.display()))
}

fn quarantine_file(path: &Path) -> Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    for attempt in 0u32..1000 {
        let candidate = if attempt == 0 {
            PathBuf::from(format!("{}.corrupt.{ts}", path.display()))
        } else {
            PathBuf::from(format!("{}.corrupt.{ts}.{attempt}", path.display()))
        };
        if !candidate.exists() {
            std::fs::rename(path, &candidate)?;
            return Ok(candidate);
        }
    }
    Err(DownloadError::Storage(format!(
        "could not allocate unique quarantine filename for {}",
        path.display()
    )))
}

fn persist_manifest(path: &Path, manifest: &ManifestFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("manifest.json");
    let parent = path
        .parent()
        .ok_or_else(|| DownloadError::Storage(format!("no parent directory for {}", path.display())))?;
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let payload = serde_json::to_vec_pretty(manifest)
        .map_err(|e| DownloadError::Storage(format!("manifest serialize: {e}")))?;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp)?;
    file.write_all(&payload)?;
    file.sync_all()?;
    drop(file);

    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(DownloadError::Io(e));
    }
    #[cfg(unix)]
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// File-backed manifest of plain offline downloads (exclusive OS lock, fsync + atomic rename,
/// rollback on failed save).
#[derive(Debug)]
pub struct DownloadStore {
    path: PathBuf,
    _lock: StoreLock,
    revocations_cursor: u64,
    needs_reconciliation: bool,
    records: BTreeMap<String, DownloadRecord>,
    extra: BTreeMap<String, Value>,
}

type LoadedManifest = (
    u64,
    bool,
    BTreeMap<String, DownloadRecord>,
    BTreeMap<String, Value>,
);

impl DownloadStore {
    /// Opens (or creates) the manifest at `path`, acquiring an exclusive OS lock on `<path>.lock`.
    ///
    /// - Refuses a second concurrent open with [`DownloadError::Conflict`].
    /// - Refuses manifests written by a newer schema version (`version > 4`) with
    ///   [`DownloadError::Storage`] instead of quarantining or silently overwriting them.
    /// - Quarantines corrupt JSON or 0-byte files to a unique `<path>.corrupt.<ts>` file, sets
    ///   `needs_disk_reconciliation() == true` so the app reconciles files on disk and keeps
    ///   polling revocations, and salvages any individually valid records when top-level JSON is
    ///   intact.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let lock = StoreLock::acquire(lock_path(&path)).map_err(|e| match e {
            aun_common::Error::Conflict(msg) => DownloadError::Conflict(msg),
            other => DownloadError::Storage(other.to_string()),
        })?;

        let (revocations_cursor, needs_reconciliation, records, extra) = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (0, false, BTreeMap::new(), BTreeMap::new())
            }
            Err(e) => return Err(DownloadError::Io(e)),
            Ok(bytes) if bytes.is_empty() => {
                quarantine_file(&path)?;
                (0, true, BTreeMap::new(), BTreeMap::new())
            }
            Ok(bytes) => Self::parse_or_salvage(&path, &bytes)?,
        };

        let store = Self {
            path,
            _lock: lock,
            revocations_cursor,
            needs_reconciliation,
            records,
            extra,
        };
        if store.needs_reconciliation && !store.path.exists() {
            // Persist the needs_reconciliation flag after quarantine so even a crash before
            // the app sweeps disk still remembers to reconcile & keep polling.
            persist_manifest(&store.path, &store.to_manifest())?;
        }
        Ok(store)
    }

    fn parse_or_salvage(path: &Path, bytes: &[u8]) -> Result<LoadedManifest> {
        let Ok(Value::Object(mut root)) = serde_json::from_slice::<Value>(bytes) else {
            quarantine_file(path)?;
            return Ok((0, true, BTreeMap::new(), BTreeMap::new()));
        };

        let version = match root.remove("version") {
            Some(Value::Number(n)) => n.as_u64().and_then(|v| u32::try_from(v).ok()),
            None => Some(0),
            Some(_) => None,
        };
        let Some(version) = version else {
            quarantine_file(path)?;
            return Ok((0, true, BTreeMap::new(), BTreeMap::new()));
        };
        if version > MANIFEST_VERSION {
            return Err(DownloadError::Storage(format!(
                "download manifest version {version} is newer than this build ({MANIFEST_VERSION})"
            )));
        }

        let revocations_cursor = root
            .remove("revocations_cursor")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let mut needs_reconciliation = root
            .remove("needs_reconciliation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut records = BTreeMap::new();
        if let Some(raw_records) = root.remove("records") {
            match raw_records {
                Value::Object(map) => {
                    for (_k, val) in map {
                        match serde_json::from_value::<DownloadRecord>(val) {
                            Ok(rec) => {
                                records.insert(record_key(&rec.user_id, &rec.video_id), rec);
                            }
                            Err(_) => {
                                // Salvage all other valid records instead of discarding the whole
                                // manifest, and mark reconciliation so the app sweeps orphaned files.
                                needs_reconciliation = true;
                            }
                        }
                    }
                }
                _ => {
                    quarantine_file(path)?;
                    return Ok((0, true, BTreeMap::new(), BTreeMap::new()));
                }
            }
        }

        let extra: BTreeMap<String, Value> = root.into_iter().collect();
        Ok((revocations_cursor, needs_reconciliation, records, extra))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Last committed `seq` cursor for `GET /api/v1/downloads/revocations?since=<seq>`.
    pub fn revocations_cursor(&self) -> u64 {
        self.revocations_cursor
    }

    /// True if a prior corrupt/zero-byte manifest was quarantined (or a corrupt record skipped)
    /// and the app has not yet reconciled its download directory on disk.
    pub fn needs_disk_reconciliation(&self) -> bool {
        self.needs_reconciliation
    }

    /// True if this device holds any local download record OR needs disk reconciliation after a
    /// quarantine. Apps use this to decide whether to poll `revocations` and `sync`, independent
    /// of the `offlineDownloads` feature switch.
    pub fn has_local_downloads(&self) -> bool {
        !self.records.is_empty() || self.needs_reconciliation
    }

    /// Given the local file paths found in the app's download directory, returns any paths not
    /// referenced by a visible (non-`Removed`) manifest record so the app can delete orphaned
    /// files (including after a quarantine) and then call [`Self::mark_reconciled`].
    pub fn reconcile_orphaned_paths(&self, disk_paths: &[String]) -> Vec<String> {
        let mut live: HashSet<&str> = HashSet::new();
        for rec in self.records.values() {
            if rec.status != DownloadStatus::Removed {
                live.insert(rec.local_path.as_str());
                if let Some(ref thumb) = rec.thumbnail_path {
                    live.insert(thumb.as_str());
                }
            }
        }
        disk_paths
            .iter()
            .filter(|p| !live.contains(p.as_str()))
            .cloned()
            .collect()
    }

    /// Clears `needs_disk_reconciliation` after the app has swept its download folder and deleted
    /// any orphaned files.
    pub fn mark_reconciled(&mut self) -> Result<()> {
        if !self.needs_reconciliation {
            return Ok(());
        }
        let mut draft = self.to_manifest();
        draft.needs_reconciliation = false;
        persist_manifest(&self.path, &draft)?;
        self.needs_reconciliation = false;
        Ok(())
    }

    fn to_manifest(&self) -> ManifestFile {
        ManifestFile {
            version: MANIFEST_VERSION,
            revocations_cursor: self.revocations_cursor,
            needs_reconciliation: self.needs_reconciliation,
            records: self.records.clone(),
            extra: self.extra.clone(),
        }
    }

    /// Atomically checks network + storage budget (excluding this item's own existing reservation
    /// so starting or resuming a queued/paused item never double-counts itself) and persists the
    /// reservation on `Allow`.
    ///
    /// Refuses `DownloadStatus::Removed` input and refuses to create over an existing unconfirmed
    /// `Removed` record (`DownloadError::Conflict`).
    pub fn try_reserve(
        &mut self,
        record: DownloadRecord,
        settings: &DownloadSettings,
        network: NetworkKind,
        available_disk_bytes: Option<u64>,
    ) -> Result<TransferDecision> {
        if record.user_id.trim().is_empty() || record.video_id.trim().is_empty() {
            return Err(DownloadError::InvalidInput(
                "user_id and video_id must be non-empty".into(),
            ));
        }
        if record.status == DownloadStatus::Removed {
            return Err(DownloadError::InvalidInput(
                "cannot reserve a download with status Removed".into(),
            ));
        }
        validate_local_only_path(&record.local_path, "local_path")?;
        if let Some(ref thumb) = record.thumbnail_path {
            validate_local_only_path(thumb, "thumbnail_path")?;
        }

        let key = record_key(&record.user_id, &record.video_id);
        if self
            .records
            .get(&key)
            .is_some_and(|existing| existing.status == DownloadStatus::Removed)
        {
            return Err(DownloadError::Conflict(
                "download is pending removal; delete local files and call confirm_removed first".into(),
            ));
        }

        let used_excluding_self = self
            .records
            .iter()
            .filter(|(k, _)| *k != &key)
            .fold(0u64, |acc, (_, r)| acc.saturating_add(r.reserved_bytes()));
        let item_bytes = record.reserved_bytes();
        let decision = can_start_download(
            settings,
            network,
            used_excluding_self,
            item_bytes,
            available_disk_bytes,
        );
        if decision != TransferDecision::Allow {
            return Ok(decision);
        }

        let mut draft = self.to_manifest();
        draft.records.insert(key, record);
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(TransferDecision::Allow)
    }

    /// Updates transfer progress on an existing active (non-`Removed`) download.
    /// Returns [`UpdateOutcome::RemovedOrMissing`] and writes nothing if the record was revoked,
    /// removed, or already confirmed deleted — preventing a late transfer write from reviving it.
    pub fn update_progress(
        &mut self,
        user_id: &str,
        video_id: &str,
        bytes_downloaded: u64,
        total_bytes: u64,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        let key = record_key(user_id, video_id);
        match self.records.get(&key) {
            None => return Ok(UpdateOutcome::RemovedOrMissing),
            Some(r) if r.status == DownloadStatus::Removed => {
                return Ok(UpdateOutcome::RemovedOrMissing);
            }
            Some(_) => {}
        }

        let mut draft = self.to_manifest();
        if let Some(rec) = draft.records.get_mut(&key) {
            rec.bytes_downloaded = bytes_downloaded;
            if total_bytes > 0 {
                rec.total_bytes = total_bytes;
            }
            if matches!(
                rec.status,
                DownloadStatus::Queued | DownloadStatus::Paused | DownloadStatus::Failed
            ) {
                rec.status = DownloadStatus::Downloading;
            }
            rec.last_accessed_at_ms = now_ms;
        }
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(UpdateOutcome::Updated)
    }

    /// Marks an existing active (non-`Removed`) download as `Completed`.
    /// Returns [`UpdateOutcome::RemovedOrMissing`] and writes nothing if the record was revoked,
    /// removed, or already confirmed deleted — preventing a late completion from reviving it.
    pub fn mark_completed(
        &mut self,
        user_id: &str,
        video_id: &str,
        final_bytes: u64,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        let key = record_key(user_id, video_id);
        match self.records.get(&key) {
            None => return Ok(UpdateOutcome::RemovedOrMissing),
            Some(r) if r.status == DownloadStatus::Removed => {
                return Ok(UpdateOutcome::RemovedOrMissing);
            }
            Some(_) => {}
        }

        let mut draft = self.to_manifest();
        if let Some(rec) = draft.records.get_mut(&key) {
            rec.status = DownloadStatus::Completed;
            rec.bytes_downloaded = final_bytes;
            rec.total_bytes = rec.total_bytes.max(final_bytes);
            rec.last_accessed_at_ms = now_ms;
        }
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(UpdateOutcome::Updated)
    }

    /// Transitions an existing active (non-`Removed`) download to `Paused`.
    pub fn mark_paused(&mut self, user_id: &str, video_id: &str, now_ms: i64) -> Result<UpdateOutcome> {
        self.transition_non_removed(user_id, video_id, DownloadStatus::Paused, now_ms)
    }

    /// Transitions an existing active (non-`Removed`) download to `Failed`.
    pub fn mark_failed(&mut self, user_id: &str, video_id: &str, now_ms: i64) -> Result<UpdateOutcome> {
        self.transition_non_removed(user_id, video_id, DownloadStatus::Failed, now_ms)
    }

    /// Transitions an existing active (non-`Removed`) download to `Downloading`.
    pub fn mark_downloading(&mut self, user_id: &str, video_id: &str, now_ms: i64) -> Result<UpdateOutcome> {
        self.transition_non_removed(user_id, video_id, DownloadStatus::Downloading, now_ms)
    }

    fn transition_non_removed(
        &mut self,
        user_id: &str,
        video_id: &str,
        new_status: DownloadStatus,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        let key = record_key(user_id, video_id);
        match self.records.get(&key) {
            None => return Ok(UpdateOutcome::RemovedOrMissing),
            Some(r) if r.status == DownloadStatus::Removed => {
                return Ok(UpdateOutcome::RemovedOrMissing);
            }
            Some(_) => {}
        }
        let mut draft = self.to_manifest();
        if let Some(rec) = draft.records.get_mut(&key) {
            rec.status = new_status;
            rec.last_accessed_at_ms = now_ms;
        }
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(UpdateOutcome::Updated)
    }

    pub fn get(&self, user_id: &str, video_id: &str) -> Option<&DownloadRecord> {
        self.records.get(&record_key(user_id, video_id))
    }

    /// Lists all visible (non-`Removed`) downloads across accounts, newest first.
    pub fn list(&self) -> Vec<DownloadRecord> {
        let mut items: Vec<DownloadRecord> = self
            .records
            .values()
            .filter(|r| r.status != DownloadStatus::Removed)
            .cloned()
            .collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.created_at_ms));
        items
    }

    /// Lists visible (non-`Removed`) downloads owned by `user_id`, newest first.
    pub fn list_for_user(&self, user_id: &str) -> Vec<DownloadRecord> {
        let mut items: Vec<DownloadRecord> = self
            .records
            .values()
            .filter(|r| r.user_id == user_id && r.status != DownloadStatus::Removed)
            .cloned()
            .collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.created_at_ms));
        items
    }

    /// Returns all records currently in `DownloadStatus::Removed` awaiting file deletion on disk.
    pub fn pending_deletions(&self) -> Vec<DownloadRecord> {
        let mut items: Vec<DownloadRecord> = self
            .records
            .values()
            .filter(|r| r.status == DownloadStatus::Removed)
            .cloned()
            .collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.created_at_ms));
        items
    }

    /// Count of active (completed or in-progress) downloads for `user_id`.
    pub fn active_count_for_user(&self, user_id: &str) -> usize {
        self.records
            .values()
            .filter(|r| r.user_id == user_id && r.is_active())
            .count()
    }

    /// Phase 1 removal: marks `(user_id, video_id)` as `Removed` and persists atomically (with
    /// rollback on failure). Caller deletes `local_path` and `thumbnail_path`, then calls
    /// [`Self::confirm_removed`].
    pub fn remove(&mut self, user_id: &str, video_id: &str) -> Result<Option<DownloadRecord>> {
        let key = record_key(user_id, video_id);
        let Some(existing) = self.records.get(&key) else {
            return Ok(None);
        };
        if existing.status == DownloadStatus::Removed {
            return Ok(Some(existing.clone()));
        }
        let mut draft = self.to_manifest();
        let Some(rec) = draft.records.get_mut(&key) else {
            return Ok(None);
        };
        rec.status = DownloadStatus::Removed;
        if rec.removed_reason.is_none() {
            rec.removed_reason = Some("user_removed".into());
        }
        let snapshot = rec.clone();
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(Some(snapshot))
    }

    /// Phase 1 sign-out removal: marks every download owned by `user_id` as `Removed` and persists
    /// atomically (with rollback on failure).
    pub fn clear_for_user(&mut self, user_id: &str) -> Result<Vec<DownloadRecord>> {
        let mut draft = self.to_manifest();
        let mut marked = Vec::new();
        let mut changed = false;
        for rec in draft.records.values_mut().filter(|r| r.user_id == user_id) {
            if rec.status != DownloadStatus::Removed {
                rec.status = DownloadStatus::Removed;
                rec.removed_reason = Some("signed_out".into());
                changed = true;
            }
            marked.push(rec.clone());
        }
        if changed {
            persist_manifest(&self.path, &draft)?;
            self.records = draft.records;
        }
        Ok(marked)
    }

    /// Phase 2 removal confirmation: drops a `Removed` record from the manifest after the caller
    /// has deleted its `local_path` and `thumbnail_path` from disk. Refuses to drop active records.
    pub fn confirm_removed(&mut self, user_id: &str, video_id: &str) -> Result<bool> {
        let key = record_key(user_id, video_id);
        let can_drop = self
            .records
            .get(&key)
            .is_some_and(|r| r.status == DownloadStatus::Removed);
        if !can_drop {
            return Ok(false);
        }
        let mut draft = self.to_manifest();
        draft.records.remove(&key);
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(true)
    }

    /// Batch version of [`Self::confirm_removed`], persisting once atomically with rollback on error.
    pub fn confirm_removed_batch(&mut self, keys: &[(String, String)]) -> Result<usize> {
        let mut draft = self.to_manifest();
        let mut dropped = 0;
        for (user_id, video_id) in keys {
            let key = record_key(user_id, video_id);
            if draft
                .records
                .get(&key)
                .is_some_and(|r| r.status == DownloadStatus::Removed)
            {
                draft.records.remove(&key);
                dropped += 1;
            }
        }
        if dropped > 0 {
            persist_manifest(&self.path, &draft)?;
            self.records = draft.records;
        }
        Ok(dropped)
    }

    pub fn is_playable_offline(&self, user_id: &str, video_id: &str) -> bool {
        self.records
            .get(&record_key(user_id, video_id))
            .is_some_and(DownloadRecord::is_playable_offline)
    }

    /// Phase 1 availability sync (`POST /api/v1/downloads/sync`): marks matching records across all
    /// accounts as `Removed` and persists atomically (with rollback on failure).
    pub fn apply_availability_sync(
        &mut self,
        results: &[AvailabilitySyncResult],
    ) -> Result<Vec<DownloadRecord>> {
        let mut draft = self.to_manifest();
        let mut to_delete = Vec::new();
        let mut changed = false;
        for item in results {
            if item.available {
                continue;
            }
            for rec in draft.records.values_mut().filter(|r| r.video_id == item.video_id) {
                if rec.status != DownloadStatus::Removed {
                    rec.status = DownloadStatus::Removed;
                    rec.removed_reason = item.reason.clone().or_else(|| Some("unavailable".into()));
                    changed = true;
                }
                to_delete.push(rec.clone());
            }
        }
        if changed {
            persist_manifest(&self.path, &draft)?;
            self.records = draft.records;
        }
        Ok(to_delete)
    }

    /// Phase 1 urgent/CSAM revocations feed (`GET /api/v1/downloads/revocations?since=<seq>`, E14-D8):
    /// marks every matching local record across all accounts as `Removed` (`removed_reason = "revoked"`)
    /// and persists atomically (with rollback on failure).
    ///
    /// Does **not** advance `revocations_cursor`; the caller must delete the returned files from
    /// disk, call [`Self::confirm_removed_batch`], and only then call [`Self::commit_revocations_cursor`]
    /// with the page's `nextCursor`.
    pub fn apply_revocations(&mut self, entries: &[RevocationEntry]) -> Result<Vec<DownloadRecord>> {
        let mut draft = self.to_manifest();
        let mut to_delete = Vec::new();
        let mut changed = false;
        for entry in entries {
            for rec in draft
                .records
                .values_mut()
                .filter(|r| r.video_id == entry.video_id)
            {
                if rec.status != DownloadStatus::Removed {
                    rec.status = DownloadStatus::Removed;
                    rec.removed_reason = Some("revoked".into());
                    changed = true;
                }
                to_delete.push(rec.clone());
            }
        }
        if changed {
            persist_manifest(&self.path, &draft)?;
            self.records = draft.records;
        }
        Ok(to_delete)
    }

    /// Monotonically advances the persisted revocations feed cursor after all local deletions for
    /// that page have succeeded on disk (with rollback on save failure).
    pub fn commit_revocations_cursor(&mut self, new_seq: u64) -> Result<()> {
        if new_seq <= self.revocations_cursor {
            return Ok(());
        }
        let mut draft = self.to_manifest();
        draft.revocations_cursor = new_seq;
        persist_manifest(&self.path, &draft)?;
        self.revocations_cursor = new_seq;
        Ok(())
    }

    /// Total local bytes reserved/used across all records (saturating sum).
    pub fn total_bytes(&self) -> u64 {
        self.records
            .values()
            .fold(0u64, |acc, r| acc.saturating_add(r.reserved_bytes()))
    }

    /// Phase 1 reclaim: marks `Failed` records as `Removed` (persisting atomically with rollback)
    /// and returns all `Removed` records so the caller can delete their local files and call
    /// [`Self::confirm_removed_batch`]. Never silently evicts active user downloads.
    pub fn purge_inactive(&mut self) -> Result<Vec<DownloadRecord>> {
        let mut draft = self.to_manifest();
        let mut to_delete = Vec::new();
        let mut changed = false;
        for rec in draft.records.values_mut() {
            match rec.status {
                DownloadStatus::Removed => to_delete.push(rec.clone()),
                DownloadStatus::Failed => {
                    rec.status = DownloadStatus::Removed;
                    if rec.removed_reason.is_none() {
                        rec.removed_reason = Some("failed_purged".into());
                    }
                    changed = true;
                    to_delete.push(rec.clone());
                }
                DownloadStatus::Queued
                | DownloadStatus::Downloading
                | DownloadStatus::Paused
                | DownloadStatus::Completed => {}
            }
        }
        if changed {
            persist_manifest(&self.path, &draft)?;
            self.records = draft.records;
        }
        Ok(to_delete)
    }

    /// Phase 1 budget reclaim: marks ONLY inactive records (`Removed` first, then `Failed`) for
    /// deletion until usage would fall to or below `max_bytes`, persists atomically (with rollback),
    /// and returns them for file deletion + [`Self::confirm_removed_batch`].
    /// Never silently evicts active (`Completed`) or in-progress user downloads.
    pub fn evict_to_budget(&mut self, max_bytes: u64) -> Result<Vec<DownloadRecord>> {
        let mut total = self.total_bytes();
        if total <= max_bytes {
            return Ok(Vec::new());
        }
        let mut order: Vec<(u8, i64, String, u64)> = self
            .records
            .iter()
            .filter_map(|(key, r)| {
                let priority = match r.status {
                    DownloadStatus::Removed => 0,
                    DownloadStatus::Failed => 1,
                    DownloadStatus::Queued
                    | DownloadStatus::Downloading
                    | DownloadStatus::Paused
                    | DownloadStatus::Completed => return None,
                };
                Some((priority, r.last_accessed_at_ms, key.clone(), r.reserved_bytes()))
            })
            .collect();
        order.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

        let mut draft = self.to_manifest();
        let mut to_delete = Vec::new();
        let mut changed = false;
        for (_, _, key, bytes) in order {
            if total <= max_bytes {
                break;
            }
            if let Some(rec) = draft.records.get_mut(&key) {
                if rec.status != DownloadStatus::Removed {
                    rec.status = DownloadStatus::Removed;
                    if rec.removed_reason.is_none() {
                        rec.removed_reason = Some("budget_reclaim".into());
                    }
                    changed = true;
                }
                total = total.saturating_sub(bytes);
                to_delete.push(rec.clone());
            }
        }
        if changed {
            persist_manifest(&self.path, &draft)?;
            self.records = draft.records;
        }
        Ok(to_delete)
    }
}
