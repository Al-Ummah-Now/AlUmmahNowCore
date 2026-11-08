//! The contract native encoders implement.

use std::sync::Arc;

use aun_common::Result;
use serde::{Deserialize, Serialize};

use super::plan::{AudioCodec, ContainerKind, ExecutionTarget, JobStage, VideoCodec};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEncoderCapability {
    pub codec: VideoCodec,
    pub target: ExecutionTarget,
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioEncoderCapability {
    pub codec: AudioCodec,
    pub target: ExecutionTarget,
}

/// What a backend can do on this device; the planner picks hardware when it fits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapabilities {
    pub name: String,
    pub video_encoders: Vec<VideoEncoderCapability>,
    pub audio_encoders: Vec<AudioEncoderCapability>,
    pub containers: Vec<ContainerKind>,
    /// Whether `synthetic:` source URIs work. Only the software backend sets this.
    pub supports_synthetic_sources: bool,
}

/// The output of an earlier stage this stage reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageArtifact {
    pub stage_id: String,
    pub path: String,
}

/// One stage to execute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageTask {
    pub job_id: String,
    pub stage: JobStage,
    pub inputs: Vec<StageArtifact>,
    /// Where an encoded-stream stage writes its result. Inside the job's work directory.
    pub output_path: String,
    pub work_dir: String,
}

/// What a stage produced. Encoded-stream stages return one path; sinks return their final files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageOutput {
    pub artifacts: Vec<String>,
}

/// Given to a backend during a stage. Cheap and thread-safe.
pub trait StageReporter: Send + Sync {
    /// Reports stage progress in `0.0..=1.0`.
    fn progress(&self, fraction: f64);
    /// Call often. Blocks while the job is paused; returns `Err(Cancelled)` once cancelled, which
    /// the backend must propagate promptly.
    fn checkpoint(&self) -> Result<()>;
}

/// Executes stages. Called from a blocking worker thread, one stage at a time per job, so an
/// implementation may block until the stage finishes.
///
/// Sink stages must write their final file atomically (write elsewhere, then rename), so a
/// cancelled or failed job never leaves a partial output.
pub trait EncodeBackend: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;
    fn execute_stage(&self, task: StageTask, reporter: Arc<dyn StageReporter>) -> Result<StageOutput>;
}

/// The name `docs/PLAN.md` uses for the native adapter.
pub use self::EncodeBackend as HardwareEncoder;
