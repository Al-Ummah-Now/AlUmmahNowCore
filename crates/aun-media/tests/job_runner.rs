#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aun_common::{Error, Result};
use aun_media::audio::{AudioFormat, write_wav_pcm16};
use aun_media::job::{
    AudioCodec, BackendCapabilities, ConformanceOptions, ConformanceStatus, ContainerKind, EncodeBackend,
    EncodePreset, EncodeRequest, ExecutionTarget, JobEvent, JobListener, JobNode, JobPlan, JobRunner,
    JobState, NodeOp, PresetKind, SoftwareBackend, StageOutput, StageReporter, StageTask, SyntheticSource,
    TransitionKind, VideoCodec, plan_encode, run_conformance,
};
use aun_media::probe::{ContainerFormat, TrackKind, probe_file};

#[derive(Default)]
struct Log(Mutex<Vec<JobEvent>>);

impl JobListener for Log {
    fn on_event(&self, event: JobEvent) {
        self.0.lock().unwrap().push(event);
    }
}

fn audio_plan(job: &str, source: &str, output: &str) -> JobPlan {
    let nodes = vec![
        JobNode::new(
            "d",
            &[],
            NodeOp::DecodeAudio {
                source: source.into(),
                start_ms: None,
                end_ms: None,
            },
        ),
        JobNode::new(
            "e",
            &["d"],
            NodeOp::EncodeAudio {
                codec: AudioCodec::Aac,
                bitrate_kbps: 128,
                sample_rate: 48_000,
                channels: 2,
                target: ExecutionTarget::Software,
            },
        ),
        JobNode::new(
            "m",
            &["e"],
            NodeOp::Mux {
                container: ContainerKind::M4a,
                output: output.into(),
            },
        ),
    ];
    JobPlan::new(job, nodes).unwrap()
}

fn write_tone(path: &std::path::Path, seconds: usize) {
    let format = AudioFormat {
        sample_rate: 48_000,
        channels: 2,
    };
    write_wav_pcm16(path, format, &vec![0.1; 96_000 * seconds]).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn software_backend_passes_conformance() {
    let dir = tempfile::tempdir().unwrap();
    let report = run_conformance(
        Arc::new(SoftwareBackend::new()),
        ConformanceOptions::new(dir.path().to_string_lossy()),
    )
    .await
    .unwrap();
    assert!(report.passed(), "{:#?}", report.failures());
    assert_eq!(report.cases.len(), 10);
    assert!(
        report.cases.iter().all(|c| c.status == ConformanceStatus::Passed),
        "{report:#?}"
    );
}

/// A backend that does nothing useful must be caught by the suite.
struct Broken;

impl EncodeBackend for Broken {
    fn capabilities(&self) -> BackendCapabilities {
        SoftwareBackend::new().capabilities()
    }
    fn execute_stage(&self, task: StageTask, _reporter: Arc<dyn StageReporter>) -> Result<StageOutput> {
        Ok(StageOutput {
            artifacts: vec![task.output_path],
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn conformance_catches_a_broken_backend() {
    let dir = tempfile::tempdir().unwrap();
    let report = run_conformance(
        Arc::new(Broken),
        ConformanceOptions::new(dir.path().to_string_lossy()),
    )
    .await
    .unwrap();
    assert!(!report.passed());
    let failed: Vec<_> = report.failures().iter().map(|c| c.name.clone()).collect();
    assert!(failed.iter().any(|n| n == "audio_only_lufs_16"), "{failed:?}");
    assert!(
        failed.iter().any(|n| n == "missing_source_fails_cleanly"),
        "{failed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn video_and_audio_mux_into_the_reference_container() {
    let dir = tempfile::tempdir().unwrap();
    let source = SyntheticSource {
        width: 1920,
        height: 1080,
        fps: 30.0,
        duration_ms: 500,
    };
    let backend = Arc::new(SoftwareBackend::new());
    let output = dir.path().join("o.mp4").to_string_lossy().into_owned();
    let plan = plan_encode(
        &EncodeRequest {
            job_id: "av".into(),
            source: source.uri(),
            output: output.clone(),
            preset: EncodePreset::builtin(PresetKind::Proxy).with_video_codec(VideoCodec::Hevc),
            thumbnails: None,
        },
        &source.media_info(),
        &backend.capabilities(),
    )
    .unwrap();
    let log = Arc::new(Log::default());
    let handle = JobRunner::new(backend)
        .start(plan, &dir.path().join("work"), log.clone())
        .unwrap();
    let outcome = handle.wait().await.unwrap();
    assert_eq!(outcome.outputs, vec![output.clone()]);

    let info = probe_file(std::path::Path::new(&output)).unwrap();
    assert_eq!(info.container, ContainerFormat::Reference);
    let video = info.first_track(TrackKind::Video).unwrap();
    assert_eq!(video.codec, "hevc");
    assert_eq!((video.width, video.height), (Some(960), Some(540)));
    assert!(info.first_track(TrackKind::Audio).is_some());
    assert!(
        !dir.path().join("work/av").exists(),
        "intermediate files must be removed"
    );

    let events = log.0.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, JobEvent::StageStarted { count: 3, .. }))
    );
    assert!(matches!(
        events.last(),
        Some(JobEvent::StateChanged {
            state: JobState::Completed,
            ..
        })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_while_paused_ends_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("s.wav");
    write_tone(&source, 10);
    let output = dir.path().join("o.m4a");
    let plan = audio_plan("p", &source.to_string_lossy(), &output.to_string_lossy());
    let backend = SoftwareBackend::new().with_step_delay(Duration::from_millis(2));
    let handle = JobRunner::new(Arc::new(backend))
        .create(plan, &dir.path().join("work"), Arc::new(Log::default()))
        .unwrap();
    handle.pause();
    handle.start().unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(handle.state(), JobState::Paused);
    assert!(!output.exists());
    handle.cancel();
    assert_eq!(handle.wait().await.unwrap_err(), Error::Cancelled);
    assert_eq!(handle.state(), JobState::Cancelled);
    assert!(!output.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn start_twice_conflicts_and_handle_calls_are_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("s.wav");
    write_tone(&source, 1);
    let output = dir.path().join("o.m4a");
    let plan = audio_plan("t", &source.to_string_lossy(), &output.to_string_lossy());
    let handle = JobRunner::new(Arc::new(SoftwareBackend::new()))
        .create(plan, &dir.path().join("work"), Arc::new(Log::default()))
        .unwrap();
    handle.start().unwrap();
    assert!(matches!(handle.start(), Err(Error::Conflict(_))));
    handle.wait().await.unwrap();
    handle.pause();
    handle.resume();
    handle.cancel();
    assert_eq!(handle.state(), JobState::Completed);
    assert!(handle.wait().await.is_ok(), "wait is repeatable");
}

struct Panicking;

impl EncodeBackend for Panicking {
    fn capabilities(&self) -> BackendCapabilities {
        SoftwareBackend::new().capabilities()
    }
    fn execute_stage(&self, _task: StageTask, _reporter: Arc<dyn StageReporter>) -> Result<StageOutput> {
        panic!("native bug");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn backend_panic_becomes_a_failed_job() {
    let dir = tempfile::tempdir().unwrap();
    let plan = audio_plan("x", "a.wav", &dir.path().join("o").to_string_lossy());
    let handle = JobRunner::new(Arc::new(Panicking))
        .start(plan, &dir.path().join("work"), Arc::new(Log::default()))
        .unwrap();
    assert!(matches!(handle.wait().await, Err(Error::Internal(_))));
    assert_eq!(handle.state(), JobState::Failed);
}

/// Reports wild progress values and records whether the final stage saw its inputs.
struct Noisy(AtomicBool);

impl EncodeBackend for Noisy {
    fn capabilities(&self) -> BackendCapabilities {
        SoftwareBackend::new().capabilities()
    }
    fn execute_stage(&self, task: StageTask, reporter: Arc<dyn StageReporter>) -> Result<StageOutput> {
        for value in [0.5, 0.2, f64::NAN, 7.0, -1.0] {
            reporter.progress(value);
        }
        if task.stage.id == "m" && task.inputs.len() == 1 && task.inputs[0].stage_id == "e" {
            self.0.store(true, Ordering::SeqCst);
        }
        Ok(StageOutput {
            artifacts: vec![task.output_path],
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn progress_is_clamped_and_inputs_are_wired() {
    let dir = tempfile::tempdir().unwrap();
    let plan = audio_plan("n", "a.wav", "o.m4a");
    let backend = Arc::new(Noisy(AtomicBool::new(false)));
    let log = Arc::new(Log::default());
    let handle = JobRunner::new(backend.clone())
        .start(plan, &dir.path().join("work"), log.clone())
        .unwrap();
    handle.wait().await.unwrap();
    assert!(backend.0.load(Ordering::SeqCst));
    let overall: Vec<f64> = log
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            JobEvent::Progress { overall_fraction, .. } => Some(*overall_fraction),
            _ => None,
        })
        .collect();
    assert!(overall.windows(2).all(|w| w[0] <= w[1]), "{overall:?}");
    assert!(overall.iter().all(|f| (0.0..=1.0).contains(f)));
}

#[tokio::test(flavor = "multi_thread")]
async fn software_backend_handles_mix_concat_gain_and_copy() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("s.wav");
    write_tone(&wav, 1);
    let wav = wav.to_string_lossy().into_owned();
    let decode = |id: &str, end| {
        JobNode::new(
            id,
            &[],
            NodeOp::DecodeAudio {
                source: wav.clone(),
                start_ms: Some(0),
                end_ms: Some(end),
            },
        )
    };
    let synthetic = SyntheticSource {
        width: 640,
        height: 360,
        fps: 30.0,
        duration_ms: 1000,
    };
    let dv = |id: &str| {
        JobNode::new(
            id,
            &[],
            NodeOp::DecodeVideo {
                source: synthetic.uri(),
                start_ms: None,
                end_ms: None,
            },
        )
    };
    let output = dir.path().join("o.mp4").to_string_lossy().into_owned();
    let nodes = vec![
        decode("a1", 500),
        decode("a2", 500),
        JobNode::new("gain", &["a2"], NodeOp::Gain { db: -6.0 }),
        JobNode::new("mix", &["a1", "gain"], NodeOp::MixAudio),
        JobNode::new(
            "silence",
            &[],
            NodeOp::GenerateAudio {
                duration_ms: 500,
                sample_rate: 48_000,
                channels: 2,
            },
        ),
        JobNode::new("join", &["mix", "silence"], NodeOp::Concat),
        JobNode::new(
            "ae",
            &["join"],
            NodeOp::EncodeAudio {
                codec: AudioCodec::Opus,
                bitrate_kbps: 96,
                sample_rate: 48_000,
                channels: 2,
                target: ExecutionTarget::Software,
            },
        ),
        dv("v1"),
        dv("v2"),
        JobNode::new(
            "fade",
            &["v1", "v2"],
            NodeOp::Transition {
                kind: TransitionKind::Crossfade,
                duration_ms: 200,
            },
        ),
        JobNode::new(
            "ve",
            &["fade"],
            NodeOp::EncodeVideo {
                codec: VideoCodec::H264,
                bitrate_kbps: 800,
                width: 640,
                height: 360,
                fps: Some(30.0),
                keyframe_interval_ms: 2000,
                target: ExecutionTarget::Software,
            },
        ),
        JobNode::new(
            "m",
            &["ve", "ae"],
            NodeOp::Mux {
                container: ContainerKind::Mp4,
                output: output.clone(),
            },
        ),
    ];
    let plan = JobPlan::new("mix", nodes).unwrap();
    let handle = JobRunner::new(Arc::new(SoftwareBackend::new()))
        .start(plan, &dir.path().join("work"), Arc::new(Log::default()))
        .unwrap();
    handle.wait().await.unwrap();
    let info = probe_file(std::path::Path::new(&output)).unwrap();
    let audio = info.first_track(TrackKind::Audio).unwrap();
    assert_eq!(audio.codec, "opus");
    assert!(audio.duration_ms.unwrap().abs_diff(1000) <= 2);
    assert_eq!(
        info.first_track(TrackKind::Video).unwrap().duration_ms,
        Some(1800)
    );

    // Stream copy of the result, trimmed.
    let copied = dir.path().join("c.mp4").to_string_lossy().into_owned();
    let nodes = vec![
        JobNode::new(
            "ca",
            &[],
            NodeOp::CopyAudio {
                source: output.clone(),
                start_ms: Some(100),
                end_ms: Some(600),
            },
        ),
        JobNode::new(
            "m",
            &["ca"],
            NodeOp::Mux {
                container: ContainerKind::M4a,
                output: copied.clone(),
            },
        ),
    ];
    let handle = JobRunner::new(Arc::new(SoftwareBackend::new()))
        .start(
            JobPlan::new("copy", nodes).unwrap(),
            &dir.path().join("work"),
            Arc::new(Log::default()),
        )
        .unwrap();
    handle.wait().await.unwrap();
    let info = probe_file(std::path::Path::new(&copied)).unwrap();
    assert_eq!(info.first_track(TrackKind::Audio).unwrap().duration_ms, Some(500));
}
