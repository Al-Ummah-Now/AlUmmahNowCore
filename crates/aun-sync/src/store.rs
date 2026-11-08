//! Local persistence: one JSON document per account (`SyncState` + outbox + downloads), replaced
//! atomically (write a temp file, then rename) so a crash never leaves a torn file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::model::{DownloadRecord, SyncState};

/// A server write waiting to be pushed. Coalesced by [`PendingOp::key`]: the latest write for a
/// key replaces older ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PendingOp {
    Progress {
        video_id: String,
        position_ms: u64,
        duration_ms: u64,
    },
    RemoveHistory {
        video_id: String,
    },
    ClearHistory,
    SetSaved {
        video_id: String,
        saved: bool,
    },
    ReorderSaved {
        video_ids: Vec<String>,
    },
    SetLiked {
        video_id: String,
        liked: bool,
    },
}

impl PendingOp {
    pub fn key(&self) -> String {
        match self {
            PendingOp::Progress { video_id, .. } => format!("progress:{video_id}"),
            PendingOp::RemoveHistory { video_id } => format!("remove:{video_id}"),
            PendingOp::ClearHistory => "clear".into(),
            PendingOp::SetSaved { video_id, .. } => format!("saved:{video_id}"),
            PendingOp::ReorderSaved { .. } => "reorder".into(),
            PendingOp::SetLiked { video_id, .. } => format!("liked:{video_id}"),
        }
    }

    /// Push order: clears and removals before progress (so a removal is not undone by a stale
    /// progress write), then library changes.
    pub fn priority(&self) -> u8 {
        match self {
            PendingOp::ClearHistory => 0,
            PendingOp::RemoveHistory { .. } => 1,
            PendingOp::Progress { .. } => 2,
            PendingOp::SetSaved { .. } => 3,
            PendingOp::ReorderSaved { .. } => 4,
            PendingOp::SetLiked { .. } => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingEntry {
    pub op: PendingOp,
    pub created_at_ms: i64,
    /// Not pushed before this time (debounced position writes).
    pub due_at_ms: i64,
    pub attempts: u32,
}

/// Everything persisted for one account.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreData {
    pub version: u32,
    pub device_id: String,
    pub state: SyncState,
    pub pending: BTreeMap<String, PendingEntry>,
    pub downloads: BTreeMap<String, DownloadRecord>,
    /// Last successful progress push per video (debounce).
    pub last_push_ms: BTreeMap<String, i64>,
    pub last_sync_ms: i64,
    /// Highest stamp time observed (local writes, pulls, merges): the Lamport floor for the
    /// next local write.
    pub max_seen_ms: i64,
}

impl StoreData {
    /// Raises the Lamport floor to cover everything now in `state`.
    pub fn observe(&mut self) {
        self.max_seen_ms = self.max_seen_ms.max(self.state.max_ts());
    }
}

pub const STORE_VERSION: u32 = 1;

/// Reads the store file; `None` if it does not exist. A corrupt file is moved aside
/// (`<name>.corrupt`) and reported as `None`, so the app starts with an empty state instead of
/// failing forever.
pub fn load(path: &Path) -> Result<Option<StoreData>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match serde_json::from_slice::<StoreData>(&bytes) {
        Ok(d) if d.version <= STORE_VERSION => Ok(Some(d)),
        Ok(d) => Err(Error::Storage(format!(
            "sync store version {} is newer than this build ({STORE_VERSION})",
            d.version
        ))),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "corrupt sync store; starting fresh");
            let aside = PathBuf::from(format!("{}.corrupt", path.display()));
            let _ = std::fs::rename(path, aside);
            Ok(None)
        }
    }
}

/// Writes the store atomically.
pub fn save(path: &Path, data: &StoreData) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Storage(format!("no parent directory for {}", path.display())))?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec(data)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("sync");
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &bytes)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn roundtrip_corrupt_and_future_versions() {
        let dir = std::env::temp_dir().join(format!("aun-sync-store-{}", std::process::id()));
        let path = dir.join("acct").join("sync.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load(&path).unwrap(), None);
        let data = StoreData {
            version: STORE_VERSION,
            device_id: "d1".into(),
            ..StoreData::default()
        };
        save(&path, &data).unwrap();
        assert_eq!(load(&path).unwrap(), Some(data.clone()));

        std::fs::write(&path, b"{oops").unwrap();
        assert_eq!(load(&path).unwrap(), None);
        assert!(path.with_file_name("sync.json.corrupt").exists());

        save(&path, &StoreData { version: 99, ..data }).unwrap();
        assert!(matches!(load(&path), Err(Error::Storage(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn op_keys_coalesce() {
        let a = PendingOp::Progress {
            video_id: "v".into(),
            position_ms: 1,
            duration_ms: 2,
        };
        let b = PendingOp::Progress {
            video_id: "v".into(),
            position_ms: 5,
            duration_ms: 2,
        };
        assert_eq!(a.key(), b.key());
        assert!(PendingOp::ClearHistory.priority() < a.priority());
        let json = serde_json::to_string(&PendingOp::SetLiked {
            video_id: "x".into(),
            liked: true,
        })
        .unwrap();
        assert!(json.contains("\"op\":\"set_liked\""));
    }
}
