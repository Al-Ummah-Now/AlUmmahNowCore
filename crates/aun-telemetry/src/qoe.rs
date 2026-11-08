//! Playback quality of experience (Epic 13.10): one [`QoeTracker`] per playback session, fed by the
//! native player wrapper, producing a [`QoeSummary`] that becomes a `qoe_summary` event.
//!
//! Definitions:
//! - **startup time**: play requested → first frame.
//! - **rebuffer**: buffering after the first frame that was not caused by a seek. Startup and
//!   seek buffering are reported separately and never count as rebuffers.
//! - **rebuffer ratio**: rebuffer time / (playing time + rebuffer time).
//! - **bitrate switches**: changes of the selected video bitrate after the first one.
//! - **average bitrate**: time-weighted over the time each bitrate was selected after the first frame.
//!
//! All times are passed in explicitly (Unix ms), so the tracker is deterministic and testable.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::event::PropValue;

/// Stable playback error codes (`playback_error.code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackErrorCode {
    /// Connection failed or dropped.
    Network,
    /// Segment/manifest request timed out.
    Timeout,
    /// HTTP 404/410 for the manifest or a segment.
    SourceNotFound,
    /// HTTP 401/403 (expired signed URL, geo/entitlement).
    Forbidden,
    /// Decoder failure or corrupt media.
    Decode,
    /// DRM licence or key failure.
    Drm,
    /// Codec/container the device cannot play.
    UnsupportedFormat,
    /// Output/renderer failure (audio device, surface lost).
    Renderer,
    Unknown,
}

impl PlaybackErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            PlaybackErrorCode::Network => "network",
            PlaybackErrorCode::Timeout => "timeout",
            PlaybackErrorCode::SourceNotFound => "source_not_found",
            PlaybackErrorCode::Forbidden => "forbidden",
            PlaybackErrorCode::Decode => "decode",
            PlaybackErrorCode::Drm => "drm",
            PlaybackErrorCode::UnsupportedFormat => "unsupported_format",
            PlaybackErrorCode::Renderer => "renderer",
            PlaybackErrorCode::Unknown => "unknown",
        }
    }
}

/// End-of-session QoE numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QoeSummary {
    pub video_id: String,
    /// `vod`, `live`, `offline` (downloaded).
    pub playback_type: String,
    pub startup_time_ms: Option<u64>,
    pub played_ms: u64,
    pub rebuffer_count: u32,
    pub rebuffer_ms: u64,
    pub rebuffer_ratio: f64,
    pub seek_count: u32,
    pub seek_buffer_ms: u64,
    pub bitrate_switches: u32,
    pub avg_bitrate_bps: Option<u64>,
    pub max_bitrate_bps: Option<u64>,
    pub error_count: u32,
    /// Code of the fatal error that ended the session, if any.
    pub fatal_error: Option<PlaybackErrorCode>,
    /// The user left before the first frame (and no fatal error happened).
    pub exited_before_start: bool,
}

impl QoeSummary {
    /// Event properties for the `qoe_summary` event.
    pub fn to_properties(&self) -> HashMap<String, PropValue> {
        let mut p: HashMap<String, PropValue> = HashMap::new();
        p.insert("video_id".into(), self.video_id.clone().into());
        p.insert("playback_type".into(), self.playback_type.clone().into());
        if let Some(s) = self.startup_time_ms {
            p.insert("startup_time_ms".into(), s.into());
        }
        p.insert("played_ms".into(), self.played_ms.into());
        p.insert(
            "rebuffer_count".into(),
            PropValue::Int(self.rebuffer_count.into()),
        );
        p.insert("rebuffer_ms".into(), self.rebuffer_ms.into());
        p.insert(
            "rebuffer_ratio".into(),
            PropValue::Float((self.rebuffer_ratio * 10_000.0).round() / 10_000.0),
        );
        p.insert("seek_count".into(), PropValue::Int(self.seek_count.into()));
        p.insert("seek_buffer_ms".into(), self.seek_buffer_ms.into());
        p.insert(
            "bitrate_switches".into(),
            PropValue::Int(self.bitrate_switches.into()),
        );
        if let Some(b) = self.avg_bitrate_bps {
            p.insert("avg_bitrate_bps".into(), b.into());
        }
        if let Some(b) = self.max_bitrate_bps {
            p.insert("max_bitrate_bps".into(), b.into());
        }
        p.insert("error_count".into(), PropValue::Int(self.error_count.into()));
        if let Some(e) = self.fatal_error {
            p.insert("fatal_error".into(), e.as_str().into());
        }
        p.insert("exited_before_start".into(), self.exited_before_start.into());
        p
    }
}

/// Tracks one playback session. Not thread-safe; the FFI facade wraps it in a mutex.
#[derive(Debug, Clone)]
pub struct QoeTracker {
    video_id: String,
    playback_type: String,
    requested_at: Option<i64>,
    first_frame_at: Option<i64>,
    playing_since: Option<i64>,
    paused: bool,
    played_ms: u64,
    buffering_since: Option<i64>,
    buffering_for_seek: bool,
    seeking: bool,
    rebuffer_count: u32,
    rebuffer_ms: u64,
    seek_count: u32,
    seek_buffer_ms: u64,
    bitrate: Option<(u64, i64)>,
    bitrate_weighted: u128,
    bitrate_time_ms: u64,
    max_bitrate: Option<u64>,
    switches: u32,
    error_count: u32,
    fatal_error: Option<PlaybackErrorCode>,
    finished: bool,
}

fn span(from: i64, to: i64) -> u64 {
    u64::try_from(to.saturating_sub(from)).unwrap_or(0)
}

impl QoeTracker {
    pub fn new(video_id: &str, playback_type: &str) -> Self {
        Self {
            video_id: video_id.to_owned(),
            playback_type: playback_type.to_owned(),
            requested_at: None,
            first_frame_at: None,
            playing_since: None,
            paused: false,
            played_ms: 0,
            buffering_since: None,
            buffering_for_seek: false,
            seeking: false,
            rebuffer_count: 0,
            rebuffer_ms: 0,
            seek_count: 0,
            seek_buffer_ms: 0,
            bitrate: None,
            bitrate_weighted: 0,
            bitrate_time_ms: 0,
            max_bitrate: None,
            switches: 0,
            error_count: 0,
            fatal_error: None,
            finished: false,
        }
    }

    pub fn play_requested(&mut self, now_ms: i64) {
        if self.requested_at.is_none() {
            self.requested_at = Some(now_ms);
        }
    }

    pub fn first_frame(&mut self, now_ms: i64) {
        if self.first_frame_at.is_some() {
            return;
        }
        self.requested_at.get_or_insert(now_ms);
        self.first_frame_at = Some(now_ms);
        self.playing_since = Some(now_ms);
        if let Some((bps, _)) = self.bitrate {
            self.bitrate = Some((bps, now_ms));
        }
    }

    fn close_playing(&mut self, now_ms: i64) {
        if let Some(since) = self.playing_since.take() {
            self.played_ms += span(since, now_ms);
        }
    }

    pub fn buffering_started(&mut self, now_ms: i64) {
        if self.first_frame_at.is_none() || self.buffering_since.is_some() {
            return; // startup buffering is part of startup time
        }
        self.close_playing(now_ms);
        self.buffering_since = Some(now_ms);
        self.buffering_for_seek = self.seeking;
    }

    pub fn buffering_ended(&mut self, now_ms: i64) {
        let Some(since) = self.buffering_since.take() else {
            return;
        };
        let d = span(since, now_ms);
        if self.buffering_for_seek {
            self.seek_buffer_ms += d;
        } else {
            self.rebuffer_count += 1;
            self.rebuffer_ms += d;
        }
        self.seeking = false;
        if !self.paused {
            self.playing_since = Some(now_ms);
        }
    }

    pub fn seeked(&mut self, _now_ms: i64) {
        self.seek_count += 1;
        self.seeking = true;
    }

    pub fn paused(&mut self, now_ms: i64) {
        self.close_playing(now_ms);
        self.paused = true;
    }

    pub fn resumed(&mut self, now_ms: i64) {
        self.paused = false;
        if self.first_frame_at.is_some() && self.buffering_since.is_none() && self.playing_since.is_none() {
            self.playing_since = Some(now_ms);
        }
        // A seek that needed no buffering is over once playback resumes.
        if self.buffering_since.is_none() {
            self.seeking = false;
        }
    }

    pub fn bitrate_changed(&mut self, now_ms: i64, bitrate_bps: u64) {
        if let Some((prev, since)) = self.bitrate {
            if prev == bitrate_bps {
                return;
            }
            if self.first_frame_at.is_some() {
                let d = span(since, now_ms);
                self.bitrate_weighted += u128::from(prev) * u128::from(d);
                self.bitrate_time_ms += d;
            }
            self.switches += 1;
        }
        self.max_bitrate = Some(self.max_bitrate.map_or(bitrate_bps, |m| m.max(bitrate_bps)));
        self.bitrate = Some((bitrate_bps, now_ms));
    }

    /// Records an error; a fatal one ends playback (the summary carries its code).
    pub fn error(&mut self, now_ms: i64, code: PlaybackErrorCode, fatal: bool) {
        self.error_count += 1;
        if fatal && self.fatal_error.is_none() {
            self.fatal_error = Some(code);
            self.close_playing(now_ms);
        }
    }

    /// Closes the session. Calling it again returns the same numbers.
    pub fn finish(&mut self, now_ms: i64) -> QoeSummary {
        if !self.finished {
            self.finished = true;
            self.close_playing(now_ms);
            if self.buffering_since.is_some() {
                // Left while stalled: the stall counts (it is what the user experienced).
                self.buffering_ended(now_ms);
                self.playing_since = None;
            }
            if let (Some((bps, since)), Some(_)) = (self.bitrate, self.first_frame_at) {
                let d = span(since, now_ms);
                self.bitrate_weighted += u128::from(bps) * u128::from(d);
                self.bitrate_time_ms += d;
                self.bitrate = Some((bps, now_ms));
            }
        }
        let denom = self.played_ms + self.rebuffer_ms;
        QoeSummary {
            video_id: self.video_id.clone(),
            playback_type: self.playback_type.clone(),
            startup_time_ms: match (self.requested_at, self.first_frame_at) {
                (Some(r), Some(f)) => Some(span(r, f)),
                _ => None,
            },
            played_ms: self.played_ms,
            rebuffer_count: self.rebuffer_count,
            rebuffer_ms: self.rebuffer_ms,
            rebuffer_ratio: if denom == 0 {
                0.0
            } else {
                self.rebuffer_ms as f64 / denom as f64
            },
            seek_count: self.seek_count,
            seek_buffer_ms: self.seek_buffer_ms,
            bitrate_switches: self.switches,
            avg_bitrate_bps: if self.bitrate_time_ms > 0 {
                u64::try_from(self.bitrate_weighted / u128::from(self.bitrate_time_ms)).ok()
            } else {
                self.bitrate.map(|(b, _)| b)
            },
            max_bitrate_bps: self.max_bitrate,
            error_count: self.error_count,
            fatal_error: self.fatal_error,
            exited_before_start: self.first_frame_at.is_none() && self.fatal_error.is_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_session() {
        let mut t = QoeTracker::new("v1", "vod");
        t.play_requested(1_000);
        t.bitrate_changed(1_100, 2_000_000);
        t.first_frame(1_800); // startup 800 ms
        // 10 s playing, 2 s rebuffer, 8 s playing.
        t.buffering_started(11_800);
        t.buffering_ended(13_800);
        // A seek with 500 ms buffering is not a rebuffer.
        t.seeked(21_800);
        t.buffering_started(21_800);
        t.buffering_ended(22_300);
        // Switch up for the last 10 s.
        t.bitrate_changed(22_300, 4_000_000);
        t.bitrate_changed(22_300, 4_000_000); // same bitrate: not a switch
        let s = t.finish(32_300);
        assert_eq!(s.startup_time_ms, Some(800));
        assert_eq!(s.rebuffer_count, 1);
        assert_eq!(s.rebuffer_ms, 2_000);
        assert_eq!(s.seek_count, 1);
        assert_eq!(s.seek_buffer_ms, 500);
        assert_eq!(s.played_ms, 10_000 + 8_000 + 10_000);
        assert!((s.rebuffer_ratio - 2_000.0 / 30_000.0).abs() < 1e-9);
        assert_eq!(s.bitrate_switches, 1);
        // 20.5 s at 2 Mbps (incl. buffering), 10 s at 4 Mbps.
        assert_eq!(
            s.avg_bitrate_bps,
            Some((2_000_000u64 * 20_500 + 4_000_000 * 10_000) / 30_500)
        );
        assert_eq!(s.max_bitrate_bps, Some(4_000_000));
        assert!(!s.exited_before_start);
        assert_eq!(t.finish(99_999), s, "finish is idempotent");
        let p = s.to_properties();
        assert_eq!(p["rebuffer_count"], PropValue::Int(1));
        assert_eq!(p["startup_time_ms"], PropValue::Int(800));
    }

    #[test]
    fn pauses_do_not_count() {
        let mut t = QoeTracker::new("v", "vod");
        t.play_requested(0);
        t.first_frame(100);
        t.paused(1_100);
        t.resumed(60_000);
        let s = t.finish(61_000);
        assert_eq!(s.played_ms, 2_000);
        assert_eq!(s.rebuffer_ratio, 0.0);
    }

    #[test]
    fn startup_buffering_and_early_exit() {
        let mut t = QoeTracker::new("v", "live");
        t.play_requested(0);
        t.buffering_started(10);
        t.buffering_ended(500);
        let s = t.finish(3_000);
        assert!(s.exited_before_start);
        assert_eq!(s.startup_time_ms, None);
        assert_eq!(s.rebuffer_count, 0);
    }

    #[test]
    fn fatal_error_and_stall_at_exit() {
        let mut t = QoeTracker::new("v", "vod");
        t.play_requested(0);
        t.first_frame(200);
        t.error(1_000, PlaybackErrorCode::Network, false);
        t.buffering_started(5_200);
        let s = t.finish(9_200);
        assert_eq!(s.rebuffer_count, 1, "stalled at exit counts");
        assert_eq!(s.rebuffer_ms, 4_000);
        assert_eq!(s.error_count, 1);

        let mut t = QoeTracker::new("v", "vod");
        t.play_requested(0);
        t.error(300, PlaybackErrorCode::Drm, true);
        let s = t.finish(400);
        assert_eq!(s.fatal_error, Some(PlaybackErrorCode::Drm));
        assert!(!s.exited_before_start);
        assert_eq!(s.to_properties()["fatal_error"], PropValue::Text("drm".into()));
        assert_eq!(PlaybackErrorCode::SourceNotFound.as_str(), "source_not_found");
    }
}
