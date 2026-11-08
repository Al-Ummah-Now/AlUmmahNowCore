//! A suite any [`EncodeBackend`] can run: the native repos call [`run_conformance`] from their own
//! tests with their VideoToolbox/MediaCodec/Media Foundation/GStreamer backend.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::backend::EncodeBackend;
use super::plan::{JobPlan, NodeOp};
use super::planner::{EncodeRequest, ThumbnailRequest, fit_dimensions, plan_encode};
use super::preset::{EncodePreset, PresetKind};
use super::runner::{JobEvent, JobHandle, JobListener, JobOutcome, JobRunner, JobState};
use super::software::SyntheticSource;
use crate::analysis::measure_loudness;
use crate::audio::{AudioFormat, write_wav_pcm16};
use crate::probe::{MediaInfo, TrackKind, probe_file};

const DURATION_TOLERANCE_MS: u64 = 150;
const LOUDNESS_TOLERANCE_LU: f64 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConformanceStatus {
    Passed,
    Failed,
    /// The backend or environment cannot run this case (for example no video fixture).
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceCase {
    pub name: String,
    pub status: ConformanceStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConformanceReport {
    pub backend: String,
    pub cases: Vec<ConformanceCase>,
}

impl ConformanceReport {
    /// `true` when nothing failed. Check [`Self::failures`] for details.
    pub fn passed(&self) -> bool {
        self.cases.iter().all(|c| c.status != ConformanceStatus::Failed)
    }

    pub fn failures(&self) -> Vec<&ConformanceCase> {
        self.cases
            .iter()
            .filter(|c| c.status == ConformanceStatus::Failed)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceOptions {
    /// An empty scratch directory for the corpus and outputs.
    pub work_dir: String,
    /// An H.264 + audio MP4/MOV of at least 2 s, for backends that cannot use synthetic sources.
    pub video_fixture: Option<String>,
    /// Limit for each job, so a hung backend fails instead of blocking forever.
    pub job_timeout_ms: u64,
}

impl ConformanceOptions {
    pub fn new(work_dir: impl Into<String>) -> Self {
        Self {
            work_dir: work_dir.into(),
            video_fixture: None,
            job_timeout_ms: 120_000,
        }
    }
}

enum Verdict {
    Pass(String),
    Skip(String),
}

type CaseResult = std::result::Result<Verdict, String>;

fn pass() -> CaseResult {
    Ok(Verdict::Pass(String::new()))
}

/// Records events and runs an optional action on the first progress report.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<JobEvent>>,
    handle: OnceLock<Arc<JobHandle>>,
    on_first_progress: Mutex<Option<fn(&JobHandle)>>,
}

impl Recorder {
    fn with_action(action: fn(&JobHandle)) -> Self {
        Self {
            on_first_progress: Mutex::new(Some(action)),
            ..Self::default()
        }
    }

    fn events(&self) -> Vec<JobEvent> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn progress_count(&self) -> usize {
        self.events()
            .iter()
            .filter(|e| matches!(e, JobEvent::Progress { .. }))
            .count()
    }
}

impl JobListener for Recorder {
    fn on_event(&self, event: JobEvent) {
        let is_progress = matches!(event, JobEvent::Progress { .. });
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
        if is_progress {
            let action = self
                .on_first_progress
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let (Some(action), Some(handle)) = (action, self.handle.get()) {
                action(handle);
            }
        }
    }
}

struct Suite {
    backend: Arc<dyn EncodeBackend>,
    root: PathBuf,
    video: Option<(String, MediaInfo)>,
    timeout: Duration,
}

/// Runs every case against `backend`. Individual failures are reported, not returned as errors.
pub async fn run_conformance(
    backend: Arc<dyn EncodeBackend>,
    options: ConformanceOptions,
) -> Result<ConformanceReport> {
    let root = PathBuf::from(&options.work_dir);
    std::fs::create_dir_all(&root)?;
    let capabilities = backend.capabilities();
    let video = match &options.video_fixture {
        Some(path) => Some((path.clone(), probe_file(Path::new(path))?)),
        None if capabilities.supports_synthetic_sources => {
            let source = SyntheticSource {
                width: 1280,
                height: 720,
                fps: 30.0,
                duration_ms: 2000,
            };
            Some((source.uri(), source.media_info()))
        }
        None => None,
    };
    let suite = Suite {
        backend,
        root,
        video,
        timeout: Duration::from_millis(options.job_timeout_ms),
    };
    let tone = suite.write_tone("tone_stereo_48k.wav", 48_000, 2, 3.0, -30.0)?;
    let mono = suite.write_tone("tone_mono_44k.wav", 44_100, 1, 3.0, -24.0)?;
    let long = suite.write_tone("tone_long.wav", 48_000, 2, 30.0, -20.0)?;

    let mut cases = Vec::new();
    let mut record = |name: &str, result: CaseResult| {
        let (status, detail) = match result {
            Ok(Verdict::Pass(d)) => (ConformanceStatus::Passed, d),
            Ok(Verdict::Skip(d)) => (ConformanceStatus::Skipped, d),
            Err(d) => (ConformanceStatus::Failed, d),
        };
        cases.push(ConformanceCase {
            name: name.to_owned(),
            status,
            detail,
        });
    };
    record("capabilities", check_capabilities(&suite));
    record(
        "audio_only_lufs_16",
        suite.audio_case(&tone, "lufs16", -16.0).await,
    );
    record(
        "audio_only_lufs_14_resamples_mono",
        suite.audio_case(&mono, "lufs14", -14.0).await,
    );
    record("progress_is_monotonic", suite.progress_case(&tone).await);
    record("cancel_stops_without_output", suite.cancel_case(&long).await);
    record("pause_and_resume", suite.pause_case(&long).await);
    record("missing_source_fails_cleanly", suite.missing_source_case().await);
    record("video_proxy", suite.video_case(PresetKind::Proxy, "proxy").await);
    record(
        "video_lecture",
        suite.video_case(PresetKind::Lecture, "lecture").await,
    );
    record("thumbnails", suite.thumbnail_case().await);
    Ok(ConformanceReport {
        backend: capabilities.name,
        cases,
    })
}

fn check_capabilities(suite: &Suite) -> CaseResult {
    let caps = suite.backend.capabilities();
    if caps.name.is_empty() {
        return Err("backend has no name".to_owned());
    }
    if caps.audio_encoders.is_empty() || caps.containers.is_empty() {
        return Err("backend lists no audio encoder or container".to_owned());
    }
    pass()
}

fn err(e: Error) -> String {
    e.to_string()
}

impl Suite {
    fn write_tone(&self, name: &str, rate: u32, channels: u32, seconds: f32, dbfs: f32) -> Result<String> {
        let path = self.root.join(name);
        let amplitude = 10f32.powf(dbfs / 20.0);
        let frames = (rate as f32 * seconds) as usize;
        let mut samples = Vec::with_capacity(frames * channels as usize);
        for i in 0..frames {
            let v = (std::f32::consts::TAU * 997.0 * i as f32 / rate as f32).sin() * amplitude;
            samples.extend(std::iter::repeat_n(v, channels as usize));
        }
        write_wav_pcm16(
            &path,
            AudioFormat {
                sample_rate: rate,
                channels,
            },
            &samples,
        )?;
        Ok(path.to_string_lossy().into_owned())
    }

    fn output(&self, name: &str) -> String {
        self.root.join(name).to_string_lossy().into_owned()
    }

    fn create(&self, plan: JobPlan, recorder: &Arc<Recorder>) -> Result<Arc<JobHandle>> {
        let listener: Arc<dyn JobListener> = recorder.clone();
        let handle =
            JobRunner::new(Arc::clone(&self.backend)).create(plan, &self.root.join("work"), listener)?;
        let _ = recorder.handle.set(Arc::clone(&handle));
        Ok(handle)
    }

    async fn run_job(&self, plan: JobPlan, recorder: &Arc<Recorder>) -> Result<JobOutcome> {
        let handle = self.create(plan, recorder)?;
        handle.start()?;
        self.wait(&handle).await
    }

    async fn wait(&self, handle: &JobHandle) -> Result<JobOutcome> {
        match tokio::time::timeout(self.timeout, handle.wait()).await {
            Ok(result) => result,
            Err(_) => {
                handle.cancel();
                Err(Error::Internal("job timed out".to_owned()))
            }
        }
    }

    fn audio_plan(&self, source: &str, output: &str, job: &str, lufs: Option<f64>) -> Result<JobPlan> {
        let info = probe_file(Path::new(source))?;
        plan_encode(
            &EncodeRequest {
                job_id: job.to_owned(),
                source: source.to_owned(),
                output: output.to_owned(),
                preset: EncodePreset::builtin(PresetKind::AudioOnly).with_loudness_target(lufs),
                thumbnails: None,
            },
            &info,
            &self.backend.capabilities(),
        )
    }

    async fn audio_case(&self, source: &str, label: &str, lufs: f64) -> CaseResult {
        let output = self.output(&format!("{label}.m4a"));
        let plan = self.audio_plan(source, &output, label, Some(lufs)).map_err(err)?;
        let recorder = Arc::new(Recorder::default());
        let outcome = self.run_job(plan, &recorder).await.map_err(err)?;
        if !outcome.outputs.contains(&output) {
            return Err(format!("outputs {:?} miss {output}", outcome.outputs));
        }
        let info = probe_file(Path::new(&output)).map_err(err)?;
        check_duration(info.duration_ms, 3000)?;
        let track = info
            .first_track(TrackKind::Audio)
            .ok_or("output has no audio track")?;
        if track.codec != "aac" {
            return Err(format!("expected aac, got {}", track.codec));
        }
        if track.channels != Some(2) || track.sample_rate != Some(48_000) {
            return Err(format!(
                "expected 48 kHz stereo, got {:?} Hz {:?} ch",
                track.sample_rate, track.channels
            ));
        }
        match measure_loudness(Path::new(&output), &CancellationToken::new()) {
            Ok(report) => {
                let got = report.integrated_lufs.ok_or("output is silent")?;
                if (got - lufs).abs() > LOUDNESS_TOLERANCE_LU {
                    return Err(format!("loudness {got:.2} LUFS, wanted {lufs}"));
                }
                Ok(Verdict::Pass(format!("{got:.2} LUFS")))
            }
            Err(Error::Unsupported(detail)) => Ok(Verdict::Pass(format!("loudness not checked: {detail}"))),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn progress_case(&self, source: &str) -> CaseResult {
        let output = self.output("progress.m4a");
        let plan = self.audio_plan(source, &output, "progress", None).map_err(err)?;
        let recorder = Arc::new(Recorder::default());
        self.run_job(plan, &recorder).await.map_err(err)?;
        let events = recorder.events();
        let overall: Vec<f64> = events
            .iter()
            .filter_map(|e| match e {
                JobEvent::Progress { overall_fraction, .. } => Some(*overall_fraction),
                _ => None,
            })
            .collect();
        if overall.is_empty() {
            return Err("no progress events".to_owned());
        }
        if overall.windows(2).any(|w| w[1] < w[0]) {
            return Err(format!("overall progress went backwards: {overall:?}"));
        }
        let last = overall.last().copied().unwrap_or(0.0);
        if (last - 1.0).abs() > 1e-6 {
            return Err(format!("final progress was {last}"));
        }
        let states: Vec<JobState> = events
            .iter()
            .filter_map(|e| match e {
                JobEvent::StateChanged { state, .. } => Some(*state),
                _ => None,
            })
            .collect();
        if states != [JobState::Running, JobState::Completed] {
            return Err(format!("unexpected states {states:?}"));
        }
        pass()
    }

    async fn cancel_case(&self, source: &str) -> CaseResult {
        let output = self.output("cancel.m4a");
        let plan = self.audio_plan(source, &output, "cancel", None).map_err(err)?;
        let recorder = Arc::new(Recorder::with_action(|h| h.cancel()));
        let handle = self.create(plan, &recorder).map_err(err)?;
        handle.start().map_err(err)?;
        match self.wait(&handle).await {
            Err(Error::Cancelled) => {}
            other => return Err(format!("expected Cancelled, got {other:?}")),
        }
        if handle.state() != JobState::Cancelled {
            return Err(format!("state is {:?}", handle.state()));
        }
        if Path::new(&output).exists() {
            return Err("cancelled job left an output file".to_owned());
        }
        pass()
    }

    async fn pause_case(&self, source: &str) -> CaseResult {
        let output = self.output("pause.m4a");
        let plan = self.audio_plan(source, &output, "pause", None).map_err(err)?;
        let recorder = Arc::new(Recorder::with_action(|h| h.pause()));
        let handle = self.create(plan, &recorder).map_err(err)?;
        handle.start().map_err(err)?;
        let deadline = tokio::time::Instant::now() + self.timeout;
        while handle.state() != JobState::Paused {
            if handle.state().is_terminal() || tokio::time::Instant::now() > deadline {
                handle.cancel();
                return Err(format!("never paused; state {:?}", handle.state()));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Reports already in flight may land just after the pause; then it must go quiet.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let before = recorder.progress_count();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let after = recorder.progress_count();
        if after != before {
            handle.cancel();
            return Err(format!("{} progress events while paused", after - before));
        }
        handle.resume();
        self.wait(&handle).await.map_err(err)?;
        if !Path::new(&output).exists() {
            return Err("resumed job produced no output".to_owned());
        }
        pass()
    }

    async fn missing_source_case(&self) -> CaseResult {
        // Planned from a real file so that execution, not planning, meets the missing source.
        let real = self.root.join("tone_stereo_48k.wav");
        let info = probe_file(&real).map_err(err)?;
        let source = self.output("does_not_exist.wav");
        let plan = plan_encode(
            &EncodeRequest {
                job_id: "missing".to_owned(),
                source,
                output: self.output("missing.m4a"),
                preset: EncodePreset::builtin(PresetKind::AudioOnly),
                thumbnails: None,
            },
            &info,
            &self.backend.capabilities(),
        )
        .map_err(err)?;
        let recorder = Arc::new(Recorder::default());
        match self.run_job(plan, &recorder).await {
            Err(Error::Cancelled) | Ok(_) => Err("expected the job to fail".to_owned()),
            Err(_) => pass(),
        }
    }

    async fn video_case(&self, kind: PresetKind, label: &str) -> CaseResult {
        let Some((source, info)) = &self.video else {
            return Ok(Verdict::Skip("no video fixture".to_owned()));
        };
        let preset = EncodePreset::builtin(kind);
        let settings = preset.video.clone().ok_or("preset has no video")?;
        let source_track = info.first_track(TrackKind::Video).ok_or("fixture has no video")?;
        let expected = fit_dimensions(
            source_track.width.unwrap_or(0),
            source_track.height.unwrap_or(0),
            settings.max_width,
            settings.max_height,
        );
        let output = self.output(&format!("{label}.mp4"));
        let plan = plan_encode(
            &EncodeRequest {
                job_id: label.to_owned(),
                source: source.clone(),
                output: output.clone(),
                preset,
                thumbnails: None,
            },
            info,
            &self.backend.capabilities(),
        );
        let plan = match plan {
            Ok(p) => p,
            Err(Error::Unsupported(detail)) => return Ok(Verdict::Skip(detail)),
            Err(e) => return Err(e.to_string()),
        };
        let recorder = Arc::new(Recorder::default());
        self.run_job(plan, &recorder).await.map_err(err)?;
        let out = probe_file(Path::new(&output)).map_err(err)?;
        check_duration(out.duration_ms, info.duration_ms.unwrap_or(0))?;
        let video = out.first_track(TrackKind::Video).ok_or("output has no video")?;
        if video.codec != settings.codec.name() {
            return Err(format!("codec {}, wanted {}", video.codec, settings.codec.name()));
        }
        if (video.width, video.height) != (Some(expected.0), Some(expected.1)) {
            return Err(format!(
                "{:?}x{:?}, wanted {expected:?}",
                video.width, video.height
            ));
        }
        if out.first_track(TrackKind::Audio).is_none() {
            return Err("output has no audio".to_owned());
        }
        pass()
    }

    async fn thumbnail_case(&self) -> CaseResult {
        let Some((source, info)) = &self.video else {
            return Ok(Verdict::Skip("no video fixture".to_owned()));
        };
        let output_dir = self.output("thumbs");
        let plan = plan_encode(
            &EncodeRequest {
                job_id: "thumbs".to_owned(),
                source: source.clone(),
                output: self.output("thumbs.mp4"),
                preset: EncodePreset::builtin(PresetKind::Proxy),
                thumbnails: Some(ThumbnailRequest {
                    timestamps_ms: vec![200, 1000, 1800],
                    width: 320,
                    output_dir,
                }),
            },
            info,
            &self.backend.capabilities(),
        );
        let plan = match plan {
            Ok(p) => p,
            Err(Error::Unsupported(detail)) => return Ok(Verdict::Skip(detail)),
            Err(e) => return Err(e.to_string()),
        };
        if !plan
            .nodes
            .iter()
            .any(|n| matches!(n.op, NodeOp::Thumbnail { .. }))
        {
            return Err("planner dropped the thumbnail node".to_owned());
        }
        let recorder = Arc::new(Recorder::default());
        let outcome = self.run_job(plan, &recorder).await.map_err(err)?;
        // The mux output plus one image per timestamp.
        if outcome.outputs.len() != 4 {
            return Err(format!("expected 4 outputs, got {:?}", outcome.outputs));
        }
        for path in &outcome.outputs {
            let size = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?.len();
            if size == 0 {
                return Err(format!("{path} is empty"));
            }
        }
        pass()
    }
}

fn check_duration(got: Option<u64>, wanted: u64) -> std::result::Result<(), String> {
    let got = got.ok_or("output has no duration")?;
    if got.abs_diff(wanted) > DURATION_TOLERANCE_MS {
        return Err(format!("duration {got} ms, wanted about {wanted} ms"));
    }
    Ok(())
}
