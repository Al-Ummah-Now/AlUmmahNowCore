//! Download scheduling policies: Wi-Fi-only, user-configurable storage cap + free-space reserve
//! accounting for unfinished downloads, and per-channel auto-download selection & eviction
//! (Epic 14, E14-D1/D2/D5).

use std::collections::{HashMap, HashSet};

use aun_player::NetworkKind;
use serde::{Deserialize, Serialize};

/// Default free-space reserve kept available on the device when OS free space is reported (256 MiB).
pub const DEFAULT_MIN_FREE_SPACE_BYTES: u64 = 256 * 1024 * 1024;

/// Conservative reservation used when the server omits `media.bytes` (100 MiB) so unknown-size
/// downloads never reserve zero (or 1) bytes against a user storage cap or free-space reserve.
pub const DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES: u64 = 100 * 1024 * 1024;

/// Returns `item_bytes` when known (> 0), or [`DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES`] when unknown.
pub fn effective_item_bytes(item_bytes: u64) -> u64 {
    if item_bytes == 0 {
        DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES
    } else {
        item_bytes
    }
}

/// Returns the total byte reservation for an unfinished download whose known total is `total_bytes`
/// (`0` = unknown) and which has already written `bytes_downloaded` to disk.
///
/// While `total_bytes == 0`, keeps at least [`DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES`] until the real
/// total is learned or the download completes (never collapsing to `bytes_downloaded` on the first
/// progress tick).
pub fn effective_item_total_bytes(total_bytes: u64, bytes_downloaded: u64) -> u64 {
    if total_bytes == 0 {
        bytes_downloaded.max(DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES)
    } else {
        total_bytes.max(bytes_downloaded)
    }
}

/// User preferences for offline downloads on a device.
///
/// Per owner decision E14-D2, there is no default tier or storage cap (`max_storage_bytes: None`);
/// the device keeps a free-space reserve (`min_free_space_bytes`), and users may optionally set a
/// local cap in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    pub wifi_only: bool,
    /// Optional user-configured local disk cap in bytes (`None` = unlimited up to free-space reserve).
    pub max_storage_bytes: Option<u64>,
    /// Free disk space kept in reserve when the app reports `available_disk_bytes`.
    pub min_free_space_bytes: u64,
    pub auto_download_enabled: bool,
    pub auto_download_charging_only: bool,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            wifi_only: true,
            max_storage_bytes: None,
            min_free_space_bytes: DEFAULT_MIN_FREE_SPACE_BYTES,
            auto_download_enabled: false,
            auto_download_charging_only: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDecision {
    Allow,
    WaitForWifi,
    Offline,
    StorageBudgetExceeded,
}

/// Decides whether a download may start or resume right now.
///
/// - `used_bytes`: total bytes reserved by OTHER records (excluding this item on resume).
/// - `pending_remaining_bytes`: remaining bytes still to be written by OTHER unfinished downloads
///   (excluding this item on resume), subtracted from `available_disk_bytes` alongside
///   `min_free_space_bytes` so concurrent queued downloads cannot oversubscribe free disk space.
/// - `item_total_bytes`: known total size of this item (`0` = unknown -> conservative 100 MiB estimate).
/// - `item_downloaded_bytes`: bytes already written to disk for this item (`0` for a fresh download,
///   `> 0` on resume so already-written bytes are not required from free space a second time).
///
/// Callers should prefer [`crate::DownloadStore::try_reserve`] so both checks execute atomically
/// under the store lock.
#[allow(clippy::too_many_arguments)]
pub fn can_start_download(
    settings: &DownloadSettings,
    network: NetworkKind,
    used_bytes: u64,
    pending_remaining_bytes: u64,
    item_total_bytes: u64,
    item_downloaded_bytes: u64,
    available_disk_bytes: Option<u64>,
) -> TransferDecision {
    match network {
        NetworkKind::Offline => return TransferDecision::Offline,
        NetworkKind::Cellular | NetworkKind::Unknown if settings.wifi_only => {
            return TransferDecision::WaitForWifi;
        }
        NetworkKind::Wifi | NetworkKind::Ethernet | NetworkKind::Cellular | NetworkKind::Unknown => {}
    }
    let effective_total = effective_item_total_bytes(item_total_bytes, item_downloaded_bytes);
    if let Some(limit) = settings.max_storage_bytes
        && used_bytes.saturating_add(effective_total) > limit
    {
        return TransferDecision::StorageBudgetExceeded;
    }
    let item_remaining = effective_total.saturating_sub(item_downloaded_bytes);
    if let Some(avail) = available_disk_bytes {
        let required_free = settings
            .min_free_space_bytes
            .saturating_add(pending_remaining_bytes)
            .saturating_add(item_remaining);
        if avail < required_free {
            return TransferDecision::StorageBudgetExceeded;
        }
    }
    TransferDecision::Allow
}

/// Auto-download rule for a subscribed channel or playlist (downloads the single progressive R2
/// `video.mp4` file per E14-D5). `max_episodes` caps the total number of auto-downloaded episodes
/// kept per channel over time (oldest auto-downloads beyond `max_episodes` are returned in
/// [`AutoDownloadPlan::to_evict`]; manual user downloads are never evicted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoDownloadRule {
    pub source_id: String,
    pub max_episodes: usize,
}

/// A candidate episode discovered from a channel or playlist feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeCandidate {
    pub video_id: String,
    pub source_id: String,
    pub published_at_ms: i64,
    pub allow_downloads: bool,
    /// Optional server-reported progressive `video.mp4` byte size (`None` uses the conservative estimate).
    #[serde(default)]
    pub estimated_bytes: Option<u64>,
}

/// An episode currently kept on this device because of auto-download (distinct from manual user
/// downloads, which are never auto-evicted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExistingAutoDownload {
    pub video_id: String,
    pub source_id: String,
    pub published_at_ms: i64,
}

/// Output of [`select_auto_downloads`]: new episodes to queue and older auto-downloaded episodes
/// that fell out of a channel's `max_episodes` window.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoDownloadPlan {
    pub to_download: Vec<String>,
    pub to_evict: Vec<String>,
}

/// Plans auto-downloads and auto-evictions per channel while enforcing `auto_download_enabled`,
/// Wi-Fi-only, charging-only, local storage budget + free-space reserve (accounting for existing
/// unfinished downloads), per-channel total `max_episodes` cap over time, creator opt-out,
/// existing downloads, and user-dismissed/deleted episodes.
#[allow(clippy::too_many_arguments)]
pub fn select_auto_downloads(
    settings: &DownloadSettings,
    network: NetworkKind,
    is_charging: bool,
    used_bytes: u64,
    pending_remaining_bytes: u64,
    available_disk_bytes: Option<u64>,
    rules: &[AutoDownloadRule],
    candidates: &[EpisodeCandidate],
    existing_video_ids: &HashSet<String>,
    existing_auto_downloads: &[ExistingAutoDownload],
    dismissed_video_ids: &HashSet<String>,
) -> AutoDownloadPlan {
    if !settings.auto_download_enabled {
        return AutoDownloadPlan::default();
    }
    match network {
        NetworkKind::Offline => return AutoDownloadPlan::default(),
        NetworkKind::Cellular | NetworkKind::Unknown if settings.wifi_only => {
            return AutoDownloadPlan::default();
        }
        NetworkKind::Wifi | NetworkKind::Ethernet | NetworkKind::Cellular | NetworkKind::Unknown => {}
    }
    if settings.auto_download_charging_only && !is_charging {
        return AutoDownloadPlan::default();
    }

    let mut plan = AutoDownloadPlan::default();
    let mut picked_this_pass: HashSet<String> = HashSet::new();
    let mut evicted_this_pass: HashSet<String> = HashSet::new();
    let mut running_used = used_bytes;
    let mut running_pending = pending_remaining_bytes;

    for rule in rules {
        let mut matching: Vec<&EpisodeCandidate> = candidates
            .iter()
            .filter(|c| c.source_id == rule.source_id && c.allow_downloads)
            .collect();
        matching.sort_by(|a, b| {
            b.published_at_ms
                .cmp(&a.published_at_ms)
                .then_with(|| a.video_id.cmp(&b.video_id))
        });

        let mut kept_auto_for_source: HashMap<String, i64> = existing_auto_downloads
            .iter()
            .filter(|e| e.source_id == rule.source_id)
            .map(|e| (e.video_id.clone(), e.published_at_ms))
            .collect();

        // Pick from the newest `max_episodes` downloadable episodes on this channel.
        for ep in matching.into_iter().take(rule.max_episodes) {
            if existing_video_ids.contains(&ep.video_id)
                || dismissed_video_ids.contains(&ep.video_id)
                || picked_this_pass.contains(&ep.video_id)
            {
                continue;
            }
            let item_bytes = effective_item_bytes(ep.estimated_bytes.unwrap_or(0));
            if can_start_download(
                settings,
                network,
                running_used,
                running_pending,
                item_bytes,
                0,
                available_disk_bytes,
            ) != TransferDecision::Allow
            {
                continue;
            }
            running_used = running_used.saturating_add(item_bytes);
            running_pending = running_pending.saturating_add(item_bytes);
            picked_this_pass.insert(ep.video_id.clone());
            plan.to_download.push(ep.video_id.clone());
            kept_auto_for_source.insert(ep.video_id.clone(), ep.published_at_ms);
        }

        // Enforce `max_episodes` over time: if existing auto-downloads + newly picked exceed the
        // channel cap, evict the oldest EXISTING auto-downloads (never evicting manual downloads).
        if kept_auto_for_source.len() > rule.max_episodes {
            let mut ranked: Vec<(String, i64)> = kept_auto_for_source.into_iter().collect();
            ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (video_id, _) in ranked.into_iter().skip(rule.max_episodes) {
                if !picked_this_pass.contains(&video_id) && evicted_this_pass.insert(video_id.clone()) {
                    plan.to_evict.push(video_id);
                }
            }
        }
    }

    plan
}
