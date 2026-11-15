#![allow(clippy::unwrap_used, clippy::expect_used)]

//! D11 pre-encode: Core's ladder must equal the server's (fixture generated from AlUmmahNow
//! `apps/media/container/ladder.mjs`), and packaged rungs must play.

use std::path::Path;
use std::process::Command;

use aun_media::CancellationToken;
use aun_media::hls::{GOP_SECONDS, SEGMENT_SECONDS, encode_fps, ladder, package_mp4, renditions_files};
use serde_json::Value;

const SERVER: &str = include_str!("fixtures/hls/server_ladder.json");

#[test]
fn ladder_matches_the_server_for_every_fixture_size() {
    let fixture: Value = serde_json::from_str(SERVER).unwrap();
    assert_eq!(fixture["segmentSeconds"], SEGMENT_SECONDS);
    assert_eq!(fixture["gopSeconds"], GOP_SECONDS);
    let cases = fixture["cases"].as_array().unwrap();
    assert!(cases.len() >= 30);
    for case in cases {
        let (w, h, r) = (
            case["width"].as_u64().unwrap() as u32,
            case["height"].as_u64().unwrap() as u32,
            case["rotation"].as_i64().unwrap() as i32,
        );
        let ours = ladder(w, h, r).unwrap();
        let theirs = case["ladder"].as_array().unwrap();
        assert_eq!(ours.len(), theirs.len(), "{w}x{h}@{r}");
        for (a, b) in ours.iter().zip(theirs) {
            assert_eq!(a.name, b["name"].as_str().unwrap(), "{w}x{h}@{r}");
            assert_eq!(
                (a.width, a.height),
                (
                    b["width"].as_u64().unwrap() as u32,
                    b["height"].as_u64().unwrap() as u32
                ),
                "{w}x{h}@{r} {}",
                a.name
            );
            assert_eq!(u64::from(a.maxrate), b["maxrate"].as_u64().unwrap());
            assert_eq!(u64::from(a.audio_bitrate), b["audio"].as_u64().unwrap());
            assert_eq!(a.bufsize, a.maxrate * 2);
            assert_eq!(
                a.profile,
                if a.height >= 720 || a.width >= 1280 {
                    "high"
                } else {
                    "main"
                }
            );
        }
    }
    assert!(ladder(0, 0, 0).is_err());
    assert_eq!(encode_fps(59.94), 30.0);
    assert_eq!(encode_fps(25.0), 25.0);
}

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn ffmpeg(args: &[&str]) {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-v", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// What a platform encoder writes for one rung: progressive MP4, H.264 with a keyframe every 2 s
/// (B-frames on), AAC stereo.
fn encode_rung(dir: &Path, width: u32, height: u32, seconds: u32) -> std::path::PathBuf {
    let out = dir.join(format!("{height}.mp4"));
    let size = format!("{width}x{height}");
    let dur = seconds.to_string();
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size={size}:rate=30:duration={dur}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=440:sample_rate=48000:duration={dur}"),
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "60",
        "-keyint_min",
        "60",
        "-sc_threshold",
        "0",
        "-bf",
        "2",
        "-c:a",
        "aac",
        "-ac",
        "2",
        "-b:a",
        "96k",
        "-movflags",
        "+faststart",
        out.to_str().unwrap(),
    ]);
    out
}

#[test]
fn packaged_rungs_decode_end_to_end_like_the_server_checks() {
    if !have_ffmpeg() {
        eprintln!("skipped: ffmpeg not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let rungs = ladder(1280, 720, 0).unwrap();
    for rung in &rungs {
        let mp4 = encode_rung(dir.path(), rung.width, rung.height, 15);
        let pkg = package_mp4(
            &mp4,
            &dir.path().join(&rung.name),
            rung.segment_seconds,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(pkg.segment_seconds, vec![6.0, 6.0, 3.0], "{}", rung.name);
        // The server decodes every segment through the playlist with -xerror (apps/media/container/server.mjs)
        let playlist = dir.path().join(&rung.name).join("index.m3u8");
        ffmpeg(&["-xerror", "-i", playlist.to_str().unwrap(), "-f", "null", "-"]);
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_streams",
                "-show_format",
                playlist.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        let info: Value = serde_json::from_slice(&probe.stdout).unwrap();
        let streams = info["streams"].as_array().unwrap();
        let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
        assert_eq!(video["codec_name"], "h264");
        assert_eq!(
            (
                video["width"].as_u64().unwrap(),
                video["height"].as_u64().unwrap()
            ),
            (u64::from(rung.width), u64::from(rung.height))
        );
        let audio = streams.iter().find(|s| s["codec_type"] == "audio").unwrap();
        assert_eq!(
            (
                audio["codec_name"].as_str().unwrap(),
                audio["channels"].as_u64().unwrap()
            ),
            ("aac", 2)
        );
        let duration: f64 = info["format"]["duration"].as_str().unwrap().parse().unwrap();
        assert!((duration - 15.0).abs() < 0.2, "{} lasts {duration}", rung.name);
    }
    let files = renditions_files(
        dir.path(),
        &rungs.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
    )
    .unwrap();
    assert_eq!(files.len(), rungs.len() * 5);
}
