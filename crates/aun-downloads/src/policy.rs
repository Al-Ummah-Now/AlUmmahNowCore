//! Download scheduling policies: Wi-Fi-only, user-configurable storage cap + free-space reserve,
//! and auto-download episode selection with Wi-Fi and charging guards (Epic 14, E14-D1/D2/D5).

use std::collections::HashSet;

use aun_player::NetworkKind;
use serde::{Deserialize, Serialize};

/// Default free-space reserve kept available on the device when OS free space is reported (256 MiB).
pub const DEFAULT_MIN_FREE_SPACE_BYTES: u64 = 256 * 1024 * 1024;

/// Conservative reservation used when the server omits `media.bytes` (100 MiB) so unknown-size
/// downloads never reserve zero bytes against a user storage cap or free-space reserve.
pub const DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES: u64 = 100 * 1024 * 1024;

/// Returns `item_bytes` when known (> 0), or [`DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES`] when unknown.
pub fn effective_item_bytes(item_bytes: u64) -> u64 {
    if item_bytes == 0 {
        DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES
    } else {
        item_bytes
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

/// Decides whether a download of `item_bytes` may start right now.
///
/// Callers should prefer [`crate::DownloadStore::try_reserve`] so budget checks exclude the item's
/// own reservation and execute atomically under the store lock.
pub fn can_start_download(
    settings: &DownloadSettings,
    network: NetworkKind,
    used_bytes: u64,
    item_bytes: u64,
    available_disk_bytes: Option<u64>,
) -> TransferDecision {
    match network {
        NetworkKind::Offline => return TransferDecision::Offline,
        NetworkKind::Cellular | NetworkKind::Unknown if settings.wifi_only => {
            return TransferDecision::WaitForWifi;
        }
        NetworkKind::Wifi | NetworkKind::Ethernet | NetworkKind::Cellular | NetworkKind::Unknown => {}
    }
    let effective = effective_item_bytes(item_bytes);
    if let Some(limit) = settings.max_storage_bytes
        && used_bytes.saturating_add(effective) > limit
    {
        return TransferDecision::StorageBudgetExceeded;
    }
    if let Some(avail) = available_disk_bytes
        && avail < effective.saturating_add(settings.min_free_space_bytes)
    {
        return TransferDecision::StorageBudgetExceeded;
    }
    TransferDecision::Allow
}

/// Auto-download rule for a subscribed channel or playlist (downloads the single progressive R2
/// `video.mp4` file per E14-D5). `max_episodes` is the total cap of newest episodes kept per
/// channel, never a per-poll batch size.
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

/// Picks new episode video IDs to queue for auto-download while enforcing `auto_download_enabled`,
/// Wi-Fi-only, charging-only, local storage budget / free-space reserve, per-channel total
/// `max_episodes` caps, creator opt-out, existing downloads, and user-dismissed/deleted episodes.
#[allow(clippy::too_many_arguments)]
pub fn select_auto_downloads(
    settings: &DownloadSettings,
    network: NetworkKind,
    is_charging: bool,
    used_bytes: u64,
    available_disk_bytes: Option<u64>,
    rules: &[AutoDownloadRule],
    candidates: &[EpisodeCandidate],
    existing_video_ids: &HashSet<String>,
    dismissed_video_ids: &HashSet<String>,
) -> Vec<String> {
    if !settings.auto_download_enabled {
        return Vec::new();
    }
    match network {
        NetworkKind::Offline => return Vec::new(),
        NetworkKind::Cellular | NetworkKind::Unknown if settings.wifi_only => return Vec::new(),
        NetworkKind::Wifi | NetworkKind::Ethernet | NetworkKind::Cellular | NetworkKind::Unknown => {}
    }
    if settings.auto_download_charging_only && !is_charging {
        return Vec::new();
    }

    let mut chosen = Vec::new();
    let mut picked_this_pass: HashSet<String> = HashSet::new();
    let mut running_used = used_bytes;
    let mut running_avail = available_disk_bytes;

    for rule in rules {
        if rule.max_episodes == 0 {
            continue;
        }
        let mut matching: Vec<&EpisodeCandidate> = candidates
            .iter()
            .filter(|c| c.source_id == rule.source_id && c.allow_downloads)
            .collect();
        matching.sort_by(|a, b| {
            b.published_at_ms
                .cmp(&a.published_at_ms)
                .then_with(|| a.video_id.cmp(&b.video_id))
        });

        // `max_episodes` caps the newest N episodes per channel in total, not per poll pass.
        for ep in matching.into_iter().take(rule.max_episodes) {
            if existing_video_ids.contains(&ep.video_id)
                || dismissed_video_ids.contains(&ep.video_id)
                || picked_this_pass.contains(&ep.video_id)
            {
                continue;
            }
            let item_bytes = effective_item_bytes(ep.estimated_bytes.unwrap_or(0));
            if can_start_download(settings, network, running_used, item_bytes, running_avail)
                != TransferDecision::Allow
            {
                continue;
            }
            running_used = running_used.saturating_add(item_bytes);
            if let Some(ref mut avail) = running_avail {
                *avail = avail.saturating_sub(item_bytes);
            }
            picked_this_pass.insert(ep.video_id.clone());
            chosen.push(ep.video_id.clone());
        }
    }
    chosen
}
