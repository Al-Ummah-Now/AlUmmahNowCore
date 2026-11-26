//! The server's HLS ladder (AlUmmahNow `apps/media/container/ladder.mjs`), mirrored so desktop
//! Studio pre-encodes exactly the rungs the server would (D11). `tests/fixtures/hls/server_ladder.json`
//! is generated from the server module and checked in the tests.

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// Target segment length; segments are cut at the first keyframe past each multiple of it.
pub const SEGMENT_SECONDS: u32 = 6;
/// Keyframe interval every rung is encoded with.
pub const GOP_SECONDS: u32 = 2;
/// Sources above [`FPS_CAP_ABOVE`] are encoded at this rate.
pub const MAX_FPS: f64 = 30.0;
pub const FPS_CAP_ABOVE: f64 = 31.0;

struct RungSpec {
    name: &'static str,
    short: u32,
    maxrate: u32,
    audio: u32,
}

const RUNGS: [RungSpec; 4] = [
    RungSpec {
        name: "360p",
        short: 360,
        maxrate: 800_000,
        audio: 96_000,
    },
    RungSpec {
        name: "480p",
        short: 480,
        maxrate: 1_400_000,
        audio: 128_000,
    },
    RungSpec {
        name: "720p",
        short: 720,
        maxrate: 2_800_000,
        audio: 128_000,
    },
    RungSpec {
        name: "1080p",
        short: 1080,
        maxrate: 5_000_000,
        audio: 128_000,
    },
];

/// One rung to encode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HlsRung {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Peak video bitrate (bits/s); the server encodes CRF 23 capped here.
    pub maxrate: u32,
    /// VBV buffer (bits): twice the maxrate.
    pub bufsize: u32,
    /// AAC stereo bitrate (bits/s).
    pub audio_bitrate: u32,
    /// H.264 profile: `high` from 720p (or 1280 wide) up, `main` below.
    pub profile: String,
    pub gop_seconds: u32,
    pub segment_seconds: u32,
    /// Frame rate cap: sources above [`FPS_CAP_ABOVE`] fps are encoded at this rate.
    pub max_fps: f64,
}

fn even(n: f64) -> u32 {
    ((n / 2.0).round() * 2.0).max(2.0) as u32
}

fn even_down(n: f64) -> u32 {
    ((n / 2.0).floor() * 2.0).max(2.0) as u32
}

/// Display size after rotation metadata (90° and 270° swap the sides).
pub fn display_size(width: u32, height: u32, rotation: i32) -> (u32, u32) {
    if rotation.unsigned_abs() % 180 == 90 {
        (height, width)
    } else {
        (width, height)
    }
}

/// The rungs the server encodes for a source of this coded size and rotation, lowest first.
pub fn ladder(width: u32, height: u32, rotation: i32) -> Result<Vec<HlsRung>> {
    let (width, height) = display_size(width, height, rotation);
    if width == 0 || height == 0 {
        return Err(Error::invalid_input("source has no video size".to_owned()));
    }
    let portrait = height > width;
    let short = width.min(height);
    let aspect = f64::from(width) / f64::from(height);
    // A few pixels short still counts (1916x1076 is "1080p")
    let mut specs: Vec<(&RungSpec, u32)> = RUNGS
        .iter()
        .filter(|r| r.short <= short + 8)
        .map(|r| (r, r.short))
        .collect();
    if specs.is_empty() {
        specs.push((&RUNGS[0], even_down(f64::from(short))));
    }
    Ok(specs
        .into_iter()
        .map(|(r, rung_short)| {
            let s = rung_short.min(even_down(f64::from(short)));
            let (w, h) = if portrait {
                (s, even(f64::from(s) / aspect))
            } else {
                (even(f64::from(s) * aspect), s)
            };
            HlsRung {
                name: r.name.to_owned(),
                width: w,
                height: h,
                maxrate: r.maxrate,
                bufsize: r.maxrate * 2,
                audio_bitrate: r.audio,
                profile: if h >= 720 || w >= 1280 { "high" } else { "main" }.to_owned(),
                gop_seconds: GOP_SECONDS,
                segment_seconds: SEGMENT_SECONDS,
                max_fps: MAX_FPS,
            }
        })
        .collect())
}

/// The frame rate to encode a source of `fps` at.
pub fn encode_fps(fps: f64) -> f64 {
    if fps > FPS_CAP_ABOVE { MAX_FPS } else { fps }
}
