//! UniFFI facade for `aun-sync` (Epic 14.5: offline-first CRDT library sync for watch state,
//! history, Continue watching, likes, Watch later, timestamped study notes, flush, and change
//! listeners).

use std::sync::Arc;

use aun_sync::{
    ApiSyncTransport, SyncChange as CoreSyncChange, SyncConfig, SyncEngine as CoreSyncEngine,
    SyncListener as CoreSyncListener,
};

use crate::AunError;
use crate::api::ApiClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SyncChangeKind {
    WatchState,
    History,
    Notes,
    Library,
}

impl From<CoreSyncChange> for SyncChangeKind {
    fn from(c: CoreSyncChange) -> Self {
        match c {
            CoreSyncChange::WatchState => Self::WatchState,
            CoreSyncChange::History => Self::History,
            CoreSyncChange::Notes => Self::Notes,
            CoreSyncChange::Library => Self::Library,
        }
    }
}

#[uniffi::export(with_foreign)]
pub trait SyncChangeListener: Send + Sync {
    fn on_changed(&self, changes: Vec<SyncChangeKind>);
}

struct ListenerBridge(Arc<dyn SyncChangeListener>);

impl CoreSyncListener for ListenerBridge {
    fn on_changed(&self, changes: Vec<CoreSyncChange>) {
        self.0.on_changed(changes.into_iter().map(Into::into).collect());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncWatchState {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub completed: bool,
    pub updated_at_ms: i64,
    pub in_history: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncHistoryItem {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub completed: bool,
    pub last_watched_at_ms: i64,
    pub title: Option<String>,
    pub thumbnail_url: Option<String>,
    pub channel_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncContinueItem {
    pub video_id: String,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub title: Option<String>,
    pub thumbnail_url: Option<String>,
    pub channel_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncLibraryItem {
    pub video_id: String,
    pub added_at_ms: i64,
    pub title: Option<String>,
    pub thumbnail_url: Option<String>,
    pub channel_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncNote {
    pub id: String,
    pub video_id: String,
    pub position_ms: u64,
    pub text: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SyncSummary {
    pub pushed: u32,
    pub dropped: u32,
    pub pending: u32,
    pub history_entries_pulled: u32,
    pub saved_pulled: u32,
    pub liked_pulled: u32,
}

/// Offline-first CRDT sync engine for a signed-in viewer account.
#[derive(Debug, uniffi::Object)]
pub struct OfflineSyncEngine {
    inner: Arc<CoreSyncEngine>,
}

#[uniffi::export(async_runtime = "tokio")]
impl OfflineSyncEngine {
    #[uniffi::constructor]
    pub fn open(
        store_path: String,
        device_id: Option<String>,
        client: Arc<ApiClient>,
    ) -> Result<Arc<Self>, AunError> {
        let mut cfg = SyncConfig {
            store_path: Some(store_path),
            ..SyncConfig::default()
        };
        if let Some(id) = device_id {
            cfg.device_id = id;
        }
        let transport = Arc::new(ApiSyncTransport::new(client.inner.clone()));
        let inner = CoreSyncEngine::open(cfg, transport)?;
        Ok(Arc::new(Self { inner }))
    }

    pub fn device_id(&self) -> String {
        self.inner.device_id().to_owned()
    }

    pub fn add_listener(&self, listener: Arc<dyn SyncChangeListener>) -> u64 {
        self.inner.add_listener(Arc::new(ListenerBridge(listener)))
    }

    pub fn remove_listener(&self, id: u64) {
        self.inner.remove_listener(id);
    }

    pub fn record_position(
        &self,
        video_id: String,
        position_ms: u64,
        duration_ms: u64,
    ) -> Result<(), AunError> {
        self.inner.record_position(&video_id, position_ms, duration_ms)?;
        Ok(())
    }

    pub fn set_completed(&self, video_id: String, completed: bool) -> Result<(), AunError> {
        self.inner.set_completed(&video_id, completed)?;
        Ok(())
    }

    pub fn watch_state(&self, video_id: String) -> Option<SyncWatchState> {
        self.inner.watch_state(&video_id).map(|w| SyncWatchState {
            video_id: w.video_id,
            position_ms: w.position_ms,
            duration_ms: w.duration_ms,
            completed: w.completed,
            updated_at_ms: w.updated_at_ms,
            in_history: w.in_history,
        })
    }

    pub fn resume_position_ms(&self, video_id: String) -> Option<u64> {
        self.inner.resume_position_ms(&video_id)
    }

    pub fn remove_from_history(&self, video_id: String) {
        self.inner.remove_from_history(&video_id);
    }

    pub fn clear_history(&self) {
        self.inner.clear_history();
    }

    pub fn history(&self, limit: u32, offset: u32) -> Vec<SyncHistoryItem> {
        self.inner
            .history(limit, offset)
            .into_iter()
            .map(|h| SyncHistoryItem {
                video_id: h.video_id,
                position_ms: h.position_ms,
                duration_ms: h.duration_ms,
                completed: h.completed,
                last_watched_at_ms: h.last_watched_at_ms,
                title: h.title,
                thumbnail_url: h.thumbnail_url,
                channel_name: h.channel_name,
            })
            .collect()
    }

    pub fn continue_watching(&self, limit: u32) -> Vec<SyncContinueItem> {
        self.inner
            .continue_watching(limit)
            .into_iter()
            .map(|c| SyncContinueItem {
                video_id: c.video_id,
                position_ms: c.position_ms,
                duration_ms: c.duration_ms,
                title: c.title,
                thumbnail_url: c.thumbnail_url,
                channel_name: c.channel_name,
            })
            .collect()
    }

    pub fn set_liked(&self, video_id: String, liked: bool) -> Result<(), AunError> {
        self.inner.set_liked(&video_id, liked)?;
        Ok(())
    }

    pub fn is_liked(&self, video_id: String) -> bool {
        self.inner.is_liked(&video_id)
    }

    pub fn liked_items(&self) -> Vec<SyncLibraryItem> {
        self.inner
            .liked_items()
            .into_iter()
            .map(|i| SyncLibraryItem {
                video_id: i.video_id,
                added_at_ms: i.added_at_ms,
                title: i.title,
                thumbnail_url: i.thumbnail_url,
                channel_name: i.channel_name,
            })
            .collect()
    }

    pub fn set_saved(&self, video_id: String, saved: bool) -> Result<(), AunError> {
        self.inner.set_saved(&video_id, saved)?;
        Ok(())
    }

    pub fn reorder_saved(&self, video_ids: Vec<String>) {
        self.inner.reorder_saved(video_ids);
    }

    pub fn is_saved(&self, video_id: String) -> bool {
        self.inner.is_saved(&video_id)
    }

    pub fn saved_items(&self) -> Vec<SyncLibraryItem> {
        self.inner
            .saved_items()
            .into_iter()
            .map(|i| SyncLibraryItem {
                video_id: i.video_id,
                added_at_ms: i.added_at_ms,
                title: i.title,
                thumbnail_url: i.thumbnail_url,
                channel_name: i.channel_name,
            })
            .collect()
    }

    pub fn add_note(&self, video_id: String, position_ms: u64, text: String) -> Result<String, AunError> {
        Ok(self.inner.add_note(&video_id, position_ms, &text)?)
    }

    pub fn update_note_text(&self, id: String, text: String) -> Result<(), AunError> {
        self.inner.update_note_text(&id, &text)?;
        Ok(())
    }

    pub fn move_note(&self, id: String, position_ms: u64) -> Result<(), AunError> {
        self.inner.move_note(&id, position_ms)?;
        Ok(())
    }

    pub fn delete_note(&self, id: String) -> Result<(), AunError> {
        self.inner.delete_note(&id)?;
        Ok(())
    }

    pub fn notes_for_video(&self, video_id: String) -> Vec<SyncNote> {
        self.inner
            .notes_for_video(&video_id)
            .into_iter()
            .map(|n| SyncNote {
                id: n.id,
                video_id: n.video_id,
                position_ms: n.position_ms,
                text: n.text,
                created_at_ms: n.created_at_ms,
                updated_at_ms: n.updated_at_ms,
            })
            .collect()
    }

    pub fn all_notes(&self, limit: u32) -> Vec<SyncNote> {
        self.inner
            .all_notes(limit)
            .into_iter()
            .map(|n| SyncNote {
                id: n.id,
                video_id: n.video_id,
                position_ms: n.position_ms,
                text: n.text,
                created_at_ms: n.created_at_ms,
                updated_at_ms: n.updated_at_ms,
            })
            .collect()
    }

    pub fn pending_count(&self) -> u32 {
        self.inner.pending_count()
    }

    pub fn next_due_ms(&self) -> Option<i64> {
        self.inner.next_due_ms()
    }

    pub fn persist(&self) -> Result<(), AunError> {
        self.inner.persist()?;
        Ok(())
    }

    pub async fn flush(&self) -> Result<SyncSummary, AunError> {
        let report = self.inner.flush().await?;
        Ok(SyncSummary {
            pushed: report.pushed,
            dropped: report.dropped,
            pending: report.pending,
            history_entries_pulled: report.history_entries_pulled,
            saved_pulled: report.saved_pulled,
            liked_pulled: report.liked_pulled,
        })
    }

    pub async fn sync(&self) -> Result<SyncSummary, AunError> {
        let report = self.inner.sync().await?;
        Ok(SyncSummary {
            pushed: report.pushed,
            dropped: report.dropped,
            pending: report.pending,
            history_entries_pulled: report.history_entries_pulled,
            saved_pulled: report.saved_pulled,
            liked_pulled: report.liked_pulled,
        })
    }
}
