//! The software reference backend: real PCM audio processing, simulated video, and a simple
//! container. It exists to exercise the runner and the conformance suite and to define correct
//! backend behaviour; its output is never a publishable encode.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use aun_common::{Error, Result};
use tokio_util::sync::CancellationToken;

use super::backend::{
    AudioEncoderCapability, BackendCapabilities, EncodeBackend, StageOutput, StageReporter, StageTask,
    VideoEncoderCapability,
};
use super::plan::{AudioCodec, ContainerKind, ExecutionTarget, JobNode, NodeOp, VideoCodec};
use crate::analysis::{LoudnessMeter, gain_to_target_db};
use crate::audio::{AudioFormat, decode_audio, sample_to_i16};
use crate::probe::{ContainerFormat, MediaInfo, TrackInfo, TrackKind};
use crate::refcontainer::{self, RefTrack};

const SYNTHETIC_PREFIX: &str = "synthetic:";
const BYTES_PER_FRAME: u64 = 16;

/// A generated source the software backend can "decode" without a file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntheticSource {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_ms: u64,
}

impl SyntheticSource {
    /// The URI to use as a plan source.
    pub fn uri(&self) -> String {
        format!(
            "{SYNTHETIC_PREFIX}{}x{}@{}+{}",
            self.width, self.height, self.fps, self.duration_ms
        )
    }

    pub fn parse(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix(SYNTHETIC_PREFIX)?;
        let (size, rest) = rest.split_once('@')?;
        let (width, height) = size.split_once('x')?;
        let (fps, duration) = rest.split_once('+')?;
        Some(Self {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            fps: fps.parse().ok()?,
            duration_ms: duration.parse().ok()?,
        })
    }

    /// Probe-equivalent information: one H.264 video track and one stereo AAC track.
    pub fn media_info(&self) -> MediaInfo {
        let base = TrackInfo {
            index: 0,
            kind: TrackKind::Video,
            codec: "h264".to_owned(),
            duration_ms: Some(self.duration_ms),
            width: Some(self.width),
            height: Some(self.height),
            fps: Some(self.fps),
            sample_rate: None,
            channels: None,
            language: None,
        };
        let audio = TrackInfo {
            index: 1,
            kind: TrackKind::Audio,
            codec: "aac".to_owned(),
            width: None,
            height: None,
            fps: None,
            sample_rate: Some(48_000),
            channels: Some(2),
            ..base.clone()
        };
        MediaInfo {
            container: ContainerFormat::Reference,
            duration_ms: Some(self.duration_ms),
            size_bytes: 0,
            bitrate_kbps: None,
            tracks: vec![base, audio],
        }
    }
}

/// See the module docs. Audio is genuinely decoded, normalised and resampled.
#[derive(Debug, Default, Clone)]
pub struct SoftwareBackend {
    delay_per_step: Option<Duration>,
}

impl SoftwareBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sleeps at every processing step, to make timing-dependent tests deterministic.
    pub fn with_step_delay(mut self, delay: Duration) -> Self {
        self.delay_per_step = Some(delay);
        self
    }

    fn step(&self) {
        if let Some(delay) = self.delay_per_step {
            std::thread::sleep(delay);
        }
    }
}

#[derive(Debug, Clone)]
struct Pcm {
    format: AudioFormat,
    samples: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct VideoMeta {
    width: u32,
    height: u32,
    fps: f64,
    duration_ms: u64,
}

impl EncodeBackend for SoftwareBackend {
    fn capabilities(&self) -> BackendCapabilities {
        let video = |codec| VideoEncoderCapability {
            codec,
            target: ExecutionTarget::Software,
            max_width: 8192,
            max_height: 8192,
        };
        let audio = |codec| AudioEncoderCapability {
            codec,
            target: ExecutionTarget::Software,
        };
        BackendCapabilities {
            name: "software-reference".to_owned(),
            video_encoders: vec![video(VideoCodec::H264), video(VideoCodec::Hevc)],
            audio_encoders: vec![audio(AudioCodec::Aac), audio(AudioCodec::Opus)],
            containers: vec![
                ContainerKind::Mp4,
                ContainerKind::Mov,
                ContainerKind::M4a,
                ContainerKind::Ogg,
            ],
            supports_synthetic_sources: true,
        }
    }

    fn execute_stage(&self, task: StageTask, reporter: Arc<dyn StageReporter>) -> Result<StageOutput> {
        let last = task
            .stage
            .nodes
            .last()
            .ok_or_else(|| Error::InvalidInput("empty stage".to_owned()))?;
        let output = Path::new(&task.output_path);
        match &last.op {
            NodeOp::EncodeAudio {
                codec,
                sample_rate,
                channels,
                ..
            } => {
                let pcm = self.eval_audio(&task, &last.id, reporter.as_ref())?;
                let format = AudioFormat {
                    sample_rate: *sample_rate,
                    channels: *channels,
                };
                let pcm = convert(pcm, format);
                self.write_audio(&pcm, codec.name(), output, reporter.as_ref())?;
                Ok(one(&task.output_path))
            }
            NodeOp::EncodeVideo {
                codec,
                width,
                height,
                fps,
                ..
            } => {
                let meta = self.eval_video(&task, &last.id)?;
                let encoded = VideoMeta {
                    width: *width,
                    height: *height,
                    fps: fps.unwrap_or(meta.fps),
                    ..meta
                };
                self.write_video(encoded, codec.name(), output, reporter.as_ref())?;
                Ok(one(&task.output_path))
            }
            NodeOp::CopyAudio {
                source,
                start_ms,
                end_ms,
            } => {
                self.copy_track(
                    source,
                    TrackKind::Audio,
                    *start_ms,
                    *end_ms,
                    output,
                    reporter.as_ref(),
                )?;
                Ok(one(&task.output_path))
            }
            NodeOp::CopyVideo {
                source,
                start_ms,
                end_ms,
            } => {
                self.copy_track(
                    source,
                    TrackKind::Video,
                    *start_ms,
                    *end_ms,
                    output,
                    reporter.as_ref(),
                )?;
                Ok(one(&task.output_path))
            }
            NodeOp::Concat => {
                self.concat(&task, output, reporter.as_ref())?;
                Ok(one(&task.output_path))
            }
            NodeOp::Mux {
                output: final_path, ..
            } => {
                self.mux(&task, Path::new(final_path), reporter.as_ref())?;
                Ok(one(final_path))
            }
            NodeOp::Thumbnail {
                timestamps_ms,
                width,
                output_dir,
            } => {
                let meta = self.eval_video(&task, &last.id)?;
                let files = self.thumbnails(meta, timestamps_ms, *width, output_dir, reporter.as_ref())?;
                Ok(StageOutput { artifacts: files })
            }
            other => Err(Error::Unsupported(format!(
                "software backend cannot end a stage with {other:?}"
            ))),
        }
    }
}

fn one(path: &str) -> StageOutput {
    StageOutput {
        artifacts: vec![path.to_owned()],
    }
}

fn find<'a>(task: &'a StageTask, id: &str) -> Result<&'a JobNode> {
    task.stage
        .nodes
        .iter()
        .find(|n| n.id == id)
        .ok_or_else(|| Error::Internal(format!("node {id} is not in the stage")))
}

fn lone_input(node: &JobNode) -> Result<&str> {
    node.inputs
        .first()
        .map(String::as_str)
        .ok_or_else(|| Error::InvalidInput(format!("{} has no input", node.id)))
}

fn clip_range(start: Option<u64>, end: Option<u64>, duration: u64) -> (u64, u64) {
    let start = start.unwrap_or(0).min(duration);
    let end = end.unwrap_or(duration).clamp(start, duration);
    (start, end)
}

fn ms_to_frames(ms: u64, rate: u32) -> usize {
    (u128::from(ms) * u128::from(rate) / 1000) as usize
}

/// Linear resampling plus channel up/down-mix: adequate for a reference implementation.
fn convert(pcm: Pcm, target: AudioFormat) -> Pcm {
    if pcm.format == target || pcm.format.channels == 0 {
        return pcm;
    }
    let source_channels = pcm.format.channels as usize;
    let target_channels = target.channels as usize;
    let source_frames = pcm.samples.len() / source_channels;
    let target_frames = (source_frames as u128 * u128::from(target.sample_rate)
        / u128::from(pcm.format.sample_rate.max(1))) as usize;
    let mono_at = |frame: usize| -> f32 {
        let begin = frame * source_channels;
        pcm.samples[begin..begin + source_channels].iter().sum::<f32>() / source_channels as f32
    };
    let channel_at = |frame: usize, channel: usize| -> f32 {
        if source_channels == target_channels {
            pcm.samples[frame * source_channels + channel]
        } else {
            mono_at(frame)
        }
    };
    let mut samples = Vec::with_capacity(target_frames * target_channels);
    let step = f64::from(pcm.format.sample_rate) / f64::from(target.sample_rate.max(1));
    for i in 0..target_frames {
        let pos = i as f64 * step;
        let left = (pos as usize).min(source_frames.saturating_sub(1));
        let right = (left + 1).min(source_frames.saturating_sub(1));
        let t = (pos - left as f64) as f32;
        for channel in 0..target_channels {
            let (a, b) = (channel_at(left, channel), channel_at(right, channel));
            samples.push(a + (b - a) * t);
        }
    }
    Pcm {
        format: target,
        samples,
    }
}

fn apply_gain(pcm: &mut Pcm, db: f64) {
    let factor = 10f64.powf(db / 20.0) as f32;
    for s in &mut pcm.samples {
        *s = (*s * factor).clamp(-1.0, 1.0);
    }
}

fn speed_change(pcm: Pcm, factor: f64) -> Pcm {
    let format = pcm.format;
    let rate = (f64::from(format.sample_rate) / factor).round().max(1.0) as u32;
    let mut out = convert(
        pcm,
        AudioFormat {
            sample_rate: rate,
            channels: format.channels,
        },
    );
    out.format = format;
    out
}

impl SoftwareBackend {
    fn eval_audio(&self, task: &StageTask, id: &str, reporter: &dyn StageReporter) -> Result<Pcm> {
        let mut cache = HashMap::new();
        self.audio_node(task, id, reporter, &mut cache)
    }

    fn audio_node(
        &self,
        task: &StageTask,
        id: &str,
        reporter: &dyn StageReporter,
        cache: &mut HashMap<String, Pcm>,
    ) -> Result<Pcm> {
        if let Some(found) = cache.get(id) {
            return Ok(found.clone());
        }
        let node = find(task, id)?;
        let pcm = match &node.op {
            NodeOp::DecodeAudio {
                source,
                start_ms,
                end_ms,
            } => self.decode(source, *start_ms, *end_ms, reporter)?,
            NodeOp::GenerateAudio {
                duration_ms,
                sample_rate,
                channels,
            } => Pcm {
                format: AudioFormat {
                    sample_rate: *sample_rate,
                    channels: *channels,
                },
                samples: vec![0.0; ms_to_frames(*duration_ms, *sample_rate) * *channels as usize],
            },
            NodeOp::EncodeAudio { .. } => self.audio_node(task, lone_input(node)?, reporter, cache)?,
            NodeOp::Gain { db } => {
                let mut pcm = self.audio_node(task, lone_input(node)?, reporter, cache)?;
                apply_gain(&mut pcm, *db);
                pcm
            }
            NodeOp::ChangeSpeed { factor } => speed_change(
                self.audio_node(task, lone_input(node)?, reporter, cache)?,
                *factor,
            ),
            NodeOp::LoudnessNormalize {
                target_lufs,
                true_peak_dbtp,
                output_channels,
            } => {
                let pcm = self.audio_node(task, lone_input(node)?, reporter, cache)?;
                let layout = AudioFormat {
                    sample_rate: pcm.format.sample_rate,
                    channels: *output_channels,
                };
                let mut pcm = convert(pcm, layout);
                let mut meter = LoudnessMeter::new(pcm.format.sample_rate, pcm.format.channels)?;
                meter.push(&pcm.samples)?;
                let gain = gain_to_target_db(&meter.finish()?, *target_lufs, *true_peak_dbtp);
                apply_gain(&mut pcm, gain);
                pcm
            }
            NodeOp::MixAudio | NodeOp::Concat => {
                let mut parts = Vec::with_capacity(node.inputs.len());
                for input in &node.inputs {
                    parts.push(self.audio_node(task, input, reporter, cache)?);
                }
                let format = parts
                    .first()
                    .map(|p| p.format)
                    .ok_or_else(|| Error::InvalidInput(format!("{id} has no inputs")))?;
                let parts = parts.into_iter().map(|p| convert(p, format));
                let mut samples: Vec<f32> = Vec::new();
                if matches!(node.op, NodeOp::Concat) {
                    for part in parts {
                        samples.extend(part.samples);
                    }
                } else {
                    for part in parts {
                        if part.samples.len() > samples.len() {
                            samples.resize(part.samples.len(), 0.0);
                        }
                        for (mixed, s) in samples.iter_mut().zip(&part.samples) {
                            *mixed = (*mixed + *s).clamp(-1.0, 1.0);
                        }
                    }
                }
                Pcm { format, samples }
            }
            other => {
                return Err(Error::Unsupported(format!("software backend audio op {other:?}")));
            }
        };
        reporter.checkpoint()?;
        cache.insert(id.to_owned(), pcm.clone());
        Ok(pcm)
    }

    fn decode(
        &self,
        source: &str,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
        reporter: &dyn StageReporter,
    ) -> Result<Pcm> {
        if let Some(synthetic) = SyntheticSource::parse(source) {
            return Ok(synthetic_tone(synthetic, start_ms, end_ms));
        }
        let mut format = None;
        let mut samples = Vec::new();
        decode_audio(Path::new(source), &CancellationToken::new(), &mut |f, block| {
            reporter.checkpoint()?;
            format = Some(f);
            samples.extend_from_slice(block);
            Ok(())
        })?;
        let format = format.ok_or_else(|| Error::InvalidInput("no audio decoded".to_owned()))?;
        let channels = format.channels as usize;
        let duration = (samples.len() / channels) as u64 * 1000 / u64::from(format.sample_rate);
        let (start, end) = clip_range(start_ms, end_ms, duration);
        let from = ms_to_frames(start, format.sample_rate) * channels;
        let to = (ms_to_frames(end, format.sample_rate) * channels).min(samples.len());
        Ok(Pcm {
            format,
            samples: samples[from..to].to_vec(),
        })
    }

    fn write_audio(&self, pcm: &Pcm, codec: &str, output: &Path, reporter: &dyn StageReporter) -> Result<()> {
        let chunk = (pcm.format.sample_rate as usize / 10).max(1) * pcm.format.channels as usize;
        let total = pcm.samples.len().div_ceil(chunk).max(1);
        let mut payload = Vec::with_capacity(pcm.samples.len() * 2);
        for (i, block) in pcm.samples.chunks(chunk).enumerate() {
            reporter.checkpoint()?;
            self.step();
            for s in block {
                payload.extend_from_slice(&sample_to_i16(*s).to_le_bytes());
            }
            reporter.progress((i + 1) as f64 / total as f64);
            reporter.checkpoint()?;
        }
        let frames = pcm.samples.len() as u64 / u64::from(pcm.format.channels);
        let track = RefTrack {
            kind: TrackKind::Audio,
            codec: codec.to_owned(),
            duration_ms: frames * 1000 / u64::from(pcm.format.sample_rate),
            width: None,
            height: None,
            fps: None,
            sample_rate: Some(pcm.format.sample_rate),
            channels: Some(pcm.format.channels),
            payload_len: payload.len() as u64,
        };
        refcontainer::write(output, &[(track, payload)])
    }

    fn eval_video(&self, task: &StageTask, id: &str) -> Result<VideoMeta> {
        let node = find(task, id)?;
        match &node.op {
            NodeOp::DecodeVideo {
                source,
                start_ms,
                end_ms,
            } => {
                let mut meta = video_source_meta(source)?;
                let (start, end) = clip_range(*start_ms, *end_ms, meta.duration_ms);
                meta.duration_ms = end - start;
                Ok(meta)
            }
            NodeOp::GenerateVideo {
                duration_ms,
                width,
                height,
                fps,
            } => Ok(VideoMeta {
                width: *width,
                height: *height,
                fps: *fps,
                duration_ms: *duration_ms,
            }),
            NodeOp::Scale { width, height } => Ok(VideoMeta {
                width: *width,
                height: *height,
                ..self.eval_video(task, lone_input(node)?)?
            }),
            NodeOp::ChangeSpeed { factor } => {
                let meta = self.eval_video(task, lone_input(node)?)?;
                Ok(VideoMeta {
                    duration_ms: (meta.duration_ms as f64 / factor).round() as u64,
                    ..meta
                })
            }
            NodeOp::Deinterlace
            | NodeOp::BurnText { .. }
            | NodeOp::EncodeVideo { .. }
            | NodeOp::Thumbnail { .. } => self.eval_video(task, lone_input(node)?),
            NodeOp::Concat | NodeOp::Transition { .. } => {
                let mut first: Option<VideoMeta> = None;
                let mut total = 0;
                for input in &node.inputs {
                    let meta = self.eval_video(task, input)?;
                    total += meta.duration_ms;
                    first.get_or_insert(meta);
                }
                let meta = first.ok_or_else(|| Error::InvalidInput(format!("{id} has no inputs")))?;
                let overlap = match &node.op {
                    NodeOp::Transition { duration_ms, .. } => *duration_ms,
                    _ => 0,
                };
                Ok(VideoMeta {
                    duration_ms: total.saturating_sub(overlap),
                    ..meta
                })
            }
            other => Err(Error::Unsupported(format!("software backend video op {other:?}"))),
        }
    }

    fn write_video(
        &self,
        meta: VideoMeta,
        codec: &str,
        output: &Path,
        reporter: &dyn StageReporter,
    ) -> Result<()> {
        let frames = (meta.duration_ms as f64 * meta.fps / 1000.0).round() as u64;
        let mut payload = Vec::with_capacity((frames * BYTES_PER_FRAME) as usize);
        for i in 0..frames {
            reporter.checkpoint()?;
            self.step();
            payload.extend_from_slice(&i.to_le_bytes());
            payload.extend_from_slice(&[0u8; (BYTES_PER_FRAME - 8) as usize]);
            reporter.progress((i + 1) as f64 / frames as f64);
            reporter.checkpoint()?;
        }
        let track = RefTrack {
            kind: TrackKind::Video,
            codec: codec.to_owned(),
            duration_ms: meta.duration_ms,
            width: Some(meta.width),
            height: Some(meta.height),
            fps: Some(meta.fps),
            sample_rate: None,
            channels: None,
            payload_len: payload.len() as u64,
        };
        refcontainer::write(output, &[(track, payload)])
    }

    fn copy_track(
        &self,
        source: &str,
        kind: TrackKind,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
        output: &Path,
        reporter: &dyn StageReporter,
    ) -> Result<()> {
        reporter.checkpoint()?;
        let path = Path::new(source);
        if SyntheticSource::parse(source).is_some() || !refcontainer::is_reference_file(path)? {
            return Err(Error::Unsupported(
                "software backend can only stream-copy reference containers".to_owned(),
            ));
        }
        let (header, _) = refcontainer::read_header(path)?;
        let index = header
            .tracks
            .iter()
            .position(|t| t.kind == kind)
            .ok_or_else(|| Error::InvalidInput(format!("no {kind:?} track in {source}")))?;
        let (mut track, payload) = refcontainer::read_payload(path, index)?;
        let (start, end) = clip_range(start_ms, end_ms, track.duration_ms);
        let byte_at = |ms: u64| match kind {
            TrackKind::Audio => {
                let rate = track.sample_rate.unwrap_or(1);
                ms_to_frames(ms, rate) * track.channels.unwrap_or(1) as usize * 2
            }
            _ => {
                let frames = (ms as f64 * track.fps.unwrap_or(1.0) / 1000.0).round() as usize;
                frames * BYTES_PER_FRAME as usize
            }
        };
        let (from, to) = (byte_at(start).min(payload.len()), byte_at(end).min(payload.len()));
        let slice = payload[from..to].to_vec();
        track.duration_ms = end - start;
        track.payload_len = slice.len() as u64;
        refcontainer::write(output, &[(track, slice)])?;
        reporter.progress(1.0);
        Ok(())
    }

    fn concat(&self, task: &StageTask, output: &Path, reporter: &dyn StageReporter) -> Result<()> {
        let mut merged: Option<(RefTrack, Vec<u8>)> = None;
        for (i, input) in task.inputs.iter().enumerate() {
            reporter.checkpoint()?;
            let (track, payload) = refcontainer::read_payload(Path::new(&input.path), 0)?;
            match &mut merged {
                None => merged = Some((track, payload)),
                Some((first, bytes)) => {
                    if first.codec != track.codec {
                        return Err(Error::InvalidInput(
                            "cannot concatenate different codecs".to_owned(),
                        ));
                    }
                    first.duration_ms += track.duration_ms;
                    first.payload_len += track.payload_len;
                    bytes.extend_from_slice(&payload);
                }
            }
            reporter.progress((i + 1) as f64 / task.inputs.len() as f64);
        }
        let (track, payload) =
            merged.ok_or_else(|| Error::InvalidInput("nothing to concatenate".to_owned()))?;
        refcontainer::write(output, &[(track, payload)])
    }

    fn mux(&self, task: &StageTask, output: &Path, reporter: &dyn StageReporter) -> Result<()> {
        let mut tracks = Vec::with_capacity(task.inputs.len());
        for input in &task.inputs {
            reporter.checkpoint()?;
            tracks.push(refcontainer::read_payload(Path::new(&input.path), 0)?);
        }
        reporter.checkpoint()?;
        refcontainer::write(output, &tracks)?;
        reporter.progress(1.0);
        Ok(())
    }

    fn thumbnails(
        &self,
        meta: VideoMeta,
        timestamps_ms: &[u64],
        width: u32,
        output_dir: &str,
        reporter: &dyn StageReporter,
    ) -> Result<Vec<String>> {
        let dir = Path::new(output_dir);
        std::fs::create_dir_all(dir)?;
        let height = (u64::from(width) * u64::from(meta.height) / u64::from(meta.width.max(1))).max(1);
        let mut files = Vec::with_capacity(timestamps_ms.len());
        for (i, ts) in timestamps_ms.iter().enumerate() {
            reporter.checkpoint()?;
            if *ts > meta.duration_ms {
                return Err(Error::InvalidInput(format!(
                    "thumbnail at {ts} ms is past the end ({} ms)",
                    meta.duration_ms
                )));
            }
            let path = dir.join(format!("thumb_{i:04}.pgm"));
            let mut image = format!("P5\n{width} {height}\n255\n").into_bytes();
            image.resize(image.len() + (u64::from(width) * height) as usize, 128);
            std::fs::write(&path, image)?;
            files.push(path.to_string_lossy().into_owned());
            reporter.progress((i + 1) as f64 / timestamps_ms.len() as f64);
        }
        Ok(files)
    }
}

fn video_source_meta(source: &str) -> Result<VideoMeta> {
    if let Some(s) = SyntheticSource::parse(source) {
        return Ok(VideoMeta {
            width: s.width,
            height: s.height,
            fps: s.fps,
            duration_ms: s.duration_ms,
        });
    }
    let path = Path::new(source);
    if !refcontainer::is_reference_file(path)? {
        return Err(Error::Unsupported(
            "software backend can only decode synthetic or reference video".to_owned(),
        ));
    }
    let (header, _) = refcontainer::read_header(path)?;
    let track = header
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .ok_or_else(|| Error::InvalidInput(format!("no video track in {source}")))?;
    Ok(VideoMeta {
        width: track.width.unwrap_or(0),
        height: track.height.unwrap_or(0),
        fps: track.fps.unwrap_or(30.0),
        duration_ms: track.duration_ms,
    })
}

/// A −30 dBFS 440 Hz stereo tone, the audio track of a synthetic source.
fn synthetic_tone(source: SyntheticSource, start_ms: Option<u64>, end_ms: Option<u64>) -> Pcm {
    let format = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
    };
    let (start, end) = clip_range(start_ms, end_ms, source.duration_ms);
    let frames = ms_to_frames(end - start, format.sample_rate);
    let amplitude = 10f32.powf(-30.0 / 20.0);
    let mut samples = Vec::with_capacity(frames * 2);
    for i in 0..frames {
        let v = (std::f32::consts::TAU * 440.0 * i as f32 / 48_000.0).sin() * amplitude;
        samples.push(v);
        samples.push(v);
    }
    Pcm { format, samples }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_uri_roundtrip() {
        let s = SyntheticSource {
            width: 1280,
            height: 720,
            fps: 29.97,
            duration_ms: 1500,
        };
        assert_eq!(SyntheticSource::parse(&s.uri()), Some(s));
        assert_eq!(SyntheticSource::parse("/a/b.mp4"), None);
        assert_eq!(SyntheticSource::parse("synthetic:nope"), None);
    }

    #[test]
    fn convert_changes_rate_and_channels() {
        let pcm = Pcm {
            format: AudioFormat {
                sample_rate: 8000,
                channels: 1,
            },
            samples: vec![0.5; 8000],
        };
        let out = convert(
            pcm,
            AudioFormat {
                sample_rate: 16_000,
                channels: 2,
            },
        );
        assert_eq!(out.samples.len(), 32_000);
        assert!(out.samples.iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn speed_shortens_audio() {
        let pcm = Pcm {
            format: AudioFormat {
                sample_rate: 8000,
                channels: 1,
            },
            samples: vec![0.1; 8000],
        };
        let out = speed_change(pcm, 2.0);
        assert_eq!(out.samples.len(), 4000);
        assert_eq!(out.format.sample_rate, 8000);
    }
}
