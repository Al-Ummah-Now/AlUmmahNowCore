//! EBU R128 loudness measurement.

use std::path::Path;

use aun_common::{Error, Result};
use ebur128::{EbuR128, Mode};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::audio::decode_audio;

/// EBU R128 measurements. Integrated and true peak are `None` for pure silence, where the true
/// values are negative infinity (which JSON cannot carry).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LoudnessReport {
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: f64,
    pub true_peak_dbtp: Option<f64>,
}

/// Incremental EBU R128 meter over interleaved `f32` audio.
pub struct LoudnessMeter {
    inner: EbuR128,
    channels: u32,
}

impl std::fmt::Debug for LoudnessMeter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoudnessMeter")
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

fn r128_err(e: ebur128::Error) -> Error {
    Error::InvalidInput(format!("loudness meter: {e:?}"))
}

impl LoudnessMeter {
    pub fn new(sample_rate: u32, channels: u32) -> Result<Self> {
        if sample_rate == 0 || channels == 0 {
            return Err(Error::InvalidInput("empty audio format".to_owned()));
        }
        let inner =
            EbuR128::new(channels, sample_rate, Mode::I | Mode::LRA | Mode::TRUE_PEAK).map_err(r128_err)?;
        Ok(Self { inner, channels })
    }

    pub fn push(&mut self, interleaved: &[f32]) -> Result<()> {
        self.inner.add_frames_f32(interleaved).map_err(r128_err)
    }

    pub fn finish(&self) -> Result<LoudnessReport> {
        let integrated = self.inner.loudness_global().map_err(r128_err)?;
        let range = self.inner.loudness_range().map_err(r128_err)?;
        let mut peak = 0.0f64;
        for channel in 0..self.channels {
            peak = peak.max(self.inner.true_peak(channel).map_err(r128_err)?);
        }
        Ok(LoudnessReport {
            integrated_lufs: integrated.is_finite().then_some(integrated),
            loudness_range_lu: if range.is_finite() { range } else { 0.0 },
            true_peak_dbtp: (peak > 0.0).then(|| 20.0 * peak.log10()),
        })
    }
}

/// Measures a whole audio file. Blocking and CPU-heavy.
pub fn measure_loudness(path: &Path, cancel: &CancellationToken) -> Result<LoudnessReport> {
    let mut meter: Option<LoudnessMeter> = None;
    decode_audio(path, cancel, &mut |format, block| {
        let m = match &mut meter {
            Some(m) => m,
            slot => slot.insert(LoudnessMeter::new(format.sample_rate, format.channels)?),
        };
        m.push(block)
    })?;
    meter
        .ok_or_else(|| Error::InvalidInput("no audio decoded".to_owned()))?
        .finish()
}

/// The gain in dB that brings `report` to `target_lufs` without pushing the true peak above
/// `ceiling_dbtp`. Returns `0.0` for silence.
pub fn gain_to_target_db(report: &LoudnessReport, target_lufs: f64, ceiling_dbtp: f64) -> f64 {
    let Some(integrated) = report.integrated_lufs else {
        return 0.0;
    };
    let gain = target_lufs - integrated;
    match report.true_peak_dbtp {
        Some(peak) => gain.min(ceiling_dbtp - peak),
        None => gain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine(freq: f32, rate: u32, seconds: f32, amp: f32, channels: u32) -> Vec<f32> {
        let n = (rate as f32 * seconds) as usize;
        let mut out = Vec::with_capacity(n * channels as usize);
        for i in 0..n {
            let v = (TAU * freq * i as f32 / rate as f32).sin() * amp;
            for _ in 0..channels {
                out.push(v);
            }
        }
        out
    }

    #[test]
    fn stereo_997hz_at_minus_23_dbfs_reads_minus_23_lufs() {
        // EBU Tech 3341 case 1: stereo 997 Hz sine at -23 dBFS is -23 LUFS.
        let amp = 10f32.powf(-23.0 / 20.0);
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        meter.push(&sine(997.0, 48_000, 20.0, amp, 2)).unwrap();
        let report = meter.finish().unwrap();
        let lufs = report.integrated_lufs.unwrap();
        assert!((lufs + 23.0).abs() < 0.1, "got {lufs}");
        let peak = report.true_peak_dbtp.unwrap();
        assert!((peak + 23.0).abs() < 0.5, "got {peak}");
        assert!(report.loudness_range_lu < 1.0);
    }

    #[test]
    fn silence_has_no_loudness() {
        let mut meter = LoudnessMeter::new(48_000, 2).unwrap();
        meter.push(&vec![0.0; 48_000 * 2 * 5]).unwrap();
        let report = meter.finish().unwrap();
        assert_eq!(report.integrated_lufs, None);
        assert_eq!(report.true_peak_dbtp, None);
        assert_eq!(gain_to_target_db(&report, -16.0, -1.0), 0.0);
    }

    #[test]
    fn gain_respects_true_peak_ceiling() {
        let report = LoudnessReport {
            integrated_lufs: Some(-30.0),
            loudness_range_lu: 3.0,
            true_peak_dbtp: Some(-5.0),
        };
        assert_eq!(gain_to_target_db(&report, -16.0, -1.0), 4.0);
        let quiet_peak = LoudnessReport {
            true_peak_dbtp: Some(-20.0),
            ..report
        };
        assert_eq!(gain_to_target_db(&quiet_peak, -16.0, -1.0), 14.0);
    }

    #[test]
    fn rejects_empty_format() {
        assert!(LoudnessMeter::new(0, 2).is_err());
        assert!(LoudnessMeter::new(48_000, 0).is_err());
    }
}
