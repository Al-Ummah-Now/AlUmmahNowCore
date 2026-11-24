//! Plain-file offline downloads manifest, revocations hygiene, storage/Wi-Fi policy, and
//! availability sync (Epic 14).
//!
//! Per owner decisions E14-D1, E14-D2, E14-D5, and E14-D8, downloads are plain progressive R2
//! `video.mp4` files (zero new R2 objects, no quality ladder, no audio-only downloads) with no
//! encryption, DRM, key storage, or license expiry.

pub mod policy;
pub mod store;

pub use policy::{
    AutoDownloadPlan, AutoDownloadRule, DEFAULT_MIN_FREE_SPACE_BYTES, DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES,
    DownloadSettings, EpisodeCandidate, ExistingAutoDownload, TransferDecision, can_start_download,
    effective_item_bytes, effective_item_total_bytes, select_auto_downloads,
};
pub use store::{
    AvailabilitySyncResult, DownloadRecord, DownloadStatus, DownloadStore, ReserveResult, RevocationEntry,
    UpdateOutcome,
};

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("invalid download input: {0}")]
    InvalidInput(String),
    #[error("download store conflict: {0}")]
    Conflict(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DownloadError>;
