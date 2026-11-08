//! Loudness, silence and scene-change analysis (Epic 30.1).

pub mod loudness;
pub mod scene;
pub mod silence;

pub use loudness::{LoudnessMeter, LoudnessReport, gain_to_target_db, measure_loudness};
pub use scene::{FrameSource, SceneChange, SceneConfig, SceneDetector, VideoFrame, detect_scenes};
pub use silence::{SilenceConfig, SilenceDetector, SilenceRange, detect_silence};

/// Milliseconds from a frame position at a sample rate, without overflow.
pub(crate) fn frames_to_ms(frames: u64, sample_rate: u32) -> u64 {
    ((u128::from(frames) * 1000) / u128::from(sample_rate.max(1))) as u64
}
