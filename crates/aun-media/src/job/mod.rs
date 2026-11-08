//! The encode job graph (Epic 30.2): presets are planned into a platform-neutral [`JobPlan`], which
//! a [`JobRunner`] executes stage by stage on an [`EncodeBackend`]. Native apps implement the
//! backend over VideoToolbox, MediaCodec, Media Foundation or GStreamer and validate it with
//! [`run_conformance`].

mod backend;
mod conformance;
mod plan;
mod planner;
mod preset;
mod runner;
mod software;

pub use backend::{
    AudioEncoderCapability, BackendCapabilities, EncodeBackend, HardwareEncoder, StageArtifact, StageOutput,
    StageReporter, StageTask, VideoEncoderCapability,
};
pub use conformance::{
    ConformanceCase, ConformanceOptions, ConformanceReport, ConformanceStatus, run_conformance,
};
pub use plan::{
    AudioCodec, ContainerKind, ExecutionTarget, JOB_PLAN_SCHEMA_VERSION, JobNode, JobPlan, JobStage, NodeOp,
    StreamType, TransitionKind, VideoCodec,
};
pub use planner::{EncodeRequest, ThumbnailRequest, fit_dimensions, plan_encode, plan_proxy};
pub use preset::{AudioSettings, EncodePreset, PresetKind, VideoSettings};
pub use runner::{JobEvent, JobHandle, JobListener, JobOutcome, JobRunner, JobState};
pub use software::{SoftwareBackend, SyntheticSource};
