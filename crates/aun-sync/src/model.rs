//! The replicated state ([`SyncState`]) and the views apps read from it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::lww::{Lww, Merge, Stamp};

/// Fraction watched at or beyond which an item counts as completed (same rule as `aun-player`).
pub const COMPLETED_FRACTION: f64 = 0.95;
/// "Continue watching" and resume start only after this much has been watched (same as `aun-player`).
pub const RESUME_MIN_MS: u64 = 30_000;

/// Per-video watch state; every field is its own LWW register.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WatchState {
    pub position_ms: Lww<u64>,
    pub duration_ms: Lww<u64>,
    pub completed: Lww<bool>,
    /// Tombstone for "remove from history". A later watch (newer position/completed stamp)
    /// brings the entry back.
    pub removed: Lww<bool>,
}

impl WatchState {
    /// Stamp of the latest viewing activity (position or completed).
    pub fn activity(&self) -> &Stamp {
        std::cmp::max(&self.position_ms.stamp, &self.completed.stamp)
    }

    /// Whether the entry shows in history, given the history-wide "clear" stamp.
    pub fn is_visible(&self, cleared: Option<&Stamp>) -> bool {
        let activity = self.activity();
        if activity.is_zero() {
            return false;
        }
        if self.removed.value && self.removed.stamp >= *activity {
            return false;
        }
        !cleared.is_some_and(|c| c >= activity)
    }

    pub fn is_completed(&self) -> bool {
        let d = self.duration_ms.value;
        self.completed.value || (d > 0 && self.position_ms.value as f64 >= d as f64 * COMPLETED_FRACTION)
    }
}

impl Merge for WatchState {
    fn join(&mut self, other: &Self) {
        self.position_ms.join(&other.position_ms);
        self.duration_ms.join(&other.duration_ms);
        self.completed.join(&other.completed);
        self.removed.join(&other.removed);
    }
}

/// A note or bookmark at a timestamp (Epic 13.7).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Note {
    pub video_id: String,
    pub created_at_ms: i64,
    pub position_ms: Lww<u64>,
    /// Empty text = a plain bookmark.
    pub text: Lww<String>,
    pub removed: Lww<bool>,
}

impl Merge for Note {
    fn join(&mut self, other: &Self) {
        // Immutable fields: identical on every replica; take the smaller creation time if a buggy
        // client ever disagreed, so the result is still order-independent.
        if self.video_id.is_empty() || (!other.video_id.is_empty() && other.video_id < self.video_id) {
            self.video_id = other.video_id.clone();
        }
        if self.created_at_ms == 0 || (other.created_at_ms != 0 && other.created_at_ms < self.created_at_ms) {
            self.created_at_ms = other.created_at_ms;
        }
        self.position_ms.join(&other.position_ms);
        self.text.join(&other.text);
        self.removed.join(&other.removed);
    }
}

/// Display metadata cached from the server (title, thumbnail, channel); last fetch wins.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VideoMeta {
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub channel_name: Option<String>,
}

/// The CRDT document: everything that syncs across a user's devices.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SyncState {
    pub watch: BTreeMap<String, WatchState>,
    /// "Clear all history" tombstone: entries with older activity are hidden.
    pub history_cleared: Option<Lww<bool>>,
    pub notes: BTreeMap<String, Note>,
    /// Watch later membership (true = saved, false = removed tombstone).
    pub saved: BTreeMap<String, Lww<bool>>,
    /// Watch later order as last set by the user.
    pub saved_order: Lww<Vec<String>>,
    pub liked: BTreeMap<String, Lww<bool>>,
    pub meta: BTreeMap<String, Lww<VideoMeta>>,
}

impl SyncState {
    /// Highest stamp time anywhere in the state.
    pub fn max_ts(&self) -> i64 {
        let watch = self.watch.values().flat_map(|w| {
            [
                &w.position_ms.stamp,
                &w.duration_ms.stamp,
                &w.completed.stamp,
                &w.removed.stamp,
            ]
        });
        let notes = self
            .notes
            .values()
            .flat_map(|n| [&n.position_ms.stamp, &n.text.stamp, &n.removed.stamp]);
        let flags = self.saved.values().chain(self.liked.values()).map(|l| &l.stamp);
        watch
            .chain(notes)
            .chain(flags)
            .chain(self.meta.values().map(|m| &m.stamp))
            .chain(self.history_cleared.iter().map(|c| &c.stamp))
            .chain(std::iter::once(&self.saved_order.stamp))
            .map(|s| s.ts_ms)
            .max()
            .unwrap_or(0)
    }

    pub fn cleared_stamp(&self) -> Option<&Stamp> {
        self.history_cleared
            .as_ref()
            .filter(|c| c.value)
            .map(|c| &c.stamp)
    }
}

impl Merge for SyncState {
    fn join(&mut self, other: &Self) {
        self.watch.join(&other.watch);
        self.history_cleared.join(&other.history_cleared);
        self.notes.join(&other.notes);
        self.saved.join(&other.saved);
        self.saved_order.join(&other.saved_order);
        self.liked.join(&other.liked);
        self.meta.join(&other.meta);
    }
}

/// Returns `a ⊔ b` without mutating either.
pub fn merged(a: &SyncState, b: &SyncState) -> SyncState {
    let mut out = a.clone();
    out.join(b);
    out
}

// ---- views --------------------------------------------------------------------------------------

/// A flattened watch state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchStateView {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub completed: bool,
    pub updated_at_ms: i64,
    pub in_history: bool,
}

/// A history row (most recent first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub completed: bool,
    pub last_watched_at_ms: i64,
    pub title: Option<String>,
    pub thumbnail_url: Option<String>,
    pub channel_name: Option<String>,
}

/// A "Continue watching" row: started (≥ 30 s), not completed, not removed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinueWatchingItem {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    /// 0.0–1.0, or 0 when the duration is unknown.
    pub progress: f64,
    pub last_watched_at_ms: i64,
    pub title: Option<String>,
    pub thumbnail_url: Option<String>,
    pub channel_name: Option<String>,
}

/// A note/bookmark row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteView {
    pub id: String,
    pub video_id: String,
    pub position_ms: u64,
    pub text: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// A saved (Watch later) or liked video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryItem {
    pub video_id: String,
    pub added_at_ms: i64,
    pub title: Option<String>,
    pub thumbnail_url: Option<String>,
    pub channel_name: Option<String>,
}

/// Download progress on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    #[default]
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
}

/// Offline download metadata. Device-local: it is stored with the sync state but never merged or
/// pushed, because files exist only on the device that downloaded them.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DownloadRecord {
    pub video_id: String,
    pub state: DownloadState,
    /// Rendition height (e.g. 720).
    pub quality: u32,
    pub bytes_total: u64,
    pub bytes_done: u64,
    /// App-relative path of the downloaded file/package.
    pub local_path: String,
    /// When the download must be re-validated or deleted (licence/expiry), Unix ms; 0 = never.
    pub expires_at_ms: i64,
    pub updated_at_ms: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(ts: i64, d: &str) -> Stamp {
        Stamp::new(ts, d)
    }

    #[test]
    fn visibility_rules() {
        let mut w = WatchState::default();
        assert!(!w.is_visible(None), "never watched");
        w.position_ms.set(60_000, s(10, "a"));
        assert!(w.is_visible(None));
        w.removed.set(true, s(11, "b"));
        assert!(!w.is_visible(None));
        w.position_ms.set(61_000, s(12, "a"));
        assert!(w.is_visible(None), "watching again resurrects");
        assert!(!w.is_visible(Some(&s(12, "z"))), "cleared after activity");
        assert!(w.is_visible(Some(&s(5, "z"))));
    }

    #[test]
    fn completion() {
        let mut w = WatchState::default();
        w.duration_ms.set(100_000, s(1, "a"));
        w.position_ms.set(94_999, s(1, "a"));
        assert!(!w.is_completed());
        w.position_ms.set(95_000, s(2, "a"));
        assert!(w.is_completed());
        let mut w = WatchState::default();
        w.completed.set(true, s(1, "a"));
        assert!(w.is_completed());
    }
}
