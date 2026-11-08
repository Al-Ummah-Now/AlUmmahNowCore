//! Encode presets.

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use super::plan::{AudioCodec, ContainerKind, VideoCodec};

/// The built-in preset families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetKind {
    Lecture,
    /// Audio-first: high audio bitrate, modest video.
    Recitation,
    Conference,
    Proxy,
    AudioOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    pub max_width: u32,
    pub max_height: u32,
    pub bitrate_kbps: u32,
    pub max_fps: Option<f64>,
    pub keyframe_interval_ms: u64,
    pub deinterlace: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioSettings {
    pub codec: AudioCodec,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u32,
    /// Integrated loudness to normalise to, or `None` to keep the source level.
    pub loudness_target_lufs: Option<f64>,
    pub true_peak_ceiling_dbtp: f64,
}

/// What to produce. Start from [`EncodePreset::builtin`] and adjust.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodePreset {
    pub kind: PresetKind,
    pub video: Option<VideoSettings>,
    pub audio: Option<AudioSettings>,
    pub container: ContainerKind,
}

fn video(max_width: u32, max_height: u32, bitrate_kbps: u32, max_fps: f64) -> VideoSettings {
    VideoSettings {
        codec: VideoCodec::H264,
        max_width,
        max_height,
        bitrate_kbps,
        max_fps: Some(max_fps),
        keyframe_interval_ms: 2000,
        deinterlace: false,
    }
}

fn audio(bitrate_kbps: u32, loudness_target_lufs: Option<f64>) -> AudioSettings {
    AudioSettings {
        codec: AudioCodec::Aac,
        bitrate_kbps,
        sample_rate: 48_000,
        channels: 2,
        loudness_target_lufs,
        true_peak_ceiling_dbtp: -1.0,
    }
}

impl EncodePreset {
    /// A built-in preset, with loudness at −16 LUFS (−14 for recitation).
    pub fn builtin(kind: PresetKind) -> Self {
        let (v, a, container) = match kind {
            PresetKind::Lecture => (
                Some(video(1920, 1080, 5000, 30.0)),
                Some(audio(160, Some(-16.0))),
                ContainerKind::Mp4,
            ),
            PresetKind::Recitation => (
                Some(video(1280, 720, 2000, 30.0)),
                Some(audio(256, Some(-14.0))),
                ContainerKind::Mp4,
            ),
            PresetKind::Conference => (
                Some(VideoSettings {
                    deinterlace: true,
                    ..video(1920, 1080, 8000, 60.0)
                }),
                Some(audio(192, Some(-16.0))),
                ContainerKind::Mp4,
            ),
            PresetKind::Proxy => (
                Some(video(960, 540, 1500, 30.0)),
                Some(audio(96, None)),
                ContainerKind::Mp4,
            ),
            PresetKind::AudioOnly => (None, Some(audio(192, Some(-16.0))), ContainerKind::M4a),
        };
        Self {
            kind,
            video: v,
            audio: a,
            container,
        }
    }

    /// Sets the video codec, if this preset has video.
    pub fn with_video_codec(mut self, codec: VideoCodec) -> Self {
        if let Some(v) = &mut self.video {
            v.codec = codec;
        }
        self
    }

    /// Sets the audio codec and moves audio-only output to a container that can hold it.
    pub fn with_audio_codec(mut self, codec: AudioCodec) -> Self {
        if let Some(a) = &mut self.audio {
            a.codec = codec;
        }
        self.container = match (self.container, codec) {
            (ContainerKind::M4a, AudioCodec::Opus) => ContainerKind::Ogg,
            (ContainerKind::Ogg, AudioCodec::Aac) => ContainerKind::M4a,
            (other, _) => other,
        };
        self
    }

    /// Sets the loudness target; `None` keeps the source level.
    pub fn with_loudness_target(mut self, lufs: Option<f64>) -> Self {
        if let Some(a) = &mut self.audio {
            a.loudness_target_lufs = lufs;
        }
        self
    }

    /// Checks that the settings make a producible output.
    pub fn validate(&self) -> Result<()> {
        if self.video.is_none() && self.audio.is_none() {
            return Err(Error::InvalidInput(
                "preset has neither video nor audio".to_owned(),
            ));
        }
        if self.video.is_some() && matches!(self.container, ContainerKind::M4a | ContainerKind::Ogg) {
            return Err(Error::InvalidInput(format!(
                "container {:?} cannot hold video",
                self.container
            )));
        }
        if let Some(a) = &self.audio {
            let fits = match self.container {
                ContainerKind::M4a => a.codec == AudioCodec::Aac,
                ContainerKind::Ogg => a.codec == AudioCodec::Opus,
                ContainerKind::Mp4 | ContainerKind::Mov => true,
            };
            if !fits {
                return Err(Error::InvalidInput(format!(
                    "container {:?} cannot hold {:?}",
                    self.container, a.codec
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_valid() {
        for kind in [
            PresetKind::Lecture,
            PresetKind::Recitation,
            PresetKind::Conference,
            PresetKind::Proxy,
            PresetKind::AudioOnly,
        ] {
            EncodePreset::builtin(kind).validate().unwrap();
        }
    }

    #[test]
    fn loudness_targets() {
        let target = |k| EncodePreset::builtin(k).audio.unwrap().loudness_target_lufs;
        assert_eq!(target(PresetKind::Lecture), Some(-16.0));
        assert_eq!(target(PresetKind::Recitation), Some(-14.0));
        assert_eq!(target(PresetKind::Proxy), None);
    }

    #[test]
    fn opus_audio_only_moves_to_ogg() {
        let p = EncodePreset::builtin(PresetKind::AudioOnly).with_audio_codec(AudioCodec::Opus);
        assert_eq!(p.container, ContainerKind::Ogg);
        p.validate().unwrap();
    }

    #[test]
    fn invalid_combinations() {
        let mut p = EncodePreset::builtin(PresetKind::Lecture);
        p.container = ContainerKind::M4a;
        assert!(p.validate().is_err());
        let mut p = EncodePreset::builtin(PresetKind::AudioOnly);
        p.audio = None;
        assert!(p.validate().is_err());
    }
}
