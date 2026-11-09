#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `SyncEngine` against an in-memory server: debounce, outbox, push/pull merge, removal inference,
//! library, notes, persistence and listeners.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use aun_common::platform::{Clock, ManualClock};
use aun_common::{Error, Result};
use aun_sync::{
    DownloadRecord, DownloadState, RemoteHistoryEntry, RemoteHistoryPage, RemoteLibraryItem,
    RemoteLibraryPage, SyncChange, SyncConfig, SyncEngine, SyncListener, SyncTransport, VideoMeta,
};

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Progress(String, u64),
    Remove(String),
    Clear,
    Saved(String, bool),
    Reorder(Vec<String>),
    Liked(String, bool),
}

/// A fake api-videos: server-side history (position, duration, lastWatchedAt from its own clock),
/// Watch later and liked lists.
struct FakeServer {
    clock: Arc<ManualClock>,
    history: Mutex<BTreeMap<String, (u64, u64, i64)>>,
    saved: Mutex<Vec<String>>,
    liked: Mutex<Vec<String>>,
    calls: Mutex<Vec<Call>>,
    offline: AtomicBool,
    reject_video: Mutex<Option<String>>,
    page_size_cap: usize,
}

impl FakeServer {
    fn new(clock: Arc<ManualClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            history: Mutex::new(BTreeMap::new()),
            saved: Mutex::new(vec![]),
            liked: Mutex::new(vec![]),
            calls: Mutex::new(vec![]),
            offline: AtomicBool::new(false),
            reject_video: Mutex::new(None),
            page_size_cap: 2,
        })
    }
    fn up(&self) -> Result<()> {
        if self.offline.load(Ordering::SeqCst) {
            Err(Error::Network("offline".into()))
        } else {
            Ok(())
        }
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
    fn page(list: &[String], cursor: Option<String>, n: usize) -> RemoteLibraryPage {
        let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
        let end = (start + n).min(list.len());
        RemoteLibraryPage {
            items: list[start..end]
                .iter()
                .map(|id| RemoteLibraryItem {
                    video_id: id.clone(),
                    added_at_ms: None,
                    meta: None,
                })
                .collect(),
            next_cursor: (end < list.len()).then(|| end.to_string()),
        }
    }
}

#[async_trait]
impl SyncTransport for FakeServer {
    async fn fetch_history(&self, cursor: Option<String>, limit: u32) -> Result<RemoteHistoryPage> {
        self.up()?;
        let h = self.history.lock().unwrap();
        let mut rows: Vec<(&String, &(u64, u64, i64))> = h.iter().collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1.2));
        let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
        let n = (limit as usize).min(self.page_size_cap);
        let end = (start + n).min(rows.len());
        Ok(RemoteHistoryPage {
            entries: rows[start..end]
                .iter()
                .map(|(id, (p, d, at))| RemoteHistoryEntry {
                    video_id: (*id).clone(),
                    position_ms: *p,
                    duration_ms: *d,
                    completed: *d > 0 && *p as f64 >= *d as f64 * 0.95,
                    last_watched_at_ms: *at,
                    meta: Some(VideoMeta {
                        title: format!("Title {id}"),
                        thumbnail_url: None,
                        channel_name: Some("Channel".into()),
                    }),
                })
                .collect(),
            next_cursor: (end < rows.len()).then(|| end.to_string()),
        })
    }
    async fn push_progress(&self, video_id: &str, position_ms: u64, duration_ms: u64) -> Result<()> {
        self.up()?;
        if self.reject_video.lock().unwrap().as_deref() == Some(video_id) {
            return Err(Error::NotFound("video deleted".into()));
        }
        self.calls
            .lock()
            .unwrap()
            .push(Call::Progress(video_id.into(), position_ms));
        self.history
            .lock()
            .unwrap()
            .insert(video_id.into(), (position_ms, duration_ms, self.clock.now_ms()));
        Ok(())
    }
    async fn remove_history_entry(&self, video_id: &str) -> Result<()> {
        self.up()?;
        self.calls.lock().unwrap().push(Call::Remove(video_id.into()));
        self.history.lock().unwrap().remove(video_id);
        Ok(())
    }
    async fn clear_history(&self) -> Result<()> {
        self.up()?;
        self.calls.lock().unwrap().push(Call::Clear);
        self.history.lock().unwrap().clear();
        Ok(())
    }
    async fn fetch_saved(&self, cursor: Option<String>, limit: u32) -> Result<RemoteLibraryPage> {
        self.up()?;
        Ok(Self::page(
            &self.saved.lock().unwrap(),
            cursor,
            (limit as usize).min(self.page_size_cap),
        ))
    }
    async fn set_saved(&self, video_id: &str, saved: bool) -> Result<()> {
        self.up()?;
        self.calls
            .lock()
            .unwrap()
            .push(Call::Saved(video_id.into(), saved));
        let mut s = self.saved.lock().unwrap();
        s.retain(|v| v != video_id);
        if saved {
            s.insert(0, video_id.into());
        }
        Ok(())
    }
    async fn reorder_saved(&self, video_ids: &[String]) -> Result<()> {
        self.up()?;
        self.calls.lock().unwrap().push(Call::Reorder(video_ids.to_vec()));
        let mut s = self.saved.lock().unwrap();
        let mut ordered: Vec<String> = video_ids.iter().filter(|v| s.contains(v)).cloned().collect();
        ordered.extend(s.iter().filter(|v| !video_ids.contains(v)).cloned());
        *s = ordered;
        Ok(())
    }
    async fn fetch_liked(&self, cursor: Option<String>, limit: u32) -> Result<RemoteLibraryPage> {
        self.up()?;
        Ok(Self::page(
            &self.liked.lock().unwrap(),
            cursor,
            (limit as usize).min(self.page_size_cap),
        ))
    }
    async fn set_liked(&self, video_id: &str, liked: bool) -> Result<()> {
        self.up()?;
        self.calls
            .lock()
            .unwrap()
            .push(Call::Liked(video_id.into(), liked));
        let mut l = self.liked.lock().unwrap();
        l.retain(|v| v != video_id);
        if liked {
            l.push(video_id.into());
        }
        Ok(())
    }
}

fn engine(
    server: &Arc<FakeServer>,
    clock: &Arc<ManualClock>,
    device: &str,
    path: Option<String>,
) -> Arc<SyncEngine> {
    SyncEngine::open_with_clock(
        SyncConfig {
            device_id: device.into(),
            store_path: path,
            ..SyncConfig::default()
        },
        server.clone(),
        clock.clone(),
    )
    .unwrap()
}

#[tokio::test]
async fn positions_are_debounced() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let e = engine(&server, &clock, "phone", None);

    e.record_position("v1", 40_000, 600_000).unwrap();
    e.sync().await.unwrap();
    assert_eq!(server.calls(), vec![Call::Progress("v1".into(), 40_000)]);

    // Every second for 20 s: local state follows, nothing is pushed (within 30 s of the last push).
    for s in 1..=20u64 {
        clock.advance(1_000);
        e.record_position("v1", 40_000 + s * 1_000, 600_000).unwrap();
        let r = e.sync().await.unwrap();
        assert_eq!(r.pushed, 0);
    }
    assert_eq!(e.watch_state("v1").unwrap().position_ms, 60_000);
    assert_eq!(e.next_due_ms(), Some(1_030_000));
    clock.advance(10_000);
    e.sync().await.unwrap();
    assert_eq!(
        server.calls().len(),
        2,
        "one more push after 30 s, with the latest position"
    );
    assert_eq!(server.calls()[1], Call::Progress("v1".into(), 60_000));

    // Pause → flush pushes immediately regardless of the debounce.
    e.record_position("v1", 61_000, 600_000).unwrap();
    e.flush().await.unwrap();
    assert_eq!(server.calls()[2], Call::Progress("v1".into(), 61_000));
}

#[tokio::test]
async fn two_devices_converge_through_the_server() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let phone = engine(&server, &clock, "phone", None);
    let tv = engine(&server, &clock, "tv", None);

    phone.record_position("v1", 120_000, 600_000).unwrap();
    phone.set_saved("v2", true).unwrap();
    phone.set_liked("v3", true).unwrap();
    phone.sync().await.unwrap();

    clock.advance(60_000);
    let r = tv.sync().await.unwrap();
    assert!(r.history_complete);
    assert_eq!(tv.resume_position_ms("v1"), Some(120_000));
    assert!(tv.is_saved("v2"));
    assert!(tv.is_liked("v3"));
    let cw = tv.continue_watching(10);
    assert_eq!(cw.len(), 1);
    assert_eq!(cw[0].title.as_deref(), Some("Title v1"));
    assert!((cw[0].progress - 0.2).abs() < 1e-9);

    // TV watches further later; phone picks it up (server's lastWatchedAt is newer).
    clock.advance(60_000);
    tv.record_position("v1", 300_000, 600_000).unwrap();
    tv.sync().await.unwrap();
    clock.advance(1_000);
    phone.sync().await.unwrap();
    assert_eq!(phone.resume_position_ms("v1"), Some(300_000));

    // Removal on the phone propagates to the TV via the full listing.
    phone.remove_from_history("v1");
    assert!(phone.history(10, 0).is_empty());
    phone.sync().await.unwrap();
    clock.advance(1_000);
    tv.sync().await.unwrap();
    assert!(tv.history(10, 0).is_empty(), "inferred removal");
    assert!(!tv.watch_state("v1").unwrap().in_history);

    // Watching again brings it back.
    clock.advance(1_000);
    tv.record_position("v1", 310_000, 600_000).unwrap();
    assert_eq!(tv.history(10, 0).len(), 1);

    // Unsave on TV → gone on the phone.
    tv.set_saved("v2", false).unwrap();
    tv.sync().await.unwrap();
    clock.advance(1_000);
    phone.sync().await.unwrap();
    assert!(!phone.is_saved("v2"));
}

#[tokio::test]
async fn completion_and_continue_watching_rules() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let e = engine(&server, &clock, "phone", None);
    e.record_position("short", 10_000, 600_000).unwrap(); // < 30 s
    clock.advance(1);
    e.record_position("done", 590_000, 600_000).unwrap(); // ≥ 95 %
    clock.advance(1);
    e.record_position("mid", 300_000, 600_000).unwrap();
    clock.advance(1);
    e.record_position("mid2", 100_000, 0).unwrap(); // unknown duration
    let ids: Vec<String> = e.continue_watching(10).into_iter().map(|c| c.video_id).collect();
    assert_eq!(ids, vec!["mid2", "mid"]);
    assert!(e.watch_state("done").unwrap().completed);
    assert_eq!(e.history(10, 0).len(), 4);
    assert_eq!(e.history(2, 1).len(), 2);

    e.set_completed("mid", true).unwrap();
    assert_eq!(e.continue_watching(10).len(), 1);

    e.clear_history();
    assert!(e.history(10, 0).is_empty());
    assert!(e.continue_watching(10).is_empty());
    e.sync().await.unwrap();
    assert_eq!(
        server.calls().first(),
        Some(&Call::Clear),
        "clear goes first and drops queued progress"
    );
    assert!(!server.calls().iter().any(|c| matches!(c, Call::Progress(..))));
}

#[tokio::test]
async fn offline_keeps_writes_and_permanent_errors_drop_them() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let e = engine(&server, &clock, "phone", None);
    e.record_position("v1", 60_000, 600_000).unwrap();
    e.set_liked("v1", true).unwrap();
    server.offline.store(true, Ordering::SeqCst);
    let err = e.sync().await.unwrap_err();
    assert!(err.is_retryable());
    assert_eq!(e.pending_count(), 2);
    assert!(e.is_liked("v1"), "local state unaffected");

    server.offline.store(false, Ordering::SeqCst);
    *server.reject_video.lock().unwrap() = Some("v1".into());
    let r = e.sync().await.unwrap();
    assert_eq!((r.pushed, r.dropped, r.pending), (1, 1, 0));
}

#[tokio::test]
async fn notes_and_bookmarks() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let a = engine(&server, &clock, "phone", None);
    let b = engine(&server, &clock, "tv", None);
    let n1 = a.add_note("v1", 90_000, "Key point: المقدمة").unwrap();
    clock.advance(10);
    let n2 = a.add_note("v1", 30_000, "").unwrap();
    let notes = a.notes_for_video("v1");
    assert_eq!(
        notes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
        vec![n2.clone(), n1.clone()]
    );
    assert!(a.update_note_text("missing", "x").is_err());

    // Notes have no server endpoint: replicate by state exchange; concurrent edits resolve LWW.
    b.merge_remote(&a.export_state());
    clock.advance(10);
    a.update_note_text(&n1, "edited on phone").unwrap();
    clock.advance(10);
    b.update_note_text(&n1, "edited on tv").unwrap();
    b.move_note(&n1, 95_000).unwrap();
    a.delete_note(&n2).unwrap();
    a.merge_remote(&b.export_state());
    b.merge_remote(&a.export_state());
    assert_eq!(a.export_state(), b.export_state());
    let notes = a.notes_for_video("v1");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "edited on tv");
    assert_eq!(notes[0].position_ms, 95_000);
    assert_eq!(a.all_notes(10).len(), 1);
}

#[tokio::test]
async fn library_order_and_paging() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    *server.saved.lock().unwrap() = vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()];
    let e = engine(&server, &clock, "phone", None);
    let r = e.sync().await.unwrap();
    assert_eq!(r.saved_pulled, 5, "three pages of two");
    let order: Vec<String> = e.saved_items().into_iter().map(|i| i.video_id).collect();
    assert_eq!(order, vec!["a", "b", "c", "d", "e"]);

    e.reorder_saved(vec!["e".into(), "a".into()]);
    clock.advance(10);
    e.set_saved("z", true).unwrap();
    let order: Vec<String> = e.saved_items().into_iter().map(|i| i.video_id).collect();
    assert_eq!(order[0], "z", "new additions on top");
    assert_eq!(&order[1..3], &["e".to_string(), "a".to_string()]);
    e.sync().await.unwrap();
    assert!(server.calls().contains(&Call::Saved("z".into(), true)));
    assert!(
        server
            .calls()
            .contains(&Call::Reorder(vec!["e".into(), "a".into()]))
    );
}

#[derive(Default)]
struct Recorder(Mutex<Vec<Vec<SyncChange>>>);
impl SyncListener for Recorder {
    fn on_changed(&self, changes: Vec<SyncChange>) {
        self.0.lock().unwrap().push(changes);
    }
}

#[tokio::test]
async fn persistence_downloads_and_listeners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("acct-1")
        .join("sync.json")
        .to_string_lossy()
        .into_owned();
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let e = engine(&server, &clock, "phone", Some(path.clone()));
    let rec = Arc::new(Recorder::default());
    let id = e.add_listener(rec.clone());
    e.record_position("v1", 45_000, 600_000).unwrap();
    e.add_note("v1", 1_000, "n").unwrap();
    e.upsert_download(DownloadRecord {
        video_id: "v1".into(),
        state: DownloadState::Completed,
        quality: 720,
        bytes_total: 10,
        bytes_done: 10,
        local_path: "downloads/v1".into(),
        ..DownloadRecord::default()
    })
    .unwrap();
    assert!(rec.0.lock().unwrap().len() >= 3);
    e.remove_listener(id);
    e.persist().unwrap();
    drop(e);

    // Reopen: state, outbox and downloads survive; the store's device id is reused when none given.
    let e2 = SyncEngine::open_with_clock(
        SyncConfig {
            store_path: Some(path.clone()),
            ..SyncConfig::default()
        },
        server.clone(),
        clock.clone(),
    )
    .unwrap();
    assert_eq!(e2.device_id(), "phone");
    assert_eq!(e2.resume_position_ms("v1"), Some(45_000));
    assert_eq!(e2.pending_count(), 1);
    assert_eq!(e2.notes_for_video("v1").len(), 1);
    assert_eq!(e2.downloads()[0].quality, 720);
    e2.remove_download("v1");
    assert!(e2.downloads().is_empty());
    assert!(e2.record_position("", 1, 1).is_err());

    // A fresh store without a device id generates one.
    let e3 = SyncEngine::open(SyncConfig::default(), server.clone()).unwrap();
    assert!(e3.device_id().starts_with("dev-"));
}

#[tokio::test]
async fn unpushed_local_position_is_not_overwritten_by_pull() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let e = engine(&server, &clock, "phone", None);
    e.record_position("v1", 50_000, 600_000).unwrap();
    e.sync().await.unwrap();
    // Server clock ahead: another write lands server-side "later" than our next local write.
    server
        .history
        .lock()
        .unwrap()
        .insert("v1".into(), (50_000, 600_000, 9_999_999_999));
    clock.advance(1_000);
    e.record_position("v1", 51_000, 600_000).unwrap(); // debounced: stays pending
    e.sync().await.unwrap();
    assert_eq!(e.resume_position_ms("v1"), Some(51_000));
}

#[tokio::test]
async fn local_edits_win_over_stamps_from_a_clock_ahead() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let e = engine(&server, &clock, "phone", None);
    e.record_position("v1", 60_000, 600_000).unwrap();
    // A peer whose clock is a day ahead removes it from history and unsaves "s".
    let mut peer = e.export_state();
    let ahead = aun_sync::Stamp::new(1_000_000 + 86_400_000, "laptop");
    peer.watch.get_mut("v1").unwrap().removed.set(true, ahead.clone());
    peer.saved.entry("s".into()).or_default().set(false, ahead);
    e.merge_remote(&peer);
    assert!(e.history(10, 0).is_empty());
    // Our clock is still behind, yet watching again and saving again take effect.
    clock.advance(1_000);
    e.record_position("v1", 61_000, 600_000).unwrap();
    e.set_saved("s", true).unwrap();
    assert_eq!(e.history(10, 0).len(), 1);
    assert!(e.is_saved("s"));
}

#[tokio::test]
async fn a_second_open_of_the_same_store_is_refused_until_the_first_closes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("acct")
        .join("sync.json")
        .to_string_lossy()
        .into_owned();
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let first = engine(&server, &clock, "desktop", Some(path.clone()));
    first.set_saved("v1", true).unwrap();

    // Many "processes" opening the profile at once: none of them gets in while the first is open.
    let opens: Vec<_> = (0..32)
        .map(|_| {
            let (server, clock, path) = (server.clone(), clock.clone(), path.clone());
            std::thread::spawn(move || {
                SyncEngine::open_with_clock(
                    SyncConfig {
                        device_id: "desktop-2".into(),
                        store_path: Some(path),
                        ..SyncConfig::default()
                    },
                    server,
                    clock,
                )
                .map(|_| ())
            })
        })
        .collect();
    for h in opens {
        assert!(matches!(h.join().unwrap(), Err(Error::Conflict(_))));
    }

    // Different accounts (different store files) are independent.
    let other = dir
        .path()
        .join("other")
        .join("sync.json")
        .to_string_lossy()
        .into_owned();
    let _other = engine(&server, &clock, "desktop", Some(other));

    first.persist().unwrap();
    drop(first);
    let reopened = engine(&server, &clock, "desktop", Some(path));
    assert_eq!(reopened.device_id(), "desktop");
}

#[tokio::test]
async fn racing_opens_of_one_store_let_exactly_one_in() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.json").to_string_lossy().into_owned();
    let clock = Arc::new(ManualClock::new(1_000_000));
    let server = FakeServer::new(clock.clone());
    let barrier = Arc::new(std::sync::Barrier::new(24));
    let opens: Vec<_> = (0..24)
        .map(|i| {
            let (server, clock, path, barrier) =
                (server.clone(), clock.clone(), path.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                SyncEngine::open_with_clock(
                    SyncConfig {
                        device_id: format!("p{i}"),
                        store_path: Some(path),
                        ..SyncConfig::default()
                    },
                    server,
                    clock,
                )
            })
        })
        .collect();
    let results: Vec<_> = opens.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .all(|r| r.is_ok() || matches!(r, Err(Error::Conflict(_))))
    );
}
