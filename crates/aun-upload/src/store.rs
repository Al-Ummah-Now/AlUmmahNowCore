//! Per-upload manifests persisted as JSON (atomic replace), so the queue and every upload's
//! session, URL and offset survive app restarts (19.2 "per-file status that survives restarts").

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::tus::TusTarget;

/// What the user asked to upload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadRequest {
    pub file_path: String,
    pub title: String,
    pub description: Option<String>,
    pub mime_type: String,
    /// The channel to publish under (sent as the `channelId` header).
    pub channel_id: Option<String>,
    pub is_public: Option<bool>,
    pub tags: Option<String>,
    pub category: Option<String>,
    /// Localised titles/descriptions, e.g. `{"ar": "…", "en": "…"}`.
    #[serde(default)]
    pub title_localized: BTreeMap<String, String>,
    #[serde(default)]
    pub description_localized: BTreeMap<String, String>,
    /// Higher runs first; FIFO within a priority.
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadState {
    Queued,
    /// Waiting for Wi-Fi / connectivity / the bandwidth schedule.
    Waiting,
    Hashing,
    Initiating,
    Uploading,
    Completing,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl UploadState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            UploadState::Done | UploadState::Failed | UploadState::Cancelled
        )
    }
}

/// Everything needed to resume an upload after a restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadManifest {
    pub id: String,
    /// The account that queued it; only that account's engine uploads it.
    pub account_id: String,
    pub request: UploadRequest,
    pub state: UploadState,
    pub file_size: u64,
    /// File modification time (Unix ms) when queued: a changed file is not resumed.
    pub file_mtime_ms: i64,
    /// SHA-256 of the whole file (hex), for duplicate detection and integrity.
    pub sha256: Option<String>,
    pub video_id: Option<String>,
    pub tus_session_id: Option<String>,
    pub target: Option<TusTarget>,
    pub upload_url: Option<String>,
    pub offset: u64,
    pub chunk_size: u64,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Earliest time to retry (Unix ms) after a backoff.
    pub not_before_ms: i64,
}

/// Directory of `<id>.json` manifests.
#[derive(Debug, Clone)]
pub struct ManifestStore {
    dir: PathBuf,
}

impl ManifestStore {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::InvalidInput(format!("bad upload id {id:?}")));
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    pub fn save(&self, m: &UploadManifest) -> Result<()> {
        let path = self.path(&m.id)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(m)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        match std::fs::remove_file(self.path(id)?) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// Every manifest on disk; unreadable files are skipped (and logged), not fatal.
    pub fn load_all(&self) -> Result<Vec<UploadManifest>> {
        let mut out = vec![];
        for entry in std::fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(&path).map_err(Error::from).and_then(|b| {
                serde_json::from_slice::<UploadManifest>(&b).map_err(|e| Error::Storage(e.to_string()))
            }) {
                Ok(m) => out.push(m),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable upload manifest")
                }
            }
        }
        out.sort_by(|a, b| {
            b.request
                .priority
                .cmp(&a.request.priority)
                .then(a.created_at_ms.cmp(&b.created_at_ms))
        });
        Ok(out)
    }
}

/// (size, mtime ms) of a file.
pub fn file_identity(path: &Path) -> Result<(u64, i64)> {
    let md = std::fs::metadata(path).map_err(|e| Error::InvalidInput(format!("{}: {e}", path.display())))?;
    if !md.is_file() {
        return Err(Error::InvalidInput(format!("{} is not a file", path.display())));
    }
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok((md.len(), mtime))
}

/// SHA-256 of a file (hex), streamed in 1 MiB blocks.
pub fn sha256_file(path: &Path) -> Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_like_ids() {
        let dir = tempfile::tempdir().unwrap();
        let s = ManifestStore::open(dir.path()).unwrap();
        assert!(s.path("../x").is_err());
        assert!(s.path("").is_err());
        assert!(s.path("u-1_a").is_ok());
    }

    #[test]
    fn sha256_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
