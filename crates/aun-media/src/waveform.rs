//! Multi-resolution waveform peaks for the Studio timeline (Epic 30.3).

use std::path::Path;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::audio::decode_audio;

/// Waveform shape: the finest bucket size and how many coarser levels to derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaveformConfig {
    /// Audio frames per bucket at the finest level.
    pub samples_per_bucket: u32,
    /// Total level count, including the finest (at least 1).
    pub levels: u32,
    /// Each level is this many times coarser than the previous one (at least 2).
    pub level_factor: u32,
}

impl Default for WaveformConfig {
    fn default() -> Self {
        Self {
            samples_per_bucket: 256,
            levels: 4,
            level_factor: 4,
        }
    }
}

/// One resolution. `mins` and `maxs` have the same length, one entry per bucket, across all
/// channels, in `-1.0..=1.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveformLevel {
    pub samples_per_bucket: u32,
    pub mins: Vec<f32>,
    pub maxs: Vec<f32>,
}

/// Peaks at every resolution, finest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveformPeaks {
    pub sample_rate: u32,
    pub channels: u32,
    pub total_frames: u64,
    pub levels: Vec<WaveformLevel>,
}

/// Incremental peak builder over interleaved audio.
#[derive(Debug)]
pub struct WaveformBuilder {
    config: WaveformConfig,
    sample_rate: u32,
    channels: usize,
    mins: Vec<f32>,
    maxs: Vec<f32>,
    current_min: f32,
    current_max: f32,
    frame_samples: usize,
    bucket_frames: u32,
    total_frames: u64,
}

impl WaveformBuilder {
    pub fn new(config: WaveformConfig, sample_rate: u32, channels: u32) -> Result<Self> {
        if sample_rate == 0 || channels == 0 {
            return Err(Error::InvalidInput("empty audio format".to_owned()));
        }
        if config.samples_per_bucket == 0 || config.levels == 0 || config.level_factor < 2 {
            return Err(Error::InvalidInput(
                "waveform needs a bucket size, a level and a factor of at least 2".to_owned(),
            ));
        }
        Ok(Self {
            config,
            sample_rate,
            channels: channels as usize,
            mins: Vec::new(),
            maxs: Vec::new(),
            current_min: f32::INFINITY,
            current_max: f32::NEG_INFINITY,
            frame_samples: 0,
            bucket_frames: 0,
            total_frames: 0,
        })
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        for sample in interleaved {
            let s = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            self.current_min = self.current_min.min(s);
            self.current_max = self.current_max.max(s);
            self.frame_samples += 1;
            if self.frame_samples == self.channels {
                self.frame_samples = 0;
                self.total_frames += 1;
                self.bucket_frames += 1;
                if self.bucket_frames == self.config.samples_per_bucket {
                    self.flush_bucket();
                }
            }
        }
    }

    fn flush_bucket(&mut self) {
        self.mins.push(self.current_min);
        self.maxs.push(self.current_max);
        self.current_min = f32::INFINITY;
        self.current_max = f32::NEG_INFINITY;
        self.bucket_frames = 0;
    }

    /// Flushes the partial bucket and builds the coarser levels.
    pub fn finish(mut self) -> WaveformPeaks {
        if self.bucket_frames > 0 {
            self.flush_bucket();
        }
        let factor = self.config.level_factor as usize;
        let mut levels = vec![WaveformLevel {
            samples_per_bucket: self.config.samples_per_bucket,
            mins: self.mins,
            maxs: self.maxs,
        }];
        for _ in 1..self.config.levels {
            let Some(prev) = levels.last() else { break };
            let next = WaveformLevel {
                samples_per_bucket: prev.samples_per_bucket.saturating_mul(factor as u32),
                mins: prev
                    .mins
                    .chunks(factor)
                    .map(|c| c.iter().copied().fold(f32::INFINITY, f32::min))
                    .collect(),
                maxs: prev
                    .maxs
                    .chunks(factor)
                    .map(|c| c.iter().copied().fold(f32::NEG_INFINITY, f32::max))
                    .collect(),
            };
            levels.push(next);
        }
        WaveformPeaks {
            sample_rate: self.sample_rate,
            channels: self.channels as u32,
            total_frames: self.total_frames,
            levels,
        }
    }
}

/// Builds peaks for a whole audio file. Blocking.
pub fn generate_waveform(
    path: &Path,
    config: WaveformConfig,
    cancel: &CancellationToken,
) -> Result<WaveformPeaks> {
    let mut builder: Option<WaveformBuilder> = None;
    decode_audio(path, cancel, &mut |format, block| {
        let b = match &mut builder {
            Some(b) => b,
            slot => slot.insert(WaveformBuilder::new(config, format.sample_rate, format.channels)?),
        };
        b.push(block);
        Ok(())
    })?;
    builder
        .map(WaveformBuilder::finish)
        .ok_or_else(|| Error::InvalidInput("no audio decoded".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(samples: u32, levels: u32, factor: u32) -> WaveformConfig {
        WaveformConfig {
            samples_per_bucket: samples,
            levels,
            level_factor: factor,
        }
    }

    #[test]
    fn min_max_per_bucket() {
        let mut b = WaveformBuilder::new(config(2, 1, 2), 8000, 1).unwrap();
        b.push(&[0.1, -0.5, 0.9, 0.2, -0.3]);
        let peaks = b.finish();
        let level = &peaks.levels[0];
        assert_eq!(level.mins, vec![-0.5, 0.2, -0.3]);
        assert_eq!(level.maxs, vec![0.1, 0.9, -0.3]);
        assert_eq!(peaks.total_frames, 5);
    }

    #[test]
    fn stereo_buckets_span_all_channels() {
        let mut b = WaveformBuilder::new(config(1, 1, 2), 8000, 2).unwrap();
        b.push(&[0.5, -0.5, 0.1, 0.2]);
        let level = b.finish().levels.remove(0);
        assert_eq!(level.mins, vec![-0.5, 0.1]);
        assert_eq!(level.maxs, vec![0.5, 0.2]);
    }

    #[test]
    fn coarse_levels_aggregate() {
        let mut b = WaveformBuilder::new(config(1, 3, 2), 8000, 1).unwrap();
        b.push(&[0.1, -0.2, 0.3, -0.4, 0.5]);
        let peaks = b.finish();
        assert_eq!(peaks.levels[1].samples_per_bucket, 2);
        assert_eq!(peaks.levels[1].mins, vec![-0.2, -0.4, 0.5]);
        assert_eq!(peaks.levels[1].maxs, vec![0.1, 0.3, 0.5]);
        assert_eq!(peaks.levels[2].samples_per_bucket, 4);
        assert_eq!(peaks.levels[2].mins, vec![-0.4, 0.5]);
        assert_eq!(peaks.levels[2].maxs, vec![0.3, 0.5]);
    }

    #[test]
    fn block_boundaries_do_not_matter() {
        let samples: Vec<f32> = (0..1000).map(|i| ((i % 17) as f32 / 17.0) - 0.5).collect();
        let mut whole = WaveformBuilder::new(WaveformConfig::default(), 8000, 2).unwrap();
        whole.push(&samples);
        let mut pieces = WaveformBuilder::new(WaveformConfig::default(), 8000, 2).unwrap();
        for chunk in samples.chunks(7) {
            pieces.push(chunk);
        }
        assert_eq!(whole.finish(), pieces.finish());
    }

    #[test]
    fn non_finite_samples_are_ignored_and_clamped() {
        let mut b = WaveformBuilder::new(config(3, 1, 2), 8000, 1).unwrap();
        b.push(&[f32::NAN, 2.0, -3.0]);
        let level = b.finish().levels.remove(0);
        assert_eq!(level.mins, vec![-1.0]);
        assert_eq!(level.maxs, vec![1.0]);
    }

    #[test]
    fn rejects_bad_config() {
        assert!(WaveformBuilder::new(config(0, 1, 2), 8000, 1).is_err());
        assert!(WaveformBuilder::new(config(1, 0, 2), 8000, 1).is_err());
        assert!(WaveformBuilder::new(config(1, 1, 1), 8000, 1).is_err());
    }

    #[test]
    fn empty_input_has_empty_levels() {
        let peaks = WaveformBuilder::new(config(4, 2, 2), 8000, 1).unwrap().finish();
        assert_eq!(peaks.total_frames, 0);
        assert!(peaks.levels.iter().all(|l| l.mins.is_empty()));
    }
}
