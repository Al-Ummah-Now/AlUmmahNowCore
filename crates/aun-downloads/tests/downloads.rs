#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use aun_downloads::{
    AutoDownloadRule, AutoDownloadSource, AvailabilitySyncResult, DEFAULT_MIN_FREE_SPACE_BYTES,
    DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES, DownloadError, DownloadOrigin, DownloadRecord, DownloadSettings,
    DownloadStatus, DownloadStore, EpisodeCandidate, ExistingAutoDownload, PlannedAutoDownload,
    RevocationEntry, TransferDecision, UpdateOutcome, can_start_download, select_auto_downloads,
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
        origin: DownloadOrigin::Manual,
        auto_source: None,
        published_at_ms: None,
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

fn reserve_ok(store: &mut DownloadStore, record: DownloadRecord) -> u64 {
    let res = store
        .try_reserve(record, &permissive_settings(), NetworkKind::Wifi, None)
        .unwrap();
    assert_eq!(res.decision, TransferDecision::Allow);
    res.reservation_id.expect("Allow must assign reservation_id")
}

fn reserve_and_complete(store: &mut DownloadStore, user_id: &str, video_id: &str, bytes: u64) -> u64 {
    let rec = sample_record(user_id, video_id, DownloadStatus::Downloading, bytes);
    let res_id = reserve_ok(store, rec);
    assert_eq!(
        store
            .mark_completed(user_id, video_id, res_id, bytes, 1_500)
            .unwrap(),
        UpdateOutcome::Updated
    );
    res_id
}

#[test]
fn rejects_remote_signed_absolute_traversal_empty_or_invalid_status_inputs() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();
    let settings = permissive_settings();

    let mut bad_thumb = sample_record("u1", "v1", DownloadStatus::Queued, 1000);
    bad_thumb.thumbnail_path = Some("https://cdn.example/signed-thumb.jpg?sig=abc".into());
    assert!(matches!(
        store.try_reserve(bad_thumb, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));

    let mut bad_media = sample_record("u1", "v2", DownloadStatus::Queued, 1000);
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
    query_path.local_path = "downloads/v5.mp4?token=abc".into();
    assert!(matches!(
        store.try_reserve(query_path, &settings, NetworkKind::Wifi, None),
        Err(DownloadError::InvalidInput(_))
    ));

    // Absolute Unix / Windows / traversal paths must be rejected so iOS container moves stay safe.
    for bad in [
        "/downloads/u1/v6.mp4",
        "\\downloads\\u1\\v6.mp4",
        "C:\\downloads\\v6.mp4",
        "downloads/../secret.mp4",
        "./downloads/v6.mp4",
        "downloads/u1//v6.mp4",
    ] {
        let mut rec = sample_record("u1", "v6", DownloadStatus::Queued, 1000);
        rec.local_path = bad.into();
        assert!(
            matches!(
                store.try_reserve(rec, &settings, NetworkKind::Wifi, None),
                Err(DownloadError::InvalidInput(_))
            ),
            "expected InvalidInput for path {bad:?}"
        );
    }

    // try_reserve MUST refuse Completed and Removed status inputs.
    for bad_status in [
        DownloadStatus::Completed,
        DownloadStatus::Removed,
        DownloadStatus::Paused,
        DownloadStatus::Failed,
    ] {
        let rec = sample_record("u1", "v7", bad_status, 1000);
        assert!(
            matches!(
                store.try_reserve(rec, &settings, NetworkKind::Wifi, None),
                Err(DownloadError::InvalidInput(_))
            ),
            "expected InvalidInput for status {bad_status:?}"
        );
    }
}

#[test]
fn probe_a_b_and_stale_reservation_tokens_never_revive_or_corrupt_downloads() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    let mut store = DownloadStore::open(&manifest).unwrap();

    // Probe A: Downloading -> apply_revocations -> late update_progress / mark_completed
    // must NOT revive the Removed record; try_reserve over unconfirmed Removed must return Conflict.
    let mut in_flight = sample_record("u1", "v-csam", DownloadStatus::Downloading, 5_000);
    in_flight.bytes_downloaded = 1_000;
    let res1 = reserve_ok(&mut store, in_flight);

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
            .update_progress("u1", "v-csam", res1, 4_000, 5_000, 2_000)
            .unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store.mark_completed("u1", "v-csam", res1, 5_000, 2_500).unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store.mark_paused("u1", "v-csam", res1, 2_600).unwrap(),
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

    // Probe B: After confirm_removed + commit_revocations_cursor(5), late callbacks return RemovedOrMissing.
    assert!(store.confirm_removed("u1", "v-csam").unwrap());
    store.commit_revocations_cursor(5).unwrap();
    assert_eq!(store.revocations_cursor(), 5);

    assert_eq!(
        store
            .update_progress("u1", "v-csam", res1, 4_500, 5_000, 3_000)
            .unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert_eq!(
        store.mark_completed("u1", "v-csam", res1, 5_000, 3_500).unwrap(),
        UpdateOutcome::RemovedOrMissing
    );
    assert!(!store.is_playable_offline("u1", "v-csam"));
    assert!(store.get("u1", "v-csam").is_none());

    // Probe C (reservation token): Suppose the user deletes and re-downloads a video (getting res2 > res1).
    // The older transfer holding res1 MUST get StaleReservation and NEVER complete res2!
    let res2 = reserve_ok(
        &mut store,
        sample_record("u1", "v-csam", DownloadStatus::Downloading, 5_000),
    );
    assert!(res2 > res1);
    assert_eq!(
        store
            .update_progress("u1", "v-csam", res1, 4_900, 5_000, 4_000)
            .unwrap(),
        UpdateOutcome::StaleReservation
    );
    assert_eq!(
        store.mark_completed("u1", "v-csam", res1, 5_000, 4_100).unwrap(),
        UpdateOutcome::StaleReservation
    );
    assert!(!store.is_playable_offline("u1", "v-csam"));
    assert_eq!(store.get("u1", "v-csam").unwrap().bytes_downloaded, 0);

    // Reopen confirms next_reservation_id persists monotonically across restarts.
    drop(store);
    let mut reopened = DownloadStore::open(&manifest).unwrap();
    assert_eq!(reopened.revocations_cursor(), 5);
    let res3 = reserve_ok(
        &mut reopened,
        sample_record("u1", "v-other", DownloadStatus::Queued, 1_000),
    );
    assert!(res3 > res2);
}

#[test]
fn try_reserve_refuses_completed_overwrite_preserves_fields_on_resume_and_guards_late_events() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();

    // Start a download with extra metadata and make progress.
    let mut rec = sample_record("u1", "v1", DownloadStatus::Downloading, 2_000);
    rec.extra
        .insert("custom_key".into(), serde_json::json!("keep-me"));
    let res1 = reserve_ok(&mut store, rec);
    assert_eq!(
        store
            .update_progress("u1", "v1", res1, 900, 2_000, 2_000)
            .unwrap(),
        UpdateOutcome::Updated
    );

    // Pause it, then send a late progress/completion callback: MUST NOT un-pause back to Downloading!
    assert_eq!(
        store.mark_paused("u1", "v1", res1, 2_100).unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(
        store
            .update_progress("u1", "v1", res1, 1_000, 2_000, 2_200)
            .unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(
        store.mark_completed("u1", "v1", res1, 2_000, 2_300).unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(store.get("u1", "v1").unwrap().status, DownloadStatus::Paused);
    assert_eq!(store.get("u1", "v1").unwrap().bytes_downloaded, 900);

    // Resume via try_reserve: preserves stored local_path, bytes_downloaded, created_at_ms,
    // thumbnail_path, and extra, while issuing a new reservation_id (invalidating res1).
    let mut resume_req = sample_record("u1", "v1", DownloadStatus::Downloading, 2_000);
    resume_req.local_path = "downloads/u1/different-path.mp4".into();
    resume_req.bytes_downloaded = 0;
    resume_req.created_at_ms = 99_999;
    resume_req.thumbnail_path = None;
    let res2 = reserve_ok(&mut store, resume_req);
    assert!(res2 > res1);
    let resumed = store.get("u1", "v1").unwrap();
    assert_eq!(resumed.local_path, "downloads/u1/v1.mp4");
    assert_eq!(resumed.bytes_downloaded, 900);
    assert_eq!(resumed.created_at_ms, 1_000);
    assert_eq!(resumed.thumbnail_path.as_deref(), Some("thumbs/u1/v1.jpg"));
    assert_eq!(
        resumed.extra.get("custom_key"),
        Some(&serde_json::json!("keep-me"))
    );

    // Complete with res2, then verify Completed is terminal against late pause/fail/progress/overwrite!
    assert_eq!(
        store.mark_completed("u1", "v1", res2, 2_000, 3_000).unwrap(),
        UpdateOutcome::Updated
    );
    assert!(store.is_playable_offline("u1", "v1"));

    assert_eq!(
        store.mark_paused("u1", "v1", res2, 3_100).unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(
        store.mark_failed("u1", "v1", res2, 3_200).unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(
        store
            .update_progress("u1", "v1", res2, 1_999, 2_000, 3_300)
            .unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(
        store.mark_downloading("u1", "v1", res2, 3_400).unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert!(store.is_playable_offline("u1", "v1"));
    assert!(store.purge_inactive().unwrap().is_empty());

    // try_reserve MUST refuse to overwrite a Completed record.
    assert!(matches!(
        store.try_reserve(
            sample_record("u1", "v1", DownloadStatus::Queued, 2_000),
            &permissive_settings(),
            NetworkKind::Wifi,
            None,
        ),
        Err(DownloadError::Conflict(_))
    ));

    // set_thumbnail_path updates artwork path cleanly without try_reserve and rejects bad paths.
    assert_eq!(
        store.set_thumbnail_path("u1", "v1", "thumbs/v1-hd.jpg").unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(
        store.get("u1", "v1").unwrap().thumbnail_path.as_deref(),
        Some("thumbs/v1-hd.jpg")
    );
    assert!(matches!(
        store.set_thumbnail_path("u1", "v1", "/abs/bad.jpg"),
        Err(DownloadError::InvalidInput(_))
    ));
}

#[test]
fn free_space_reserve_counts_promised_bytes_resumes_fairly_and_keeps_unknown_estimate() {
    // E14-D2: default settings have NO fixed storage cap, only free-space reserve.
    let defaults = DownloadSettings::default();
    assert_eq!(defaults.max_storage_bytes, None);
    assert_eq!(defaults.min_free_space_bytes, DEFAULT_MIN_FREE_SPACE_BYTES);

    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();

    // Probe 1a: 1,000 bytes free, 256 reserve, five 600-byte items queued sequentially:
    // ONLY the 1st gets Allow (256 + 600 = 856 <= 1000); items 2-5 MUST be rejected because
    // item 1 already promised 600 remaining bytes from free disk space (256 + 600 + 600 = 1456 > 1000).
    let free_space_settings = DownloadSettings {
        wifi_only: false,
        max_storage_bytes: None,
        min_free_space_bytes: 256,
        auto_download_enabled: false,
        auto_download_charging_only: false,
    };
    let mut allowed = 0;
    for i in 0..5 {
        let rec = sample_record("u1", &format!("v-q{i}"), DownloadStatus::Queued, 600);
        let res = store
            .try_reserve(rec, &free_space_settings, NetworkKind::Wifi, Some(1_000))
            .unwrap();
        if res.decision == TransferDecision::Allow {
            allowed += 1;
        } else {
            assert_eq!(res.decision, TransferDecision::StorageBudgetExceeded);
            assert_eq!(res.reservation_id, None);
        }
    }
    assert_eq!(
        allowed, 1,
        "concurrent queued items must not oversubscribe free space"
    );
    assert_eq!(store.pending_remaining_bytes(), 600);

    // Probe 1b: Resuming an item with 900 of 1,000 bytes already on disk when 400 bytes are free
    // and reserve is 256 -> only 100 remaining bytes are needed (256 + 100 = 356 <= 400 -> Allow).
    let dir2 = tempdir().unwrap();
    let mut store2 = DownloadStore::open(dir2.path().join("manifest.json")).unwrap();
    let res1 = reserve_ok(
        &mut store2,
        sample_record("u1", "v-resume", DownloadStatus::Downloading, 1_000),
    );
    assert_eq!(
        store2
            .update_progress("u1", "v-resume", res1, 900, 1_000, 2_000)
            .unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(store2.pending_remaining_bytes(), 100);
    assert_eq!(
        store2.mark_paused("u1", "v-resume", res1, 2_100).unwrap(),
        UpdateOutcome::Updated
    );
    let resume_res = store2
        .try_reserve(
            sample_record("u1", "v-resume", DownloadStatus::Downloading, 1_000),
            &free_space_settings,
            NetworkKind::Wifi,
            Some(400),
        )
        .unwrap();
    assert_eq!(
        resume_res.decision,
        TransferDecision::Allow,
        "resumed item must not double-count already-written bytes against free space"
    );

    // Probe 1c: Unknown-size item (total_bytes == 0) MUST keep the 100 MiB estimate even after
    // the first progress tick (e.g. 1 byte downloaded, total still 0) until total is known!
    let dir3 = tempdir().unwrap();
    let mut store3 = DownloadStore::open(dir3.path().join("manifest.json")).unwrap();
    let unknown_rec = sample_record("u1", "v-unknown", DownloadStatus::Downloading, 0);
    assert_eq!(unknown_rec.reserved_bytes(), DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES);
    let res_u = reserve_ok(&mut store3, unknown_rec);
    assert_eq!(
        store3
            .update_progress("u1", "v-unknown", res_u, 1, 0, 2_000)
            .unwrap(),
        UpdateOutcome::Updated
    );
    let after_tick = store3.get("u1", "v-unknown").unwrap();
    assert_eq!(after_tick.reserved_bytes(), DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES);
    assert_eq!(
        after_tick.remaining_bytes(),
        DEFAULT_UNKNOWN_ITEM_ESTIMATE_BYTES - 1
    );
    // Once real total is learned (e.g. 500_000 bytes), reservation updates to real total.
    assert_eq!(
        store3
            .update_progress("u1", "v-unknown", res_u, 100_000, 500_000, 3_000)
            .unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(store3.get("u1", "v-unknown").unwrap().reserved_bytes(), 500_000);
    assert_eq!(store3.get("u1", "v-unknown").unwrap().remaining_bytes(), 400_000);
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

    reserve_and_complete(&mut store, "u1", "v1", 1_000);
    reserve_and_complete(&mut store, "u2", "v1", 2_000);
    reserve_and_complete(&mut store, "u1", "v2", 3_000);

    assert!(store.has_local_downloads());
    assert!(store.is_playable_offline("u1", "v1"));
    assert!(store.is_playable_offline("u2", "v1"));
    assert_eq!(store.list_for_user("u1").len(), 2);
    assert_eq!(store.list_for_user("u2").len(), 1);
    assert_eq!(store.active_count_for_user("u1"), 2);

    // Phase 1 removal for u1 must not touch u2's copy.
    let marked = store.remove("u1", "v1").unwrap().unwrap();
    assert_eq!(marked.status, DownloadStatus::Removed);
    assert!(!store.is_playable_offline("u1", "v1"));
    assert!(store.is_playable_offline("u2", "v1"));
    assert_eq!(store.list_for_user("u1").len(), 1);
    assert_eq!(store.pending_deletions().len(), 1);

    // Simulate crash before deleting u1/v1 on disk.
    drop(store);
    let mut reopened = DownloadStore::open(&manifest).unwrap();
    assert_eq!(reopened.pending_deletions().len(), 1);
    assert_eq!(reopened.pending_deletions()[0].user_id, "u1");
    assert!(!reopened.is_playable_offline("u1", "v1"));

    // Phase 2 confirmation drops Removed record; refuses to drop active records.
    assert!(reopened.confirm_removed("u1", "v1").unwrap());
    assert!(reopened.pending_deletions().is_empty());
    assert!(reopened.get("u1", "v1").is_none());
    assert!(!reopened.confirm_removed("u2", "v1").unwrap());
    assert!(reopened.is_playable_offline("u2", "v1"));

    // Availability sync marks unavailable videos Removed across accounts.
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

    reserve_and_complete(&mut store, "u1", "v-csam", 4_000);
    reserve_and_complete(&mut store, "u2", "v-csam", 4_000);
    reserve_and_complete(&mut store, "u1", "v-ok", 2_000);

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
    assert_eq!(store.revocations_cursor(), 0);

    drop(store);
    let mut recovered = DownloadStore::open(&manifest).unwrap();
    assert_eq!(recovered.revocations_cursor(), 0);
    assert_eq!(recovered.pending_deletions().len(), 2);

    let keys: Vec<(String, String)> = to_delete
        .iter()
        .map(|r| (r.user_id.clone(), r.video_id.clone()))
        .collect();
    assert_eq!(recovered.confirm_removed_batch(&keys).unwrap(), 2);
    recovered.commit_revocations_cursor(12).unwrap();
    recovered.commit_revocations_cursor(5).unwrap();
    assert_eq!(recovered.revocations_cursor(), 12);

    drop(recovered);
    let final_store = DownloadStore::open(&manifest).unwrap();
    assert_eq!(final_store.revocations_cursor(), 12);
    assert_eq!(final_store.list().len(), 1);
    assert!(final_store.pending_deletions().is_empty());
}

#[test]
fn quarantine_recovery_save_failure_salvage_backup_and_container_safe_reconciliation() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");
    let root_str = "/var/mobile/Containers/Data/Application/NEW-UUID/Documents";

    // 1) Corrupt JSON is quarantined, keeps has_local_downloads() == true, flags reconciliation.
    std::fs::write(&manifest, b"{not valid json").unwrap();
    let mut store = DownloadStore::open(&manifest).unwrap();
    assert!(store.needs_disk_reconciliation());
    assert!(store.has_local_downloads());
    assert_eq!(store.revocations_cursor(), 0);

    let orphans = store
        .reconcile_orphaned_paths(
            root_str,
            &[
                format!("{root_str}/downloads/u1/orphan.mp4"),
                "thumbs/orphan.jpg".into(),
                format!("{root_str}/manifest.json"),
                format!("{root_str}/manifest.json.lock"),
                format!("{root_str}/manifest.json.corrupt.12345"),
                format!("{root_str}/downloads/u1/inprogress.mp4.part"),
                "/some/other/directory/not-ours.mp4".into(),
            ],
        )
        .unwrap();
    assert_eq!(
        orphans,
        vec![
            format!("{root_str}/downloads/u1/orphan.mp4"),
            "thumbs/orphan.jpg".to_string(),
        ]
    );
    store.mark_reconciled().unwrap();
    assert!(!store.needs_disk_reconciliation());
    assert!(!store.has_local_downloads());
    drop(store);

    // 2) Simulate save failure right after quarantine (manifest.json absent, .corrupt.* present):
    //    reopening MUST STILL report needs_disk_reconciliation() == true and has_local_downloads() == true!
    std::fs::remove_file(&manifest).unwrap();
    let recovered_after_save_fail = DownloadStore::open(&manifest).unwrap();
    assert!(
        recovered_after_save_fail.needs_disk_reconciliation(),
        "reconcile flag must survive even if save after quarantine failed"
    );
    assert!(recovered_after_save_fail.has_local_downloads());
    drop(recovered_after_save_fail);

    // 3) 0-byte manifest is quarantined to a distinct file.
    std::fs::write(&manifest, b"").unwrap();
    let store_zero = DownloadStore::open(&manifest).unwrap();
    assert!(store_zero.needs_disk_reconciliation());
    assert!(store_zero.has_local_downloads());
    drop(store_zero);

    let corrupt_count_before_salvage = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt."))
        .count();
    assert_eq!(corrupt_count_before_salvage, 2);

    // 4) Future manifest version (99) is refused with Storage error and NOT quarantined.
    let future_json = r#"{"version":99,"revocations_cursor":10,"records":{}}"#;
    std::fs::write(&manifest, future_json).unwrap();
    assert!(matches!(
        DownloadStore::open(&manifest),
        Err(DownloadError::Storage(_))
    ));
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), future_json);

    // 5) Partial record corruption: valid record salvaged, forensic .corrupt.<ts> backup saved,
    //    unknown fields preserved, and reconciliation resolves iOS /private/var/... container paths
    //    without EVER listing live files as orphans!
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
                "thumbnail_path": "thumbs/v-good.jpg",
                "duration_seconds": 300,
                "local_path": "downloads/u1/v-good.mp4",
                "bytes_downloaded": 500,
                "total_bytes": 500,
                "status": "completed",
                "removed_reason": null,
                "created_at_ms": 100,
                "last_accessed_at_ms": 100,
                "reservation_id": 4,
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

    let corrupt_count_after_salvage = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".corrupt."))
        .count();
    assert_eq!(
        corrupt_count_after_salvage, 3,
        "partial record salvage must keep a forensic .corrupt.<ts> backup of unparseable records"
    );

    // Container moved to NEW-UUID and macOS/iOS reports /private/var/... paths:
    // live files MUST NOT be listed as orphans; only truly orphaned files are returned.
    let orphans2 = salvaged
        .reconcile_orphaned_paths(
            root_str,
            &[
                format!("/private{root_str}/downloads/u1/v-good.mp4"),
                format!("{root_str}/thumbs/v-good.jpg"),
                "downloads/u1/v-good.mp4".into(),
                format!("/private{root_str}/downloads/u1/v-bad.mp4"),
                "downloads/u1/v-stale.mp4".into(),
            ],
        )
        .unwrap();
    assert_eq!(
        orphans2,
        vec![
            format!("/private{root_str}/downloads/u1/v-bad.mp4"),
            "downloads/u1/v-stale.mp4".to_string(),
        ]
    );
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

    reserve_and_complete(&mut store, "u1", "v1", 2_000);
    assert!(store.is_playable_offline("u1", "v1"));

    std::fs::set_permissions(&subdir, std::fs::Permissions::from_mode(0o555)).unwrap();

    let err = store.apply_revocations(&[RevocationEntry {
        seq: 1,
        video_id: "v1".into(),
    }]);
    assert!(err.is_err());
    assert!(store.is_playable_offline("u1", "v1"));
    assert!(store.pending_deletions().is_empty());

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
fn never_silently_evicts_active_user_downloads() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();

    reserve_and_complete(&mut store, "u1", "v-active", 500);
    let res_fail = reserve_ok(
        &mut store,
        sample_record("u1", "v-failed", DownloadStatus::Downloading, 400),
    );
    assert_eq!(
        store
            .update_progress("u1", "v-failed", res_fail, 400, 400, 1_200)
            .unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(
        store.mark_failed("u1", "v-failed", res_fail, 1_300).unwrap(),
        UpdateOutcome::Updated
    );

    let evicted = store.evict_to_budget(600).unwrap();
    assert_eq!(evicted.len(), 1);
    assert_eq!(evicted[0].video_id, "v-failed");
    assert_eq!(evicted[0].status, DownloadStatus::Removed);
    assert!(store.confirm_removed("u1", "v-failed").unwrap());
    assert!(store.get("u1", "v-active").is_some());

    let evicted_again = store.evict_to_budget(100).unwrap();
    assert!(evicted_again.is_empty());
    assert!(store.get("u1", "v-active").is_some());
}

#[test]
fn transfer_policy_enforces_wifi_storage_cap_and_pending_free_space() {
    let settings = DownloadSettings {
        wifi_only: true,
        max_storage_bytes: Some(10_000),
        min_free_space_bytes: 500,
        auto_download_enabled: true,
        auto_download_charging_only: true,
    };

    assert_eq!(
        can_start_download(&settings, NetworkKind::Offline, 0, 0, 1_000, 0, None),
        TransferDecision::Offline
    );
    assert_eq!(
        can_start_download(&settings, NetworkKind::Cellular, 0, 0, 1_000, 0, None),
        TransferDecision::WaitForWifi
    );
    assert_eq!(
        can_start_download(&settings, NetworkKind::Wifi, 9_500, 0, 1_000, 0, None),
        TransferDecision::StorageBudgetExceeded
    );
    // Free-space check includes pending_remaining_bytes from other unfinished items:
    // 500 reserve + 800 pending + 300 item = 1600 > 1500 available -> exceeded.
    assert_eq!(
        can_start_download(&settings, NetworkKind::Wifi, 1_000, 800, 300, 0, Some(1_500)),
        TransferDecision::StorageBudgetExceeded
    );
    assert_eq!(
        can_start_download(&settings, NetworkKind::Wifi, 1_000, 800, 300, 0, Some(1_600)),
        TransferDecision::Allow
    );
}

#[test]
fn auto_download_caps_kept_episodes_on_growing_feed_and_never_evicts_manual_downloads() {
    let ch1 = AutoDownloadSource::channel("ch-1");
    let rules = vec![AutoDownloadRule {
        source: ch1.clone(),
        max_episodes: 2,
    }];
    let strict = DownloadSettings {
        wifi_only: true,
        max_storage_bytes: Some(10_000),
        min_free_space_bytes: 500,
        auto_download_enabled: true,
        auto_download_charging_only: true,
    };

    let empty_set: HashSet<String> = HashSet::new();
    let base_candidates = vec![EpisodeCandidate {
        video_id: "v0".into(),
        source: ch1.clone(),
        published_at_ms: 100,
        allow_downloads: true,
        estimated_bytes: Some(1_000),
    }];

    // Disabled / cellular / not charging -> empty plan.
    let disabled = DownloadSettings {
        auto_download_enabled: false,
        ..strict.clone()
    };
    assert!(
        select_auto_downloads(
            &disabled,
            NetworkKind::Wifi,
            true,
            0,
            0,
            None,
            &rules,
            &base_candidates,
            &empty_set,
            &[],
            &empty_set,
        )
        .to_download
        .is_empty()
    );
    assert!(
        select_auto_downloads(
            &strict,
            NetworkKind::Cellular,
            true,
            0,
            0,
            None,
            &rules,
            &base_candidates,
            &empty_set,
            &[],
            &empty_set,
        )
        .to_download
        .is_empty()
    );
    assert!(
        select_auto_downloads(
            &strict,
            NetworkKind::Wifi,
            false,
            0,
            0,
            None,
            &rules,
            &base_candidates,
            &empty_set,
            &[],
            &empty_set,
        )
        .to_download
        .is_empty()
    );

    // Growing feed simulation: 8 consecutive polls, 1 new episode arrives each poll, plus a
    // manual user download (`v-manual`) on the same channel.
    // Total kept auto-downloads MUST NEVER exceed max_episodes (2), and `v-manual` MUST NEVER be evicted!
    let mut all_candidates: Vec<EpisodeCandidate> = Vec::new();
    let mut existing_ids: HashSet<String> = HashSet::from(["v-manual".to_string()]);
    let mut existing_auto: Vec<ExistingAutoDownload> = Vec::new();
    let mut dismissed: HashSet<String> = HashSet::new();

    for poll in 1..=8i64 {
        let vid = format!("ep-{poll}");
        all_candidates.push(EpisodeCandidate {
            video_id: vid,
            source: ch1.clone(),
            published_at_ms: poll * 1_000,
            allow_downloads: true,
            estimated_bytes: Some(1_000),
        });

        let plan = select_auto_downloads(
            &strict,
            NetworkKind::Wifi,
            true,
            (existing_auto.len() as u64) * 1_000,
            0,
            Some(50_000),
            &rules,
            &all_candidates,
            &existing_ids,
            &existing_auto,
            &dismissed,
        );

        assert!(!plan.to_evict.contains(&"v-manual".to_string()));
        for item in &plan.to_download {
            let pub_ms = all_candidates
                .iter()
                .find(|c| c.video_id == item.video_id)
                .unwrap()
                .published_at_ms;
            existing_ids.insert(item.video_id.clone());
            existing_auto.push(ExistingAutoDownload {
                video_id: item.video_id.clone(),
                source: item.source.clone(),
                published_at_ms: pub_ms,
                size_bytes: 1_000,
            });
        }
        for evict_id in &plan.to_evict {
            existing_ids.remove(evict_id);
            existing_auto.retain(|e| &e.video_id != evict_id);
        }

        assert!(
            existing_auto.len() <= 2,
            "poll {poll}: kept auto-downloads must stay <= max_episodes (got {existing_auto:?})"
        );
    }
    let kept_ids: HashSet<String> = existing_auto.iter().map(|e| e.video_id.clone()).collect();
    assert_eq!(kept_ids, HashSet::from(["ep-7".to_string(), "ep-8".to_string()]));
    assert!(existing_ids.contains("v-manual"));

    // User deletes ep-8 and dismisses it: ep-8 must NEVER be re-picked.
    existing_ids.remove("ep-8");
    existing_auto.retain(|e| e.video_id != "ep-8");
    dismissed.insert("ep-8".into());
    let after_dismiss = select_auto_downloads(
        &strict,
        NetworkKind::Wifi,
        true,
        1_000,
        0,
        Some(50_000),
        &rules,
        &all_candidates,
        &existing_ids,
        &existing_auto,
        &dismissed,
    );
    assert!(
        !after_dismiss
            .to_download
            .iter()
            .any(|item| item.video_id == "ep-8")
    );

    // Free-space check with pending_remaining_bytes prevents auto-download oversubscription:
    // available 1,600 - 500 reserve - 600 pending = 500 left < 1,000 item -> picks nothing.
    let tight_free = select_auto_downloads(
        &strict,
        NetworkKind::Wifi,
        true,
        0,
        600,
        Some(1_600),
        &rules,
        &all_candidates,
        &empty_set,
        &[],
        &empty_set,
    );
    assert!(tight_free.to_download.is_empty());
}

#[test]
fn late_mark_downloading_and_mark_failed_cannot_mutate_paused_or_failed_downloads() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();
    let settings = DownloadSettings {
        wifi_only: false,
        max_storage_bytes: None,
        min_free_space_bytes: 256,
        auto_download_enabled: false,
        auto_download_charging_only: false,
    };

    // Reserve item A (600 bytes) under 1,000 free / 256 reserve -> Allow.
    let res_a = store
        .try_reserve(
            sample_record("u1", "v-a", DownloadStatus::Downloading, 600),
            &settings,
            NetworkKind::Wifi,
            Some(1_000),
        )
        .unwrap()
        .reservation_id
        .unwrap();

    // Pause A, then send late mark_downloading and late mark_failed (e.g. cancelled transfer error):
    // BOTH must return AlreadyTerminal, A stays Paused, and purge_inactive MUST NOT delete A!
    assert_eq!(
        store.mark_paused("u1", "v-a", res_a, 2_000).unwrap(),
        UpdateOutcome::Updated
    );
    assert_eq!(
        store.mark_downloading("u1", "v-a", res_a, 2_100).unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(
        store.mark_failed("u1", "v-a", res_a, 2_200).unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(store.get("u1", "v-a").unwrap().status, DownloadStatus::Paused);
    assert!(
        store.purge_inactive().unwrap().is_empty(),
        "purge_inactive must never delete a Paused download after a late failure callback"
    );

    // Reserve item B (600 bytes) in a fresh store, mark Failed, then allow C (600 bytes):
    // late mark_downloading on Failed B must return AlreadyTerminal so it cannot bypass budget!
    let dir2 = tempdir().unwrap();
    let mut store2 = DownloadStore::open(dir2.path().join("manifest.json")).unwrap();
    let res_b = store2
        .try_reserve(
            sample_record("u1", "v-b", DownloadStatus::Downloading, 600),
            &settings,
            NetworkKind::Wifi,
            Some(1_000),
        )
        .unwrap()
        .reservation_id
        .unwrap();
    assert_eq!(
        store2.mark_failed("u1", "v-b", res_b, 2_000).unwrap(),
        UpdateOutcome::Updated
    );
    let res_c = store2
        .try_reserve(
            sample_record("u1", "v-c", DownloadStatus::Downloading, 600),
            &settings,
            NetworkKind::Wifi,
            Some(1_000),
        )
        .unwrap();
    assert_eq!(res_c.decision, TransferDecision::Allow);
    assert_eq!(
        store2.mark_downloading("u1", "v-b", res_b, 2_500).unwrap(),
        UpdateOutcome::AlreadyTerminal
    );
    assert_eq!(store2.get("u1", "v-b").unwrap().status, DownloadStatus::Failed);
    assert_eq!(store2.pending_remaining_bytes(), 600);
}

#[test]
fn reconcile_orphaned_paths_normalizes_slashes_skips_uris_and_traversals_and_rejects_relative_roots() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();
    reserve_and_complete(&mut store, "u1", "v1", 1_000);
    let root = "/var/mobile/Containers/Data/Application/UUID/Documents";

    // Live file in various path forms, URIs, and traversal forms must NEVER be listed as orphans;
    // real orphans with doubled slashes ARE still detected.
    let orphans = store
        .reconcile_orphaned_paths(
            &format!("{root}/"),
            &[
                format!("{root}//downloads/u1/v1.mp4"),
                format!("{root}/./downloads/u1/v1.mp4"),
                "downloads//u1/v1.mp4".into(),
                format!("{root}/downloads/x/../u1/v1.mp4"),
                format!("file://{root}/downloads/u1/v1.mp4"),
                format!("file:{root}/downloads/u1/v1.mp4"),
                format!("{root}//downloads/u1/real-orphan.mp4"),
            ],
        )
        .unwrap();
    assert_eq!(orphans, vec![format!("{root}//downloads/u1/real-orphan.mp4")]);

    // Relative roots (including `./root` and `downloads`) and single-slash `file:/...` URIs MUST
    // be rejected with InvalidInput.
    for bad_root in [
        "",
        "./root",
        "downloads",
        "file:///var/tmp",
        "file:/var/tmp",
        "/var/../etc",
    ] {
        assert!(
            matches!(
                store.reconcile_orphaned_paths(bad_root, &["downloads/u1/v1.mp4".into()]),
                Err(DownloadError::InvalidInput(_))
            ),
            "expected InvalidInput for root {bad_root:?}"
        );
    }
}

#[test]
fn auto_download_prevents_thrashing_when_kept_leave_feed_rolls_forward_at_cap_and_tracks_origin() {
    let ch1 = AutoDownloadSource::channel("ch-1");
    let rules = vec![AutoDownloadRule {
        source: ch1.clone(),
        max_episodes: 2,
    }];
    let settings = DownloadSettings {
        wifi_only: false,
        max_storage_bytes: Some(2_000),
        min_free_space_bytes: 0,
        auto_download_enabled: true,
        auto_download_charging_only: false,
    };

    // Opus probe: v9 and v8 are kept (auto-downloaded), then creator disables downloads on both
    // (or they leave page 1), leaving older v7 and v6 in candidates. Over 7 passes, ZERO downloads
    // and ZERO evictions may happen!
    let mut existing_ids: HashSet<String> = HashSet::from(["v9".into(), "v8".into()]);
    let mut existing_auto = vec![
        ExistingAutoDownload {
            video_id: "v9".into(),
            source: ch1.clone(),
            published_at_ms: 9_000,
            size_bytes: 1_000,
        },
        ExistingAutoDownload {
            video_id: "v8".into(),
            source: ch1.clone(),
            published_at_ms: 8_000,
            size_bytes: 1_000,
        },
    ];
    let older_candidates = vec![
        EpisodeCandidate {
            video_id: "v7".into(),
            source: ch1.clone(),
            published_at_ms: 7_000,
            allow_downloads: true,
            estimated_bytes: Some(1_000),
        },
        EpisodeCandidate {
            video_id: "v6".into(),
            source: ch1.clone(),
            published_at_ms: 6_000,
            allow_downloads: true,
            estimated_bytes: Some(1_000),
        },
    ];
    let dismissed: HashSet<String> = HashSet::new();
    for pass in 0..7 {
        let plan = select_auto_downloads(
            &settings,
            NetworkKind::Wifi,
            true,
            2_000,
            0,
            None,
            &rules,
            &older_candidates,
            &existing_ids,
            &existing_auto,
            &dismissed,
        );
        assert!(
            plan.to_download.is_empty() && plan.to_evict.is_empty(),
            "pass {pass}: older candidates must not exceed cap or thrash when kept episodes leave feed (got {plan:?})"
        );
    }

    // Full storage cap (2,000 / 2,000 used by v9 + v8): when newer v10 (1,000 bytes) arrives,
    // planner MUST evict v8 and download v10 so the channel moves forward!
    let newer_candidates = vec![EpisodeCandidate {
        video_id: "v10".into(),
        source: ch1.clone(),
        published_at_ms: 10_000,
        allow_downloads: true,
        estimated_bytes: Some(1_000),
    }];
    let roll_plan = select_auto_downloads(
        &settings,
        NetworkKind::Wifi,
        true,
        2_000,
        0,
        None,
        &rules,
        &newer_candidates,
        &existing_ids,
        &existing_auto,
        &dismissed,
    );
    assert_eq!(roll_plan.to_evict, vec!["v8".to_string()]);
    assert_eq!(
        roll_plan.to_download,
        vec![PlannedAutoDownload {
            video_id: "v10".to_string(),
            source: ch1.clone(),
        }]
    );

    // Store tracks DownloadOrigin::Auto vs Manual and supports promoting Auto -> Manual via mark_manual.
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();
    let mut auto_rec = sample_record("u1", "v-auto", DownloadStatus::Downloading, 1_000);
    auto_rec.origin = DownloadOrigin::Auto;
    auto_rec.published_at_ms = Some(5_000);
    let r = reserve_ok(&mut store, auto_rec);
    store.mark_completed("u1", "v-auto", r, 1_000, 5_100).unwrap();
    assert_eq!(store.existing_auto_downloads_for_user("u1").len(), 1);
    assert_eq!(store.existing_auto_downloads_for_user("u1")[0].size_bytes, 1_000);

    // User taps Download on the auto-downloaded episode -> upgraded to Manual, never auto-evicted.
    assert_eq!(store.mark_manual("u1", "v-auto").unwrap(), UpdateOutcome::Updated);
    assert_eq!(store.get("u1", "v-auto").unwrap().origin, DownloadOrigin::Manual);
    assert!(store.existing_auto_downloads_for_user("u1").is_empty());

    // Keep compiler happy about mut bindings used above.
    existing_ids.clear();
    existing_auto.clear();
}

#[test]
fn reservation_tokens_never_restart_at_1_after_full_quarantine() {
    let dir = tempdir().unwrap();
    let manifest = dir.path().join("manifest.json");

    let res1 = {
        let mut store = DownloadStore::open(&manifest).unwrap();
        reserve_ok(
            &mut store,
            sample_record("u1", "v1", DownloadStatus::Downloading, 5_000),
        )
    };

    // Corrupt manifest completely so open() quarantines it and has no stored counter.
    std::fs::write(&manifest, b"{corrupt").unwrap();
    let mut reopened = DownloadStore::open(&manifest).unwrap();
    assert!(reopened.needs_disk_reconciliation());

    let res2 = reserve_ok(
        &mut reopened,
        sample_record("u1", "v1", DownloadStatus::Downloading, 5_000),
    );
    assert!(
        res2 > res1,
        "post-quarantine reservation token ({res2}) must strictly exceed pre-quarantine token ({res1})"
    );
    assert_eq!(
        reopened.mark_completed("u1", "v1", res1, 3, 2_000).unwrap(),
        UpdateOutcome::StaleReservation
    );
    assert!(!reopened.is_playable_offline("u1", "v1"));
}

#[test]
fn forensic_copy_failure_keeps_original_manifest_intact_and_surfaces_error() {
    let dir = tempdir().unwrap();
    // Create a valid filename near NAME_MAX (255 bytes on APFS/ext4) so appending `.corrupt.<ts>`
    // fails with ENAMETOOLONG (while `.lock` = +5 bytes still fits).
    let long_name = format!("{}.json", "m".repeat(240));
    let manifest = dir.path().join(&long_name);
    let partial_json = r#"{
        "version": 4,
        "revocations_cursor": 3,
        "records": {
            "u1\u001fv-good": {
                "video_id": "v-good",
                "user_id": "u1",
                "title": "Good",
                "channel_id": "ch-1",
                "channel_name": "Scholar",
                "thumbnail_path": null,
                "duration_seconds": 100,
                "local_path": "downloads/u1/v-good.mp4",
                "bytes_downloaded": 500,
                "total_bytes": 500,
                "status": "completed",
                "removed_reason": null,
                "created_at_ms": 100,
                "last_accessed_at_ms": 100,
                "reservation_id": 10
            },
            "u1\u001fv-bad": {
                "video_id": "v-bad",
                "status": "corrupt_status"
            }
        }
    }"#;
    std::fs::write(&manifest, partial_json).unwrap();

    let err = DownloadStore::open(&manifest);
    assert!(
        err.is_err(),
        "open() must surface an error if the forensic copy cannot be created"
    );
    assert_eq!(
        std::fs::read_to_string(&manifest).unwrap(),
        partial_json,
        "original manifest must remain untouched when forensic copy fails"
    );
}

#[test]
fn try_reserve_resets_fresh_bytes_downloaded_and_refuses_path_collisions_across_accounts() {
    let dir = tempdir().unwrap();
    let mut store = DownloadStore::open(dir.path().join("manifest.json")).unwrap();
    let tight = DownloadSettings {
        wifi_only: false,
        max_storage_bytes: Some(500_000),
        min_free_space_bytes: 0,
        auto_download_enabled: false,
        auto_download_charging_only: false,
    };

    // Fresh download claiming all bytes already downloaded must NOT bypass the budget!
    let mut forged = sample_record("u1", "v-forged", DownloadStatus::Downloading, 1_000_000);
    forged.bytes_downloaded = 1_000_000;
    let decision = store
        .try_reserve(forged, &tight, NetworkKind::Wifi, Some(100))
        .unwrap();
    assert_eq!(decision.decision, TransferDecision::StorageBudgetExceeded);

    // When allowed, a fresh download always starts at bytes_downloaded == 0.
    let mut fresh = sample_record("u1", "v-ok", DownloadStatus::Downloading, 1_000);
    fresh.bytes_downloaded = 999;
    let res = reserve_ok(&mut store, fresh);
    assert!(res > 0);
    assert_eq!(store.get("u1", "v-ok").unwrap().bytes_downloaded, 0);

    // A second account attempting to reserve the exact same local_path must get Conflict.
    let mut colliding = sample_record("u2", "v-ok", DownloadStatus::Queued, 1_000);
    colliding.local_path = "downloads/u1/v-ok.mp4".into();
    assert!(matches!(
        store.try_reserve(colliding, &permissive_settings(), NetworkKind::Wifi, None),
        Err(DownloadError::Conflict(_))
    ));
}

#[test]
fn auto_download_playlist_rule_missing_channel_id_and_combined_rules_never_thrash() {
    let settings = DownloadSettings {
        wifi_only: false,
        max_storage_bytes: None,
        min_free_space_bytes: 0,
        auto_download_enabled: true,
        auto_download_charging_only: false,
    };

    // 1) Playlist rule P on videos of channel C (9 episodes, cap 2):
    //    must keep strictly 2 episodes over repeated passes (not download all 9).
    {
        let dir = tempdir().unwrap();
        let mut store = DownloadStore::open(dir.path().join("playlist.json")).unwrap();
        let pl = AutoDownloadSource::playlist("pl-1");
        let rules = vec![AutoDownloadRule {
            source: pl.clone(),
            max_episodes: 2,
        }];
        let candidates: Vec<EpisodeCandidate> = (1..=9i64)
            .map(|i| EpisodeCandidate {
                video_id: format!("v{i}"),
                source: pl.clone(),
                published_at_ms: i * 1_000,
                allow_downloads: true,
                estimated_bytes: Some(1_000),
            })
            .collect();
        let dismissed = HashSet::new();
        let mut total_downloads = 0usize;

        for _pass in 0..5 {
            let existing_ids: HashSet<String> = store
                .list_for_user("u1")
                .into_iter()
                .map(|r| r.video_id)
                .collect();
            let existing_auto = store.existing_auto_downloads_for_user("u1");
            let plan = select_auto_downloads(
                &settings,
                NetworkKind::Wifi,
                true,
                store.total_bytes(),
                store.pending_remaining_bytes(),
                None,
                &rules,
                &candidates,
                &existing_ids,
                &existing_auto,
                &dismissed,
            );
            for ev in &plan.to_evict {
                store.remove("u1", ev).unwrap();
                store.confirm_removed("u1", ev).unwrap();
            }
            for item in &plan.to_download {
                let c = candidates.iter().find(|c| c.video_id == item.video_id).unwrap();
                let mut rec = sample_record("u1", &item.video_id, DownloadStatus::Downloading, 1_000);
                rec.channel_id = Some("ch-1".into());
                rec.origin = DownloadOrigin::Auto;
                rec.auto_source = Some(item.source.clone());
                rec.published_at_ms = Some(c.published_at_ms);
                let rid = reserve_ok(&mut store, rec);
                store
                    .mark_completed("u1", &item.video_id, rid, 1_000, 10_000)
                    .unwrap();
                total_downloads += 1;
            }
            assert!(store.list_for_user("u1").len() <= 2);
        }
        assert_eq!(total_downloads, 2);
        let kept: HashSet<String> = store
            .list_for_user("u1")
            .into_iter()
            .map(|r| r.video_id)
            .collect();
        assert_eq!(kept, HashSet::from(["v8".to_string(), "v9".to_string()]));
    }

    // 2) Channel rule C on records with `channel_id: None` (explicit `auto_source: Channel("ch-1")`):
    //    must still enforce cap 2, and `try_reserve` must reject an Auto record with neither
    //    `auto_source` nor `channel_id`.
    {
        let dir = tempdir().unwrap();
        let mut store = DownloadStore::open(dir.path().join("no-channel-id.json")).unwrap();
        let mut invalid_auto = sample_record("u1", "v-invalid", DownloadStatus::Queued, 1_000);
        invalid_auto.origin = DownloadOrigin::Auto;
        invalid_auto.channel_id = None;
        invalid_auto.auto_source = None;
        assert!(matches!(
            store.try_reserve(invalid_auto, &permissive_settings(), NetworkKind::Wifi, None),
            Err(DownloadError::InvalidInput(_))
        ));

        let ch = AutoDownloadSource::channel("ch-1");
        let rules = vec![AutoDownloadRule {
            source: ch.clone(),
            max_episodes: 2,
        }];
        let candidates: Vec<EpisodeCandidate> = (1..=6i64)
            .map(|i| EpisodeCandidate {
                video_id: format!("v{i}"),
                source: ch.clone(),
                published_at_ms: i * 1_000,
                allow_downloads: true,
                estimated_bytes: Some(1_000),
            })
            .collect();
        let dismissed = HashSet::new();
        let mut total_downloads = 0usize;

        for _pass in 0..4 {
            let existing_ids: HashSet<String> = store
                .list_for_user("u1")
                .into_iter()
                .map(|r| r.video_id)
                .collect();
            let existing_auto = store.existing_auto_downloads_for_user("u1");
            let plan = select_auto_downloads(
                &settings,
                NetworkKind::Wifi,
                true,
                store.total_bytes(),
                store.pending_remaining_bytes(),
                None,
                &rules,
                &candidates,
                &existing_ids,
                &existing_auto,
                &dismissed,
            );
            for ev in &plan.to_evict {
                store.remove("u1", ev).unwrap();
                store.confirm_removed("u1", ev).unwrap();
            }
            for item in &plan.to_download {
                let c = candidates.iter().find(|c| c.video_id == item.video_id).unwrap();
                let mut rec = sample_record("u1", &item.video_id, DownloadStatus::Downloading, 1_000);
                rec.channel_id = None;
                rec.origin = DownloadOrigin::Auto;
                rec.auto_source = Some(item.source.clone());
                rec.published_at_ms = Some(c.published_at_ms);
                let rid = reserve_ok(&mut store, rec);
                store
                    .mark_completed("u1", &item.video_id, rid, 1_000, 10_000)
                    .unwrap();
                total_downloads += 1;
            }
        }
        assert_eq!(total_downloads, 2);
        assert_eq!(store.existing_auto_downloads_for_user("u1").len(), 2);
    }

    // 3) Overlapping channel and playlist rules share the same two files. A new shared video
    //    replaces one file, and subsequent polls keep the same pair.
    {
        let dir = tempdir().unwrap();
        let mut store = DownloadStore::open(dir.path().join("combined.json")).unwrap();
        let ch = AutoDownloadSource::channel("shared-1");
        let pl = AutoDownloadSource::playlist("shared-1");
        let rules = vec![
            AutoDownloadRule {
                source: ch.clone(),
                max_episodes: 2,
            },
            AutoDownloadRule {
                source: pl.clone(),
                max_episodes: 2,
            },
        ];
        let mut candidates = Vec::new();
        for i in 1..=9i64 {
            candidates.push(EpisodeCandidate {
                video_id: format!("v{i}"),
                source: ch.clone(),
                published_at_ms: i * 1_000,
                allow_downloads: true,
                estimated_bytes: Some(1_000),
            });
            candidates.push(EpisodeCandidate {
                video_id: format!("v{i}"),
                source: pl.clone(),
                published_at_ms: i * 1_000,
                allow_downloads: true,
                estimated_bytes: Some(1_000),
            });
        }
        let dismissed = HashSet::new();
        let mut total_downloads = 0usize;
        let mut total_evictions = 0usize;

        for pass in 0..8 {
            if pass == 4 {
                for source in [&ch, &pl] {
                    candidates.push(EpisodeCandidate {
                        video_id: "v10".into(),
                        source: source.clone(),
                        published_at_ms: 10_000,
                        allow_downloads: true,
                        estimated_bytes: Some(1_000),
                    });
                }
            }
            let existing_ids: HashSet<String> = store
                .list_for_user("u1")
                .into_iter()
                .map(|r| r.video_id)
                .collect();
            let existing_auto = store.existing_auto_downloads_for_user("u1");
            let plan = select_auto_downloads(
                &settings,
                NetworkKind::Wifi,
                true,
                store.total_bytes(),
                store.pending_remaining_bytes(),
                None,
                &rules,
                &candidates,
                &existing_ids,
                &existing_auto,
                &dismissed,
            );
            for ev in &plan.to_evict {
                store.remove("u1", ev).unwrap();
                store.confirm_removed("u1", ev).unwrap();
                total_evictions += 1;
            }
            for item in &plan.to_download {
                // Take the owning rule source directly from the plan (never hard-coded).
                let c = candidates
                    .iter()
                    .find(|c| c.video_id == item.video_id && c.source == item.source)
                    .unwrap();
                let mut rec = sample_record("u1", &item.video_id, DownloadStatus::Downloading, 1_000);
                rec.channel_id = Some("shared-1".into());
                rec.origin = DownloadOrigin::Auto;
                rec.auto_source = Some(item.source.clone());
                rec.published_at_ms = Some(c.published_at_ms);
                let rid = reserve_ok(&mut store, rec);
                store
                    .mark_completed("u1", &item.video_id, rid, 1_000, 10_000)
                    .unwrap();
                total_downloads += 1;
            }
        }
        assert_eq!(total_downloads, 3);
        assert_eq!(total_evictions, 1);
        let kept: HashSet<_> = store
            .list_for_user("u1")
            .into_iter()
            .map(|r| r.video_id)
            .collect();
        assert_eq!(kept, HashSet::from(["v10".into(), "v9".into()]));
    }

    // 4) Duplicate candidate entries must be deduplicated before applying max_episodes.
    {
        let ch = AutoDownloadSource::channel("ch-dup");
        let rules = vec![AutoDownloadRule {
            source: ch.clone(),
            max_episodes: 2,
        }];
        let dup_candidates = vec![
            EpisodeCandidate {
                video_id: "v2".into(),
                source: ch.clone(),
                published_at_ms: 2_000,
                allow_downloads: true,
                estimated_bytes: Some(1_000),
            },
            EpisodeCandidate {
                video_id: "v2".into(),
                source: ch.clone(),
                published_at_ms: 2_000,
                allow_downloads: true,
                estimated_bytes: Some(1_000),
            },
            EpisodeCandidate {
                video_id: "v1".into(),
                source: ch,
                published_at_ms: 1_000,
                allow_downloads: true,
                estimated_bytes: Some(1_000),
            },
        ];
        let empty = HashSet::new();
        let plan = select_auto_downloads(
            &settings,
            NetworkKind::Wifi,
            true,
            0,
            0,
            None,
            &rules,
            &dup_candidates,
            &empty,
            &[],
            &empty,
        );
        assert_eq!(
            plan.to_download,
            vec![
                PlannedAutoDownload {
                    video_id: "v2".to_string(),
                    source: AutoDownloadSource::channel("ch-dup"),
                },
                PlannedAutoDownload {
                    video_id: "v1".to_string(),
                    source: AutoDownloadSource::channel("ch-dup"),
                },
            ]
        );
    }
}

#[test]
fn shared_episode_is_retained_when_only_one_rule_rolls_forward() {
    let ch = AutoDownloadSource::channel("channel");
    let pl = AutoDownloadSource::playlist("playlist");
    let settings = DownloadSettings {
        auto_download_enabled: true,
        max_storage_bytes: Some(3_000),
        ..Default::default()
    };
    let rules = [
        AutoDownloadRule {
            source: ch.clone(),
            max_episodes: 2,
        },
        AutoDownloadRule {
            source: pl.clone(),
            max_episodes: 2,
        },
    ];
    let candidate = |id: &str, source: &AutoDownloadSource, published_at_ms| EpisodeCandidate {
        video_id: id.into(),
        source: source.clone(),
        published_at_ms,
        allow_downloads: true,
        estimated_bytes: Some(1_000),
    };
    let candidates = [
        candidate("new-channel", &ch, 4_000),
        candidate("channel-only", &ch, 3_000),
        candidate("shared", &ch, 2_000),
        candidate("shared", &pl, 2_000),
        candidate("playlist-only", &pl, 1_000),
        candidate("older-playlist", &pl, 0),
    ];
    let owned = [
        ExistingAutoDownload {
            video_id: "channel-only".into(),
            source: ch.clone(),
            published_at_ms: 3_000,
            size_bytes: 1_000,
        },
        ExistingAutoDownload {
            video_id: "shared".into(),
            source: ch.clone(),
            published_at_ms: 2_000,
            size_bytes: 1_000,
        },
    ];
    let existing_ids = owned.iter().map(|e| e.video_id.clone()).collect();
    for rules in [rules.clone(), [rules[1].clone(), rules[0].clone()]] {
        let plan = select_auto_downloads(
            &settings,
            NetworkKind::Wifi,
            true,
            2_000,
            0,
            None,
            &rules,
            &candidates,
            &existing_ids,
            &owned,
            &HashSet::new(),
        );
        // Three files fit the budget. "shared" still belongs in the playlist window, so it
        // cannot fund a fourth file when the channel rolls forward.
        assert!(plan.to_evict.is_empty());
        assert_eq!(plan.to_download.len(), 1);
        assert_eq!(
            plan.to_download[0].video_id,
            if rules[0].source == ch {
                "new-channel"
            } else {
                "playlist-only"
            }
        );
    }
}
