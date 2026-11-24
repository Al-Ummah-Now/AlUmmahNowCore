#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use aun_downloads::{
    AutoDownloadRule, AvailabilitySyncResult, DEFAULT_MIN_FREE_SPACE_BYTES,
    DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES, DownloadError, DownloadRecord, DownloadSettings, DownloadStatus,
    DownloadStore, EpisodeCandidate, RevocationEntry, TransferDecision, UpdateOutcome, can_start_download,
    select_auto_downloads,
};
use aun_player::NetworkKind;
use tempfile::tempdir;

fn sample_record(user_id: &str, video_id: &str, status: DownloadStatus, bytes: u64) -> DownloadRecord {
    DownloadRecord {
        video_id: video_id.into(),
        user_id: user_id.into(),
        rung: Some("480p".into()),
        title: format!("Lecture {video_id}"),
        channel_id: Some("ch-1".into()),
        channel_name: Some("Scholar".into()),
        thumbnail_path: Some(format!("/thumbs/{video_id}.jpg")),
        duration_seconds: 600,
        local_path: format!("/downloads/{user_id}/{video_id}.mp4"),
        bytes_downloaded: bytes,
        total_bytes: bytes,
        status,
        removed_reason: None,
        created_at_ms: 1_000,
        last_accessed_at_ms: 1_000,
        extra: BTreeMap::new(),
    }
}

fn permissive_settings() -> DownloadSettings {
    DownloadSettings {
        wifi_only: false,
        max_storage_bytes: None,
        min_free_space_bytes: 0,
        auto_download_enabled: false,
        auto_download_charging_only: false,
    }
}

fn reserve_ok(store: &mut DownloadStore, record: DownloadRecord) {
    let decision = store
        .try_reserve(record, &permissive_settings(), NetworkKind::Wifi, None)
        .unwrap();
    assert_eq!(decision, TransferDecision::Allow);
}

#[test]
fn rejects_remote_signed_empty_or_query_paths() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();
    let settings = permissive_settings();

    let mut bad_thumb = sample_record("u1", "v1", DownloadStatus::Completed, 1000);
    bad_thumb.thumbnail_path = Some("https://cdn.example/signed-thumb.jpg?sig=abc".into());
    assert!(matches!(
        store.try_reserve(bad_thumb, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));

    let mut bad_media = sample_record("u1", "v2", DownloadStatus::Completed, 1000);
    bad_media.local_path = "https://media.alummahnow.net/priv/tok/v2/r1/video.mp4".into();
    assert!(matches!(
        store.try_reserve(bad_media, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));

    let mut empty_path = sample_record("u1", "v3", DownloadStatus::Queued, 1000);
    empty_path.local_path = "   ".into();
    assert!(matches!(
        store.try_reserve(empty_path, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));

    let mut schemeless_url = sample_record("u1", "v4", DownloadStatus::Queued, 1000);
    schemeless_url.local_path = "//cdn.example/priv/tok/v4.mp4".into();
    assert!(matches!(
        store.try_reserve(schemeless_url, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));

    let mut query_path = sample_record("u1", "v5", DownloadStatus::Queued, 1000);
    query_path.local_path = "/local/v5.mp4?token=abc".into();
    assert!(matches!(
        store.try_reserve(query_path, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));

    let removed_input = sample_record("u1", "v6", DownloadStatus::Removed, 1000);
    assert!(matches!(
        store.try_reserve(removed_input, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));
}

#[test]
fn probe_a_and_b_late_progress_or_completion_never_revives_revoked_download() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    let mut store = DownloadStore::open(&manifest).unwrap();

    // Probe A: Downloading -> apply_revocations -> late update_progress / mark_completed
    // must NOT revive the Removed record; try_reserve over unconfirmed Removed must return Conflict.
    let mut in_flight = sample_record("u1", "v-csam", DownloadStatus::Downloading, 5_000);
    in_flight.bytes_downloaded = 1_000;
    reserve_ok(&mut store, in_flight);

    let to_delete = store
        .apply_revocations(&[RevocationEntry {
            seq: 5,
            video_id: "v-csam".into(),
        }])
        .unwrap();
    assert_eq!(to_delete.len(), 1);
    assert!(!store.is_playable_offline("u1", "v-csam"));

    assert_eq!(
        store
            .update_progress("u1", "v-csam", 4_000, 5_000, 2_000)
            .unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store.mark_completed("u1", "v-csam", 5_000, 2_500).unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store.mark_paused("u1", "v-csam", 2_600).unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert!(!store.is_playable_offline("u1", "v-csam"));
    assert_eq!(store.pending_deletions().len(), 1);
    assert_eq!(store.get("u1", "v-csam").unwrap().status, DownloadStatus::Removed);

    // try_reserve over an unconfirmed Removed record refuses with Conflict.
    let retry_rec = sample_record("u1", "v-csam", DownloadStatus::Queued, 5_000);
    assert!(matches!(
        store.try_reserve(retry_rec, &permissive_settings(), NetworkKind::Wifi, None),
        Err(DownloadError::Conflict(_))
    ));

    // Probe B: After confirm_removed + commit_revocations_cursor(5), a late progress or completion
    // callback from an already-running transfer MUST still return RemovedOrMissing and never revive.
    assert!(store.confirm_removed("u1", "v-csam").unwrap());
    store.commit_revocations_cursor(5).unwrap();
    assert_eq!(store.revocations_cursor(), 5);

    assert_eq!(
        store
            .update_progress("u1", "v-csam", 4_500, 5_000, 3_000)
            .unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store.mark_completed("u1", "v-csam", 5_000, 3_500).unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert!(!store.is_playable_offline("u1", "v-csam"));
    assert!(store.get("u1", "v-csam").is_none());

    // Reopen confirms nothing was revived on disk.
    drop(store);
    let reopened = DownloadStore::open(&manifest).unwrap();
    assert_eq!(reopened.revocations_cursor(), 5);
    assert!(!reopened.is_playable_offline("u1", "v-csam"));
    assert!(reopened.list().is_empty());
}

#[test]
fn exclusive_lock_refuses_second_concurrent_open() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    let store1 = DownloadStore::open(&manifest).unwrap();

    let second = DownloadStore::open(&manifest);
    assert!(matches!(second, Err(DownloadError::Conflict(_))));

    drop(store1);
    let store2 = DownloadStore::open(&manifest).unwrap();
    assert_eq!(store2.revocations_cursor(), 0);
}

#[test]
fn isolates_accounts_and_uses_crash_safe_two_phase_removal() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    let mut store = DownloadStore::open(&manifest).unwrap();

    reserve_ok(
        &mut store,
        sample_record("u1", "v1", DownloadStatus::Completed, 1_000),
    );
    reserve_ok(
        &mut store,
        sample_record("u2", "v1", DownloadStatus::Completed, 2_000),
    );
    reserve_ok(
        &mut store,
        sample_record("u1", "v2", DownloadStatus::Completed, 3_000),
    );

    assert!(store.has_local_downloads());
    assert!(store.is_playable_offline("u1", "v1"));
    assert!(store.is_playable_offline("u2", "v1"));
    assert_eq!(store.list_for_user("u1").len(), 2);
    assert_eq!(store.list_for_user("u2").len(), 1);
    assert_eq!(store.active_count_for_user("u1"), 2);

    // Phase 1 removal for u1 must not touch u2's copy, hides u1/v1 from UI lists & playback,
    // and keeps u1/v1 in pending_deletions across a simulated crash until confirm_removed.
    let marked = store.remove("u1", "v1").unwrap().unwrap();
    assert_eq!(marked.status, DownloadStatus::Removed);
    assert!(!store.is_playable_offline("u1", "v1"));
    assert!(store.is_playable_offline("u2", "v1"));
    assert_eq!(store.list_for_user("u1").len(), 1);
    assert_eq!(store.pending_deletions().len(), 1);

    // Simulate crash before deleting u1/v1 on disk: release handle and reopen.
    drop(store);
    let mut reopened = DownloadStore::open(&manifest).unwrap();
    assert_eq!(reopened.pending_deletions().len(), 1);
    assert_eq!(reopened.pending_deletions()[0].user_id, "u1");
    assert!(!reopened.is_playable_offline("u1", "v1"));

    // Phase 2: after deleting file on disk, confirm_removed drops it from the manifest.
    assert!(reopened.confirm_removed("u1", "v1").unwrap());
    assert!(reopened.pending_deletions().is_empty());
    assert!(reopened.get("u1", "v1").is_none());

    // Active records cannot be dropped via confirm_removed directly.
    assert!(!reopened.confirm_removed("u2", "v1").unwrap());
    assert!(reopened.is_playable_offline("u2", "v1"));

    // Availability sync marks unavailable videos Removed across accounts (Phase 1).
    let to_delete = reopened
        .apply_availability_sync(&[
            AvailabilitySyncResult {
                video_id: "v1".into(),
                available: false,
                reason: Some("unavailable".into()),
            },
            AvailabilitySyncResult {
                video_id: "v2".into(),
                available: true,
                reason: None,
            },
        ])
        .unwrap();
    assert_eq!(to_delete.len(), 1);
    assert_eq!(to_delete[0].user_id, "u2");
    assert_eq!(to_delete[0].status, DownloadStatus::Removed);
    assert_eq!(to_delete[0].removed_reason.as_deref(), Some("unavailable"));
    assert!(!reopened.is_playable_offline("u2", "v1"));
    assert!(reopened.is_playable_offline("u1", "v2"));

    // Confirm Phase 2 batch deletion.
    let dropped = reopened
        .confirm_removed_batch(&[("u2".into(), "v1".into())])
        .unwrap();
    assert_eq!(dropped, 1);

    drop(reopened);
    let reopened2 = DownloadStore::open(&manifest).unwrap();
    assert_eq!(reopened2.list().len(), 1);
    assert!(reopened2.is_playable_offline("u1", "v2"));
}

#[test]
fn revocations_feed_purges_across_accounts_and_commits_cursor_only_after_confirm() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    let mut store = DownloadStore::open(&manifest).unwrap();
    assert_eq!(store.revocations_cursor(), 0);
    assert!(!store.has_local_downloads());

    reserve_ok(
        &mut store,
        sample_record("u1", "v-csam", DownloadStatus::Completed, 4_000),
    );
    reserve_ok(
        &mut store,
        sample_record("u2", "v-csam", DownloadStatus::Completed, 4_000),
    );
    reserve_ok(
        &mut store,
        sample_record("u1", "v-ok", DownloadStatus::Completed, 2_000),
    );

    // Apply revocations page: marks matching videos Removed across all accounts, does NOT advance cursor.
    let to_delete = store
        .apply_revocations(&[
            RevocationEntry {
                seq: 11,
                video_id: "v-not-local".into(),
            },
            RevocationEntry {
                seq: 12,
                video_id: "v-csam".into(),
            },
        ])
        .unwrap();
    assert_eq!(to_delete.len(), 2);
    assert!(!store.is_playable_offline("u1", "v-csam"));
    assert!(!store.is_playable_offline("u2", "v-csam"));
    assert!(store.is_playable_offline("u1", "v-ok"));
    assert_eq!(
        store.revocations_cursor(),
        0,
        "cursor must not advance before file deletion"
    );

    // Simulate crash before file deletion completes: reopen still has cursor 0 and 2 pending deletions.
    drop(store);
    let mut recovered = DownloadStore::open(&manifest).unwrap();
    assert_eq!(recovered.revocations_cursor(), 0);
    assert_eq!(recovered.pending_deletions().len(), 2);

    // Confirm file deletions, then commit cursor monotonically.
    let keys: Vec<(String, String)> = to_delete
        .iter()
        .map(|r| (r.user_id.clone(), r.video_id.clone()))
        .collect();
    assert_eq!(recovered.confirm_removed_batch(&keys).unwrap(), 2);
    recovered.commit_revocations_cursor(12).unwrap();
    // Stale lower cursor must never move backwards.
    recovered.commit_revocations_cursor(5).unwrap();
    assert_eq!(recovered.revocations_cursor(), 12);

    drop(recovered);
    let final_store = DownloadStore::open(&manifest).unwrap();
    assert_eq!(final_store.revocations_cursor(), 12);
    assert_eq!(final_store.list().len(), 1);
    assert!(final_store.pending_deletions().is_empty());
}

#[test]
fn quarantine_zero_byte_versioning_salvage_and_disk_reconciliation() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");

    // 1) Corrupt JSON is quarantined to a unique .corrupt.<ts> file, keeps has_local_downloads()
    //    true so revocation polling continues, and flags needs_disk_reconciliation().
    std::fs::write(&manifest, b"{not valid json").unwrap();
    let mut store = DownloadStore::open(&manifest).unwrap();
    assert!(store.needs_disk_reconciliation());
    assert!(
        store.has_local_downloads(),
        "must keep polling revocations & sync until disk reconciliation completes"
    );
    assert_eq!(store.revocations_cursor(), 0);

    // Reconcile orphaned files on disk, then mark_reconciled clears the flag.
    let orphans =
        store.reconcile_orphaned_paths(&["/downloads/u1/orphan.mp4".into(), "/thumbs/orphan.jpg".into()]);
    assert_eq!(orphans.len(), 2);
    store.mark_reconciled().unwrap();
    assert!(!store.needs_disk_reconciliation());
    assert!(!store.has_local_downloads());
    drop(store);

    // 2) 0-byte manifest (crash-truncated) is ALSO quarantined to a distinct file, never silently empty.
    std::fs::write(&manifest, b"").unwrap();
    let store_zero = DownloadStore::open(&manifest).unwrap();
    assert!(store_zero.needs_disk_reconciliation());
    assert!(store_zero.has_local_downloads());
    drop(store_zero);

    let corrupt_files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt."))
        .collect();
    assert_eq!(
        corrupt_files.len(),
        2,
        "second quarantine must get a unique filename without clobbering the first"
    );

    // 3) Future manifest version (99) is refused with Storage error and NOT quarantined.
    let future_json = r#"{"version":99,"revocations_cursor":10,"records":{}}"#;
    std::fs::write(&manifest, future_json).unwrap();
    assert!(matches!(
        DownloadStore::open(&manifest),
        Err(DownloadError::Storage(_))
    ));
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), future_json);

    // 4) Partial record corruption + unknown top-level & per-record fields:
    //    valid record is salvaged, unknown fields are preserved on save, reconciliation is flagged.
    let partial_json = r#"{
        "version": 4,
        "revocations_cursor": 7,
        "future_top_field": "keep-me",
        "records": {
            "u1\u001fv-good": {
                "video_id": "v-good",
                "user_id": "u1",
                "rung": "480p",
                "title": "Good",
                "channel_id": "ch-1",
                "channel_name": "Scholar",
                "thumbnail_path": "/thumbs/v-good.jpg",
                "duration_seconds": 300,
                "local_path": "/downloads/u1/v-good.mp4",
                "bytes_downloaded": 500,
                "total_bytes": 500,
                "status": "completed",
                "removed_reason": null,
                "created_at_ms": 100,
                "last_accessed_at_ms": 100,
                "future_rec_field": 123
            },
            "u1\u001fv-bad": {
                "video_id": "v-bad",
                "status": "not_a_real_status"
            }
        }
    }"#;
    std::fs::write(&manifest, partial_json).unwrap();
    let mut salvaged = DownloadStore::open(&manifest).unwrap();
    assert_eq!(salvaged.revocations_cursor(), 7);
    assert!(salvaged.needs_disk_reconciliation());
    assert!(salvaged.is_playable_offline("u1", "v-good"));
    assert_eq!(salvaged.list().len(), 1);
    let orphans2 = salvaged.reconcile_orphaned_paths(&[
        "/downloads/u1/v-good.mp4".into(),
        "/thumbs/v-good.jpg".into(),
        "/downloads/u1/v-bad.mp4".into(),
    ]);
    assert_eq!(orphans2, vec!["/downloads/u1/v-bad.mp4".to_string()]);
    salvaged.mark_reconciled().unwrap();
    drop(salvaged);

    let saved_text = std::fs::read_to_string(&manifest).unwrap();
    assert!(saved_text.contains("\"future_top_field\""));
    assert!(saved_text.contains("\"future_rec_field\""));
}

#[cfg(unix)]
#[test]
fn failed_save_rolls_back_in_memory_state_so_retry_persists() {
    let dir = tempdir().unwrap();
    let subdir = dir.path().join("readonly-test");
    std::fs::create_dir_all(&subdir).unwrap();
    let manifest = subdir.join("manifest.json");
    let mut store = DownloadStore::open(&manifest).unwrap();

    reserve_ok(
        &mut store,
        sample_record("u1", "v1", DownloadStatus::Completed, 2_000),
    );
    assert!(store.is_playable_offline("u1", "v1"));

    // Make parent directory read-only so temp-file creation fails during persist_manifest.
    std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let err = store.apply_revocations(&[RevocationEntry {
        seq: 1,
        video_id: "v1".into(),
    }]);
    assert!(err.is_err(), "persist into read-only dir must fail");
    // In-memory state MUST have rolled back so the record is still Completed (not mutated without disk save).
    assert!(store.is_playable_offline("u1", "v1"));
    assert!(store.pending_deletions().is_empty());

    // Restore write permission; retry MUST succeed and persist Removed to disk.
    std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let to_delete = store
        .apply_revocations(&[RevocationEntry {
            seq: 1,
            video_id: "v1".into(),
        }])
        .unwrap();
    assert_eq!(to_delete.len(), 1);
    assert!(!store.is_playable_offline("u1", "v1"));

    drop(store);
    let reopened = DownloadStore::open(&manifest).unwrap();
    assert!(!reopened.is_playable_offline("u1", "v1"));
    assert_eq!(reopened.pending_deletions().len(), 1);
}

#[test]
fn budget_has_no_default_cap_never_double_counts_resumes_and_estimates_unknown_size() {
    // E14-D2: default settings have NO fixed storage cap, only free-space reserve.
    let defaults = DownloadSettings::default();
    assert_eq!(defaults.max_storage_bytes, None);
    assert_eq!(defaults.min_free_space_bytes, DEFAULT_MIN_FREE_SPACE_BYTES);

    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();
    let settings = DownloadSettings {
        wifi_only: true,
        max_storage_bytes: Some(2_000),
        min_free_space_bytes: 500,
        auto_download_enabled: false,
        auto_download_charging_only: true,
    };

    // Queue a 1,200-byte item under a 2,000-byte cap.
    let mut queued = sample_record("u1", "v-queued", DownloadStatus::Queued, 0);
    queued.total_bytes = 1_200;
    assert_eq!(
        store
            .try_reserve(queued.clone(), &settings, NetworkKind::Wifi, Some(5_000))
            .unwrap(),
        TransferDecision::Allow
    );
    assert_eq!(store.total_bytes(), 1_200);

    // Resuming/starting the same 1,200-byte item via try_reserve MUST exclude its own reservation
    // (1,200 <= 2,000 -> Allow, NOT 2,400 > 2,000 -> StorageBudgetExceeded).
    let mut downloading = queued;
    downloading.status = DownloadStatus::Downloading;
    assert_eq!(
        store
            .try_reserve(downloading, &settings, NetworkKind::Wifi, Some(5_000))
            .unwrap(),
        TransferDecision::Allow
    );

    // Another 1,000-byte item exceeds the 2,000-byte cap (1,200 + 1,000 = 2,200 > 2,000).
    let second = sample_record("u1", "v-second", DownloadStatus::Queued, 1_000);
    assert_eq!(
        store
            .try_reserve(second, &settings, NetworkKind::Wifi, Some(5_000))
            .unwrap(),
        TransferDecision::StorageBudgetExceeded
    );

    // Unknown-size item (total_bytes = 0) reserves DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES (100 MiB),
    // never 0 bytes, so a 2,000-byte cap or low available disk space rejects it.
    let unknown = sample_record("u1", "v-unknown", DownloadStatus::Queued, 0);
    assert_eq!(unknown.reserved_bytes(), DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES);
    assert_eq!(
        store
            .try_reserve(unknown, &settings, NetworkKind::Wifi, Some(5_000))
            .unwrap(),
        TransferDecision::StorageBudgetExceeded
    );

    // Free-space reserve check when max_storage_bytes is None:
    // available 600 < item 200 + min_free_space 500 -> StorageBudgetExceeded.
    let uncapped = DownloadSettings {
        max_storage_bytes: None,
        min_free_space_bytes: 500,
        ..settings
    };
    assert_eq!(
        can_start_download(&uncapped, NetworkKind::Wifi, 0, 200, Some(600)),
        TransferDecision::StorageBudgetExceeded
    );
    assert_eq!(
        can_start_download(&uncapped, NetworkKind::Wifi, 0, 200, Some(700)),
        TransferDecision::Allow
    );
}

#[test]
fn never_silently_evicts_active_user_downloads() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();

    reserve_ok(
        &mut store,
        sample_record("u1", "v-active", DownloadStatus::Completed, 500),
    );
    reserve_ok(
        &mut store,
        sample_record("u1", "v-failed", DownloadStatus::Failed, 400),
    );

    let evicted = store.evict_to_budget(600).unwrap();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].video_id, "v-failed");
    assert_eq!(evicted[0].status, DownloadStatus::Removed);
    assert!(store.confirm_removed("u1", "v-failed").unwrap());
    assert!(store.get("u1", "v-active").is_some());

    // Even if budget is lower than active download size, active downloads are NEVER evicted.
    let evicted_again = store.evict_to_budget(100).unwrap();
    assert!(evicted_again.is_empty());
    assert!(store.get("u1", "v-active").is_some());
}

#[test]
fn transfer_policy_enforces_wifi_and_local_storage_budget() {
    let settings = DownloadSettings {
        wifi_only: true,
        max_storage_bytes: Some(10_000),
        min_free_space_bytes: 0,
        auto_download_enabled: true,
        auto_download_charging_only: true,
    };

    assert_eq!(
        can_start_download(&settings, NetworkKind::Offline, 0, 1_000, None),
        TransferDecision::Offline
    );
    assert_eq!(
        can_start_download(&settings, NetworkKind::Cellular, 0, 1_000, None),
        TransferDecision::WaitForWifi
    );
    assert_eq!(
        can_start_download(&settings, NetworkKind::Wifi, 9_500, 1_000, None),
        TransferDecision::StorageBudgetExceeded
    );
    assert_eq!(
        can_start_download(&settings, NetworkKind::Wifi, 5_000, 1_000, None),
        TransferDecision::Allow
    );
}

#[test]
fn auto_download_caps_per_channel_total_respects_dismissed_and_budget() {
    let rules = vec![AutoDownloadRule {
        source_id: "ch-1".into(),
        max_episodes: 2,
    }];
    let candidates: Vec<EpisodeCandidate> = (0..8)
        .map(|i| EpisodeCandidate {
            video_id: format!("v{i}"),
            source_id: "ch-1".into(),
            published_at_ms: 1_000 - (i as i64) * 10,
            allow_downloads: i != 0, // v0 opted out by creator
            estimated_bytes: Some(1_000),
        })
        .collect();

    let strict = DownloadSettings {
        wifi_only: true,
        max_storage_bytes: Some(10_000),
        min_free_space_bytes: 0,
        auto_download_enabled: true,
        auto_download_charging_only: true,
    };

    // Disabled / cellular / offline / not charging -> empty.
    let disabled = DownloadSettings {
        auto_download_enabled: false,
        ..strict.clone()
    };
    let empty: HashSet<String> = HashSet::new();
    assert!(
        select_auto_downloads(
            &disabled,
            NetworkKind::Wifi,
            true,
            0,
            None,
            &rules,
            &candidates,
            &empty,
            &empty
        )
        .is_empty()
    );
    assert!(
        select_auto_downloads(
            &strict,
            NetworkKind::Cellular,
            true,
            0,
            None,
            &rules,
            &candidates,
            &empty,
            &empty
        )
        .is_empty()
    );
    assert!(
        select_auto_downloads(
            &strict,
            NetworkKind::Wifi,
            false,
            0,
            None,
            &rules,
            &candidates,
            &empty,
            &empty
        )
        .is_empty()
    );

    // 4 consecutive poll passes with max_episodes = 2 MUST NOT drain the archive:
    // Pass 1 picks newest 2 downloadable episodes (v1, v2); passes 2-4 return empty.
    let mut existing: HashSet<String> = HashSet::new();
    let mut dismissed: HashSet<String> = HashSet::new();
    let pass1 = select_auto_downloads(
        &strict,
        NetworkKind::Wifi,
        true,
        0,
        None,
        &rules,
        &candidates,
        &existing,
        &dismissed,
    );
    assert_eq!(pass1, vec!["v1".to_string(), "v2".to_string()]);
    existing.extend(pass1);

    for _ in 0..3 {
        let next = select_auto_downloads(
            &strict,
            NetworkKind::Wifi,
            true,
            2_000,
            None,
            &rules,
            &candidates,
            &existing,
            &dismissed,
        );
        assert!(
            next.is_empty(),
            "repeated polls must not download older archive episodes beyond max_episodes"
        );
    }

    // If user deletes v1 and adds it to dismissed_video_ids, v1 is NEVER re-picked.
    existing.remove("v1");
    dismissed.insert("v1".into());
    let after_dismiss = select_auto_downloads(
        &strict,
        NetworkKind::Wifi,
        true,
        1_000,
        None,
        &rules,
        &candidates,
        &existing,
        &dismissed,
    );
    assert!(after_dismiss.is_empty());

    // Tight storage budget stops before overshooting (only 1,500 bytes left -> picks only v1).
    let tight_budget = select_auto_downloads(
        &strict,
        NetworkKind::Wifi,
        true,
        8_500,
        None,
        &rules,
        &candidates,
        &empty,
        &empty,
    );
    assert_eq!(tight_budget, vec!["v1".to_string()]);
}
