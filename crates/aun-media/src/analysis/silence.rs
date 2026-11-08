//! Silence detection over 10 ms RMS windows.

use std::path::Path;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::frames_to_ms;
use crate::audio::decode_audio;

/// Tuning for [`SilenceDetector`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SilenceConfig {
    /// Windows quieter than this RMS level count as silent.
    pub threshold_dbfs: f64,
    /// Shorter gaps are ignored.
    pub min_duration_ms: u64,
}

impl Default for SilenceConfig {
    fn default() -> Self {
        Self {
            threshold_dbfs: -50.0,
            min_duration_ms: 500,
        }
    }
}

/// A half-open range `[start_ms, end_ms)` of silence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilenceRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl SilenceRange {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// Incremental silence detector over interleaved audio.
#[derive(Debug)]
pub struct SilenceDetector {
    config: SilenceConfig,
    sample_rate: u32,
    channels: usize,
    window_samples: usize,
    threshold_power: f64,
    energy: f64,
    in_window: usize,
    frames_done: u64,
    silence_start: Option<u64>,
    ranges: Vec<SilenceRange>,
}

impl SilenceDetector {
    pub fn new(config: SilenceConfig, sample_rate: u32, channels: u32) -> Result<Self> {
        if sample_rate == 0 || channels == 0 {
            return Err(Error::InvalidInput("empty audio format".to_owned()));
        }
        let channels = channels as usize;
        let window_frames = (sample_rate as usize / 100).max(1);
        Ok(Self {
            config,
            sample_rate,
            channels,
            window_samples: window_frames * channels,
            threshold_power: 10f64.powf(config.threshold_dbfs / 10.0),
            energy: 0.0,
            in_window: 0,
            frames_done: 0,
            silence_start: None,
            ranges: Vec::new(),
        })
    }

    pub fn push(&mut self, interleaved: &[f32]) {
        for sample in interleaved {
            self.energy += f64::from(*sample) * f64::from(*sample);
            self.in_window += 1;
            if self.in_window == self.window_samples {
                self.close_window();
            }
        }
    }

    fn close_window(&mut self) {
        let power = self.energy / self.in_window as f64;
        let frames = (self.in_window / self.channels) as u64;
        let start = self.frames_done;
        self.frames_done += frames;
        self.energy = 0.0;
        self.in_window = 0;
        if power < self.threshold_power {
            self.silence_start.get_or_insert(start);
        } else if let Some(began) = self.silence_start.take() {
            self.record(began, start);
        }
    }

    fn record(&mut self, start: u64, end: u64) {
        let range = SilenceRange {
            start_ms: frames_to_ms(start, self.sample_rate),
            end_ms: frames_to_ms(end, self.sample_rate),
        };
        if range.duration_ms() >= self.config.min_duration_ms {
            self.ranges.push(range);
        }
    }

    /// Flushes the partial window and returns every range found.
    pub fn finish(mut self) -> Vec<SilenceRange> {
        if self.in_window >= self.channels {
            self.close_window();
        }
        if let Some(began) = self.silence_start.take() {
            let end = self.frames_done;
            self.record(began, end);
        }
        self.ranges
    }
}

/// Finds silences in a whole audio file. Blocking.
pub fn detect_silence(
    path: &Path,
    config: SilenceConfig,
    cancel: &CancellationToken,
) -> Result<Vec<SilenceRange>> {
    let mut detector: Option<SilenceDetector> = None;
    decode_audio(path, cancel, &mut |format, block| {
        let d = match &mut detector {
            Some(d) => d,
            slot => slot.insert(SilenceDetector::new(config, format.sample_rate, format.channels)?),
        };
        d.push(block);
        Ok(())
    })?;
    detector
        .map(SilenceDetector::finish)
        .ok_or_else(|| Error::InvalidInput("no audio decoded".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_then(rate: usize, tone_ms: usize, silence_ms: usize, tone2_ms: usize) -> Vec<f32> {
        let mut v = vec![0.5f32; rate * tone_ms / 1000];
        v.extend(vec![0.0f32; rate * silence_ms / 1000]);
        v.extend(vec![0.5f32; rate * tone2_ms / 1000]);
        v
    }

    #[test]
    fn finds_gap_within_a_window() {
        let mut d = SilenceDetector::new(SilenceConfig::default(), 48_000, 1).unwrap();
        d.push(&tone_then(48_000, 1000, 800, 1000));
        let ranges = d.finish();
        assert_eq!(ranges.len(), 1);
        assert!(ranges[0].start_ms.abs_diff(1000) <= 10, "{ranges:?}");
        assert!(ranges[0].end_ms.abs_diff(1800) <= 10, "{ranges:?}");
    }

    #[test]
    fn ignores_short_gaps() {
        let mut d = SilenceDetector::new(SilenceConfig::default(), 48_000, 1).unwrap();
        d.push(&tone_then(48_000, 500, 200, 500));
        assert!(d.finish().is_empty());
    }

    #[test]
    fn trailing_and_leading_silence() {
        let mut d = SilenceDetector::new(SilenceConfig::default(), 16_000, 1).unwrap();
        d.push(&tone_then(16_000, 0, 1000, 500));
        d.push(&vec![0.0; 16_000]);
        let ranges = d.finish();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0].start_ms, 0);
        assert_eq!(ranges[1].end_ms, 2500);
    }

    #[test]
    fn stereo_handles_block_boundaries() {
        let samples = tone_then(48_000 * 2, 1000, 1000, 1000);
        let mut d = SilenceDetector::new(SilenceConfig::default(), 48_000, 2).unwrap();
        for chunk in samples.chunks(77) {
            d.push(chunk);
        }
        let ranges = d.finish();
        assert_eq!(ranges.len(), 1);
        assert!(ranges[0].duration_ms().abs_diff(1000) <= 20);
    }

    #[test]
    fn quiet_noise_is_silence_at_default_threshold() {
        let mut d = SilenceDetector::new(SilenceConfig::default(), 8_000, 1).unwrap();
        d.push(&vec![0.0005; 8_000]);
        assert_eq!(d.finish().len(), 1);
    }
}
