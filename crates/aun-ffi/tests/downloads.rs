#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::thread;

use aun_core::{
    ApiClient, ApiClientConfig, AunError, OfflineAutoDownloadPlan, OfflineAutoDownloadRule,
    OfflineAutoDownloadSource, OfflineAutoSourceKind, OfflineAvailabilityResult, OfflineDownloadItem,
    OfflineDownloadKey, OfflineDownloadOrigin, OfflineDownloadSettings, OfflineDownloadStatus,
    OfflineDownloadStore, OfflineEpisodeCandidate, OfflineExistingAutoDownload, OfflineNetworkKind,
    OfflineRevocationEntry, OfflineSyncEngine, OfflineTransferDecision, OfflineUpdateOutcome, PlayerCommand,
    PlayerInput, PlayerMachine, PlayerMediaItem, PlayerPlaybackState, PlayerSleepTimer, SyncChangeKind,
    SyncChangeListener, TokenSource, offline_can_start_download, offline_default_download_settings,
    offline_select_auto_downloads,
};
use tempfile::tempdir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_item(
    user_id: &str,
    video_id: &str,
    status: OfflineDownloadStatus,
    bytes: u64,
) -> OfflineDownloadItem {
    OfflineDownloadItem {
        video_id: video_id.into(),
        user_id: user_id.into(),
        rung: Some("480p".into()),
        title: format!("Surah {video_id}"),
        channel_id: Some("ch-1".into()),
        channel_name: Some("Recitations".into()),
        thumbnail_path: Some(format!("thumbs/{user_id}/{video_id}.jpg")),
        duration_seconds: 600,
        local_path: format!("downloads/{user_id}/{video_id}.mp4"),
        bytes_downloaded: 0,
        total_bytes: bytes,
        status,
        removed_reason: None,
        created_at_ms: 1_000,
        last_accessed_at_ms: 1_000,
        reservation_id: 0,
        origin: OfflineDownloadOrigin::Manual,
        auto_source: None,
        published_at_ms: None,
    }
}

#[test]
fn ffi_download_store_revocations_non_revival_reconciliation_and_policy() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("manifest.json");
    let manifest_str = manifest_path.to_string_lossy().to_string();
    let download_root = "/var/mobile/Containers/Data/Application/CURRENT-UUID/Documents";

    // Default settings have NO fixed storage cap (E14-D2).
    let defaults = offline_default_download_settings();
    assert_eq!(defaults.max_storage_bytes, None);
    assert!(defaults.min_free_space_bytes > 0);

    let store = OfflineDownloadStore::open(manifest_str.clone()).unwrap();
    assert!(!store.has_local_downloads().unwrap());
    assert!(!store.needs_disk_reconciliation().unwrap());
    assert_eq!(store.revocations_cursor().unwrap(), 0);

    // Second concurrent open on the same manifest fails with Conflict.
    assert!(matches!(
        OfflineDownloadStore::open(manifest_str.clone()),
        Err(AunError::Conflict { .. })
    ));

    let settings = OfflineDownloadSettings {
        wifi_only: true,
        max_storage_bytes: Some(1_200),
        min_free_space_bytes: 0,
        auto_download_enabled: true,
        auto_download_charging_only: true,
    };

    // Reserve a Downloading item, update progress with its reservation_id, then revoke it:
    // late progress/completion MUST NOT revive it.
    let in_flight = sample_item("user-1", "vid-1", OfflineDownloadStatus::Downloading, 1_024);
    let reserve1 = store
        .try_reserve(in_flight, settings.clone(), OfflineNetworkKind::Wifi, None)
        .unwrap();
    assert_eq!(reserve1.decision, OfflineTransferDecision::Allow);
    let res_id = reserve1.reservation_id.expect("Allow returns reservation_id");

    assert!(store.has_local_downloads().unwrap());
    assert_eq!(store.active_count_for_user("user-1".into()).unwrap(), 1);
    assert_eq!(store.pending_remaining_bytes().unwrap(), 1_024);
    assert_eq!(
        store
            .update_progress("user-1".into(), "vid-1".into(), res_id, 512, 1_024, 1_500)
            .unwrap(),
        OfflineUpdateOutcome::Updated
    );
    assert_eq!(store.pending_remaining_bytes().unwrap(), 512);

    // Wrong reservation_id returns StaleReservation without mutating state.
    assert_eq!(
        store
            .update_progress("user-1".into(), "vid-1".into(), res_id + 99, 900, 1_024, 1_600)
            .unwrap(),
        OfflineUpdateOutcome::StaleReservation
    );

    let decision = offline_can_start_download(
        settings.clone(),
        OfflineNetworkKind::Wifi,
        1_024,
        512,
        512,
        0,
        None,
    );
    assert_eq!(decision, OfflineTransferDecision::StorageBudgetExceeded);

    // Auto-download enforces Wi-Fi + charging + dismissed + budget guards and returns evictions.
    let ch1 = OfflineAutoDownloadSource {
        kind: OfflineAutoSourceKind::Channel,
        id: "ch-1".into(),
    };
    let rules = vec![OfflineAutoDownloadRule {
        source: ch1.clone(),
        max_episodes: 1,
    }];
    let candidates = vec![OfflineEpisodeCandidate {
        video_id: "vid-2".into(),
        source: ch1.clone(),
        published_at_ms: 2_000,
        allow_downloads: true,
        estimated_bytes: Some(150),
    }];
    let existing_auto = vec![OfflineExistingAutoDownload {
        video_id: "vid-old".into(),
        source: ch1,
        published_at_ms: 1_000,
        size_bytes: 150,
    }];
    assert!(
        offline_select_auto_downloads(
            settings.clone(),
            OfflineNetworkKind::Wifi,
            false,
            1_024,
            0,
            None,
            rules.clone(),
            candidates.clone(),
            vec![],
            vec![],
            vec![],
        )
        .to_download
        .is_empty()
    );
    assert_eq!(
        offline_select_auto_downloads(
            settings.clone(),
            OfflineNetworkKind::Wifi,
            true,
            1_024,
            0,
            None,
            rules.clone(),
            candidates.clone(),
            vec!["vid-old".into()],
            existing_auto.clone(),
            vec![],
        ),
        OfflineAutoDownloadPlan {
            to_download: vec!["vid-2".to_string()],
            to_evict: vec!["vid-old".to_string()],
        }
    );
    assert!(
        offline_select_auto_downloads(
            settings.clone(),
            OfflineNetworkKind::Wifi,
            true,
            1_024,
            0,
            None,
            rules,
            candidates,
            vec![],
            vec![],
            vec!["vid-2".into()],
        )
        .to_download
        .is_empty()
    );

    // Revocations feed marks Removed (Phase 1) without advancing cursor until confirmed.
    let revoked = store
        .apply_revocations(vec![OfflineRevocationEntry {
            seq: 42,
            video_id: "vid-1".into(),
        }])
        .unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(store.revocations_cursor().unwrap(), 0);
    assert_eq!(store.pending_deletions().unwrap().len(), 1);

    // Probe A via FFI: late update_progress / mark_completed return RemovedOrMissing.
    assert_eq!(
        store
            .update_progress("user-1".into(), "vid-1".into(), res_id, 900, 1_024, 2_000)
            .unwrap(),
        OfflineUpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store
            .mark_completed("user-1".into(), "vid-1".into(), res_id, 1_024, 2_100)
            .unwrap(),
        OfflineUpdateOutcome::RemovedOrMissing
    );
    assert!(
        !store
            .is_playable_offline("user-1".into(), "vid-1".into())
            .unwrap()
    );

    // Phase 2 confirmation + monotonic cursor commit + Probe B via FFI.
    assert_eq!(
        store
            .confirm_removed_batch(vec![OfflineDownloadKey {
                user_id: "user-1".into(),
                video_id: "vid-1".into(),
            }])
            .unwrap(),
        1
    );
    store.commit_revocations_cursor(42).unwrap();
    assert_eq!(store.revocations_cursor().unwrap(), 42);
    assert!(store.pending_deletions().unwrap().is_empty());
    assert_eq!(
        store
            .mark_completed("user-1".into(), "vid-1".into(), res_id, 1_024, 2_200)
            .unwrap(),
        OfflineUpdateOutcome::RemovedOrMissing
    );

    // Reserve -> complete -> update thumbnail -> container-safe reconciliation -> availability sync.
    let vid3 = sample_item("user-1", "vid-3", OfflineDownloadStatus::Downloading, 500);
    let reserve3 = store
        .try_reserve(vid3, settings, OfflineNetworkKind::Wifi, None)
        .unwrap();
    assert_eq!(reserve3.decision, OfflineTransferDecision::Allow);
    let res3 = reserve3.reservation_id.unwrap();
    assert_eq!(
        store
            .mark_completed("user-1".into(), "vid-3".into(), res3, 500, 2_300)
            .unwrap(),
        OfflineUpdateOutcome::Updated
    );
    assert_eq!(
        store
            .set_thumbnail_path("user-1".into(), "vid-3".into(), "thumbs/vid-3-v2.jpg".into())
            .unwrap(),
        OfflineUpdateOutcome::Updated
    );
    // Late pause/fail on Completed must return AlreadyTerminal and keep vid-3 playable.
    assert_eq!(
        store
            .mark_paused("user-1".into(), "vid-3".into(), res3, 2_400)
            .unwrap(),
        OfflineUpdateOutcome::AlreadyTerminal
    );
    assert!(
        store
            .is_playable_offline("user-1".into(), "vid-3".into())
            .unwrap()
    );

    let orphans = store
        .reconcile_orphaned_paths(
            download_root.into(),
            vec![
                format!("/private{download_root}/downloads/user-1/vid-3.mp4"),
                format!("{download_root}/thumbs/vid-3-v2.jpg"),
                format!("{download_root}/downloads/user-1/stray.mp4"),
            ],
        )
        .unwrap();
    assert_eq!(
        orphans,
        vec![format!("{download_root}/downloads/user-1/stray.mp4")]
    );

    let removed = store
        .apply_availability_sync(vec![OfflineAvailabilityResult {
            video_id: "vid-3".into(),
            available: false,
            reason: Some("unavailable".into()),
        }])
        .unwrap();
    assert_eq!(removed.len(), 1);
    assert!(store.confirm_removed("user-1".into(), "vid-3".into()).unwrap());
    store.mark_reconciled().unwrap();
}

#[test]
fn ffi_concurrent_try_reserve_enforces_budget_atomically() {
    let dir = tempdir().unwrap();
    let store = OfflineDownloadStore::open(dir.path().join("race.json").to_string_lossy().into()).unwrap();
    let settings = OfflineDownloadSettings {
        wifi_only: false,
        max_storage_bytes: Some(1_500),
        min_free_space_bytes: 0,
        auto_download_enabled: false,
        auto_download_charging_only: false,
    };

    // Spawn 6 threads each trying to reserve a 1,000-byte item under a 1,500-byte cap.
    // Exactly ONE must succeed; the other 5 must get StorageBudgetExceeded.
    let mut handles = Vec::new();
    for i in 0..6 {
        let store = store.clone();
        let settings = settings.clone();
        handles.push(thread::spawn(move || {
            let item = sample_item(
                "user-1",
                &format!("vid-{i}"),
                OfflineDownloadStatus::Queued,
                1_000,
            );
            store
                .try_reserve(item, settings, OfflineNetworkKind::Wifi, None)
                .unwrap()
        }));
    }

    let mut allowed = 0;
    let mut exceeded = 0;
    for h in handles {
        let res = h.join().unwrap();
        match res.decision {
            OfflineTransferDecision::Allow => {
                assert!(res.reservation_id.is_some());
                allowed += 1;
            }
            OfflineTransferDecision::StorageBudgetExceeded => {
                assert_eq!(res.reservation_id, None);
                exceeded += 1;
            }
            other => panic!("unexpected decision: {other:?}"),
        }
    }
    assert_eq!(allowed, 1);
    assert_eq!(exceeded, 5);
    assert_eq!(store.total_bytes().unwrap(), 1_000);
}

#[test]
fn ffi_player_machine_smoke() {
    let player = PlayerMachine::new();
    let cmds = player
        .handle(
            PlayerInput::LoadQueue {
                items: vec![PlayerMediaItem {
                    id: "vid-1".into(),
                    duration_ms: 120_000,
                    resume_position_ms: Some(40_000),
                    chapters: vec![],
                    is_live: false,
                }],
                start_index: 0,
                play_when_ready: true,
            },
            10_000,
        )
        .unwrap();
    assert!(cmds.iter().any(|c| matches!(
        c,
        PlayerCommand::Load {
            item_id,
            start_position_ms: 40_000,
            ..
        } if item_id == "vid-1"
    )));
    player
        .handle(
            PlayerInput::SetSleepTimer {
                timer: PlayerSleepTimer::EndOfItem,
            },
            10_000,
        )
        .unwrap();
    let end_cmds = player.handle(PlayerInput::Ended, 90_000).unwrap();
    assert!(
        end_cmds
            .iter()
            .any(|c| matches!(c, PlayerCommand::SleepTimerFired))
    );
    assert_eq!(player.snapshot().unwrap().state, PlayerPlaybackState::Ended);
}

struct RecordingListener {
    events: Mutex<Vec<SyncChangeKind>>,
}

impl SyncChangeListener for RecordingListener {
    fn on_changed(&self, changes: Vec<SyncChangeKind>) {
        self.events.lock().unwrap().extend(changes);
    }
}

#[test]
fn ffi_offline_sync_engine_full_surface_and_persistence() {
    let dir = tempdir().unwrap();
    let store_path = dir.path().join("sync.json").to_string_lossy().to_string();
    let client = ApiClient::new(
        ApiClientConfig {
            base_url: "http://127.0.0.1:9".into(),
            user_agent: Some("AunTest/1.0".into()),
            locale: Some("ar".into()),
            timeout_ms: Some(1_000),
            max_attempts: Some(1),
            client: None,
        },
        None,
    )
    .unwrap();

    let engine =
        OfflineSyncEngine::open(store_path.clone(), Some("dev-test".into()), client.clone()).unwrap();
    assert_eq!(engine.device_id(), "dev-test");

    // Second concurrent open on the same sync store returns Conflict.
    assert!(matches!(
        OfflineSyncEngine::open(store_path.clone(), Some("dev-2".into()), client.clone()),
        Err(AunError::Conflict { .. })
    ));

    let listener = Arc::new(RecordingListener {
        events: Mutex::new(Vec::new()),
    });
    let lid = engine.add_listener(listener.clone());

    // Watch state, history, continue watching, remove & clear.
    engine.record_position("vid-1".into(), 45_000, 120_000).unwrap();
    let ws = engine.watch_state("vid-1".into()).unwrap();
    assert_eq!(ws.position_ms, 45_000);
    assert!(!ws.completed);
    assert!(ws.in_history);
    assert_eq!(engine.resume_position_ms("vid-1".into()), Some(45_000));
    assert_eq!(engine.continue_watching(10).len(), 1);
    assert_eq!(engine.history(10, 0).len(), 1);

    engine.set_completed("vid-1".into(), true).unwrap();
    assert!(engine.watch_state("vid-1".into()).unwrap().completed);
    assert!(engine.continue_watching(10).is_empty());

    engine.remove_from_history("vid-1".into());
    assert!(engine.history(10, 0).is_empty());
    engine.record_position("vid-2".into(), 35_000, 100_000).unwrap();
    assert_eq!(engine.history(10, 0).len(), 1);
    engine.clear_history();
    assert!(engine.history(10, 0).is_empty());

    // Likes & Watch later (saved + reorder).
    engine.set_liked("vid-1".into(), true).unwrap();
    assert!(engine.is_liked("vid-1".into()));
    assert_eq!(engine.liked_items().len(), 1);

    engine.set_saved("vid-1".into(), true).unwrap();
    engine.set_saved("vid-2".into(), true).unwrap();
    engine.reorder_saved(vec!["vid-2".into(), "vid-1".into()]);
    let saved_ids: Vec<String> = engine.saved_items().into_iter().map(|i| i.video_id).collect();
    assert_eq!(saved_ids, vec!["vid-2".to_string(), "vid-1".to_string()]);
    assert!(engine.is_saved("vid-2".into()));

    // Timestamped study notes CRUD.
    let note_id = engine
        .add_note("vid-1".into(), 12_000, "Key ayah".into())
        .unwrap();
    engine
        .update_note_text(note_id.clone(), "Key ayah tafsir".into())
        .unwrap();
    engine.move_note(note_id.clone(), 15_000).unwrap();
    let notes = engine.notes_for_video("vid-1".into());
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].text, "Key ayah tafsir");
    assert_eq!(notes[0].position_ms, 15_000);
    assert_eq!(engine.all_notes(10).len(), 1);

    engine.delete_note(note_id).unwrap();
    assert!(engine.notes_for_video("vid-1".into()).is_empty());
    assert!(engine.pending_count() > 0);
    let _ = engine.next_due_ms();

    // Verify listener received all change categories, then unsubscribe.
    {
        let events = listener.events.lock().unwrap();
        assert!(events.contains(&SyncChangeKind::WatchState));
        assert!(events.contains(&SyncChangeKind::History));
        assert!(events.contains(&SyncChangeKind::Library));
        assert!(events.contains(&SyncChangeKind::Notes));
    }
    engine.remove_listener(lid);

    // Persist and reopen: saved & liked survive across restarts.
    engine.persist().unwrap();
    drop(engine);

    let reopened = OfflineSyncEngine::open(store_path, None, client).unwrap();
    assert_eq!(reopened.device_id(), "dev-test");
    assert!(reopened.is_liked("vid-1".into()));
    assert_eq!(reopened.saved_items().len(), 2);
}

struct StaticToken;

impl TokenSource for StaticToken {
    fn access_token(&self) -> Option<String> {
        Some("test-access-token".into())
    }
    fn refresh(&self) -> Result<Option<String>, AunError> {
        Ok(Some("test-access-token".into()))
    }
}

#[tokio::test]
async fn ffi_offline_sync_engine_flush_and_sync_over_http() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/watch-history/vid-local/progress"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "videoId": "vid-local",
            "progress": 42.0,
            "duration": 120.0,
            "percentageWatched": 35.0,
            "completed": false,
            "timestamp": "2026-11-23T00:00:00Z"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/videos/vid-local/likes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "liked": true
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/library/watch-later"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "saved": true
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/watch-history"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "entries": [{
                "id": "h-1",
                "userId": "u-1",
                "videoId": "vid-remote-hist",
                "videoTitle": "Tafsir Remote",
                "videoThumbnail": "https://cdn.example/t.jpg",
                "videoDuration": 300.0,
                "channelId": "ch-9",
                "channelName": "Remote Scholar",
                "firstWatchedAt": "2026-11-22T12:00:00Z",
                "lastWatchedAt": "2026-11-23T12:00:00Z",
                "watchCount": 1.0,
                "progress": 90.0,
                "completed": false,
                "percentageWatched": 30.0
            }],
            "total": 1.0,
            "hasMore": false
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/library/watch-later"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{
                "videoId": "vid-remote-saved",
                "addedAt": "2026-11-23T10:00:00Z",
                "title": "Saved Lecture"
            }],
            "hasMore": false
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/api/library/liked"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{
                "videoId": "vid-remote-liked",
                "addedAt": "2026-11-23T11:00:00Z",
                "title": "Liked Lecture"
            }],
            "hasMore": false
        })))
        .mount(&server)
        .await;

    let dir = tempdir().unwrap();
    let store_path = dir.path().join("sync-http.json").to_string_lossy().to_string();
    let client = ApiClient::new(
        ApiClientConfig {
            base_url: server.uri(),
            user_agent: Some("AunTest/1.0".into()),
            locale: Some("en".into()),
            timeout_ms: Some(5_000),
            max_attempts: Some(1),
            client: None,
        },
        Some(Arc::new(StaticToken)),
    )
    .unwrap();

    let engine = OfflineSyncEngine::open(store_path, Some("dev-http".into()), client).unwrap();

    // Queue local mutations offline, then flush them over HTTP.
    engine
        .record_position("vid-local".into(), 42_000, 120_000)
        .unwrap();
    engine.set_liked("vid-local".into(), true).unwrap();
    engine.set_saved("vid-local".into(), true).unwrap();
    assert_eq!(engine.pending_count(), 3);

    let flushed = engine.flush().await.unwrap();
    assert_eq!(flushed.pushed, 3);
    assert_eq!(flushed.pending, 0);
    assert_eq!(engine.pending_count(), 0);

    // Full sync pulls remote history, Watch later, and liked items and merges them locally.
    let synced = engine.sync().await.unwrap();
    assert_eq!(synced.history_entries_pulled, 1);
    assert_eq!(synced.saved_pulled, 1);
    assert_eq!(synced.liked_pulled, 1);
    assert_eq!(engine.resume_position_ms("vid-remote-hist".into()), Some(90_000));
    assert!(engine.is_saved("vid-remote-saved".into()));
    assert!(engine.is_liked("vid-remote-liked".into()));
}
