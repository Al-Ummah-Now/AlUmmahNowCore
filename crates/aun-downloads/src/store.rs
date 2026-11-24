//! Persistent local manifest of plain progressive `video.mp4` downloads (Epic 14, E14-D1/D5/D8).
//!
//! - Exclusive OS lock (`StoreLock` on `<path>.lock`) prevents two open handles from overwriting
//!   each other's revocations or downloads.
//! - Records are keyed by `(user_id, video_id)` so multiple accounts on one device never clobber
//!   or orphan each other's local media files.
//! - Every `local_path` and `thumbnail_path` is stored strictly as a clean relative path under the
//!   app's download root (never absolute paths, `..`, or remote/signed URLs) so iOS container UUID
//!   relocations and `/private/var/...` symlinks never break playback or orphan reconciliation.
//! - No encryption, DRM, quality ladder, audio-only downloads, or license expiry (E14-D1, E14-D5).
//! - Crash-safe two-phase deletion + non-revivable `Removed` records + generation tokens:
//!   - `try_reserve` refuses `Completed` and `Removed` inputs, refuses overwriting `Completed` or
//!     unconfirmed `Removed` records (`Conflict`), preserves stored fields & `extra` on resume,
//!     atomically checks both user storage cap and OS free-space reserve (accounting for space
//!     promised to all other unfinished downloads), and returns a monotonic `reservation_id`.
//!   - Progress/lifecycle mutators (`update_progress`, `mark_downloading`, `mark_paused`,
//!     `mark_completed`, `mark_failed`) require the active `reservation_id` so an older transfer
//!     can never mutate or complete a newer reservation.
//!   - `Completed` is terminal except for explicit removal/revocation/availability-sync; late
//!     `mark_paused`, `mark_failed`, or `update_progress` cannot downgrade a finished download, and
//!     late `update_progress` cannot un-pause a `Paused` or `Failed` download.

use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aun_common::lock::StoreLock;
use aun_player::NetworkKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::policy::{
    DownloadSettings, ExistingAutoDownload, TransferDecision, can_start_download, effective_item_total_bytes,
};
use crate::{DownloadError, Result};

const MANIFEST_VERSION: u32 = 4;

/// Whether a download was explicitly requested by the user or queued by channel auto-download.
/// Auto-eviction only ever evicts [`DownloadOrigin::Auto`] records; tapping Download on an
/// auto-downloaded video upgrades it to [`DownloadOrigin::Manual`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadOrigin {
    #[default]
    Manual,
    Auto,
}

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
    /// The record belongs to a newer `reservation_id`; this older transfer MUST abort without
    /// touching the newer reservation's files.
    StaleReservation,
    /// The record is already `Completed` (or `Paused`/`Failed` for a late progress/completion tick)
    /// so the late event was ignored without mutating state.
    AlreadyTerminal,
}

/// Result of [`DownloadStore::try_reserve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReserveResult {
    pub decision: TransferDecision,
    /// Monotonic generation token for this reservation (`Some(id)` iff `decision == Allow`).
    pub reservation_id: Option<u64>,
}

/// One downloaded (or downloading) plain progressive R2 `video.mp4` file on this device.
/// Never stores signed URLs or absolute paths; paths are relative to the app's download root.
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
    /// Optional relative path under the download root for locally saved artwork.
    pub thumbnail_path: Option<String>,
    pub duration_seconds: u64,
    /// Relative path under the download root to the plain `video.mp4` file (never absolute or a URL).
    pub local_path: String,
    pub bytes_downloaded: u64,
    pub total_bytes: u64,
    pub status: DownloadStatus,
    pub removed_reason: Option<String>,
    pub created_at_ms: i64,
    pub last_accessed_at_ms: i64,
    /// Monotonic reservation generation assigned by `try_reserve`.
    #[serde(default)]
    pub reservation_id: u64,
    /// Whether the user manually requested this download or auto-download queued it.
    #[serde(default)]
    pub origin: DownloadOrigin,
    /// Optional episode publish timestamp (ms since epoch) used for auto-download `max_episodes` ranking.
    #[serde(default)]
    pub published_at_ms: Option<i64>,
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

    fn is_unfinished(&self) -> bool {
        matches!(
            self.status,
            DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Paused
        )
    }

    /// Bytes counted against the local storage cap.
    /// For unfinished unknown-size items (`total_bytes == 0`), preserves at least the 100 MiB
    /// estimate until the real total is known or the download completes.
    pub fn reserved_bytes(&self) -> u64 {
        if self.status == DownloadStatus::Completed {
            self.bytes_downloaded.max(self.total_bytes)
        } else if self.is_unfinished() {
            effective_item_total_bytes(self.total_bytes, self.bytes_downloaded)
        } else {
            self.bytes_downloaded
        }
    }

    /// Additional bytes still promised to be written to disk for this download (`0` once finished,
    /// failed, or removed). Used for OS free-space reserve accounting across concurrent downloads.
    pub fn remaining_bytes(&self) -> u64 {
        if self.is_unfinished() {
            self.reserved_bytes().saturating_sub(self.bytes_downloaded)
        } else {
            0
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

/// Validates and normalizes a relative local file path under the app's download root.
///
/// Rejects empty paths, absolute paths (leading `/` or `\`, Windows drive prefixes `C:...`),
/// remote/signed URLs, query/fragment characters (`?`, `#`), NUL bytes, and `.` / `..` segments.
fn validate_and_normalize_relative_path(path: &str, field: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must be a non-empty relative local file path"
        )));
    }
    if trimmed.contains('\0') {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must not contain NUL bytes"
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
            "{field} must be a relative local file path, never a remote or signed URL"
        )));
    }
    if lower.contains('?') || lower.contains('#') {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must not contain URL query or fragment characters"
        )));
    }
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must be relative to the download root, never an absolute path"
        )));
    }
    // Reject Windows drive-letter prefixes (e.g. "C:foo" or "C:\foo") or scheme prefixes.
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(DownloadError::InvalidInput(format!(
            "{field} must be relative to the download root, never a drive-qualified path"
        )));
    }
    let normalized = trimmed.replace('\\', "/");
    let mut parts = Vec::new();
    for seg in normalized.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(DownloadError::InvalidInput(format!(
                "{field} must not contain empty, '.' or '..' path segments"
            )));
        }
        parts.push(seg);
    }
    Ok(parts.join("/"))
}

static RESERVATION_FLOOR: AtomicU64 = AtomicU64::new(0);

/// Produces a strictly advancing reservation token base seeded from wall-clock microseconds (and a
/// process-wide monotonic floor) so a full manifest quarantine never restarts tokens at 1 and can
/// never collide with an older in-flight background transfer's token.
fn fresh_reservation_base() -> u64 {
    let now_us = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX / 2))
        .unwrap_or(1)
        .max(1);
    loop {
        let prev = RESERVATION_FLOOR.load(Ordering::Relaxed);
        let next = now_us.max(prev.saturating_add(1));
        if RESERVATION_FLOOR
            .compare_exchange(prev, next, Ordering::SeqCst, Ordering::Relaxed)
            .is_ok()
        {
            return next;
        }
    }
}

fn record_reservation_floor(token: u64) {
    RESERVATION_FLOOR.fetch_max(token, Ordering::SeqCst);
}

fn has_uri_scheme(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    lower.contains("://") || lower.starts_with("data:")
}

fn is_absolute_local_path(raw: &str) -> bool {
    raw.starts_with('/')
        || (raw.len() >= 2 && raw.as_bytes()[0].is_ascii_alphabetic() && raw.as_bytes()[1] == b':')
}

/// Normalizes slashes, collapses repeated `//` and `.` segments, and rejects URIs or any path
/// containing `..` segments. Returns `(is_absolute, normalized)` or `None` if rejected/empty.
fn normalize_disk_path_safe(raw: &str) -> Option<(bool, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains('\0') || has_uri_scheme(trimmed) {
        return None;
    }
    let replaced = trimmed.replace('\\', "/");
    if replaced.starts_with("//") {
        // Reject UNC / scheme-relative paths.
        return None;
    }
    let bytes = replaced.as_bytes();
    let (prefix, rest, is_abs) = if let Some(stripped) = replaced.strip_prefix('/') {
        ("/", stripped, true)
    } else if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        // Require drive-absolute (`C:/...`), reject drive-relative (`C:foo`).
        let after = replaced[2..].strip_prefix('/')?;
        (&replaced[..2], after, true)
    } else {
        ("", replaced.as_str(), false)
    };

    let mut segs: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return None;
        }
        segs.push(seg);
    }

    if is_abs {
        if prefix == "/" {
            if segs.is_empty() {
                Some((true, "/".to_string()))
            } else {
                Some((true, format!("/{}", segs.join("/"))))
            }
        } else if segs.is_empty() {
            Some((true, format!("{prefix}/")))
        } else {
            Some((true, format!("{prefix}/{}", segs.join("/"))))
        }
    } else if segs.is_empty() {
        None
    } else {
        Some((false, segs.join("/")))
    }
}

fn strip_private_prefix(path: &str) -> &str {
    path.strip_prefix("/private").unwrap_or(path)
}

fn is_temp_or_manifest_path(rel_path: &str, manifest_file_name: &str) -> bool {
    let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let lower = file_name.to_ascii_lowercase();
    if file_name == manifest_file_name
        || file_name.starts_with(&format!("{manifest_file_name}.corrupt."))
        || file_name == format!("{manifest_file_name}.lock")
        || (file_name.starts_with('.') && lower.ends_with(".tmp"))
    {
        return true;
    }
    lower.ends_with(".tmp") || lower.ends_with(".part") || lower.ends_with(".download")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ManifestFile {
    version: u32,
    #[serde(default)]
    revocations_cursor: u64,
    #[serde(default)]
    next_reservation_id: u64,
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

fn manifest_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("manifest.json")
        .to_string()
}

fn has_quarantined_sibling(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let prefix = format!("{}.corrupt.", manifest_file_name(path));
    let Ok(entries) = std::fs::read_dir(parent) else {
        return false;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            return true;
        }
    }
    false
}

/// Moves a completely corrupt or 0-byte manifest to a unique `<path>.corrupt.<ts>[.n]` sibling via
/// atomic `rename` (0 extra disk blocks, so quarantine still works on a 100% full disk).
fn quarantine_move(path: &Path) -> Result<PathBuf> {
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
            #[cfg(unix)]
            if let Some(parent) = candidate.parent()
                && let Ok(dir) = File::open(parent)
            {
                let _ = dir.sync_all();
            }
            return Ok(candidate);
        }
    }
    Err(DownloadError::Storage(format!(
        "could not allocate unique quarantine filename for {}",
        path.display()
    )))
}

/// Preserves a forensic copy of partially-salvaged manifest bytes at a unique
/// `<path>.corrupt.<ts>[.n]` sibling WITHOUT overwriting `path` first.
///
/// Prefers `std::fs::hard_link` (0 extra disk blocks, works even on a full disk) and falls back to
/// writing + fsyncing a copy only when hard-linking is unsupported; propagates any error so the
/// caller never replaces the original manifest without a forensic copy on disk.
fn preserve_quarantine_copy(path: &Path, raw_bytes: &[u8]) -> Result<PathBuf> {
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
            match std::fs::hard_link(path, &candidate) {
                Ok(()) => {
                    #[cfg(unix)]
                    if let Some(parent) = candidate.parent()
                        && let Ok(dir) = File::open(parent)
                    {
                        let _ = dir.sync_all();
                    }
                    return Ok(candidate);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Unsupported | std::io::ErrorKind::CrossesDevices
                    ) =>
                {
                    let mut file = OpenOptions::new().write(true).create_new(true).open(&candidate)?;
                    let write_res = file.write_all(raw_bytes).and_then(|()| file.sync_all());
                    drop(file);
                    if let Err(we) = write_res {
                        let _ = std::fs::remove_file(&candidate);
                        return Err(DownloadError::Io(we));
                    }
                    #[cfg(unix)]
                    if let Some(parent) = candidate.parent()
                        && let Ok(dir) = File::open(parent)
                    {
                        let _ = dir.sync_all();
                    }
                    return Ok(candidate);
                }
                Err(e) => return Err(DownloadError::Io(e)),
            }
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
    let name = manifest_file_name(path);
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
    let write_res = file.write_all(&payload).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp);
        return Err(DownloadError::Io(e));
    }

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
/// rollback on failed save, relative-only local paths, monotonic reservation tokens).
#[derive(Debug)]
pub struct DownloadStore {
    path: PathBuf,
    _lock: StoreLock,
    revocations_cursor: u64,
    next_reservation_id: u64,
    needs_reconciliation: bool,
    records: BTreeMap<String, DownloadRecord>,
    extra: BTreeMap<String, Value>,
}

struct LoadedManifest {
    revocations_cursor: u64,
    next_reservation_id: u64,
    needs_reconciliation: bool,
    records: BTreeMap<String, DownloadRecord>,
    extra: BTreeMap<String, Value>,
    needs_persist: bool,
}

impl DownloadStore {
    /// Opens (or creates) the manifest at `path`, acquiring an exclusive OS lock on `<path>.lock`.
    ///
    /// - Refuses a second concurrent open with [`DownloadError::Conflict`].
    /// - Refuses manifests written by a newer schema version (`version > 4`) with
    ///   [`DownloadError::Storage`] without quarantining or overwriting them.
    /// - Copies corrupt JSON, 0-byte manifests, or manifests with unparseable records to a unique
    ///   `<path>.corrupt.<ts>[.n]` sibling BEFORE atomically replacing `path` with a manifest
    ///   carrying `needs_reconciliation: true`, so a disk-full failure or crash can never lose the
    ///   reconcile flag or stop revocation polling.
    /// - Also treats a missing manifest alongside existing `.corrupt.*` siblings as needing
    ///   reconciliation.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let lock = StoreLock::acquire(lock_path(&path)).map_err(|e| match e {
            aun_common::Error::Conflict(msg) => DownloadError::Conflict(msg),
            other => DownloadError::Storage(other.to_string()),
        })?;

        let loaded = match std::fs::read(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => LoadedManifest {
                revocations_cursor: 0,
                next_reservation_id: fresh_reservation_base(),
                needs_reconciliation: has_quarantined_sibling(&path),
                records: BTreeMap::new(),
                extra: BTreeMap::new(),
                needs_persist: false,
            },
            Err(e) => return Err(DownloadError::Io(e)),
            Ok(bytes) if bytes.is_empty() => {
                quarantine_move(&path)?;
                LoadedManifest {
                    revocations_cursor: 0,
                    next_reservation_id: fresh_reservation_base(),
                    needs_reconciliation: true,
                    records: BTreeMap::new(),
                    extra: BTreeMap::new(),
                    needs_persist: true,
                }
            }
            Ok(bytes) => Self::parse_or_salvage(&path, &bytes)?,
        };

        let max_record_res_id = loaded
            .records
            .values()
            .map(|r| r.reservation_id)
            .max()
            .unwrap_or(0);
        let initial_res_id = loaded.next_reservation_id.max(max_record_res_id);
        record_reservation_floor(initial_res_id);
        let store = Self {
            path,
            _lock: lock,
            revocations_cursor: loaded.revocations_cursor,
            next_reservation_id: initial_res_id,
            needs_reconciliation: loaded.needs_reconciliation,
            records: loaded.records,
            extra: loaded.extra,
        };
        if loaded.needs_persist {
            // Best-effort save after quarantine/salvage: if disk is currently full, the store
            // still opens with `needs_reconciliation: true` so the app can reconcile and delete
            // orphaned files to free space, and `.corrupt.*` keeps the flag alive across restarts.
            let _ = persist_manifest(&store.path, &store.to_manifest());
        }
        Ok(store)
    }

    fn parse_or_salvage(path: &Path, bytes: &[u8]) -> Result<LoadedManifest> {
        let Ok(Value::Object(mut root)) = serde_json::from_slice::<Value>(bytes) else {
            quarantine_move(path)?;
            return Ok(LoadedManifest {
                revocations_cursor: 0,
                next_reservation_id: fresh_reservation_base(),
                needs_reconciliation: true,
                records: BTreeMap::new(),
                extra: BTreeMap::new(),
                needs_persist: true,
            });
        };

        let version = match root.remove("version") {
            Some(Value::Number(n)) => n.as_u64().and_then(|v| u32::try_from(v).ok()),
            None => Some(0),
            Some(_) => None,
        };
        let Some(version) = version else {
            quarantine_move(path)?;
            return Ok(LoadedManifest {
                revocations_cursor: 0,
                next_reservation_id: fresh_reservation_base(),
                needs_reconciliation: true,
                records: BTreeMap::new(),
                extra: BTreeMap::new(),
                needs_persist: true,
            });
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
        let next_reservation_id = root
            .remove("next_reservation_id")
            .and_then(|v| v.as_u64())
            .filter(|&v| v > 0)
            .unwrap_or_else(fresh_reservation_base);
        let mut needs_reconciliation = root
            .remove("needs_reconciliation")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut records = BTreeMap::new();
        let mut had_corrupt_record = false;
        if let Some(raw_records) = root.remove("records") {
            match raw_records {
                Value::Object(map) => {
                    for (_k, val) in map {
                        match serde_json::from_value::<DownloadRecord>(val) {
                            Ok(rec) => {
                                records.insert(record_key(&rec.user_id, &rec.video_id), rec);
                            }
                            Err(_) => {
                                had_corrupt_record = true;
                                needs_reconciliation = true;
                            }
                        }
                    }
                }
                _ => {
                    quarantine_move(path)?;
                    return Ok(LoadedManifest {
                        revocations_cursor: 0,
                        next_reservation_id: fresh_reservation_base(),
                        needs_reconciliation: true,
                        records: BTreeMap::new(),
                        extra: BTreeMap::new(),
                        needs_persist: true,
                    });
                }
            }
        }

        if had_corrupt_record {
            // Preserve a forensic copy BEFORE replacing the original manifest. Propagate errors so
            // the original file is never overwritten without a preserved forensic copy.
            preserve_quarantine_copy(path, bytes)?;
        }

        let extra: BTreeMap<String, Value> = root.into_iter().collect();
        Ok(LoadedManifest {
            revocations_cursor,
            next_reservation_id,
            needs_reconciliation,
            records,
            extra,
            needs_persist: had_corrupt_record,
        })
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

    /// Given the app's current absolute `download_root` directory and the file paths discovered on
    /// disk (relative to `download_root` OR absolute paths under `download_root`, including
    /// macOS/iOS `/private/var/...` aliases), returns the paths NOT referenced by any live
    /// (non-`Removed`) record so the app can delete orphaned files and then call
    /// [`Self::mark_reconciled`].
    ///
    /// - Rejects relative, empty, URI, or `..`-containing `download_root` values with
    ///   [`DownloadError::InvalidInput`].
    /// - Collapses repeated slashes (`//`) and `.` segments, and skips any disk path that is a URI
    ///   (e.g. `file://...`), contains `..`, is outside `download_root`, or is a temp/manifest/lock
    ///   file, so a live file is never returned as an orphan.
    /// - Never strips `download_root` from stored record paths (which are already relative).
    pub fn reconcile_orphaned_paths(
        &self,
        download_root: &str,
        disk_paths: &[String],
    ) -> Result<Vec<String>> {
        let Some((root_is_abs, raw_root)) = normalize_disk_path_safe(download_root) else {
            return Err(DownloadError::InvalidInput(
                "download_root must be a clean absolute local directory path".into(),
            ));
        };
        if !root_is_abs || !is_absolute_local_path(&raw_root) {
            return Err(DownloadError::InvalidInput(
                "download_root must be an absolute local directory path".into(),
            ));
        }

        let mut root_prefixes: Vec<String> = Vec::new();
        let mut push_prefix = |p: String| {
            let trimmed = p.trim_end_matches('/').to_string();
            if !trimmed.is_empty() && !root_prefixes.contains(&trimmed) {
                root_prefixes.push(trimmed);
            }
        };
        push_prefix(raw_root.clone());
        let stripped = strip_private_prefix(&raw_root);
        if stripped != raw_root {
            push_prefix(stripped.to_string());
        } else if raw_root.starts_with('/') {
            push_prefix(format!("/private{raw_root}"));
        }
        if let Ok(canon) = std::fs::canonicalize(download_root)
            && let Some((true, canon_s)) = normalize_disk_path_safe(&canon.to_string_lossy())
        {
            push_prefix(canon_s.clone());
            let canon_stripped = strip_private_prefix(&canon_s);
            if canon_stripped != canon_s {
                push_prefix(canon_stripped.to_string());
            } else if canon_s.starts_with('/') {
                push_prefix(format!("/private{canon_s}"));
            }
        }

        let mut live: HashSet<String> = HashSet::new();
        for rec in self.records.values() {
            if rec.status != DownloadStatus::Removed {
                if let Some((false, norm_rel)) = normalize_disk_path_safe(&rec.local_path) {
                    live.insert(norm_rel);
                } else {
                    live.insert(rec.local_path.clone());
                }
                if let Some(ref thumb) = rec.thumbnail_path {
                    if let Some((false, norm_rel)) = normalize_disk_path_safe(thumb) {
                        live.insert(norm_rel);
                    } else {
                        live.insert(thumb.clone());
                    }
                }
            }
        }

        let m_name = manifest_file_name(&self.path);
        let mut orphans = Vec::new();
        for original in disk_paths {
            let Some((is_abs, norm)) = normalize_disk_path_safe(original) else {
                // Skip empty paths, URIs (`file://...`), and paths containing `..`.
                continue;
            };
            let rel = if is_abs {
                let mut matched = None;
                for prefix in &root_prefixes {
                    let prefix_with_slash = format!("{prefix}/");
                    if let Some(rest) = norm.strip_prefix(&prefix_with_slash) {
                        matched = Some(rest.to_string());
                        break;
                    }
                }
                let Some(rel) = matched else {
                    // Never recommend deleting an absolute path outside `download_root`.
                    continue;
                };
                rel
            } else {
                norm
            };

            if rel.is_empty() || is_temp_or_manifest_path(&rel, &m_name) {
                continue;
            }
            if !live.contains(&rel) {
                orphans.push(original.clone());
            }
        }
        Ok(orphans)
    }

    /// Clears `needs_disk_reconciliation` after the app has swept its download folder and deleted
    /// any orphaned files. Persists the manifest so future opens do not re-trigger reconciliation
    /// from old `.corrupt.*` diagnostic files.
    pub fn mark_reconciled(&mut self) -> Result<()> {
        if !self.needs_reconciliation && self.path.exists() {
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
            next_reservation_id: self.next_reservation_id,
            needs_reconciliation: self.needs_reconciliation,
            records: self.records.clone(),
            extra: self.extra.clone(),
        }
    }

    /// Atomically checks network, user storage cap, and OS free-space reserve (accounting for space
    /// promised to all other unfinished downloads, excluding this item's own existing reservation
    /// and already-downloaded bytes on resume), then persists the reservation with a fresh
    /// monotonic `reservation_id`.
    ///
    /// - Refuses `DownloadStatus::Completed` and `DownloadStatus::Removed` input (`InvalidInput`).
    /// - Refuses to overwrite an existing `Completed` or unconfirmed `Removed` record (`Conflict`).
    /// - On resume of an existing `Queued` / `Downloading` / `Paused` / `Failed` record, preserves
    ///   its stored `local_path`, `bytes_downloaded`, `created_at_ms`, `thumbnail_path` (unless
    ///   provided), and `extra` fields while issuing a new `reservation_id` that invalidates any
    ///   older in-flight transfer.
    pub fn try_reserve(
        &mut self,
        mut record: DownloadRecord,
        settings: &DownloadSettings,
        network: NetworkKind,
        available_disk_bytes: Option<u64>,
    ) -> Result<ReserveResult> {
        if record.user_id.trim().is_empty() || record.video_id.trim().is_empty() {
            return Err(DownloadError::InvalidInput(
                "user_id and video_id must be non-empty".into(),
            ));
        }
        if !matches!(
            record.status,
            DownloadStatus::Queued | DownloadStatus::Downloading
        ) {
            return Err(DownloadError::InvalidInput(
                "try_reserve only accepts Queued or Downloading status".into(),
            ));
        }
        record.local_path = validate_and_normalize_relative_path(&record.local_path, "local_path")?;
        if let Some(ref thumb) = record.thumbnail_path {
            record.thumbnail_path = Some(validate_and_normalize_relative_path(thumb, "thumbnail_path")?);
        }

        let key = record_key(&record.user_id, &record.video_id);
        let merged = if let Some(existing) = self.records.get(&key) {
            match existing.status {
                DownloadStatus::Removed => {
                    return Err(DownloadError::Conflict(
                        "download is pending removal; delete local files and call confirm_removed first"
                            .into(),
                    ));
                }
                DownloadStatus::Completed => {
                    return Err(DownloadError::Conflict(
                        "download is already Completed; remove it first to re-download".into(),
                    ));
                }
                DownloadStatus::Queued
                | DownloadStatus::Downloading
                | DownloadStatus::Paused
                | DownloadStatus::Failed => {
                    // Resume: preserve stored local_path, progress, created_at_ms, thumbnail_path,
                    // and forward-compatible `extra` fields.
                    let mut resumed = existing.clone();
                    resumed.status = record.status;
                    if record.total_bytes > 0 {
                        resumed.total_bytes = record.total_bytes;
                    }
                    if record.rung.is_some() {
                        resumed.rung = record.rung;
                    }
                    if !record.title.is_empty() {
                        resumed.title = record.title;
                    }
                    if record.channel_id.is_some() {
                        resumed.channel_id = record.channel_id;
                    }
                    if record.channel_name.is_some() {
                        resumed.channel_name = record.channel_name;
                    }
                    if record.thumbnail_path.is_some() {
                        resumed.thumbnail_path = record.thumbnail_path;
                    }
                    if record.duration_seconds > 0 {
                        resumed.duration_seconds = record.duration_seconds;
                    }
                    if record.origin == DownloadOrigin::Manual {
                        resumed.origin = DownloadOrigin::Manual;
                    }
                    if record.published_at_ms.is_some() {
                        resumed.published_at_ms = record.published_at_ms;
                    }
                    resumed.last_accessed_at_ms = record.last_accessed_at_ms;
                    resumed.removed_reason = None;
                    for (k, v) in record.extra {
                        resumed.extra.insert(k, v);
                    }
                    resumed
                }
            }
        } else {
            // Fresh downloads always start at 0 downloaded bytes so callers cannot bypass the
            // storage cap or OS free-space reserve by pre-populating `bytes_downloaded`.
            record.bytes_downloaded = 0;
            record
        };

        // Refuse local_path / thumbnail_path collisions with any OTHER record (including another
        // account's download or a record pending deletion) so removing one never deletes another's file.
        for (other_key, other) in &self.records {
            if other_key == &key {
                continue;
            }
            if other.local_path == merged.local_path
                || other.thumbnail_path.as_deref() == Some(merged.local_path.as_str())
            {
                return Err(DownloadError::Conflict(format!(
                    "local_path '{}' is already in use by another download record",
                    merged.local_path
                )));
            }
            if let Some(ref thumb) = merged.thumbnail_path
                && (other.local_path == *thumb || other.thumbnail_path.as_deref() == Some(thumb.as_str()))
            {
                return Err(DownloadError::Conflict(format!(
                    "thumbnail_path '{thumb}' is already in use by another download record"
                )));
            }
        }

        let used_excluding_self = self
            .records
            .iter()
            .filter(|(k, _)| *k != &key)
            .fold(0u64, |acc, (_, r)| acc.saturating_add(r.reserved_bytes()));
        let pending_remaining_excluding_self = self
            .records
            .iter()
            .filter(|(k, _)| *k != &key)
            .fold(0u64, |acc, (_, r)| acc.saturating_add(r.remaining_bytes()));

        let decision = can_start_download(
            settings,
            network,
            used_excluding_self,
            pending_remaining_excluding_self,
            merged.total_bytes,
            merged.bytes_downloaded,
            available_disk_bytes,
        );
        if decision != TransferDecision::Allow {
            return Ok(ReserveResult {
                decision,
                reservation_id: None,
            });
        }

        let new_res_id = self.next_reservation_id.saturating_add(1);
        let mut to_store = merged;
        to_store.reservation_id = new_res_id;

        let mut draft = self.to_manifest();
        draft.next_reservation_id = new_res_id;
        draft.records.insert(key, to_store);
        persist_manifest(&self.path, &draft)?;
        self.next_reservation_id = new_res_id;
        record_reservation_floor(new_res_id);
        self.records = draft.records;
        Ok(ReserveResult {
            decision: TransferDecision::Allow,
            reservation_id: Some(new_res_id),
        })
    }

    /// Updates transfer progress on an active `Queued` or `Downloading` download matching
    /// `reservation_id`. Never revives a `Removed` record, never mutates a newer reservation,
    /// never downgrades a `Completed` download, and never un-pauses a `Paused` or `Failed` one.
    pub fn update_progress(
        &mut self,
        user_id: &str,
        video_id: &str,
        reservation_id: u64,
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
            Some(r) if r.reservation_id != reservation_id => {
                return Ok(UpdateOutcome::StaleReservation);
            }
            Some(r)
                if matches!(
                    r.status,
                    DownloadStatus::Completed | DownloadStatus::Paused | DownloadStatus::Failed
                ) =>
            {
                return Ok(UpdateOutcome::AlreadyTerminal);
            }
            Some(_) => {}
        }

        let mut draft = self.to_manifest();
        if let Some(rec) = draft.records.get_mut(&key) {
            rec.bytes_downloaded = bytes_downloaded;
            if total_bytes > 0 {
                rec.total_bytes = total_bytes;
            }
            if rec.status == DownloadStatus::Queued {
                rec.status = DownloadStatus::Downloading;
            }
            rec.last_accessed_at_ms = now_ms;
        }
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(UpdateOutcome::Updated)
    }

    /// Marks an active `Queued` or `Downloading` download matching `reservation_id` as `Completed`.
    /// Never revives a `Removed` record, never completes a newer reservation from an old transfer,
    /// and ignores late calls on already-`Completed`, `Paused`, or `Failed` records.
    pub fn mark_completed(
        &mut self,
        user_id: &str,
        video_id: &str,
        reservation_id: u64,
        final_bytes: u64,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        let key = record_key(user_id, video_id);
        match self.records.get(&key) {
            None => return Ok(UpdateOutcome::RemovedOrMissing),
            Some(r) if r.status == DownloadStatus::Removed => {
                return Ok(UpdateOutcome::RemovedOrMissing);
            }
            Some(r) if r.reservation_id != reservation_id => {
                return Ok(UpdateOutcome::StaleReservation);
            }
            Some(r)
                if matches!(
                    r.status,
                    DownloadStatus::Completed | DownloadStatus::Paused | DownloadStatus::Failed
                ) =>
            {
                return Ok(UpdateOutcome::AlreadyTerminal);
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

    /// Transitions an active non-`Completed` download matching `reservation_id` to `Paused`.
    /// A late pause on a `Completed` download returns `AlreadyTerminal` and leaves it playable.
    pub fn mark_paused(
        &mut self,
        user_id: &str,
        video_id: &str,
        reservation_id: u64,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        self.transition_unfinished(user_id, video_id, reservation_id, DownloadStatus::Paused, now_ms)
    }

    /// Transitions an active non-`Completed` download matching `reservation_id` to `Failed`.
    /// A late failure on a `Completed` download returns `AlreadyTerminal` so `purge_inactive` can
    /// never delete a finished download.
    pub fn mark_failed(
        &mut self,
        user_id: &str,
        video_id: &str,
        reservation_id: u64,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        self.transition_unfinished(user_id, video_id, reservation_id, DownloadStatus::Failed, now_ms)
    }

    /// Transitions an active non-`Completed` download matching `reservation_id` to `Downloading`.
    pub fn mark_downloading(
        &mut self,
        user_id: &str,
        video_id: &str,
        reservation_id: u64,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        self.transition_unfinished(
            user_id,
            video_id,
            reservation_id,
            DownloadStatus::Downloading,
            now_ms,
        )
    }

    /// Attaches or updates the local relative `thumbnail_path` on an existing non-`Removed` record
    /// after artwork finishes downloading, without needing another `try_reserve`.
    pub fn set_thumbnail_path(
        &mut self,
        user_id: &str,
        video_id: &str,
        thumbnail_path: &str,
    ) -> Result<UpdateOutcome> {
        let normalized = validate_and_normalize_relative_path(thumbnail_path, "thumbnail_path")?;
        let key = record_key(user_id, video_id);
        match self.records.get(&key) {
            None => return Ok(UpdateOutcome::RemovedOrMissing),
            Some(r) if r.status == DownloadStatus::Removed => {
                return Ok(UpdateOutcome::RemovedOrMissing);
            }
            Some(_) => {}
        }
        for (other_key, other) in &self.records {
            if other_key != &key
                && (other.local_path == normalized
                    || other.thumbnail_path.as_deref() == Some(normalized.as_str()))
            {
                return Err(DownloadError::Conflict(format!(
                    "thumbnail_path '{normalized}' is already in use by another download record"
                )));
            }
        }
        let mut draft = self.to_manifest();
        if let Some(rec) = draft.records.get_mut(&key) {
            rec.thumbnail_path = Some(normalized);
        }
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(UpdateOutcome::Updated)
    }

    /// Promotes an existing non-`Removed` record (e.g. an already-completed `Auto` download) to
    /// [`DownloadOrigin::Manual`] so per-channel `max_episodes` auto-eviction will never remove it.
    pub fn mark_manual(&mut self, user_id: &str, video_id: &str) -> Result<UpdateOutcome> {
        let key = record_key(user_id, video_id);
        match self.records.get(&key) {
            None => return Ok(UpdateOutcome::RemovedOrMissing),
            Some(r) if r.status == DownloadStatus::Removed => {
                return Ok(UpdateOutcome::RemovedOrMissing);
            }
            Some(r) if r.origin == DownloadOrigin::Manual => return Ok(UpdateOutcome::Updated),
            Some(_) => {}
        }
        let mut draft = self.to_manifest();
        if let Some(rec) = draft.records.get_mut(&key) {
            rec.origin = DownloadOrigin::Manual;
        }
        persist_manifest(&self.path, &draft)?;
        self.records = draft.records;
        Ok(UpdateOutcome::Updated)
    }

    /// Returns the store's active `Auto`-origin downloads for `user_id` formatted for
    /// [`crate::select_auto_downloads`] (so apps do not have to maintain a separate auto-download list).
    pub fn existing_auto_downloads_for_user(&self, user_id: &str) -> Vec<ExistingAutoDownload> {
        self.records
            .values()
            .filter(|r| {
                r.user_id == user_id
                    && r.origin == DownloadOrigin::Auto
                    && r.is_active()
                    && r.channel_id.is_some()
            })
            .map(|r| ExistingAutoDownload {
                video_id: r.video_id.clone(),
                source_id: r.channel_id.clone().unwrap_or_default(),
                published_at_ms: r.published_at_ms.unwrap_or(r.created_at_ms),
                size_bytes: r.reserved_bytes(),
            })
            .collect()
    }

    fn transition_unfinished(
        &mut self,
        user_id: &str,
        video_id: &str,
        reservation_id: u64,
        new_status: DownloadStatus,
        now_ms: i64,
    ) -> Result<UpdateOutcome> {
        let key = record_key(user_id, video_id);
        match self.records.get(&key) {
            None => return Ok(UpdateOutcome::RemovedOrMissing),
            Some(r) if r.status == DownloadStatus::Removed => {
                return Ok(UpdateOutcome::RemovedOrMissing);
            }
            Some(r) if r.reservation_id != reservation_id => {
                return Ok(UpdateOutcome::StaleReservation);
            }
            Some(r) if !matches!(r.status, DownloadStatus::Queued | DownloadStatus::Downloading) => {
                // Completed, Paused, and Failed are terminal for this transfer; resuming must go
                // through `try_reserve` (with a fresh budget check and new reservation_id).
                return Ok(UpdateOutcome::AlreadyTerminal);
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

    /// Additional bytes still promised to be written by all unfinished downloads (saturating sum).
    pub fn pending_remaining_bytes(&self) -> u64 {
        self.records
            .values()
            .fold(0u64, |acc, r| acc.saturating_add(r.remaining_bytes()))
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
