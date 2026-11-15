//! FFI facade for `aun-media` (Epic 30): analysis, waveforms, thumbnails, subtitles and the EDL.
//!
//! Small results are typed records. Deep, versioned documents (probe results, EDL projects,
//! transcripts) cross the boundary as JSON so the apps can decode them with their own Codable /
//! kotlinx.serialization / System.Text.Json models generated from the published schemas.
//!
//! Long operations run on a blocking thread and take a [`MediaCancel`] the app can trigger.

use std::path::PathBuf;
use std::sync::Arc;

use aun_media::CancellationToken;

use crate::AunError;

fn internal(e: impl std::fmt::Display) -> AunError {
    AunError::Internal {
        detail: e.to_string(),
    }
}

fn json<T: serde::Serialize>(v: &T) -> Result<String, AunError> {
    serde_json::to_string(v).map_err(internal)
}

fn from_json<T: serde::de::DeserializeOwned>(s: &str, what: &str) -> Result<T, AunError> {
    serde_json::from_str(s).map_err(|e| AunError::InvalidInput {
        detail: format!("{what}: {e}"),
    })
}

/// Runs blocking media work off the caller's executor.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> aun_common::Result<T> + Send + 'static,
) -> Result<T, AunError> {
    Ok(tokio::task::spawn_blocking(f).await.map_err(internal)??)
}

/// Cancels a running media operation. One per operation (or share one to cancel a batch).
#[derive(Debug, Default, uniffi::Object)]
pub struct MediaCancel {
    token: CancellationToken,
}

#[uniffi::export]
impl MediaCancel {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn cancel(&self) {
        self.token.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

fn token(c: Option<Arc<MediaCancel>>) -> CancellationToken {
    c.map(|c| c.token.clone()).unwrap_or_default()
}

// ---- probe ---------------------------------------------------------------------------------

/// Container/codec information as JSON (`aun_media::probe::MediaInfo`).
#[uniffi::export(async_runtime = "tokio")]
pub async fn media_probe_json(path: String) -> Result<String, AunError> {
    let info = blocking(move || aun_media::probe::probe_file(&PathBuf::from(path))).await?;
    json(&info)
}

/// Keyframe timestamps (ms) of the first video track.
#[uniffi::export(async_runtime = "tokio")]
pub async fn media_keyframes(path: String) -> Result<Vec<u64>, AunError> {
    blocking(move || aun_media::probe::probe_keyframes(&PathBuf::from(path))).await
}

// ---- loudness / silence ----------------------------------------------------------------------

/// EBU R128 measurement. `None` values mean the file was (near) silent.
#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct LoudnessReport {
    pub integrated_lufs: Option<f64>,
    pub loudness_range_lu: f64,
    pub true_peak_dbtp: Option<f64>,
}

impl From<aun_media::analysis::LoudnessReport> for LoudnessReport {
    fn from(r: aun_media::analysis::LoudnessReport) -> Self {
        Self {
            integrated_lufs: r.integrated_lufs,
            loudness_range_lu: r.loudness_range_lu,
            true_peak_dbtp: r.true_peak_dbtp,
        }
    }
}

impl From<LoudnessReport> for aun_media::analysis::LoudnessReport {
    fn from(r: LoudnessReport) -> Self {
        Self {
            integrated_lufs: r.integrated_lufs,
            loudness_range_lu: r.loudness_range_lu,
            true_peak_dbtp: r.true_peak_dbtp,
        }
    }
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn media_measure_loudness(
    path: String,
    cancel: Option<Arc<MediaCancel>>,
) -> Result<LoudnessReport, AunError> {
    let t = token(cancel);
    Ok(
        blocking(move || aun_media::analysis::measure_loudness(&PathBuf::from(path), &t))
            .await?
            .into(),
    )
}

/// Gain (dB) to reach `target_lufs` (e.g. -14 for streaming) without exceeding `ceiling_dbtp`.
#[uniffi::export]
pub fn media_gain_to_target_db(report: LoudnessReport, target_lufs: f64, ceiling_dbtp: f64) -> f64 {
    aun_media::analysis::gain_to_target_db(&report.into(), target_lufs, ceiling_dbtp)
}

#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct SilenceConfig {
    pub threshold_dbfs: f64,
    pub min_duration_ms: u64,
}

#[uniffi::export]
pub fn silence_config_default() -> SilenceConfig {
    let d = aun_media::analysis::SilenceConfig::default();
    SilenceConfig {
        threshold_dbfs: d.threshold_dbfs,
        min_duration_ms: d.min_duration_ms,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TimeRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn media_detect_silence(
    path: String,
    config: SilenceConfig,
    cancel: Option<Arc<MediaCancel>>,
) -> Result<Vec<TimeRange>, AunError> {
    let t = token(cancel);
    let cfg = aun_media::analysis::SilenceConfig {
        threshold_dbfs: config.threshold_dbfs,
        min_duration_ms: config.min_duration_ms,
    };
    let ranges = blocking(move || aun_media::analysis::detect_silence(&PathBuf::from(path), cfg, &t)).await?;
    Ok(ranges
        .into_iter()
        .map(|r| TimeRange {
            start_ms: r.start_ms,
            end_ms: r.end_ms,
        })
        .collect())
}

// ---- waveform / thumbnails -------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct WaveformConfig {
    pub samples_per_bucket: u32,
    pub levels: u32,
    pub level_factor: u32,
}

#[uniffi::export]
pub fn waveform_config_default() -> WaveformConfig {
    let d = aun_media::waveform::WaveformConfig::default();
    WaveformConfig {
        samples_per_bucket: d.samples_per_bucket,
        levels: d.levels,
        level_factor: d.level_factor,
    }
}

/// One zoom level: per-bucket min/max sample values in [-1, 1].
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct WaveformLevel {
    pub samples_per_bucket: u32,
    pub mins: Vec<f32>,
    pub maxs: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct WaveformPeaks {
    pub sample_rate: u32,
    pub channels: u32,
    pub total_frames: u64,
    /// Finest first.
    pub levels: Vec<WaveformLevel>,
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn media_generate_waveform(
    path: String,
    config: WaveformConfig,
    cancel: Option<Arc<MediaCancel>>,
) -> Result<WaveformPeaks, AunError> {
    let t = token(cancel);
    let cfg = aun_media::waveform::WaveformConfig {
        samples_per_bucket: config.samples_per_bucket,
        levels: config.levels,
        level_factor: config.level_factor,
    };
    let p = blocking(move || aun_media::waveform::generate_waveform(&PathBuf::from(path), cfg, &t)).await?;
    Ok(WaveformPeaks {
        sample_rate: p.sample_rate,
        channels: p.channels,
        total_frames: p.total_frames,
        levels: p
            .levels
            .into_iter()
            .map(|l| WaveformLevel {
                samples_per_bucket: l.samples_per_bucket,
                mins: l.mins,
                maxs: l.maxs,
            })
            .collect(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct SceneChange {
    pub pts_ms: u64,
    pub score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct ThumbnailConfig {
    pub count: u32,
    pub margin_ms: u64,
    pub snap_window_ms: u64,
    pub cut_offset_ms: u64,
}

#[uniffi::export]
pub fn thumbnail_config_default() -> ThumbnailConfig {
    let d = aun_media::thumbnails::ThumbnailConfig::default();
    ThumbnailConfig {
        count: d.count,
        margin_ms: d.margin_ms,
        snap_window_ms: d.snap_window_ms,
        cut_offset_ms: d.cut_offset_ms,
    }
}

/// Filmstrip / poster timestamps, nudged just after nearby scene cuts.
#[uniffi::export]
pub fn media_select_thumbnail_timestamps(
    duration_ms: u64,
    scenes: Vec<SceneChange>,
    config: ThumbnailConfig,
) -> Vec<u64> {
    let scenes: Vec<_> = scenes
        .into_iter()
        .map(|s| aun_media::analysis::SceneChange {
            pts_ms: s.pts_ms,
            score: s.score,
        })
        .collect();
    aun_media::thumbnails::select_thumbnail_timestamps(
        duration_ms,
        &scenes,
        aun_media::thumbnails::ThumbnailConfig {
            count: config.count,
            margin_ms: config.margin_ms,
            snap_window_ms: config.snap_window_ms,
            cut_offset_ms: config.cut_offset_ms,
        },
    )
}

// ---- transcription models & subtitles ---------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ModelTier {
    Tiny,
    Base,
    Small,
    Medium,
    LargeV3Turbo,
}

impl From<aun_media::transcribe::ModelTier> for ModelTier {
    fn from(t: aun_media::transcribe::ModelTier) -> Self {
        use aun_media::transcribe::ModelTier as T;
        match t {
            T::Tiny => Self::Tiny,
            T::Base => Self::Base,
            T::Small => Self::Small,
            T::Medium => Self::Medium,
            T::LargeV3Turbo => Self::LargeV3Turbo,
        }
    }
}

/// A downloadable whisper model (see docs/MODELS.md for licences).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelDescriptor {
    pub id: String,
    pub tier: ModelTier,
    pub quantization: Option<String>,
    pub file_name: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[uniffi::export]
pub fn transcription_model_catalog() -> Vec<ModelDescriptor> {
    aun_media::transcribe::model_catalog()
        .into_iter()
        .map(descriptor)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct SubtitleOptions {
    pub max_line_chars: u32,
    pub word_timestamps: bool,
}

#[uniffi::export]
pub fn subtitle_options_default() -> SubtitleOptions {
    let d = aun_media::transcribe::SubtitleOptions::default();
    SubtitleOptions {
        max_line_chars: d.max_line_chars,
        word_timestamps: d.word_timestamps,
    }
}

fn subtitle_opts(o: SubtitleOptions) -> aun_media::transcribe::SubtitleOptions {
    aun_media::transcribe::SubtitleOptions {
        max_line_chars: o.max_line_chars,
        word_timestamps: o.word_timestamps,
    }
}

/// WebVTT from a transcript (`aun_media::transcribe::Transcript` JSON).
#[uniffi::export]
pub fn transcript_to_webvtt(transcript_json: String, options: SubtitleOptions) -> Result<String, AunError> {
    let t: aun_media::transcribe::Transcript = from_json(&transcript_json, "transcript")?;
    Ok(aun_media::transcribe::to_webvtt(&t, &subtitle_opts(options)))
}

/// SRT from a transcript (`aun_media::transcribe::Transcript` JSON).
#[uniffi::export]
pub fn transcript_to_srt(transcript_json: String, options: SubtitleOptions) -> Result<String, AunError> {
    let t: aun_media::transcribe::Transcript = from_json(&transcript_json, "transcript")?;
    Ok(aun_media::transcribe::to_srt(&t, &subtitle_opts(options)))
}

// ---- EDL ----------------------------------------------------------------------------------------

/// The EDL schema version this core writes (`schemas/edl/v{N}.json`).
#[uniffi::export]
pub fn edl_schema_version() -> u32 {
    aun_media::edl::CURRENT_SCHEMA_VERSION
}

/// Loads a project of any supported schema version and returns it migrated to the current one.
#[uniffi::export]
pub fn edl_load_project(json: String) -> Result<String, AunError> {
    Ok(aun_media::edl::load_project(&json)?.to_json()?)
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct EdlIssue {
    pub code: String,
    pub item: String,
    pub message: String,
}

/// Problems that would stop a render (overlaps, missing media, bad ranges…). Empty = valid.
#[uniffi::export]
pub fn edl_validate(json: String) -> Result<Vec<EdlIssue>, AunError> {
    let project = aun_media::edl::load_project(&json)?;
    Ok(project
        .validate()
        .into_iter()
        .map(|i| EdlIssue {
            code: i.code,
            item: i.item,
            message: i.message,
        })
        .collect())
}

/// Chapters derived from the project's chapter markers, as JSON.
#[uniffi::export]
pub fn edl_chapters_json(json: String) -> Result<String, AunError> {
    let project = aun_media::edl::load_project(&json)?;
    self::json(&project.chapters())
}

// ---- EDL editing (E19 19.5) ------------------------------------------------------------------------

/// Applies one edit (`aun_media::edl::EditOp` JSON, e.g. `{"op":"split","clipId":"c1","atMs":5000,
/// "newId":"c2"}`) and returns the edited project. A refused edit returns an error and changes nothing.
#[uniffi::export]
pub fn edl_apply_json(project_json: String, op_json: String) -> Result<String, AunError> {
    let mut project = aun_media::edl::load_project(&project_json)?;
    let op: aun_media::edl::EditOp = from_json(&op_json, "edit")?;
    project.apply(op)?;
    Ok(project.to_json()?)
}

/// The clip tracks in timeline order with source paths resolved (`aun_media::edl::FlatTimeline`
/// JSON), for building the platform composition that renders an export.
#[uniffi::export]
pub fn edl_flatten_json(project_json: String) -> Result<String, AunError> {
    let project = aun_media::edl::load_project(&project_json)?;
    json(&project.flatten()?)
}

// ---- On-device transcription (D10) -------------------------------------------------------------------

/// Progress of a long media operation.
#[uniffi::export(with_foreign)]
pub trait MediaProgress: Send + Sync {
    /// `done` out of `total` (bytes for downloads, percent for transcription).
    fn on_progress(&self, done: u64, total: u64);
}

struct ForeignDownload(Arc<dyn MediaProgress>);

impl aun_media::transcribe::DownloadListener for ForeignDownload {
    fn on_progress(&self, downloaded_bytes: u64, total_bytes: u64) {
        self.0.on_progress(downloaded_bytes, total_bytes);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ModelStatus {
    pub model: ModelDescriptor,
    pub installed: bool,
    /// Bytes of an interrupted download that the next download resumes from.
    pub partial_bytes: u64,
}

fn descriptor(m: aun_media::transcribe::ModelDescriptor) -> ModelDescriptor {
    ModelDescriptor {
        id: m.id,
        tier: m.tier.into(),
        quantization: m.quantization,
        file_name: m.file_name,
        url: m.url,
        size_bytes: m.size_bytes,
        sha256: m.sha256,
    }
}

/// Whisper models on this computer: list, download (resumable, checksum-verified), delete.
#[derive(Debug, uniffi::Object)]
pub struct ModelStore {
    manager: Arc<aun_media::transcribe::ModelManager>,
}

#[uniffi::export(async_runtime = "tokio")]
impl ModelStore {
    /// `dir` is the app's own models folder (created when needed).
    #[uniffi::constructor]
    pub fn new(dir: String) -> Arc<Self> {
        let fetcher = Arc::new(aun_media::transcribe::HttpModelFetcher::new());
        Arc::new(Self {
            manager: Arc::new(aun_media::transcribe::ModelManager::new(dir, fetcher)),
        })
    }

    pub fn list(&self) -> Result<Vec<ModelStatus>, AunError> {
        Ok(self
            .manager
            .list()?
            .into_iter()
            .map(|s| ModelStatus {
                model: descriptor(s.descriptor),
                installed: s.installed,
                partial_bytes: s.partial_bytes,
            })
            .collect())
    }

    /// The installed model's file, or `None`.
    pub fn path(&self, id: String) -> Result<Option<String>, AunError> {
        Ok(self.manager.model_path(&id)?)
    }

    pub fn delete(&self, id: String) -> Result<(), AunError> {
        Ok(self.manager.delete(&id)?)
    }

    pub fn disk_usage_bytes(&self) -> Result<u64, AunError> {
        Ok(self.manager.disk_usage_bytes()?)
    }

    /// Downloads and verifies a model; returns its path. Cancelling keeps the partial file.
    pub async fn download(
        &self,
        id: String,
        progress: Arc<dyn MediaProgress>,
        cancel: Option<Arc<MediaCancel>>,
    ) -> Result<String, AunError> {
        let t = token(cancel);
        Ok(self
            .manager
            .download(&id, Arc::new(ForeignDownload(progress)), &t)
            .await?)
    }
}

/// Whether this build can transcribe on the device (built with whisper.cpp).
#[uniffi::export]
pub fn transcription_on_device_available() -> bool {
    cfg!(feature = "whisper")
}

/// Transcribes a media file on this computer with a downloaded model and returns the transcript
/// (`aun_media::transcribe::Transcript` JSON). `language` is ISO 639-1 or `None` to detect it.
/// Progress is reported in percent.
#[uniffi::export(async_runtime = "tokio")]
pub async fn transcribe_on_device_json(
    model_path: String,
    media_path: String,
    language: Option<String>,
    progress: Arc<dyn MediaProgress>,
    cancel: Option<Arc<MediaCancel>>,
) -> Result<String, AunError> {
    #[cfg(feature = "whisper")]
    {
        let t = token(cancel);
        let transcript = blocking(move || {
            let transcriber = aun_media::transcribe::WhisperTranscriber::new(&PathBuf::from(model_path))?;
            let options = aun_media::transcribe::TranscribeOptions {
                language,
                threads: None,
            };
            aun_media::transcribe::transcribe_file(
                &transcriber,
                &PathBuf::from(media_path),
                &options,
                &mut |p| progress.on_progress(u64::from(p), 100),
                &t,
            )
        })
        .await?;
        json(&transcript)
    }
    #[cfg(not(feature = "whisper"))]
    {
        let _ = (model_path, media_path, language, progress, cancel);
        Err(AunError::Unsupported {
            detail: "this build has no on-device transcription".to_owned(),
        })
    }
}

/// Caption cues for the server's on-device transcript upload (`[{startMs,endMs,text}]` JSON), at
/// most two `max_line_chars` lines each.
#[uniffi::export]
pub fn transcript_to_caption_segments_json(
    transcript_json: String,
    max_line_chars: u32,
) -> Result<String, AunError> {
    let t: aun_media::transcribe::Transcript = from_json(&transcript_json, "transcript")?;
    json(&aun_media::transcribe::caption_segments(&t, max_line_chars))
}

// ---- Pre-encoded renditions (D11) ----------------------------------------------------------------------

/// One rung of the server's HLS ladder.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HlsRung {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub maxrate: u32,
    pub bufsize: u32,
    pub audio_bitrate: u32,
    pub profile: String,
    pub gop_seconds: u32,
    pub segment_seconds: u32,
    pub max_fps: f64,
}

/// The rungs the server encodes for a source of this coded size and rotation, lowest first.
#[uniffi::export]
pub fn hls_ladder(width: u32, height: u32, rotation: i32) -> Result<Vec<HlsRung>, AunError> {
    Ok(aun_media::hls::ladder(width, height, rotation)?
        .into_iter()
        .map(|r| HlsRung {
            name: r.name,
            width: r.width,
            height: r.height,
            maxrate: r.maxrate,
            bufsize: r.bufsize,
            audio_bitrate: r.audio_bitrate,
            profile: r.profile,
            gop_seconds: r.gop_seconds,
            segment_seconds: r.segment_seconds,
            max_fps: r.max_fps,
        })
        .collect())
}

/// The frame rate to encode a source of `fps` at (sources above 31 fps are encoded at 30).
#[uniffi::export]
pub fn hls_encode_fps(fps: f64) -> f64 {
    aun_media::hls::encode_fps(fps)
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct StagedFile {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct HlsPackage {
    pub files: Vec<StagedFile>,
    pub segment_seconds: Vec<f64>,
    pub duration_ms: u64,
}

fn staged(files: Vec<aun_media::hls::PackagedFile>) -> Vec<StagedFile> {
    files
        .into_iter()
        .map(|f| StagedFile {
            path: f.path,
            bytes: f.bytes,
        })
        .collect()
}

/// Packages one rung's progressive MP4 into fMP4 HLS in `out_dir` (`index.m3u8`, `init.mp4`,
/// `seg_00001.m4s`…), cut like the server's encode. No re-encoding.
#[uniffi::export(async_runtime = "tokio")]
pub async fn hls_package_mp4(
    input_mp4: String,
    out_dir: String,
    segment_seconds: u32,
    cancel: Option<Arc<MediaCancel>>,
) -> Result<HlsPackage, AunError> {
    let t = token(cancel);
    let pkg = blocking(move || {
        aun_media::hls::package_mp4(
            &PathBuf::from(input_mp4),
            &PathBuf::from(out_dir),
            segment_seconds,
            &t,
        )
    })
    .await?;
    Ok(HlsPackage {
        files: staged(pkg.files),
        segment_seconds: pkg.segment_seconds,
        duration_ms: pkg.duration_ms,
    })
}

/// The `files` list for `POST /api/videos/:id/renditions`: every file under `root_dir/<rung>/`
/// as `<rung>/<file>`, each rung's playlist first.
#[uniffi::export]
pub fn renditions_files(root_dir: String, rungs: Vec<String>) -> Result<Vec<StagedFile>, AunError> {
    Ok(staged(aun_media::hls::renditions_files(
        &PathBuf::from(root_dir),
        &rungs,
    )?))
}
