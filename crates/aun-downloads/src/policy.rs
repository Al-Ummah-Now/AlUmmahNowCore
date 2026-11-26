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

/// Kind of auto-download rule source (channel or playlist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoSourceKind {
    Channel,
    Playlist,
}

/// Identifies the specific auto-download rule (`kind` + `id`) that owns an auto-downloaded episode,
/// keeping channel and playlist identities distinct even when their IDs are equal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AutoDownloadSource {
    pub kind: AutoSourceKind,
    pub id: String,
}

impl AutoDownloadSource {
    pub fn channel(id: impl Into<String>) -> Self {
        Self {
            kind: AutoSourceKind::Channel,
            id: id.into(),
        }
    }

    pub fn playlist(id: impl Into<String>) -> Self {
        Self {
            kind: AutoSourceKind::Playlist,
            id: id.into(),
        }
    }
}

/// Auto-download rule for a subscribed channel or playlist (downloads the single progressive R2
/// `video.mp4` file per E14-D5). `max_episodes` caps the total number of auto-downloaded episodes
/// kept per rule over time (oldest auto-downloads beyond `max_episodes` are returned in
/// [`AutoDownloadPlan::to_evict`]; manual user downloads are never evicted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoDownloadRule {
    pub source: AutoDownloadSource,
    pub max_episodes: usize,
}

/// A candidate episode discovered from a channel or playlist feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpisodeCandidate {
    pub video_id: String,
    pub source: AutoDownloadSource,
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
    pub source: AutoDownloadSource,
    pub published_at_ms: i64,
    /// Bytes currently reserved by this episode against the user's storage cap (`0` if unknown).
    /// Credited back when a newer episode displaces this one so a rule at its storage cap can
    /// still roll forward to newer episodes.
    #[serde(default)]
    pub size_bytes: u64,
}

/// One episode selected by [`select_auto_downloads`], carrying both its `video_id` and the
/// owning rule [`AutoDownloadSource`] so callers can persist `DownloadRecord.auto_source`
/// accurately even when a video appears in both a channel rule and a playlist rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAutoDownload {
    pub video_id: String,
    pub source: AutoDownloadSource,
}

/// Output of [`select_auto_downloads`]: new episodes to queue (each tagged with its owning rule
/// source) and older auto-downloaded episodes displaced from a rule's `max_episodes` window.
/// An episode can fill a slot in each matching rule, while its download has one owning source.
/// Membership in other rules comes from the current candidates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoDownloadPlan {
    pub to_download: Vec<PlannedAutoDownload>,
    pub to_evict: Vec<String>,
}

/// Plans auto-downloads and auto-evictions per channel while enforcing `auto_download_enabled`,
/// Wi-Fi-only, charging-only, local storage budget + free-space reserve (accounting for existing
/// unfinished downloads and bytes freed by displaced auto-downloads), per-channel total
/// `max_episodes` cap over time (ranking existing auto-downloads and candidates together so
/// episodes that drop out of the feed never cause older videos to thrash), creator opt-out,
/// existing downloads, and user-dismissed/deleted episodes.
///
/// Pass the full set of active rules. A download whose owning rule has been removed is evicted
/// once it falls outside all matching active rules' windows.
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

    // Only auto-downloads fill rule slots. Manual downloads stay out of auto-download planning.
    let resident_ids: HashSet<&str> = existing_auto_downloads
        .iter()
        .map(|e| e.video_id.as_str())
        .collect();
    let mut active_owned_ids = HashSet::new();
    let mut protected_ids = HashSet::new();
    let mut windows = Vec::with_capacity(rules.len());
    for rule in rules {
        let mut owned_by_id: HashMap<&str, &ExistingAutoDownload> = HashMap::new();
        for e in existing_auto_downloads.iter().filter(|e| e.source == rule.source) {
            owned_by_id
                .entry(e.video_id.as_str())
                .and_modify(|prev| {
                    if e.published_at_ms > prev.published_at_ms {
                        *prev = e;
                    }
                })
                .or_insert(e);
        }
        active_owned_ids.extend(owned_by_id.keys().copied());
        let mut owned: Vec<_> = owned_by_id.values().copied().collect();
        owned.sort_by_key(|e| (std::cmp::Reverse(e.published_at_ms), e.video_id.as_str()));

        let mut eligible_by_id: HashMap<&str, &EpisodeCandidate> = HashMap::new();
        for c in candidates {
            let id = c.video_id.as_str();
            if c.source == rule.source
                && c.allow_downloads
                && (!existing_video_ids.contains(id) || resident_ids.contains(id))
                && !dismissed_video_ids.contains(id)
                && !owned_by_id.contains_key(id)
            {
                eligible_by_id
                    .entry(id)
                    .and_modify(|prev| {
                        if c.published_at_ms > prev.published_at_ms {
                            *prev = c;
                        }
                    })
                    .or_insert(c);
            }
        }

        // Include owned episodes even if they fell out of the feed. Existing episodes win ties
        // over new ones so a poll does not replace a file with one from the same timestamp.
        let mut ranked: Vec<_> = owned
            .iter()
            .map(|e| (e.published_at_ms, true, e.video_id.as_str(), None))
            .chain(eligible_by_id.into_values().map(|c| {
                let id = c.video_id.as_str();
                (c.published_at_ms, resident_ids.contains(id), id, Some(c))
            }))
            .collect();
        ranked.sort_by_key(|&(published_at, resident, id, _)| {
            (std::cmp::Reverse(published_at), std::cmp::Reverse(resident), id)
        });
        ranked.truncate(rule.max_episodes);
        protected_ids.extend(ranked.iter().map(|&(_, _, id, _)| id));
        let desired = ranked
            .into_iter()
            .filter_map(|(_, _, _, candidate)| candidate)
            .collect::<Vec<_>>();
        windows.push((rule, owned, desired));
    }

    let mut plan = AutoDownloadPlan::default();
    let mut picked_this_pass = HashSet::new();
    let mut evicted_this_pass = HashSet::new();
    let mut running_used = used_bytes;
    let mut running_pending = pending_remaining_bytes;

    // Keep downloads from removed rules for as long as another rule still wants them. Once
    // they leave every window, recover their storage before planning replacement downloads.
    for e in existing_auto_downloads {
        let id = e.video_id.as_str();
        if !active_owned_ids.contains(id) && !protected_ids.contains(id) && evicted_this_pass.insert(id) {
            running_used = running_used.saturating_sub(e.size_bytes);
            plan.to_evict.push(e.video_id.clone());
        }
    }

    for (rule, mut owned, desired) in windows {
        let mut occupied = owned.len().min(rule.max_episodes);
        // Trim excess owned episodes first. Other rules can still retain the same file.
        if owned.len() > rule.max_episodes {
            for excess in owned.drain(rule.max_episodes..) {
                let id = excess.video_id.as_str();
                if !protected_ids.contains(id) && evicted_this_pass.insert(id) {
                    running_used = running_used.saturating_sub(excess.size_bytes);
                    plan.to_evict.push(excess.video_id.clone());
                }
            }
        }

        for ep in desired {
            let id = ep.video_id.as_str();
            let needs_download = !resident_ids.contains(id) && !picked_this_pass.contains(id);
            let (victim_index, evict) = if occupied < rule.max_episodes {
                (None, None)
            } else if let Some(index) = owned.iter().rposition(|e| {
                let victim_id = e.video_id.as_str();
                !protected_ids.contains(victim_id) && !evicted_this_pass.contains(victim_id)
            }) {
                (Some(index), Some(owned[index]))
            } else {
                // The rule can give up its slot even if another rule still needs the file.
                (owned.len().checked_sub(1), None)
            };
            let effective_used = running_used.saturating_sub(evict.map_or(0, |e| e.size_bytes));
            let item_bytes = effective_item_bytes(ep.estimated_bytes.unwrap_or(0));
            if needs_download
                && can_start_download(
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

            if let Some(index) = victim_index {
                owned.remove(index);
            } else {
                occupied += 1;
            }
            if let Some(e) = evict {
                evicted_this_pass.insert(e.video_id.as_str());
                plan.to_evict.push(e.video_id.clone());
            }
            running_used = effective_used;

            if needs_download {
                running_used = running_used.saturating_add(item_bytes);
                running_pending = running_pending.saturating_add(item_bytes);
                picked_this_pass.insert(id);
                plan.to_download.push(PlannedAutoDownload {
                    video_id: ep.video_id.clone(),
                    source: rule.source.clone(),
                });
            }
        }
    }

    plan
}
