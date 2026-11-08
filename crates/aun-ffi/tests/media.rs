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
