//! Cross-process exclusive locks for on-device stores.
//!
//! The engines keep their state in memory and replace the store file (or a manifest) on persist.
//! Two processes on the same profile would each load the same file and overwrite each other's
//! writes, so an engine takes a [`StoreLock`] on a sidecar `.lock` file for as long as it is open,
//! and a second open fails with [`Error::Conflict`] instead of silently losing data.
//!
//! The lock is an OS advisory lock (`flock` on Unix, `LockFileEx` on Windows) held on an open
//! handle: it is released when the [`StoreLock`] is dropped or the process dies, so a crash never
//! leaves a stale lock behind. The `.lock` file itself is left in place and reused.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// An exclusive lock on `path`, held until dropped.
#[derive(Debug)]
pub struct StoreLock {
    path: PathBuf,
    // Keeps the handle (and with it the OS lock) open.
    _file: File,
}

impl StoreLock {
    /// Takes the lock without waiting. Creates `path` (and its parent directories) if needed.
    ///
    /// Returns [`Error::Conflict`] if another process, or another open store in this process,
    /// already holds it.
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        // Called through the trait: `File::try_lock` (std, Rust 1.89) is newer than our MSRV.
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(Self { path, _file: file }),
            Err(fs4::TryLockError::WouldBlock) => Err(Error::Conflict(format!(
                "{} is in use by another process (is the app already running?)",
                path.display()
            ))),
            Err(fs4::TryLockError::Error(e)) => {
                Err(Error::Storage(format!("could not lock {}: {e}", path.display())))
            }
        }
    }

    /// The lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn second_lock_conflicts_until_the_first_is_dropped() {
        let dir = std::env::temp_dir().join(format!("aun-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("store.lock");
        let first = StoreLock::acquire(&path).unwrap();
        assert_eq!(first.path(), path.as_path());
        assert!(matches!(StoreLock::acquire(&path), Err(Error::Conflict(_))));
        drop(first);
        let again = StoreLock::acquire(&path).unwrap();
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_threads_racing_get_exactly_one_lock() {
        let dir = std::env::temp_dir().join(format!("aun-lock-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("store.lock");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(32));
        let handles: Vec<_> = (0..32)
            .map(|_| {
                let (path, barrier) = (path.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    StoreLock::acquire(&path)
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let won = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(won, 1, "{results:?}");
        assert!(
            results
                .iter()
                .all(|r| r.is_ok() || matches!(r, Err(Error::Conflict(_))))
        );
        drop(results);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
