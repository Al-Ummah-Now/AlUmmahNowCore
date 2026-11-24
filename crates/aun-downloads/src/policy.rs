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
    /// Bytes currently reserved by this episode against the user's storage cap (`0` if unknown).
    /// Credited back when a newer episode displaces this one so a channel at its storage cap can
    /// still roll forward to newer episodes.
    #[serde(default)]
    pub size_bytes: u64,
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
/// unfinished downloads and bytes freed by displaced auto-downloads), per-channel total
/// `max_episodes` cap over time (ranking existing auto-downloads and candidates together so
/// episodes that drop out of the feed never cause older videos to thrash), creator opt-out,
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
        // Deduplicate existing auto-downloads for this channel from the store, newest first.
        let mut existing_by_id: HashMap<&str, ExistingAutoDownload> = HashMap::new();
        for e in existing_auto_downloads
            .iter()
            .filter(|e| e.source_id == rule.source_id)
        {
            existing_by_id
                .entry(e.video_id.as_str())
                .and_modify(|prev| {
                    if e.published_at_ms > prev.published_at_ms {
                        *prev = e.clone();
                    }
                })
                .or_insert_with(|| e.clone());
        }
        let mut kept_auto: Vec<ExistingAutoDownload> = existing_by_id.into_values().collect();
        kept_auto.sort_by(|a, b| {
            b.published_at_ms
                .cmp(&a.published_at_ms)
                .then_with(|| a.video_id.cmp(&b.video_id))
        });

        // If the user lowered `max_episodes` below what is already kept, evict the excess oldest
        // auto-downloads first and credit their reserved bytes back to `running_used`.
        if kept_auto.len() > rule.max_episodes {
            for excess in kept_auto.split_off(rule.max_episodes) {
                if evicted_this_pass.insert(excess.video_id.clone()) {
                    running_used = running_used.saturating_sub(excess.size_bytes);
                    plan.to_evict.push(excess.video_id);
                }
            }
        }

        if rule.max_episodes == 0 {
            continue;
        }

        let kept_ids: HashSet<String> = kept_auto.iter().map(|e| e.video_id.clone()).collect();

        // Rank existing auto-downloads AND eligible new candidates TOGETHER newest-first so that
        // kept newer episodes that leave the candidate feed (e.g. creator turned off downloads or
        // fell off page 1) still hold their slots in the `max_episodes` window and never cause
        // older candidates to download and evict in a loop.
        enum RankedEntry<'e, 'c> {
            Existing(&'e ExistingAutoDownload),
            Candidate(&'c EpisodeCandidate),
        }
        impl RankedEntry<'_, '_> {
            fn published_at_ms(&self) -> i64 {
                match self {
                    Self::Existing(e) => e.published_at_ms,
                    Self::Candidate(c) => c.published_at_ms,
                }
            }
            fn video_id(&self) -> &str {
                match self {
                    Self::Existing(e) => &e.video_id,
                    Self::Candidate(c) => &c.video_id,
                }
            }
            fn is_existing(&self) -> bool {
                matches!(self, Self::Existing(_))
            }
        }

        let mut combined: Vec<RankedEntry<'_, '_>> = kept_auto.iter().map(RankedEntry::Existing).collect();
        for c in candidates {
            if c.source_id == rule.source_id
                && c.allow_downloads
                && !existing_video_ids.contains(&c.video_id)
                && !dismissed_video_ids.contains(&c.video_id)
                && !picked_this_pass.contains(&c.video_id)
                && !kept_ids.contains(&c.video_id)
            {
                combined.push(RankedEntry::Candidate(c));
            }
        }
        combined.sort_by(|a, b| {
            b.published_at_ms()
                .cmp(&a.published_at_ms())
                .then_with(|| b.is_existing().cmp(&a.is_existing()))
                .then_with(|| a.video_id().cmp(b.video_id()))
        });

        let top_candidates: Vec<&EpisodeCandidate> = combined
            .into_iter()
            .take(rule.max_episodes)
            .filter_map(|entry| match entry {
                RankedEntry::Candidate(c) => Some(c),
                RankedEntry::Existing(_) => None,
            })
            .collect();

        for ep in top_candidates {
            if picked_this_pass.contains(&ep.video_id) {
                continue;
            }
            let item_bytes = effective_item_bytes(ep.estimated_bytes.unwrap_or(0));

            if kept_auto.len() >= rule.max_episodes {
                // Accepting this newer episode will displace the oldest kept auto-download.
                // Credit the displaced episode's reserved bytes against the user storage cap so a
                // channel at its cap can roll forward to newer episodes.
                let Some(oldest_pos) = kept_auto
                    .iter()
                    .rposition(|e| !picked_this_pass.contains(&e.video_id))
                else {
                    continue;
                };
                let displaced_bytes = kept_auto[oldest_pos].size_bytes;
                let effective_used = running_used.saturating_sub(displaced_bytes);
                if can_start_download(
                    settings,
                    network,
                    effective_used,
                    running_pending,
                    item_bytes,
                    0,
                    available_disk_bytes,
                ) != TransferDecision::Allow
                {
                    continue;
                }
                let victim = kept_auto.remove(oldest_pos);
                if evicted_this_pass.insert(victim.video_id.clone()) {
                    plan.to_evict.push(victim.video_id);
                }
                running_used = effective_used.saturating_add(item_bytes);
                running_pending = running_pending.saturating_add(item_bytes);
                picked_this_pass.insert(ep.video_id.clone());
                plan.to_download.push(ep.video_id.clone());
                kept_auto.push(ExistingAutoDownload {
                    video_id: ep.video_id.clone(),
                    source_id: ep.source_id.clone(),
                    published_at_ms: ep.published_at_ms,
                    size_bytes: item_bytes,
                });
                kept_auto.sort_by(|a, b| {
                    b.published_at_ms
                        .cmp(&a.published_at_ms)
                        .then_with(|| a.video_id.cmp(&b.video_id))
                });
            } else {
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
                kept_auto.push(ExistingAutoDownload {
                    video_id: ep.video_id.clone(),
                    source_id: ep.source_id.clone(),
                    published_at_ms: ep.published_at_ms,
                    size_bytes: item_bytes,
                });
                kept_auto.sort_by(|a, b| {
                    b.published_at_ms
                        .cmp(&a.published_at_ms)
                        .then_with(|| a.video_id.cmp(&b.video_id))
                });
            }
        }
    }

    plan
}
