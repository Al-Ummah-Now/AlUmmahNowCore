//! Watch-state, history, notes and offline-library sync with last-writer-wins registers
//! (Epic 11, 9.9, 13.7, 14.5).
//!
//! - [`SyncState`] is a state-based CRDT: per-field [`Lww`] registers stamped with
//!   `(timestamp, device id)`, tombstones for removals, and a merge that is commutative,
//!   associative and idempotent.
//! - [`SyncEngine`] is local-first. Writes apply at once and are persisted atomically to a JSON
//!   store per account; server writes are coalesced and debounced (positions at most every 30 s per
//!   video); `sync()` pushes and pulls through a [`SyncTransport`]. Queries: history, "Continue
//!   watching", notes per video, Watch later, liked, downloads.
//! - [`ApiSyncTransport`] maps the transport onto today's api-videos endpoints. Server gaps (no
//!   per-field timestamps, no notes or "continue watching" endpoint) are listed in `docs/SYNC.md`.

mod engine;
mod lww;
mod model;
pub mod store;
pub mod time;
mod transport;

pub use engine::{SERVER_DEVICE, SyncChange, SyncConfig, SyncEngine, SyncListener, SyncReport};
pub use lww::{Lww, Merge, Stamp};
pub use model::{
    COMPLETED_FRACTION, ContinueWatchingItem, DownloadRecord, DownloadState, HistoryItem, LibraryItem, Note,
    NoteView, RESUME_MIN_MS, SyncState, VideoMeta, WatchState, WatchStateView, merged,
};
pub use transport::{
    ApiSyncTransport, RemoteHistoryEntry, RemoteHistoryPage, RemoteLibraryItem, RemoteLibraryPage,
    SyncTransport, parse_library_page,
};
