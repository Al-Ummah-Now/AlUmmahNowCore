//! [`SyncEngine`]: local-first watch state, history, notes and library with LWW merge, a debounced
//! outbox and pull/push through a [`SyncTransport`].
//!
//! Every user action is applied to the local CRDT state immediately (the UI never waits for the
//! network) and queued as a coalesced server write. [`SyncEngine::sync`] pushes due writes, then
//! pulls the server's view and merges it with LWW stamps.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aun_common::platform::{Clock, SystemClock};
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::lww::{Lww, Merge, Stamp};
use crate::model::{
    ContinueWatchingItem, DownloadRecord, HistoryItem, LibraryItem, Note, NoteView, RESUME_MIN_MS, SyncState,
    VideoMeta, WatchState, WatchStateView,
};
use crate::store::{self, PendingEntry, PendingOp, STORE_VERSION, StoreData};
use crate::transport::{RemoteLibraryPage, SyncTransport};

/// Device id used for stamps of values that come from the server. `~` sorts after letters and
/// digits, so at an identical millisecond the server's value wins.
pub const SERVER_DEVICE: &str = "~server";

/// Engine settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// Stable per-install id (LWW tiebreak). Empty = generate one and keep it in the store.
    pub device_id: String,
    /// The per-account store file; `None` keeps state in memory (tests, signed-out browsing).
    pub store_path: Option<String>,
    /// A video's position is pushed at most this often while it keeps changing (default 30 s).
    pub position_push_interval_ms: u64,
    /// Local writes are flushed to disk at most this often, plus on `sync()` / `persist()`.
    pub persist_interval_ms: u64,
    pub history_page_size: u32,
    /// Pages pulled per sync (newest first). If the server has more, removals elsewhere are not
    /// inferred for older entries.
    pub max_history_pages: u32,
    pub library_page_size: u32,
    pub max_library_pages: u32,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            device_id: String::new(),
            store_path: None,
            position_push_interval_ms: 30_000,
            persist_interval_ms: 5_000,
            history_page_size: 50,
            max_history_pages: 4,
            library_page_size: 50,
            max_library_pages: 10,
        }
    }
}

/// What changed (for UI refresh).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncChange {
    WatchState,
    History,
    Notes,
    Library,
    Downloads,
}

/// Change notifications, implemented natively. Called after the change, never while an internal
/// lock is held, possibly from a background thread.
pub trait SyncListener: Send + Sync {
    fn on_changed(&self, changes: Vec<SyncChange>);
}

/// Outcome of [`SyncEngine::sync`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub pushed: u32,
    /// Writes the server rejected permanently (e.g. the video was deleted); dropped.
    pub dropped: u32,
    /// Writes still waiting (debounced or failed).
    pub pending: u32,
    pub history_entries_pulled: u32,
    pub saved_pulled: u32,
    pub liked_pulled: u32,
    /// Whether the full history list was seen, which allows inferring removals made elsewhere.
    pub history_complete: bool,
}

struct Inner {
    data: StoreData,
    dirty: bool,
    last_persist_ms: i64,
}

/// The sync engine. One per signed-in account; share the `Arc`.
pub struct SyncEngine {
    config: SyncConfig,
    device: String,
    path: Option<PathBuf>,
    transport: Arc<dyn SyncTransport>,
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
    listeners: Mutex<Vec<(u64, Arc<dyn SyncListener>)>>,
    next_listener: std::sync::atomic::AtomicU64,
    sync_lock: tokio::sync::Mutex<()>,
    id_counter: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for SyncEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncEngine")
            .field("device", &self.device)
            .field("path", &self.path)
            .finish()
    }
}

fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    format!("{:016x}", h.finish())
}

impl SyncEngine {
    /// Opens (or creates) the store and returns the engine.
    pub fn open(config: SyncConfig, transport: Arc<dyn SyncTransport>) -> Result<Arc<Self>> {
        Self::open_with_clock(config, transport, Arc::new(SystemClock))
    }

    pub fn open_with_clock(
        config: SyncConfig,
        transport: Arc<dyn SyncTransport>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>> {
        let path = config.store_path.as_ref().map(PathBuf::from);
        let mut data = match &path {
            Some(p) => store::load(p)?.unwrap_or_default(),
            None => StoreData::default(),
        };
        data.version = STORE_VERSION;
        let mut dirty = false;
        if !config.device_id.is_empty() {
            if data.device_id != config.device_id {
                data.device_id = config.device_id.clone();
                dirty = true;
            }
        } else if data.device_id.is_empty() {
            data.device_id = format!("dev-{}", random_suffix());
            dirty = true;
        }
        let engine = Arc::new(Self {
            device: data.device_id.clone(),
            config,
            path,
            transport,
            clock,
            inner: Mutex::new(Inner {
                data,
                dirty,
                last_persist_ms: 0,
            }),
            listeners: Mutex::new(Vec::new()),
            next_listener: std::sync::atomic::AtomicU64::new(1),
            sync_lock: tokio::sync::Mutex::new(()),
            id_counter: std::sync::atomic::AtomicU64::new(0),
        });
        if dirty {
            engine.persist()?;
        }
        Ok(engine)
    }

    pub fn device_id(&self) -> &str {
        &self.device
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn add_listener(&self, listener: Arc<dyn SyncListener>) -> u64 {
        let id = self
            .next_listener
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((id, listener));
        id
    }

    pub fn remove_listener(&self, id: u64) {
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(i, _)| *i != id);
    }

    fn notify(&self, changes: Vec<SyncChange>) {
        if changes.is_empty() {
            return;
        }
        let listeners: Vec<Arc<dyn SyncListener>> = self
            .listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(_, l)| l.clone())
            .collect();
        for l in listeners {
            l.on_changed(changes.clone());
        }
    }

    /// Writes the store now if anything changed.
    pub fn persist(&self) -> Result<()> {
        let snapshot = {
            let mut inner = self.lock();
            if !inner.dirty {
                return Ok(());
            }
            inner.dirty = false;
            inner.last_persist_ms = self.clock.now_ms();
            inner.data.clone()
        };
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Err(e) = store::save(path, &snapshot) {
            self.lock().dirty = true;
            return Err(e);
        }
        Ok(())
    }

    /// Persists if the last write is older than `persist_interval_ms`; errors are logged (the
    /// in-memory state is still correct and the next persist retries).
    fn persist_throttled(&self) {
        let due = {
            let inner = self.lock();
            inner.dirty
                && self.clock.now_ms().saturating_sub(inner.last_persist_ms)
                    >= self.config.persist_interval_ms as i64
        };
        if due && let Err(e) = self.persist() {
            tracing::warn!(error = %e, "could not persist sync store");
        }
    }

    fn enqueue(data: &mut StoreData, op: PendingOp, now: i64, due_at: i64) {
        let key = op.key();
        match data.pending.get_mut(&key) {
            Some(e) => {
                e.op = op;
                e.due_at_ms = e.due_at_ms.min(due_at.max(now));
            }
            None => {
                data.pending.insert(
                    key,
                    PendingEntry {
                        op,
                        created_at_ms: now,
                        due_at_ms: due_at.max(now),
                        attempts: 0,
                    },
                );
            }
        }
    }

    fn mutate<R>(&self, changes: Vec<SyncChange>, f: impl FnOnce(&mut StoreData, i64, Stamp) -> R) -> R {
        let r = {
            let mut inner = self.lock();
            let now = self.clock.now_ms();
            // Lamport floor: a local write is stamped after everything already observed, so a
            // server stamp or a peer with a clock ahead of ours cannot hide it.
            let ts = now.max(inner.data.max_seen_ms.saturating_add(1));
            inner.data.max_seen_ms = ts;
            let stamp = Stamp::new(ts, self.device.clone());
            let r = f(&mut inner.data, now, stamp);
            inner.dirty = true;
            r
        };
        self.persist_throttled();
        self.notify(changes);
        r
    }

    // ---- watch state ---------------------------------------------------------------------------

    /// Records the playback position (feed `aun-player`'s `SavePosition` here). Local state updates
    /// immediately; the server write is debounced to one per `position_push_interval_ms` per video.
    /// Reaching 95 % marks the video completed.
    pub fn record_position(&self, video_id: &str, position_ms: u64, duration_ms: u64) -> Result<()> {
        if video_id.is_empty() {
            return Err(Error::InvalidInput("empty video id".into()));
        }
        let interval = self.config.position_push_interval_ms as i64;
        self.mutate(
            vec![SyncChange::WatchState, SyncChange::History],
            |data, now, stamp| {
                let w = data.state.watch.entry(video_id.to_owned()).or_default();
                w.position_ms.write(position_ms, stamp.clone());
                if duration_ms > 0 {
                    w.duration_ms.write(duration_ms, stamp.clone());
                }
                let done = duration_ms > 0
                    && position_ms as f64 >= duration_ms as f64 * crate::model::COMPLETED_FRACTION;
                if done && !w.completed.value {
                    w.completed.write(true, stamp.clone());
                }
                let duration_ms = if duration_ms > 0 {
                    duration_ms
                } else {
                    w.duration_ms.value
                };
                let due = data
                    .last_push_ms
                    .get(video_id)
                    .map_or(now, |last| last.saturating_add(interval));
                Self::enqueue(
                    data,
                    PendingOp::Progress {
                        video_id: video_id.to_owned(),
                        position_ms,
                        duration_ms,
                    },
                    now,
                    due,
                );
                data.pending.remove(&format!("remove:{video_id}"));
            },
        );
        Ok(())
    }

    /// Marks a video watched (or not) explicitly ("Mark as watched").
    pub fn set_completed(&self, video_id: &str, completed: bool) -> Result<()> {
        if video_id.is_empty() {
            return Err(Error::InvalidInput("empty video id".into()));
        }
        self.mutate(
            vec![SyncChange::WatchState, SyncChange::History],
            |data, now, stamp| {
                let w = data.state.watch.entry(video_id.to_owned()).or_default();
                w.completed.write(completed, stamp.clone());
                let duration = w.duration_ms.value;
                if completed && duration > 0 {
                    // The server derives "completed" from progress: push the end position.
                    w.position_ms.write(duration, stamp);
                    Self::enqueue(
                        data,
                        PendingOp::Progress {
                            video_id: video_id.to_owned(),
                            position_ms: duration,
                            duration_ms: duration,
                        },
                        now,
                        now,
                    );
                }
            },
        );
        Ok(())
    }

    pub fn watch_state(&self, video_id: &str) -> Option<WatchStateView> {
        let inner = self.lock();
        let st = &inner.data.state;
        st.watch.get(video_id).map(|w| WatchStateView {
            video_id: video_id.to_owned(),
            position_ms: w.position_ms.value,
            duration_ms: w.duration_ms.value,
            completed: w.is_completed(),
            updated_at_ms: w.activity().ts_ms,
            in_history: w.is_visible(st.cleared_stamp()),
        })
    }

    /// The saved position to hand to `aun-player` as `resume_position_ms` (`None` if never watched).
    pub fn resume_position_ms(&self, video_id: &str) -> Option<u64> {
        self.watch_state(video_id)
            .filter(|w| w.updated_at_ms > 0)
            .map(|w| w.position_ms)
    }

    /// Removes one video from history (keeps nothing visible until it is watched again).
    pub fn remove_from_history(&self, video_id: &str) {
        self.mutate(
            vec![SyncChange::History, SyncChange::WatchState],
            |data, now, stamp| {
                data.state
                    .watch
                    .entry(video_id.to_owned())
                    .or_default()
                    .removed
                    .write(true, stamp);
                data.pending.remove(&format!("progress:{video_id}"));
                Self::enqueue(
                    data,
                    PendingOp::RemoveHistory {
                        video_id: video_id.to_owned(),
                    },
                    now,
                    now,
                );
            },
        );
    }

    /// Clears all history (on every device after sync).
    pub fn clear_history(&self) {
        self.mutate(
            vec![SyncChange::History, SyncChange::WatchState],
            |data, now, stamp| {
                let mut c = data.state.history_cleared.take().unwrap_or_default();
                c.write(true, stamp);
                data.state.history_cleared = Some(c);
                data.pending.retain(|_, e| {
                    !matches!(e.op, PendingOp::Progress { .. } | PendingOp::RemoveHistory { .. })
                });
                Self::enqueue(data, PendingOp::ClearHistory, now, now);
            },
        );
    }

    fn meta_of(st: &SyncState, id: &str) -> (Option<String>, Option<String>, Option<String>) {
        match st.meta.get(id) {
            Some(m) => (
                Some(m.value.title.clone()).filter(|t| !t.is_empty()),
                m.value.thumbnail_url.clone(),
                m.value.channel_name.clone(),
            ),
            None => (None, None, None),
        }
    }

    /// History, most recent first.
    pub fn history(&self, limit: u32, offset: u32) -> Vec<HistoryItem> {
        let inner = self.lock();
        let st = &inner.data.state;
        let cleared = st.cleared_stamp();
        let mut rows: Vec<(&String, &WatchState)> =
            st.watch.iter().filter(|(_, w)| w.is_visible(cleared)).collect();
        rows.sort_by(|a, b| b.1.activity().cmp(a.1.activity()).then_with(|| a.0.cmp(b.0)));
        rows.into_iter()
            .skip(offset as usize)
            .take(limit as usize)
            .map(|(id, w)| {
                let (title, thumbnail_url, channel_name) = Self::meta_of(st, id);
                HistoryItem {
                    video_id: id.clone(),
                    position_ms: w.position_ms.value,
                    duration_ms: w.duration_ms.value,
                    completed: w.is_completed(),
                    last_watched_at_ms: w.activity().ts_ms,
                    title,
                    thumbnail_url,
                    channel_name,
                }
            })
            .collect()
    }

    /// "Continue watching": in history, at least 30 s in, not completed; most recent first.
    pub fn continue_watching(&self, limit: u32) -> Vec<ContinueWatchingItem> {
        let inner = self.lock();
        let st = &inner.data.state;
        let cleared = st.cleared_stamp();
        let mut rows: Vec<(&String, &WatchState)> = st
            .watch
            .iter()
            .filter(|(_, w)| {
                w.is_visible(cleared) && !w.is_completed() && w.position_ms.value >= RESUME_MIN_MS
            })
            .collect();
        rows.sort_by(|a, b| b.1.activity().cmp(a.1.activity()).then_with(|| a.0.cmp(b.0)));
        rows.into_iter()
            .take(limit as usize)
            .map(|(id, w)| {
                let (title, thumbnail_url, channel_name) = Self::meta_of(st, id);
                let d = w.duration_ms.value;
                ContinueWatchingItem {
                    video_id: id.clone(),
                    position_ms: w.position_ms.value,
                    duration_ms: d,
                    progress: if d > 0 {
                        (w.position_ms.value as f64 / d as f64).min(1.0)
                    } else {
                        0.0
                    },
                    last_watched_at_ms: w.activity().ts_ms,
                    title,
                    thumbnail_url,
                    channel_name,
                }
            })
            .collect()
    }

    // ---- notes / bookmarks ---------------------------------------------------------------------

    /// Adds a note (empty text = bookmark) at a timestamp; returns its id.
    pub fn add_note(&self, video_id: &str, position_ms: u64, text: &str) -> Result<String> {
        if video_id.is_empty() {
            return Err(Error::InvalidInput("empty video id".into()));
        }
        let n = self.id_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!(
            "{}-{:x}-{n}-{}",
            self.device,
            self.clock.now_ms(),
            &random_suffix()[..8]
        );
        let id2 = id.clone();
        self.mutate(vec![SyncChange::Notes], move |data, now, stamp| {
            data.state.notes.insert(
                id2,
                Note {
                    video_id: video_id.to_owned(),
                    created_at_ms: now,
                    position_ms: Lww::new(position_ms, stamp.clone()),
                    text: Lww::new(text.to_owned(), stamp.clone()),
                    removed: Lww::new(false, stamp),
                },
            );
        });
        Ok(id)
    }

    fn edit_note(&self, id: &str, f: impl FnOnce(&mut Note, Stamp)) -> Result<()> {
        let exists = {
            let inner = self.lock();
            inner.data.state.notes.get(id).is_some_and(|n| !n.removed.value)
        };
        if !exists {
            return Err(Error::NotFound(format!("note {id}")));
        }
        self.mutate(vec![SyncChange::Notes], |data, _now, stamp| {
            if let Some(n) = data.state.notes.get_mut(id) {
                f(n, stamp);
            }
        });
        Ok(())
    }

    pub fn update_note_text(&self, id: &str, text: &str) -> Result<()> {
        self.edit_note(id, |n, s| {
            n.text.write(text.to_owned(), s);
        })
    }

    pub fn move_note(&self, id: &str, position_ms: u64) -> Result<()> {
        self.edit_note(id, |n, s| {
            n.position_ms.write(position_ms, s);
        })
    }

    pub fn delete_note(&self, id: &str) -> Result<()> {
        self.edit_note(id, |n, s| {
            n.removed.write(true, s);
        })
    }

    fn note_view(id: &str, n: &Note) -> NoteView {
        NoteView {
            id: id.to_owned(),
            video_id: n.video_id.clone(),
            position_ms: n.position_ms.value,
            text: n.text.value.clone(),
            created_at_ms: n.created_at_ms,
            updated_at_ms: n.text.stamp.ts_ms.max(n.position_ms.stamp.ts_ms),
        }
    }

    /// Notes of one video, by position.
    pub fn notes_for_video(&self, video_id: &str) -> Vec<NoteView> {
        let inner = self.lock();
        let mut v: Vec<NoteView> = inner
            .data
            .state
            .notes
            .iter()
            .filter(|(_, n)| n.video_id == video_id && !n.removed.value)
            .map(|(id, n)| Self::note_view(id, n))
            .collect();
        v.sort_by(|a, b| a.position_ms.cmp(&b.position_ms).then_with(|| a.id.cmp(&b.id)));
        v
    }

    /// All notes, most recently edited first.
    pub fn all_notes(&self, limit: u32) -> Vec<NoteView> {
        let inner = self.lock();
        let mut v: Vec<NoteView> = inner
            .data
            .state
            .notes
            .iter()
            .filter(|(_, n)| !n.removed.value)
            .map(|(id, n)| Self::note_view(id, n))
            .collect();
        v.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        v.truncate(limit as usize);
        v
    }

    // ---- library -------------------------------------------------------------------------------

    /// Adds to / removes from Watch later.
    pub fn set_saved(&self, video_id: &str, saved: bool) -> Result<()> {
        if video_id.is_empty() {
            return Err(Error::InvalidInput("empty video id".into()));
        }
        self.mutate(vec![SyncChange::Library], |data, now, stamp| {
            data.state
                .saved
                .entry(video_id.to_owned())
                .or_default()
                .write(saved, stamp);
            Self::enqueue(
                data,
                PendingOp::SetSaved {
                    video_id: video_id.to_owned(),
                    saved,
                },
                now,
                now,
            );
        });
        Ok(())
    }

    /// Sets the Watch later order. `video_ids` is a prefix: saved items not listed keep their
    /// current relative order after it (ids not saved are ignored by the views).
    pub fn reorder_saved(&self, video_ids: Vec<String>) {
        let current: Vec<String> = self.saved_items().into_iter().map(|i| i.video_id).collect();
        self.mutate(vec![SyncChange::Library], |data, now, stamp| {
            let listed: HashSet<&str> = video_ids.iter().map(String::as_str).collect();
            let mut full = video_ids.clone();
            full.extend(current.into_iter().filter(|v| !listed.contains(v.as_str())));
            data.state.saved_order.write(full, stamp);
            Self::enqueue(data, PendingOp::ReorderSaved { video_ids }, now, now);
        });
    }

    pub fn set_liked(&self, video_id: &str, liked: bool) -> Result<()> {
        if video_id.is_empty() {
            return Err(Error::InvalidInput("empty video id".into()));
        }
        self.mutate(vec![SyncChange::Library], |data, now, stamp| {
            data.state
                .liked
                .entry(video_id.to_owned())
                .or_default()
                .write(liked, stamp);
            Self::enqueue(
                data,
                PendingOp::SetLiked {
                    video_id: video_id.to_owned(),
                    liked,
                },
                now,
                now,
            );
        });
        Ok(())
    }

    pub fn is_saved(&self, video_id: &str) -> bool {
        self.lock()
            .data
            .state
            .saved
            .get(video_id)
            .is_some_and(|f| f.value)
    }

    pub fn is_liked(&self, video_id: &str) -> bool {
        self.lock()
            .data
            .state
            .liked
            .get(video_id)
            .is_some_and(|f| f.value)
    }

    fn library_items(
        st: &SyncState,
        set: &BTreeMap<String, Lww<bool>>,
        order: Option<&[String]>,
    ) -> Vec<LibraryItem> {
        let mut present: Vec<(&String, &Lww<bool>)> = set.iter().filter(|(_, f)| f.value).collect();
        // Newest first, then apply the explicit order (ordered ids first, in that order).
        present.sort_by(|a, b| b.1.stamp.cmp(&a.1.stamp).then_with(|| a.0.cmp(b.0)));
        if let Some(order) = order {
            let rank: BTreeMap<&str, usize> =
                order.iter().enumerate().map(|(i, id)| (id.as_str(), i)).collect();
            present.sort_by_key(|(id, _)| rank.get(id.as_str()).copied().unwrap_or(usize::MAX));
        }
        present
            .into_iter()
            .map(|(id, f)| {
                let (title, thumbnail_url, channel_name) = Self::meta_of(st, id);
                LibraryItem {
                    video_id: id.clone(),
                    added_at_ms: f.stamp.ts_ms,
                    title,
                    thumbnail_url,
                    channel_name,
                }
            })
            .collect()
    }

    /// Watch later, in the user's order (unordered additions first, newest first).
    pub fn saved_items(&self) -> Vec<LibraryItem> {
        let inner = self.lock();
        let st = &inner.data.state;
        let order = &st.saved_order.value;
        // Items added after the last reorder go on top: rank them before ordered ones.
        let mut items = Self::library_items(st, &st.saved, Some(order));
        let ordered: HashSet<&str> = order.iter().map(String::as_str).collect();
        items.sort_by_key(|i| ordered.contains(i.video_id.as_str()));
        items
    }

    pub fn liked_items(&self) -> Vec<LibraryItem> {
        let inner = self.lock();
        let st = &inner.data.state;
        Self::library_items(st, &st.liked, None)
    }

    // ---- downloads (device-local) --------------------------------------------------------------

    pub fn upsert_download(&self, mut record: DownloadRecord) -> Result<()> {
        if record.video_id.is_empty() {
            return Err(Error::InvalidInput("empty video id".into()));
        }
        self.mutate(vec![SyncChange::Downloads], |data, now, _| {
            record.updated_at_ms = now;
            data.downloads.insert(record.video_id.clone(), record);
        });
        Ok(())
    }

    pub fn remove_download(&self, video_id: &str) {
        self.mutate(vec![SyncChange::Downloads], |data, _, _| {
            data.downloads.remove(video_id);
        });
    }

    pub fn downloads(&self) -> Vec<DownloadRecord> {
        let mut v: Vec<DownloadRecord> = self.lock().data.downloads.values().cloned().collect();
        v.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a.video_id.cmp(&b.video_id))
        });
        v
    }

    // ---- CRDT state exchange -------------------------------------------------------------------

    /// The replicated state (for device-to-device exchange or a future LWW-aware server).
    pub fn export_state(&self) -> SyncState {
        self.lock().data.state.clone()
    }

    /// Merges another replica's state (LWW per field). Does not queue server writes.
    pub fn merge_remote(&self, other: &SyncState) {
        self.mutate(
            vec![
                SyncChange::WatchState,
                SyncChange::History,
                SyncChange::Notes,
                SyncChange::Library,
            ],
            |data, _, _| {
                data.state.join(other);
                data.observe();
            },
        );
    }

    // ---- push / pull ---------------------------------------------------------------------------

    pub fn pending_count(&self) -> u32 {
        self.lock().data.pending.len() as u32
    }

    /// When the next debounced write becomes due (schedule a `sync()` then), if any.
    pub fn next_due_ms(&self) -> Option<i64> {
        self.lock().data.pending.values().map(|e| e.due_at_ms).min()
    }

    /// Makes every pending position write due now (call on pause, app background or sign-out),
    /// then pushes and pulls.
    pub async fn flush(&self) -> Result<SyncReport> {
        {
            let mut inner = self.lock();
            let now = self.clock.now_ms();
            for e in inner.data.pending.values_mut() {
                e.due_at_ms = e.due_at_ms.min(now);
            }
        }
        self.sync().await
    }

    /// Pushes due writes, then pulls history, Watch later and liked videos and merges them.
    /// On a retryable failure (offline, 5xx) the failed write stays queued and the error is returned;
    /// a permanent rejection (4xx other than 401/403) drops the write. Concurrent calls are serialised.
    pub async fn sync(&self) -> Result<SyncReport> {
        let _guard = self.sync_lock.lock().await;
        let mut report = SyncReport::default();
        let push_result = self.push(&mut report).await;
        if let Err(e) = push_result {
            self.persist()?;
            return Err(e);
        }
        let pull_result = self.pull(&mut report).await;
        report.pending = self.pending_count();
        {
            let mut inner = self.lock();
            if pull_result.is_ok() {
                inner.data.last_sync_ms = self.clock.now_ms();
            }
            inner.dirty = true;
        }
        self.persist()?;
        pull_result?;
        self.notify(vec![
            SyncChange::WatchState,
            SyncChange::History,
            SyncChange::Library,
        ]);
        Ok(report)
    }

    async fn push(&self, report: &mut SyncReport) -> Result<()> {
        let now = self.clock.now_ms();
        let mut due: Vec<(String, PendingEntry)> = {
            let inner = self.lock();
            inner
                .data
                .pending
                .iter()
                .filter(|(_, e)| e.due_at_ms <= now)
                .map(|(k, e)| (k.clone(), e.clone()))
                .collect()
        };
        due.sort_by(|a, b| {
            a.1.op
                .priority()
                .cmp(&b.1.op.priority())
                .then_with(|| a.1.created_at_ms.cmp(&b.1.created_at_ms))
                .then_with(|| a.0.cmp(&b.0))
        });
        for (key, entry) in due {
            let result = match &entry.op {
                PendingOp::Progress {
                    video_id,
                    position_ms,
                    duration_ms,
                } => {
                    self.transport
                        .push_progress(video_id, *position_ms, *duration_ms)
                        .await
                }
                PendingOp::RemoveHistory { video_id } => self.transport.remove_history_entry(video_id).await,
                PendingOp::ClearHistory => self.transport.clear_history().await,
                PendingOp::SetSaved { video_id, saved } => self.transport.set_saved(video_id, *saved).await,
                PendingOp::ReorderSaved { video_ids } => self.transport.reorder_saved(video_ids).await,
                PendingOp::SetLiked { video_id, liked } => self.transport.set_liked(video_id, *liked).await,
            };
            let mut inner = self.lock();
            inner.dirty = true;
            // Only remove the entry we sent: a newer write for the same key may have arrived.
            let unchanged = inner.data.pending.get(&key).is_some_and(|e| e.op == entry.op);
            match result {
                Ok(()) => {
                    report.pushed += 1;
                    if let PendingOp::Progress { video_id, .. } = &entry.op {
                        inner
                            .data
                            .last_push_ms
                            .insert(video_id.clone(), self.clock.now_ms());
                    }
                    if unchanged {
                        inner.data.pending.remove(&key);
                    }
                }
                Err(e) if e.is_retryable() || matches!(e, Error::Unauthorized(_) | Error::Forbidden(_)) => {
                    if let Some(p) = inner.data.pending.get_mut(&key) {
                        p.attempts += 1;
                    }
                    return Err(e);
                }
                Err(e) => {
                    tracing::warn!(op = %key, error = %e, "sync write rejected; dropping it");
                    report.dropped += 1;
                    if unchanged {
                        inner.data.pending.remove(&key);
                    }
                }
            }
        }
        Ok(())
    }

    async fn pull(&self, report: &mut SyncReport) -> Result<()> {
        let pull_ms = self.clock.now_ms();
        let server_now = Stamp::new(pull_ms, SERVER_DEVICE);

        // History.
        let mut cursor = None;
        let mut seen: HashSet<String> = HashSet::new();
        let mut complete = false;
        let mut entries = Vec::new();
        for _ in 0..self.config.max_history_pages.max(1) {
            let page = self
                .transport
                .fetch_history(cursor.take(), self.config.history_page_size.max(1))
                .await?;
            entries.extend(page.entries);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => {
                    complete = true;
                    break;
                }
            }
        }
        report.history_entries_pulled = entries.len() as u32;
        report.history_complete = complete;
        {
            let mut inner = self.lock();
            let data = &mut inner.data;
            // Videos with an unpushed position keep the local value until that write lands
            // (the server's lastWatchedAt is server time and may beat a slightly-behind device clock).
            let unpushed: HashSet<String> = data
                .pending
                .values()
                .filter_map(|p| match &p.op {
                    PendingOp::Progress { video_id, .. } => Some(video_id.clone()),
                    _ => None,
                })
                .collect();
            for e in entries {
                seen.insert(e.video_id.clone());
                if let Some(meta) = e.meta.clone() {
                    Self::set_meta(&mut data.state, &e.video_id, meta, &server_now);
                }
                if unpushed.contains(&e.video_id) {
                    continue;
                }
                let ts = if e.last_watched_at_ms > 0 {
                    e.last_watched_at_ms
                } else {
                    pull_ms
                };
                let stamp = Stamp::new(ts, SERVER_DEVICE);
                let w = data.state.watch.entry(e.video_id.clone()).or_default();
                w.position_ms.set(e.position_ms, stamp.clone());
                if e.duration_ms > 0 {
                    w.duration_ms.set(e.duration_ms, stamp.clone());
                }
                w.completed.set(e.completed, stamp);
            }
            if complete {
                // Entries the server no longer lists were removed on another device (or the
                // history was cleared there), unless we have newer local activity or writes.
                let pending: HashSet<String> = data
                    .pending
                    .values()
                    .filter_map(|p| match &p.op {
                        PendingOp::Progress { video_id, .. } | PendingOp::RemoveHistory { video_id } => {
                            Some(video_id.clone())
                        }
                        _ => None,
                    })
                    .collect();
                let cleared = data.state.cleared_stamp().cloned();
                for (id, w) in data.state.watch.iter_mut() {
                    if seen.contains(id) || pending.contains(id) || !w.is_visible(cleared.as_ref()) {
                        continue;
                    }
                    if w.activity() < &server_now {
                        w.removed.set(true, server_now.clone());
                    }
                }
            }
            data.observe();
        }

        // Watch later and liked.
        let saved = self.pull_library(true).await?;
        report.saved_pulled = saved.0;
        let liked = self.pull_library(false).await?;
        report.liked_pulled = liked.0;
        Ok(())
    }

    fn set_meta(state: &mut SyncState, video_id: &str, meta: VideoMeta, stamp: &Stamp) {
        let entry = state.meta.entry(video_id.to_owned()).or_default();
        if entry.value != meta {
            entry.set(meta, stamp.clone());
        }
    }

    /// Pulls a library list; returns (items, complete).
    async fn pull_library(&self, saved: bool) -> Result<(u32, bool)> {
        let pull_ms = self.clock.now_ms();
        let server_now = Stamp::new(pull_ms, SERVER_DEVICE);
        let mut cursor = None;
        let mut items = Vec::new();
        let mut complete = false;
        for _ in 0..self.config.max_library_pages.max(1) {
            let limit = self.config.library_page_size.max(1);
            let page: RemoteLibraryPage = if saved {
                self.transport.fetch_saved(cursor.take(), limit).await?
            } else {
                self.transport.fetch_liked(cursor.take(), limit).await?
            };
            items.extend(page.items);
            match page.next_cursor {
                Some(c) => cursor = Some(c),
                None => {
                    complete = true;
                    break;
                }
            }
        }
        let count = items.len() as u32;
        let mut inner = self.lock();
        let data = &mut inner.data;
        let pending_prefix = if saved { "saved:" } else { "liked:" };
        let pending: HashSet<String> = data
            .pending
            .keys()
            .filter_map(|k| k.strip_prefix(pending_prefix).map(str::to_owned))
            .collect();
        let remote_ids: Vec<String> = items.iter().map(|i| i.video_id.clone()).collect();
        let remote: HashSet<&str> = remote_ids.iter().map(String::as_str).collect();
        for item in &items {
            if let Some(meta) = &item.meta {
                Self::set_meta(&mut data.state, &item.video_id, meta.clone(), &server_now);
            }
        }
        let set = if saved {
            &mut data.state.saved
        } else {
            &mut data.state.liked
        };
        for id in &remote_ids {
            if pending.contains(id) {
                continue;
            }
            let flag = set.entry(id.clone()).or_default();
            if !flag.value {
                flag.set(true, server_now.clone());
            }
        }
        if complete {
            for (id, flag) in set.iter_mut() {
                if flag.value
                    && !remote.contains(id.as_str())
                    && !pending.contains(id)
                    && flag.stamp < server_now
                {
                    flag.set(false, server_now.clone());
                }
            }
            if saved && !data.pending.contains_key("reorder") && data.state.saved_order.value != remote_ids {
                data.state.saved_order.set(remote_ids, server_now);
            }
            data.observe();
        }
        Ok((count, complete))
    }
}
