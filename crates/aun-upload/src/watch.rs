//! Folder watching for Studio desktop (19.2 "watch folder"): new video files that have stopped
//! growing are handed to a callback (typically `UploadEngine::enqueue`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aun_common::{Error, Result};
use notify::{RecursiveMode, Watcher};

/// Called once per new, stable video file.
pub type OnFile = Arc<dyn Fn(PathBuf) + Send + Sync>;

/// Watches a folder; drop or call [`FolderWatcher::stop`] to stop.
pub struct FolderWatcher {
    _watcher: notify::RecommendedWatcher,
    stop: tokio_util::sync::CancellationToken,
}

impl std::fmt::Debug for FolderWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FolderWatcher").finish_non_exhaustive()
    }
}

impl FolderWatcher {
    /// Starts watching `dir`. A file is reported once its size is unchanged for `settle`
    /// (cameras and copies write for a while). Files already present are not reported.
    pub fn start(dir: impl AsRef<Path>, recursive: bool, settle: Duration, on_file: OnFile) -> Result<Self> {
        // FSEvents reports canonical paths (`/private/var/...`), so watch the canonical one.
        let dir = std::fs::canonicalize(dir.as_ref()).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let pending: Arc<Mutex<HashMap<PathBuf, (u64, tokio::time::Instant)>>> = Arc::default();
        let existing = scan(&dir, recursive);
        let p2 = pending.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if !(ev.kind.is_create() || ev.kind.is_modify()) {
                return;
            }
            if let Ok(mut p) = p2.lock() {
                for path in ev.paths {
                    if crate::video_mime_for(&path).is_some() {
                        p.entry(path).or_insert((u64::MAX, tokio::time::Instant::now()));
                    }
                }
            }
        })
        .map_err(|e| Error::Platform(e.to_string()))?;
        let mode = if recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        watcher
            .watch(dir.as_ref(), mode)
            .map_err(|e| Error::Platform(e.to_string()))?;

        let stop = tokio_util::sync::CancellationToken::new();
        let token = stop.clone();
        tokio::spawn(async move {
            // Files present at start are not "new"; the scan below is a fallback for file
            // systems that don't deliver events (network shares, some external drives).
            let mut reported: HashSet<PathBuf> = existing;
            let tick = (settle / 4).max(Duration::from_millis(50));
            let scan_every = Duration::from_secs(5).max(settle);
            let mut last_scan = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(tick) => {}
                }
                if last_scan.elapsed() >= scan_every {
                    last_scan = tokio::time::Instant::now();
                    let found = scan(&dir, recursive);
                    if let Ok(mut p) = pending.lock() {
                        for path in found.into_iter().filter(|f| !reported.contains(f)) {
                            p.entry(path).or_insert((u64::MAX, last_scan));
                        }
                    }
                }
                let ready = settle_pass(&pending, &mut reported, &dir, settle);
                for path in ready {
                    on_file(path);
                }
            }
        });
        Ok(Self {
            _watcher: watcher,
            stop,
        })
    }

    pub fn stop(&self) {
        self.stop.cancel();
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Video files under `dir` (canonical paths).
fn scan(dir: &Path, recursive: bool) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() && recursive => stack.push(path),
                Ok(t) if t.is_file() && crate::video_mime_for(&path).is_some() => {
                    out.insert(path);
                }
                _ => {}
            }
        }
    }
    out
}

/// A regular file (not a symlink) whose real location is inside `root`: a link dropped into the
/// watch folder must not make us upload a file from elsewhere on disk.
fn plain_file_under(path: &Path, root: &Path) -> Option<std::fs::Metadata> {
    let md = std::fs::symlink_metadata(path).ok()?;
    if !md.file_type().is_file() {
        return None;
    }
    // A hard link can point at a file the person who dropped it couldn't read themselves; we'd
    // upload it with the user's permissions. Camera/OS copies are single-link files.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if md.nlink() > 1 {
            return None;
        }
    }
    let real = std::fs::canonicalize(path).ok()?;
    real.starts_with(root).then_some(md)
}

/// Returns files whose size hasn't changed for `settle`.
fn settle_pass(
    pending: &Mutex<HashMap<PathBuf, (u64, tokio::time::Instant)>>,
    reported: &mut HashSet<PathBuf>,
    root: &Path,
    settle: Duration,
) -> Vec<PathBuf> {
    let Ok(mut p) = pending.lock() else { return vec![] };
    let now = tokio::time::Instant::now();
    let mut ready = vec![];
    p.retain(|path, (size, since)| {
        let Some(md) = plain_file_under(path, root) else {
            return false;
        };
        if reported.contains(path) {
            return false;
        }
        if md.len() != *size {
            *size = md.len();
            *since = now;
            return true;
        }
        if md.len() > 0 && now.duration_since(*since) >= settle {
            reported.insert(path.clone());
            ready.push(path.clone());
            return false;
        }
        true
    });
    ready
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_new_stable_video_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.mp4"), b"old").unwrap();
        let seen: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
        let s2 = seen.clone();
        let w = FolderWatcher::start(
            dir.path(),
            false,
            Duration::from_millis(300),
            Arc::new(move |p| s2.lock().unwrap().push(p)),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::fs::write(dir.path().join("clip.mp4"), b"data").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        // A symlink to a video outside the folder is ignored.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.mp4"), b"s").unwrap();
        #[cfg(unix)]
        std::fs::hard_link(outside.path().join("secret.mp4"), dir.path().join("hard.mp4")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path().join("secret.mp4"), dir.path().join("link.mp4")).unwrap();
        for _ in 0..50 {
            if !seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].ends_with("clip.mp4"));
        w.stop();
    }
}
