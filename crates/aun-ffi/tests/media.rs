//! The media facade end to end on generated audio and the EDL fixtures.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use aun_core::*;

const EDL_V0: &str = include_str!("../../aun-media/tests/fixtures/edl/v0_khutbah.json");

/// 1 s of a 440 Hz tone, 1 s of silence, 1 s of tone (mono, 48 kHz).
fn tone_gap_tone(dir: &std::path::Path) -> String {
    let rate = 48_000usize;
    let mut samples = Vec::with_capacity(rate * 3);
    for i in 0..rate * 3 {
        let in_gap = (rate..rate * 2).contains(&i);
        let v = if in_gap {
            0.0
        } else {
            (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5
        };
        samples.push(v);
    }
    let path = dir.join("tone.wav");
    aun_media::audio::write_wav_pcm16(
        &path,
        aun_media::audio::AudioFormat {
            sample_rate: rate as u32,
            channels: 1,
        },
        &samples,
    )
    .unwrap();
    path.to_string_lossy().into_owned()
}

#[tokio::test]
async fn analysis_on_generated_audio() {
    let dir = tempfile::tempdir().unwrap();
    let wav = tone_gap_tone(dir.path());

    let probe: serde_json::Value =
        serde_json::from_str(&media_probe_json(wav.clone()).await.unwrap()).unwrap();
    assert!(probe.is_object());

    let loud = media_measure_loudness(wav.clone(), None).await.unwrap();
    let lufs = loud.integrated_lufs.expect("tone is not silent");
    assert!((-20.0..-5.0).contains(&lufs), "{lufs}");
    let gain = media_gain_to_target_db(loud, -14.0, -1.0);
    assert!(gain.is_finite());

    let mut cfg = silence_config_default();
    cfg.min_duration_ms = 500;
    let gaps = media_detect_silence(wav.clone(), cfg, None).await.unwrap();
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(
        (900..=1100).contains(&gaps[0].start_ms) && (1900..=2100).contains(&gaps[0].end_ms),
        "{gaps:?}"
    );

    let peaks = media_generate_waveform(wav.clone(), waveform_config_default(), None)
        .await
        .unwrap();
    assert_eq!(peaks.sample_rate, 48_000);
    assert!(!peaks.levels.is_empty());
    assert_eq!(peaks.levels[0].mins.len(), peaks.levels[0].maxs.len());

    let cancel = MediaCancel::new();
    cancel.cancel();
    assert!(matches!(
        media_measure_loudness(wav, Some(cancel)).await,
        Err(AunError::Cancelled)
    ));
    assert!(media_probe_json("/nonexistent.wav".into()).await.is_err());
}

#[test]
fn thumbnails_catalog_and_subtitles() {
    let ts = media_select_thumbnail_timestamps(60_000, vec![], thumbnail_config_default());
    assert!(!ts.is_empty() && ts.iter().all(|t| *t <= 60_000));
    let catalog = transcription_model_catalog();
    assert!(
        catalog
            .iter()
            .any(|m| m.tier == ModelTier::Tiny && m.sha256.len() == 64)
    );

    let transcript = serde_json::json!({
        "language": "ar",
        "segments": [{"start_ms": 0, "end_ms": 1500, "text": "بسم الله", "words": []}]
    })
    .to_string();
    let vtt = transcript_to_webvtt(transcript.clone(), subtitle_options_default()).unwrap();
    assert!(vtt.starts_with("WEBVTT") && vtt.contains("بسم الله"), "{vtt}");
    let srt = transcript_to_srt(transcript, subtitle_options_default()).unwrap();
    assert!(srt.contains("00:00:00,000 --> 00:00:01,500"), "{srt}");
    assert!(matches!(
        transcript_to_srt("{".into(), subtitle_options_default()),
        Err(AunError::InvalidInput { .. })
    ));
}

#[test]
fn edl_migrates_and_validates() {
    let migrated = edl_load_project(EDL_V0.into()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&migrated).unwrap();
    assert_eq!(v["schemaVersion"].as_u64(), Some(u64::from(edl_schema_version())));
    // Loading the migrated output again is stable.
    assert_eq!(edl_load_project(migrated.clone()).unwrap(), migrated);
    assert!(edl_validate(migrated.clone()).unwrap().is_empty());
    assert!(
        serde_json::from_str::<serde_json::Value>(&edl_chapters_json(migrated).unwrap())
            .unwrap()
            .is_array()
    );
    assert!(edl_load_project("{\"schemaVersion\": 999}".into()).is_err());
}

#[test]
fn edl_edits_cross_the_boundary_as_json() {
    let project = edl_load_project(EDL_V0.into()).unwrap();
    let v: serde_json::Value = serde_json::from_str(&project).unwrap();
    let track = v["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["clips"].as_array().is_some_and(|c| !c.is_empty()))
        .unwrap();
    let clip = &track["clips"][0];
    let id = clip["id"].as_str().unwrap();
    let start = clip["timelineStartMs"].as_u64().unwrap();
    let split = format!(
        r#"{{"op":"split","clipId":"{id}","atMs":{},"newId":"cut-right"}}"#,
        start + 500
    );
    let edited = edl_apply_json(project.clone(), split).unwrap();
    assert!(edited.contains("cut-right"));
    let joined = edl_apply_json(
        edited,
        format!(r#"{{"op":"join","leftId":"{id}","rightId":"cut-right"}}"#),
    )
    .unwrap();
    assert_eq!(edl_load_project(joined).unwrap(), project);

    let missing = edl_apply_json(
        project.clone(),
        r#"{"op":"rippleDeleteClip","clipId":"ghost"}"#.into(),
    );
    assert!(matches!(missing, Err(AunError::NotFound { .. })));
    assert!(matches!(
        edl_apply_json(project.clone(), r#"{"op":"explode"}"#.into()),
        Err(AunError::InvalidInput { .. })
    ));

    let flat: serde_json::Value = serde_json::from_str(&edl_flatten_json(project).unwrap()).unwrap();
    assert!(flat["durationMs"].as_u64().unwrap() > 0);
    assert!(flat["tracks"][0]["clips"][0]["path"].is_string());
}

#[test]
fn caption_segments_ladder_and_renditions_listing() {
    let transcript =
        r#"{"language":"ar","segments":[{"start_ms":0,"end_ms":1200,"text":" السلام عليكم ","words":[]}]}"#;
    let cues: serde_json::Value =
        serde_json::from_str(&transcript_to_caption_segments_json(transcript.into(), 42).unwrap()).unwrap();
    assert_eq!(
        cues,
        serde_json::json!([{"startMs": 0, "endMs": 1200, "text": "السلام عليكم"}])
    );

    let rungs = hls_ladder(1920, 1080, 0).unwrap();
    assert_eq!(
        rungs.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["360p", "480p", "720p", "1080p"]
    );
    assert_eq!(
        (
            rungs[3].width,
            rungs[3].height,
            rungs[3].gop_seconds,
            rungs[3].segment_seconds
        ),
        (1920, 1080, 2, 6)
    );
    assert_eq!(
        hls_ladder(1920, 1080, 90).unwrap()[0].width,
        360,
        "rotated phone video is portrait"
    );
    assert_eq!(hls_encode_fps(60.0), 30.0);

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("360p")).unwrap();
    std::fs::write(dir.path().join("360p/seg_00001.m4s"), [0u8; 10]).unwrap();
    std::fs::write(dir.path().join("360p/index.m3u8"), "#EXTM3U\n").unwrap();
    let files = renditions_files(dir.path().to_string_lossy().into_owned(), vec!["360p".into()]).unwrap();
    assert_eq!(
        files[0],
        StagedFile {
            path: "360p/index.m3u8".into(),
            bytes: 8
        }
    );
    assert_eq!(files.len(), 2);
}

#[tokio::test]
async fn models_and_on_device_transcription_without_the_feature() {
    let dir = tempfile::tempdir().unwrap();
    let store = ModelStore::new(dir.path().to_string_lossy().into_owned());
    let models = store.list().unwrap();
    assert!(!models.is_empty());
    assert!(models.iter().all(|m| !m.installed));
    assert_eq!(store.path(models[0].model.id.clone()).unwrap(), None);
    assert_eq!(store.disk_usage_bytes().unwrap(), 0);
    if !transcription_on_device_available() {
        struct Quiet;
        impl MediaProgress for Quiet {
            fn on_progress(&self, _: u64, _: u64) {}
        }
        let r = transcribe_on_device_json(
            "m.bin".into(),
            "a.wav".into(),
            None,
            std::sync::Arc::new(Quiet),
            None,
        )
        .await;
        assert!(matches!(r, Err(AunError::Unsupported { .. })));
    }
}
