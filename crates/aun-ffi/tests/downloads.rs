#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::thread;

use aun_core::{
    ApiClient, ApiClientConfig, AunError, OfflineAutoDownloadRule, OfflineAvailabilityResult,
    OfflineDownloadItem, OfflineDownloadKey, OfflineDownloadSettings, OfflineDownloadStatus,
    OfflineDownloadStore, OfflineEpisodeCandidate, OfflineNetworkKind, OfflineRevocationEntry,
    OfflineSyncEngine, OfflineTransferDecision, OfflineUpdateOutcome, PlayerCommand, PlayerInput,
    PlayerMachine, PlayerMediaItem, PlayerPlaybackState, PlayerSleepTimer, SyncChangeKind,
    SyncChangeListener, offline_can_start_download, offline_default_download_settings,
    offline_select_auto_downloads,
};
use tempfile::tempdir;

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
        thumbnail_path: Some(format!("/local/{video_id}.jpg")),
        duration_seconds: 600,
        local_path: format!("/local/{user_id}/{video_id}.mp4"),
        bytes_downloaded: bytes,
        total_bytes: bytes,
        status,
        removed_reason: None,
        created_at_ms: 1_000,
        last_accessed_at_ms: 1_000,
    }
}

#[test]
fn ffi_download_store_revocations_non_revival_reconciliation_and_policy() {
    let dir = tempdir().unwrap();
    let manifest_path = dir.path().join("manifest.json");
    let manifest_str = manifest_path.to_string_lossy().to_string();

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

    // Reserve a Downloading item, update progress, then revoke it: late progress/completion MUST NOT revive it.
    let mut in_flight = sample_item("user-1", "vid-1", OfflineDownloadStatus::Downloading, 1_024);
    in_flight.bytes_downloaded = 256;
    assert_eq!(
        store
            .try_reserve(in_flight, settings.clone(), OfflineNetworkKind::Wifi, None)
            .unwrap(),
        OfflineTransferDecision::Allow
    );
    assert!(store.has_local_downloads().unwrap());
    assert_eq!(store.active_count_for_user("user-1".into()).unwrap(), 1);
    assert_eq!(
        store
            .update_progress("user-1".into(), "vid-1".into(), 512, 1_024, 1_500)
            .unwrap(),
        OfflineUpdateOutcome::Updated
    );

    let decision = offline_can_start_download(settings.clone(), OfflineNetworkKind::Wifi, 1024, 512, None);
    assert_eq!(decision, OfflineTransferDecision::StorageBudgetExceeded);

    // Auto-download enforces Wi-Fi + charging + dismissed + budget guards.
    let rules = vec![OfflineAutoDownloadRule {
        source_id: "ch-1".into(),
        max_episodes: 1,
    }];
    let candidates = vec![OfflineEpisodeCandidate {
        video_id: "vid-2".into(),
        source_id: "ch-1".into(),
        published_at_ms: 2_000,
        allow_downloads: true,
        estimated_bytes: Some(150),
    }];
    assert!(
        offline_select_auto_downloads(
            settings.clone(),
            OfflineNetworkKind::Wifi,
            false,
            1_024,
            None,
            rules.clone(),
            candidates.clone(),
            vec![],
            vec![],
        )
        .is_empty()
    );
    assert_eq!(
        offline_select_auto_downloads(
            settings.clone(),
            OfflineNetworkKind::Wifi,
            true,
            1_024,
            None,
            rules.clone(),
            candidates.clone(),
            vec![],
            vec![],
        ),
        vec!["vid-2".to_string()]
    );
    assert!(
        offline_select_auto_downloads(
            settings.clone(),
            OfflineNetworkKind::Wifi,
            true,
            1_024,
            None,
            rules,
            candidates,
            vec![],
            vec!["vid-2".into()],
        )
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
            .update_progress("user-1".into(), "vid-1".into(), 900, 1_024, 2_000)
            .unwrap(),
        OfflineUpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store
            .mark_completed("user-1".into(), "vid-1".into(), 1_024, 2_100)
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
            .mark_completed("user-1".into(), "vid-1".into(), 1_024, 2_200)
            .unwrap(),
        OfflineUpdateOutcome::RemovedOrMissing
    );

    // Availability sync smoke on a fresh record.
    let vid3 = sample_item("user-1", "vid-3", OfflineDownloadStatus::Completed, 500);
    assert_eq!(
        store
            .try_reserve(vid3, settings, OfflineNetworkKind::Wifi, None)
            .unwrap(),
        OfflineTransferDecision::Allow
    );
    assert!(
        store
            .is_playable_offline("user-1".into(), "vid-3".into())
            .unwrap()
    );
    let orphans = store
        .reconcile_orphaned_paths(vec![
            "/local/user-1/vid-3.mp4".into(),
            "/local/vid-3.jpg".into(),
            "/local/user-1/stray.mp4".into(),
        ])
        .unwrap();
    assert_eq!(orphans, vec!["/local/user-1/stray.mp4".to_string()]);

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
        match h.join().unwrap() {
            OfflineTransferDecision::Allow => allowed += 1,
            OfflineTransferDecision::StorageBudgetExceeded => exceeded += 1,
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
