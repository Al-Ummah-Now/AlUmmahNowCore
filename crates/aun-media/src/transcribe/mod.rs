//! Transcription (Epic 30.4). The types, model manager, subtitle writers and chapter suggestions
//! are always compiled; inference needs the `whisper` feature, or any other [`Transcriber`].

mod chapters;
mod models;
mod subtitles;
#[cfg(feature = "whisper")]
mod whisper;

use std::path::Path;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::audio::decode_audio;

pub use chapters::{ChapterConfig, ChapterSuggestion, suggest_chapters};
#[cfg(feature = "http-download")]
pub use models::HttpModelFetcher;
pub use models::{
    ChunkSink, DownloadListener, FetchInfo, ModelDescriptor, ModelFetcher, ModelManager, ModelStatus,
    ModelTier, model_catalog,
};
pub use subtitles::{SubtitleOptions, to_srt, to_webvtt};
#[cfg(feature = "whisper")]
pub use whisper::WhisperTranscriber;

/// The sample rate [`Transcriber`] input must use.
pub const TRANSCRIBE_SAMPLE_RATE: u32 = 16_000;

/// A word with its own timing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptWord {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// Model confidence in `0.0..=1.0`.
    pub confidence: f64,
}

/// A sentence-sized stretch of speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub words: Vec<TranscriptWord>,
}

/// A full transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    /// ISO 639-1 language, detected or requested.
    pub language: String,
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TranscribeOptions {
    /// ISO 639-1 language, or `None` to detect it.
    pub language: Option<String>,
    /// Inference threads; `None` picks a default.
    pub threads: Option<u32>,
}

/// Turns speech into a transcript. Blocking and CPU-heavy: call it on a worker thread.
pub trait Transcriber: Send + Sync {
    /// `pcm` is mono at [`TRANSCRIBE_SAMPLE_RATE`]. `progress` receives percentages in `0..=100`.
    fn transcribe(
        &self,
        pcm: &[f32],
        options: &TranscribeOptions,
        progress: &mut dyn FnMut(u32),
        cancel: &CancellationToken,
    ) -> Result<Transcript>;
}

/// Decodes an audio file, converts it to 16 kHz mono and transcribes it.
pub fn transcribe_file(
    transcriber: &dyn Transcriber,
    path: &Path,
    options: &TranscribeOptions,
    progress: &mut dyn FnMut(u32),
    cancel: &CancellationToken,
) -> Result<Transcript> {
    let pcm = load_speech_pcm(path, cancel)?;
    transcriber.transcribe(&pcm, options, progress, cancel)
}

/// Decodes a file to mono 16 kHz, low-pass filtered, ready for a [`Transcriber`].
pub fn load_speech_pcm(path: &Path, cancel: &CancellationToken) -> Result<Vec<f32>> {
    let mut mono = Vec::new();
    let mut rate = 0;
    decode_audio(path, cancel, &mut |format, block| {
        rate = format.sample_rate;
        let channels = format.channels as usize;
        mono.extend(
            block
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
        Ok(())
    })?;
    if rate == 0 {
        return Err(Error::InvalidInput("no audio decoded".to_owned()));
    }
    Ok(resample(&mono, rate, TRANSCRIBE_SAMPLE_RATE))
}

/// Windowed-sinc resampling; when downsampling the cutoff drops to the new Nyquist frequency.
pub(crate) fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    const ZEROS: f64 = 12.0;
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0);
    let half_width = ZEROS / cutoff;
    let output_len = (input.len() as f64 * ratio).round() as usize;
    let mut output = Vec::with_capacity(output_len);
    for n in 0..output_len {
        let centre = n as f64 / ratio;
        let first = (centre - half_width).ceil().max(0.0) as usize;
        let last = ((centre + half_width).floor() as usize).min(input.len() - 1);
        let mut value = 0.0;
        for (i, sample) in input.iter().enumerate().take(last + 1).skip(first) {
            let x = i as f64 - centre;
            let sinc = if x.abs() < 1e-9 {
                1.0
            } else {
                let px = std::f64::consts::PI * x * cutoff;
                px.sin() / px
            };
            let window = 0.5 + 0.5 * (std::f64::consts::PI * x / half_width).cos();
            value += f64::from(*sample) * sinc * window * cutoff;
        }
        output.push(value as f32);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f64::consts::TAU * freq * i as f64 / f64::from(rate)).sin() as f32)
            .collect()
    }

    fn rms(samples: &[f32]) -> f64 {
        (samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    #[test]
    fn speech_band_tone_passes() {
        let out = resample(&sine(1000.0, 44_100, 44_100), 44_100, 16_000);
        assert_eq!(out.len(), 16_000);
        let steady = &out[1000..15_000];
        assert!((rms(steady) - 0.707).abs() < 0.02, "{}", rms(steady));
    }

    #[test]
    fn content_above_nyquist_is_removed() {
        let out = resample(&sine(12_000.0, 48_000, 48_000), 48_000, 16_000);
        assert!(rms(&out[1000..15_000]) < 0.01);
    }

    #[test]
    fn same_rate_is_identity() {
        let input = vec![0.1, 0.2];
        assert_eq!(resample(&input, 16_000, 16_000), input);
        assert!(resample(&[], 8000, 16_000).is_empty());
    }
}
