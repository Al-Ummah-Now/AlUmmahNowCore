//! On-disk bundle cache: `<dir>/config.json` and `<dir>/bundles/<locale>.json`, each written
//! atomically (temp file + rename), so a crash never leaves a half-written bundle.

use std::path::{Path, PathBuf};

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::source::RemoteConfig;

/// A cached bundle with its validators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedBundle {
    pub locale: String,
    pub version: String,
    /// Checksum from `/api/i18n/metadata` when the bundle was fetched (also sent as `If-None-Match`).
    pub checksum: Option<String>,
    pub updated_at: String,
    pub fetched_at_ms: i64,
    pub translations: serde_json::Value,
}

/// The cache directory.
#[derive(Debug, Clone)]
pub struct DiskCache {
    dir: PathBuf,
}

impl DiskCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn bundle_path(&self, locale: &str) -> Result<PathBuf> {
        if locale.is_empty() || !locale.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(Error::InvalidInput(format!("bad locale for cache: {locale:?}")));
        }
        Ok(self.dir.join("bundles").join(format!("{locale}.json")))
    }

    pub fn load_bundle(&self, locale: &str) -> Result<Option<CachedBundle>> {
        read_json(&self.bundle_path(locale)?)
    }

    pub fn save_bundle(&self, bundle: &CachedBundle) -> Result<()> {
        write_json_atomic(&self.bundle_path(&bundle.locale)?, bundle)
    }

    /// Every cached bundle. Unreadable files are skipped and logged: a corrupt cache must never
    /// block start-up, because the next sync rewrites it.
    pub fn load_all_bundles(&self) -> Vec<CachedBundle> {
        let Ok(entries) = std::fs::read_dir(self.dir.join("bundles")) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match read_json::<CachedBundle>(&path) {
                Ok(Some(b)) => out.push(b),
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable i18n cache file")
                }
            }
        }
        out.sort_by(|a, b| a.locale.cmp(&b.locale));
        out
    }

    pub fn load_config(&self) -> Result<Option<RemoteConfig>> {
        read_json(&self.dir.join("config.json"))
    }

    pub fn save_config(&self, config: &RemoteConfig) -> Result<()> {
        write_json_atomic(&self.dir.join("config.json"), config)
    }

    /// Deletes everything (sign-out does not need this; it is for "reset app").
    pub fn clear(&self) -> Result<()> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::Storage(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Storage(format!("no parent directory for {}", path.display())))?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec(value)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("cache");
    let tmp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
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

    fn bundle(locale: &str) -> CachedBundle {
        CachedBundle {
            locale: locale.into(),
            version: "v1".into(),
            checksum: Some("abc".into()),
            updated_at: "2026-01-01T00:00:00Z".into(),
            fetched_at_ms: 1,
            translations: serde_json::json!({"a": "b"}),
        }
    }

    #[test]
    fn roundtrip_and_overwrite() {
        let dir = std::env::temp_dir().join(format!("aun-i18n-cache-{}", std::process::id()));
        let cache = DiskCache::new(&dir);
        cache.clear().unwrap();
        assert_eq!(cache.load_bundle("ar").unwrap(), None);
        cache.save_bundle(&bundle("ar")).unwrap();
        let mut b = bundle("ar");
        b.version = "v2".into();
        cache.save_bundle(&b).unwrap();
        assert_eq!(cache.load_bundle("ar").unwrap().unwrap().version, "v2");
        cache.save_bundle(&bundle("en")).unwrap();
        std::fs::write(dir.join("bundles").join("xx.json"), b"{not json").unwrap();
        let all = cache.load_all_bundles();
        assert_eq!(
            all.iter().map(|b| b.locale.as_str()).collect::<Vec<_>>(),
            vec!["ar", "en"]
        );
        assert!(matches!(cache.load_bundle("xx"), Err(Error::Storage(_))));
        assert!(matches!(cache.load_bundle("../etc"), Err(Error::InvalidInput(_))));
        // No temp files left behind.
        let leftovers = std::fs::read_dir(dir.join("bundles"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
        cache.clear().unwrap();
        cache.clear().unwrap();
    }
}
