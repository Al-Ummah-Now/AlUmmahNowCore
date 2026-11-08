#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use aun_common::Error;
use aun_media::CancellationToken;
use aun_media::analysis::{SilenceConfig, detect_silence, measure_loudness};
use aun_media::audio::{AudioFormat, write_wav_pcm16};
use aun_media::probe::{ContainerFormat, TrackKind, probe_file, probe_keyframes};
use aun_media::waveform::{WaveformConfig, generate_waveform};

fn tone(path: &Path, rate: u32, channels: u32, seconds: f32, amp: f32) -> Vec<f32> {
    let n = (rate as f32 * seconds) as usize;
    let mut samples = Vec::new();
    for i in 0..n {
        let v = (std::f32::consts::TAU * 440.0 * i as f32 / rate as f32).sin() * amp;
        for _ in 0..channels {
            samples.push(v);
        }
    }
    write_wav_pcm16(
        path,
        AudioFormat {
            sample_rate: rate,
            channels,
        },
        &samples,
    )
    .unwrap();
    samples
}

#[test]
fn probes_wav() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.wav");
    tone(&path, 48_000, 2, 2.0, 0.5);
    let info = probe_file(&path).unwrap();
    assert_eq!(info.container, ContainerFormat::Wav);
    assert_eq!(info.duration_ms, Some(2000));
    let track = info.first_track(TrackKind::Audio).unwrap();
    assert_eq!(track.sample_rate, Some(48_000));
    assert_eq!(track.channels, Some(2));
    assert!(track.codec.starts_with("pcm"), "{}", track.codec);
    assert!(info.bitrate_kbps.unwrap() > 1000);
}

#[test]
fn loudness_silence_waveform_on_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.wav");
    let rate = 48_000;
    let mut samples = vec![0.0f32; rate as usize * 2];
    samples.extend((0..rate as usize * 6).map(|i| (i as f32 * 0.0575).sin() * 0.25));
    write_wav_pcm16(
        &path,
        AudioFormat {
            sample_rate: rate,
            channels: 1,
        },
        &samples,
    )
    .unwrap();
    let token = CancellationToken::new();
    let report = measure_loudness(&path, &token).unwrap();
    assert!(report.integrated_lufs.unwrap() < -10.0);

    let silences = detect_silence(&path, SilenceConfig::default(), &token).unwrap();
    assert_eq!(silences.len(), 1);
    assert_eq!(silences[0].start_ms, 0);
    assert!(silences[0].end_ms.abs_diff(2000) <= 10);

    let peaks = generate_waveform(&path, WaveformConfig::default(), &token).unwrap();
    assert_eq!(peaks.total_frames, samples.len() as u64);
    assert_eq!(peaks.levels.len(), 4);
    let finest = &peaks.levels[0];
    assert!(finest.maxs[0].abs() < 0.001, "first bucket is silent");
    let max = finest.maxs.iter().copied().fold(f32::MIN, f32::max);
    assert!((max - 0.25).abs() < 0.01, "{max}");
}

/// Builds a minimal MP4 with one 1280×720 H.264 track at 30 fps and one 48 kHz AAC track.
mod mp4 {
    pub fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    fn full(version: u8, rest: &[u8]) -> Vec<u8> {
        let mut v = vec![version, 0, 0, 0];
        v.extend_from_slice(rest);
        v
    }

    fn be32s(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    fn mvhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut body = full(0, &be32s(&[0, 0, timescale, duration]));
        body.resize(100, 0);
        boxed(b"mvhd", &body)
    }

    fn tkhd(id: u32, width: u32, height: u32) -> Vec<u8> {
        let mut body = full(0, &be32s(&[0, 0, id, 0, 0]));
        body.resize(76, 0);
        body.extend_from_slice(&(width << 16).to_be_bytes());
        body.extend_from_slice(&(height << 16).to_be_bytes());
        boxed(b"tkhd", &body)
    }

    fn mdia(timescale: u32, duration: u32, handler: &[u8; 4], stbl_children: Vec<u8>) -> Vec<u8> {
        let mut mdhd = full(0, &be32s(&[0, 0, timescale, duration]));
        // "ara", packed.
        mdhd.extend_from_slice(&0x0641u16.to_be_bytes());
        mdhd.extend_from_slice(&[0, 0]);
        let mut hdlr = full(0, &[0, 0, 0, 0]);
        hdlr.extend_from_slice(handler);
        hdlr.extend_from_slice(&[0; 12]);
        hdlr.push(0);
        let minf = boxed(b"minf", &boxed(b"stbl", &stbl_children));
        let body = [boxed(b"mdhd", &mdhd), boxed(b"hdlr", &hdlr), minf].concat();
        boxed(b"mdia", &body)
    }

    fn stsd(entry: Vec<u8>) -> Vec<u8> {
        let mut body = full(0, &1u32.to_be_bytes());
        body.extend_from_slice(&entry);
        boxed(b"stsd", &body)
    }

    fn video_entry() -> Vec<u8> {
        let mut body = vec![0u8; 24];
        body.extend_from_slice(&1280u16.to_be_bytes());
        body.extend_from_slice(&720u16.to_be_bytes());
        body.resize(78, 0);
        boxed(b"avc1", &body)
    }

    fn audio_entry() -> Vec<u8> {
        let mut body = vec![0u8; 16];
        body.extend_from_slice(&2u16.to_be_bytes());
        body.extend_from_slice(&[0; 6]);
        body.extend_from_slice(&(48_000u32 << 16).to_be_bytes());
        boxed(b"mp4a", &body)
    }

    pub fn file() -> Vec<u8> {
        let mut ftyp = b"isom".to_vec();
        ftyp.extend_from_slice(&[0, 0, 2, 0]);
        ftyp.extend_from_slice(b"isomiso2avc1mp41");

        let video_tables = [
            stsd(video_entry()),
            boxed(b"stts", &full(0, &be32s(&[1, 90, 1]))),
            boxed(b"stss", &full(0, &be32s(&[3, 1, 31, 61]))),
        ]
        .concat();
        let video = [tkhd(1, 1280, 720), mdia(30, 90, b"vide", video_tables)].concat();

        let audio_tables = [
            stsd(audio_entry()),
            boxed(b"stts", &full(0, &be32s(&[1, 141, 1024]))),
        ]
        .concat();
        let audio = [tkhd(2, 0, 0), mdia(48_000, 144_000, b"soun", audio_tables)].concat();

        let moov = [mvhd(1000, 3000), boxed(b"trak", &video), boxed(b"trak", &audio)].concat();
        [
            boxed(b"ftyp", &ftyp),
            boxed(b"moov", &moov),
            boxed(b"mdat", &[0; 32]),
        ]
        .concat()
    }
}

#[test]
fn probes_mp4_metadata_and_keyframes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v.mp4");
    std::fs::write(&path, mp4::file()).unwrap();
    let info = probe_file(&path).unwrap();
    assert_eq!(info.container, ContainerFormat::Mp4);
    assert_eq!(info.duration_ms, Some(3000));
    assert_eq!(info.tracks.len(), 2);

    let video = info.first_track(TrackKind::Video).unwrap();
    assert_eq!(video.codec, "h264");
    assert_eq!((video.width, video.height), (Some(1280), Some(720)));
    assert!((video.fps.unwrap() - 30.0).abs() < 1e-6);
    assert_eq!(video.duration_ms, Some(3000));
    assert_eq!(video.language.as_deref(), Some("ara"));

    let audio = info.first_track(TrackKind::Audio).unwrap();
    assert_eq!(audio.codec, "aac");
    assert_eq!(audio.channels, Some(2));
    assert_eq!(audio.sample_rate, Some(48_000));

    assert_eq!(probe_keyframes(&path).unwrap(), vec![0, 1000, 2000]);
}

#[test]
fn malformed_mp4_is_an_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad.mp4");
    let good = mp4::file();
    // Every truncation and a sweep of byte corruptions must return, never panic.
    for cut in (9..good.len()).step_by(7) {
        std::fs::write(&path, &good[..cut]).unwrap();
        let _ = probe_file(&path);
        let _ = probe_keyframes(&path);
    }
    for i in (8..good.len()).step_by(3) {
        let mut corrupt = good.clone();
        corrupt[i] = 0xFF;
        std::fs::write(&path, &corrupt).unwrap();
        let _ = probe_file(&path);
        let _ = probe_keyframes(&path);
    }
    let header_only = mp4::boxed(b"ftyp", b"isom\0\0\0\0");
    std::fs::write(&path, header_only).unwrap();
    assert!(matches!(probe_file(&path), Err(Error::InvalidInput(_))));
}

#[test]
fn keyframes_need_mp4() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.wav");
    tone(&path, 8000, 1, 0.5, 0.5);
    assert!(matches!(probe_keyframes(&path), Err(Error::Unsupported(_))));
    assert!(matches!(
        probe_file(Path::new("/nonexistent.mp4")),
        Err(Error::NotFound(_))
    ));
}
