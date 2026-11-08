//! The serialisable job DAG and its validation.

use std::collections::{HashMap, HashSet};

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// Bumped when a [`JobPlan`] change is not backwards compatible.
pub const JOB_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    Hevc,
}

impl VideoCodec {
    /// The name [`crate::probe`] reports for this codec.
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    Aac,
    Opus,
}

impl AudioCodec {
    /// The name [`crate::probe`] reports for this codec.
    pub fn name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Opus => "opus",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    Mp4,
    Mov,
    M4a,
    Ogg,
}

/// Where a stage should run. Chosen by the planner from [`super::BackendCapabilities`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionTarget {
    Hardware,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Crossfade,
    DipToBlack,
}

/// What flows along a graph edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamType {
    RawVideo,
    RawAudio,
    EncodedVideo,
    EncodedAudio,
    /// Final outputs; nothing consumes them.
    Sink,
}

/// One operation. `start_ms`/`end_ms` are source positions; `None` means the source's edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NodeOp {
    DecodeVideo {
        source: String,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
    },
    DecodeAudio {
        source: String,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
    },
    /// Black frames, for timeline gaps.
    GenerateVideo {
        duration_ms: u64,
        width: u32,
        height: u32,
        fps: f64,
    },
    /// Digital silence, for timeline gaps.
    GenerateAudio {
        duration_ms: u64,
        sample_rate: u32,
        channels: u32,
    },
    Deinterlace,
    Scale {
        width: u32,
        height: u32,
    },
    /// Retimes raw video or audio; `factor` 2.0 plays twice as fast.
    ChangeSpeed {
        factor: f64,
    },
    /// Burns shaped text into raw video between two output-relative times.
    BurnText {
        text: String,
        start_ms: u64,
        end_ms: u64,
        font_family: String,
        size_pt: f64,
        color: String,
        position: String,
    },
    Transition {
        kind: TransitionKind,
        duration_ms: u64,
    },
    Gain {
        db: f64,
    },
    MixAudio,
    /// Joins streams of one type, end to end, in input order.
    Concat,
    /// Normalises as heard in `output_channels`, so a mono source upmixed to stereo lands on target.
    LoudnessNormalize {
        target_lufs: f64,
        true_peak_dbtp: f64,
        output_channels: u32,
    },
    EncodeVideo {
        codec: VideoCodec,
        bitrate_kbps: u32,
        width: u32,
        height: u32,
        fps: Option<f64>,
        keyframe_interval_ms: u64,
        target: ExecutionTarget,
    },
    EncodeAudio {
        codec: AudioCodec,
        bitrate_kbps: u32,
        sample_rate: u32,
        channels: u32,
        target: ExecutionTarget,
    },
    /// Copies compressed video without re-encoding.
    CopyVideo {
        source: String,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
    },
    /// Copies compressed audio without re-encoding.
    CopyAudio {
        source: String,
        start_ms: Option<u64>,
        end_ms: Option<u64>,
    },
    Mux {
        container: ContainerKind,
        output: String,
    },
    /// Writes one image per timestamp into `output_dir`.
    Thumbnail {
        timestamps_ms: Vec<u64>,
        width: u32,
        output_dir: String,
    },
}

/// A graph node. `inputs` name other nodes in this plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobNode {
    pub id: String,
    pub inputs: Vec<String>,
    #[serde(flatten)]
    pub op: NodeOp,
}

impl JobNode {
    pub fn new(id: impl Into<String>, inputs: &[&str], op: NodeOp) -> Self {
        Self {
            id: id.into(),
            inputs: inputs.iter().map(|s| (*s).to_owned()).collect(),
            op,
        }
    }
}

/// A unit of work for one backend call: an encoded-output or sink node and every raw-stream node
/// feeding it, in dependency order. A backend can run a stage as a single native pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobStage {
    /// The id of the stage's final node.
    pub id: String,
    pub nodes: Vec<JobNode>,
    /// Ids of the stages whose output this stage consumes, in input order.
    pub input_stages: Vec<String>,
    /// Relative cost, used to weight overall progress.
    pub weight: f64,
}

/// A validated, serialisable graph of media operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobPlan {
    pub schema_version: u32,
    pub id: String,
    pub nodes: Vec<JobNode>,
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::InvalidInput(format!("job plan: {}", detail.into()))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl JobPlan {
    /// Builds a plan and validates it.
    pub fn new(id: impl Into<String>, nodes: Vec<JobNode>) -> Result<Self> {
        let plan = Self {
            schema_version: JOB_PLAN_SCHEMA_VERSION,
            id: id.into(),
            nodes,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Serialises the plan to JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// Parses and validates JSON produced by [`Self::to_json`].
    pub fn from_json(json: &str) -> Result<Self> {
        let plan: Self = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
        plan.validate()?;
        Ok(plan)
    }

    /// Checks ids, references, acyclicity, parameters and stream types.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != JOB_PLAN_SCHEMA_VERSION {
            return Err(Error::Unsupported(format!(
                "job plan schema version {}",
                self.schema_version
            )));
        }
        if !valid_id(&self.id) {
            return Err(invalid(format!("bad job id {:?}", self.id)));
        }
        if self.nodes.is_empty() {
            return Err(invalid("no nodes"));
        }
        let mut ids = HashSet::new();
        for node in &self.nodes {
            if !valid_id(&node.id) {
                return Err(invalid(format!("bad node id {:?}", node.id)));
            }
            if !ids.insert(node.id.as_str()) {
                return Err(invalid(format!("duplicate node id {}", node.id)));
            }
        }
        for node in &self.nodes {
            for input in &node.inputs {
                if !ids.contains(input.as_str()) {
                    return Err(invalid(format!("{} reads unknown node {input}", node.id)));
                }
            }
            check_params(node)?;
        }
        let order = self.topological_order()?;
        self.check_types(&order)?;
        Ok(())
    }

    /// Node indexes with inputs before their consumers; errors on a cycle.
    fn topological_order(&self) -> Result<Vec<usize>> {
        let index: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let mut pending: Vec<usize> = self.nodes.iter().map(|n| n.inputs.len()).collect();
        let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for (i, node) in self.nodes.iter().enumerate() {
            for input in &node.inputs {
                let from = *index
                    .get(input.as_str())
                    .ok_or_else(|| invalid(format!("unknown node {input}")))?;
                consumers[from].push(i);
            }
        }
        let mut ready: Vec<usize> = (0..self.nodes.len()).filter(|i| pending[*i] == 0).collect();
        ready.reverse();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(i) = ready.pop() {
            order.push(i);
            for consumer in consumers[i].iter().rev() {
                pending[*consumer] -= 1;
                if pending[*consumer] == 0 {
                    ready.push(*consumer);
                }
            }
        }
        if order.len() != self.nodes.len() {
            return Err(invalid("graph has a cycle"));
        }
        Ok(order)
    }

    fn check_types(&self, order: &[usize]) -> Result<()> {
        let mut types: HashMap<&str, StreamType> = HashMap::new();
        let mut consumed: HashSet<&str> = HashSet::new();
        let mut has_sink = false;
        for i in order {
            let node = &self.nodes[*i];
            let inputs: Vec<StreamType> = node
                .inputs
                .iter()
                .map(|n| {
                    types
                        .get(n.as_str())
                        .copied()
                        .ok_or_else(|| invalid(format!("unknown input {n}")))
                })
                .collect::<Result<_>>()?;
            let output = output_type(node, &inputs)?;
            has_sink |= output == StreamType::Sink;
            consumed.extend(node.inputs.iter().map(String::as_str));
            types.insert(node.id.as_str(), output);
        }
        if !has_sink {
            return Err(invalid("no mux or thumbnail output"));
        }
        for node in &self.nodes {
            if types.get(node.id.as_str()) != Some(&StreamType::Sink) && !consumed.contains(node.id.as_str())
            {
                return Err(invalid(format!("output of {} is unused", node.id)));
            }
        }
        Ok(())
    }

    /// Splits a valid plan into backend stages, in execution order.
    pub fn stages(&self) -> Result<Vec<JobStage>> {
        let order = self.topological_order()?;
        let by_id: HashMap<&str, &JobNode> = self.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
        let mut types: HashMap<&str, StreamType> = HashMap::new();
        let mut stages = Vec::new();
        for i in &order {
            let node = &self.nodes[*i];
            let inputs: Vec<StreamType> = node
                .inputs
                .iter()
                .filter_map(|n| types.get(n.as_str()).copied())
                .collect();
            let output = output_type(node, &inputs)?;
            types.insert(node.id.as_str(), output);
            if matches!(output, StreamType::RawVideo | StreamType::RawAudio) {
                continue;
            }
            let mut members: HashSet<&str> = HashSet::new();
            let mut input_stages = Vec::new();
            collect_stage(node, &by_id, &types, &mut members, &mut input_stages);
            let nodes = order
                .iter()
                .map(|j| &self.nodes[*j])
                .filter(|n| members.contains(n.id.as_str()))
                .cloned()
                .collect();
            stages.push(JobStage {
                id: node.id.clone(),
                nodes,
                input_stages,
                weight: stage_weight(&node.op),
            });
        }
        Ok(stages)
    }
}

/// Adds `node` and its raw ancestors; encoded inputs become stage dependencies.
fn collect_stage<'a>(
    node: &'a JobNode,
    by_id: &HashMap<&'a str, &'a JobNode>,
    types: &HashMap<&str, StreamType>,
    members: &mut HashSet<&'a str>,
    input_stages: &mut Vec<String>,
) {
    if !members.insert(node.id.as_str()) {
        return;
    }
    for input in &node.inputs {
        let raw = matches!(
            types.get(input.as_str()),
            Some(StreamType::RawVideo | StreamType::RawAudio)
        );
        match by_id.get(input.as_str()) {
            Some(parent) if raw => collect_stage(parent, by_id, types, members, input_stages),
            Some(_) if !input_stages.contains(input) => input_stages.push(input.clone()),
            _ => {}
        }
    }
}

fn stage_weight(op: &NodeOp) -> f64 {
    match op {
        NodeOp::EncodeVideo { .. } => 10.0,
        NodeOp::EncodeAudio { .. } | NodeOp::Thumbnail { .. } => 1.0,
        _ => 0.2,
    }
}

fn check_params(node: &JobNode) -> Result<()> {
    let id = &node.id;
    let range_ok = |start: &Option<u64>, end: &Option<u64>| match (start, end) {
        (Some(s), Some(e)) => s < e,
        _ => true,
    };
    let bad = |what: &str| Err(invalid(format!("{id}: {what}")));
    let dims_ok = |w: u32, h: u32| w > 0 && h > 0 && w.is_multiple_of(2) && h.is_multiple_of(2);
    match &node.op {
        NodeOp::DecodeVideo {
            source,
            start_ms,
            end_ms,
        }
        | NodeOp::DecodeAudio {
            source,
            start_ms,
            end_ms,
        }
        | NodeOp::CopyVideo {
            source,
            start_ms,
            end_ms,
        }
        | NodeOp::CopyAudio {
            source,
            start_ms,
            end_ms,
        } => {
            if source.is_empty() {
                return bad("empty source");
            }
            if !range_ok(start_ms, end_ms) {
                return bad("start must be before end");
            }
        }
        NodeOp::GenerateVideo {
            duration_ms,
            width,
            height,
            fps,
        } => {
            if *duration_ms == 0 || !dims_ok(*width, *height) || !(*fps > 0.0 && fps.is_finite()) {
                return bad("needs a duration, even dimensions and a frame rate");
            }
        }
        NodeOp::GenerateAudio {
            duration_ms,
            sample_rate,
            channels,
        } => {
            if *duration_ms == 0 || *sample_rate == 0 || *channels == 0 {
                return bad("needs a duration and an audio format");
            }
        }
        NodeOp::Scale { width, height } => {
            if !dims_ok(*width, *height) {
                return bad("dimensions must be positive and even");
            }
        }
        NodeOp::ChangeSpeed { factor } => {
            if !(factor.is_finite() && (0.1..=16.0).contains(factor)) {
                return bad("speed must be between 0.1 and 16");
            }
        }
        NodeOp::BurnText {
            text,
            start_ms,
            end_ms,
            size_pt,
            ..
        } => {
            if text.is_empty() || start_ms >= end_ms || !(*size_pt > 0.0 && size_pt.is_finite()) {
                return bad("needs text, a time range and a size");
            }
        }
        NodeOp::Transition { duration_ms, .. } => {
            if *duration_ms == 0 {
                return bad("transition needs a duration");
            }
        }
        NodeOp::Gain { db } => {
            if !db.is_finite() {
                return bad("gain must be finite");
            }
        }
        NodeOp::LoudnessNormalize {
            target_lufs,
            true_peak_dbtp,
            output_channels,
        } => {
            if !(target_lufs.is_finite() && true_peak_dbtp.is_finite())
                || *target_lufs >= 0.0
                || *output_channels == 0
            {
                return bad("loudness target must be negative");
            }
        }
        NodeOp::EncodeVideo {
            bitrate_kbps,
            width,
            height,
            fps,
            keyframe_interval_ms,
            ..
        } => {
            if *bitrate_kbps == 0 || !dims_ok(*width, *height) || *keyframe_interval_ms == 0 {
                return bad("needs a bitrate, even dimensions and a keyframe interval");
            }
            if fps.is_some_and(|f| !(f > 0.0 && f.is_finite())) {
                return bad("bad frame rate");
            }
        }
        NodeOp::EncodeAudio {
            bitrate_kbps,
            sample_rate,
            channels,
            ..
        } => {
            if *bitrate_kbps == 0 || *sample_rate == 0 || *channels == 0 {
                return bad("needs a bitrate and an audio format");
            }
        }
        NodeOp::Mux { output, .. } => {
            if output.is_empty() {
                return bad("empty output path");
            }
        }
        NodeOp::Thumbnail {
            timestamps_ms,
            width,
            output_dir,
        } => {
            if timestamps_ms.is_empty() || *width == 0 || output_dir.is_empty() {
                return bad("needs timestamps, a width and an output directory");
            }
        }
        NodeOp::Deinterlace | NodeOp::MixAudio | NodeOp::Concat => {}
    }
    Ok(())
}

/// The stream a node produces, or an error if its inputs do not fit.
fn output_type(node: &JobNode, inputs: &[StreamType]) -> Result<StreamType> {
    use StreamType::{EncodedAudio, EncodedVideo, RawAudio, RawVideo, Sink};
    let mismatch = || {
        Err(invalid(format!(
            "{}: inputs {inputs:?} do not fit this operation",
            node.id
        )))
    };
    let only = |expected: StreamType, output: StreamType| {
        if inputs == [expected] {
            Ok(output)
        } else {
            mismatch()
        }
    };
    match &node.op {
        NodeOp::DecodeVideo { .. } | NodeOp::GenerateVideo { .. } if inputs.is_empty() => Ok(RawVideo),
        NodeOp::DecodeAudio { .. } | NodeOp::GenerateAudio { .. } if inputs.is_empty() => Ok(RawAudio),
        NodeOp::CopyVideo { .. } if inputs.is_empty() => Ok(EncodedVideo),
        NodeOp::CopyAudio { .. } if inputs.is_empty() => Ok(EncodedAudio),
        NodeOp::Deinterlace | NodeOp::Scale { .. } | NodeOp::BurnText { .. } => only(RawVideo, RawVideo),
        NodeOp::ChangeSpeed { .. } => match inputs {
            [RawVideo] => Ok(RawVideo),
            [RawAudio] => Ok(RawAudio),
            _ => mismatch(),
        },
        NodeOp::Transition { .. } if inputs == [RawVideo, RawVideo] => Ok(RawVideo),
        NodeOp::Gain { .. } | NodeOp::LoudnessNormalize { .. } => only(RawAudio, RawAudio),
        NodeOp::MixAudio if !inputs.is_empty() && inputs.iter().all(|t| *t == RawAudio) => Ok(RawAudio),
        NodeOp::Concat => match inputs.first() {
            Some(first) if *first != Sink && inputs.iter().all(|t| t == first) => Ok(*first),
            _ => mismatch(),
        },
        NodeOp::EncodeVideo { .. } => only(RawVideo, EncodedVideo),
        NodeOp::EncodeAudio { .. } => only(RawAudio, EncodedAudio),
        NodeOp::Mux { .. } => {
            let videos = inputs.iter().filter(|t| **t == EncodedVideo).count();
            let audios = inputs.iter().filter(|t| **t == EncodedAudio).count();
            if videos + audios == inputs.len() && videos <= 1 && audios <= 1 && !inputs.is_empty() {
                Ok(Sink)
            } else {
                mismatch()
            }
        }
        NodeOp::Thumbnail { .. } => only(RawVideo, Sink),
        _ => mismatch(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(id: &str) -> JobNode {
        JobNode::new(
            id,
            &[],
            NodeOp::DecodeAudio {
                source: "a.wav".into(),
                start_ms: None,
                end_ms: None,
            },
        )
    }

    fn encode(id: &str, input: &str) -> JobNode {
        JobNode::new(
            id,
            &[input],
            NodeOp::EncodeAudio {
                codec: AudioCodec::Aac,
                bitrate_kbps: 128,
                sample_rate: 48_000,
                channels: 2,
                target: ExecutionTarget::Software,
            },
        )
    }

    fn mux(id: &str, inputs: &[&str]) -> JobNode {
        JobNode::new(
            id,
            inputs,
            NodeOp::Mux {
                container: ContainerKind::M4a,
                output: "out.m4a".into(),
            },
        )
    }

    fn message(nodes: Vec<JobNode>) -> String {
        JobPlan::new("j", nodes).unwrap_err().to_string()
    }

    #[test]
    fn valid_plan_roundtrips_as_json() {
        let plan = JobPlan::new("j", vec![decode("d"), encode("e", "d"), mux("m", &["e"])]).unwrap();
        let json = plan.to_json().unwrap();
        assert!(json.contains("\"op\":\"decode_audio\""), "{json}");
        assert_eq!(JobPlan::from_json(&json).unwrap(), plan);
    }

    #[test]
    fn stages_group_raw_nodes_with_their_encoder() {
        let plan = JobPlan::new("j", vec![decode("d"), encode("e", "d"), mux("m", &["e"])]).unwrap();
        let stages = plan.stages().unwrap();
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].id, "e");
        assert_eq!(stages[0].nodes.len(), 2);
        assert_eq!(stages[1].id, "m");
        assert_eq!(stages[1].input_stages, vec!["e".to_owned()]);
    }

    #[test]
    fn declared_order_does_not_matter() {
        let plan = JobPlan::new("j", vec![mux("m", &["e"]), encode("e", "d"), decode("d")]).unwrap();
        let ids: Vec<_> = plan.stages().unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["e", "m"]);
    }

    #[test]
    fn rejects_structural_errors() {
        assert!(message(vec![]).contains("no nodes"));
        assert!(message(vec![decode("d"), decode("d")]).contains("duplicate"));
        assert!(message(vec![encode("e", "ghost"), mux("m", &["e"])]).contains("unknown"));
        assert!(message(vec![decode("bad/id")]).contains("bad node id"));
        assert!(message(vec![decode("d"), encode("e", "d")]).contains("no mux"));
        assert!(
            message(vec![decode("d"), decode("x"), encode("e", "d"), mux("m", &["e"])]).contains("unused")
        );
    }

    #[test]
    fn rejects_cycles() {
        let a = JobNode::new("a", &["b"], NodeOp::Gain { db: 1.0 });
        let b = JobNode::new("b", &["a"], NodeOp::Gain { db: 1.0 });
        assert!(message(vec![a, b, decode("d"), encode("e", "d"), mux("m", &["e"])]).contains("cycle"));
    }

    #[test]
    fn rejects_type_mismatches() {
        // Muxing raw audio.
        assert!(message(vec![decode("d"), mux("m", &["d"])]).contains("do not fit"));
        // Encoding video from an audio stream.
        let bad = JobNode::new(
            "e",
            &["d"],
            NodeOp::EncodeVideo {
                codec: VideoCodec::H264,
                bitrate_kbps: 1000,
                width: 640,
                height: 360,
                fps: None,
                keyframe_interval_ms: 2000,
                target: ExecutionTarget::Software,
            },
        );
        assert!(message(vec![decode("d"), bad, mux("m", &["e"])]).contains("do not fit"));
        // Two audio streams in one mux.
        assert!(
            message(vec![
                decode("d"),
                encode("e1", "d"),
                encode("e2", "d"),
                mux("m", &["e1", "e2"])
            ])
            .contains("do not fit")
        );
    }

    #[test]
    fn rejects_bad_parameters() {
        let scale = JobNode::new(
            "s",
            &["d"],
            NodeOp::Scale {
                width: 641,
                height: 360,
            },
        );
        assert!(message(vec![decode("d"), scale]).contains("even"));
        let speed = JobNode::new("s", &["d"], NodeOp::ChangeSpeed { factor: f64::NAN });
        assert!(message(vec![decode("d"), speed]).contains("speed"));
        let range = JobNode::new(
            "d",
            &[],
            NodeOp::DecodeAudio {
                source: "a".into(),
                start_ms: Some(5),
                end_ms: Some(5),
            },
        );
        assert!(message(vec![range]).contains("before"));
    }

    #[test]
    fn future_schema_is_unsupported() {
        let mut plan = JobPlan::new("j", vec![decode("d"), encode("e", "d"), mux("m", &["e"])]).unwrap();
        plan.schema_version = 99;
        let json = plan.to_json().unwrap();
        assert!(matches!(JobPlan::from_json(&json), Err(Error::Unsupported(_))));
    }
}
