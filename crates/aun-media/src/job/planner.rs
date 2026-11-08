//! Turns a preset and a probed source into a [`JobPlan`].

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use super::backend::BackendCapabilities;
use super::plan::{AudioCodec, ExecutionTarget, JobNode, JobPlan, NodeOp, VideoCodec};
use super::preset::{EncodePreset, PresetKind};
use crate::probe::{MediaInfo, TrackKind};

/// Extracts images alongside an encode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbnailRequest {
    pub timestamps_ms: Vec<u64>,
    pub width: u32,
    pub output_dir: String,
}

/// One encode to plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodeRequest {
    pub job_id: String,
    pub source: String,
    pub output: String,
    pub preset: EncodePreset,
    pub thumbnails: Option<ThumbnailRequest>,
}

/// Scales `width × height` to fit a box, keeping aspect ratio, never upscaling, on even dimensions.
pub fn fit_dimensions(width: u32, height: u32, max_width: u32, max_height: u32) -> (u32, u32) {
    let even = |v: f64| (((v / 2.0).round() as u32) * 2).max(2);
    if width == 0 || height == 0 {
        return (even(f64::from(max_width)), even(f64::from(max_height)));
    }
    let scale = (f64::from(max_width) / f64::from(width))
        .min(f64::from(max_height) / f64::from(height))
        .min(1.0);
    (even(f64::from(width) * scale), even(f64::from(height) * scale))
}

pub(crate) fn pick_video_target(
    caps: &BackendCapabilities,
    codec: VideoCodec,
    width: u32,
    height: u32,
) -> Result<ExecutionTarget> {
    let fits = |target: ExecutionTarget| {
        caps.video_encoders
            .iter()
            .any(|e| e.codec == codec && e.target == target && width <= e.max_width && height <= e.max_height)
    };
    if fits(ExecutionTarget::Hardware) {
        Ok(ExecutionTarget::Hardware)
    } else if fits(ExecutionTarget::Software) {
        Ok(ExecutionTarget::Software)
    } else {
        Err(Error::Unsupported(format!(
            "{} cannot encode {codec:?} at {width}x{height}",
            caps.name
        )))
    }
}

pub(crate) fn pick_audio_target(caps: &BackendCapabilities, codec: AudioCodec) -> Result<ExecutionTarget> {
    let has = |target: ExecutionTarget| {
        caps.audio_encoders
            .iter()
            .any(|e| e.codec == codec && e.target == target)
    };
    if has(ExecutionTarget::Hardware) {
        Ok(ExecutionTarget::Hardware)
    } else if has(ExecutionTarget::Software) {
        Ok(ExecutionTarget::Software)
    } else {
        Err(Error::Unsupported(format!(
            "{} cannot encode {codec:?}",
            caps.name
        )))
    }
}

/// Plans an encode. Tracks the source lacks are skipped; the source is never upscaled.
pub fn plan_encode(request: &EncodeRequest, info: &MediaInfo, caps: &BackendCapabilities) -> Result<JobPlan> {
    let preset = &request.preset;
    preset.validate()?;
    if !caps.containers.contains(&preset.container) {
        return Err(Error::Unsupported(format!(
            "{} cannot write {:?}",
            caps.name, preset.container
        )));
    }
    let source_video = info.first_track(TrackKind::Video);
    let source_audio = info.first_track(TrackKind::Audio);
    let decode_video = || NodeOp::DecodeVideo {
        source: request.source.clone(),
        start_ms: None,
        end_ms: None,
    };

    let mut nodes = Vec::new();
    let mut mux_inputs: Vec<&str> = Vec::new();

    if let (Some(settings), Some(track)) = (&preset.video, source_video) {
        let (source_w, source_h) = (track.width.unwrap_or(0), track.height.unwrap_or(0));
        let (width, height) = fit_dimensions(source_w, source_h, settings.max_width, settings.max_height);
        let target = pick_video_target(caps, settings.codec, width, height)?;
        let mut last = "v_decode";
        nodes.push(JobNode::new(last, &[], decode_video()));
        if settings.deinterlace {
            nodes.push(JobNode::new("v_deinterlace", &[last], NodeOp::Deinterlace));
            last = "v_deinterlace";
        }
        if (width, height) != (source_w, source_h) {
            nodes.push(JobNode::new("v_scale", &[last], NodeOp::Scale { width, height }));
            last = "v_scale";
        }
        let fps = match (settings.max_fps, track.fps) {
            (Some(max), Some(source)) => Some(max.min(source)),
            (max, source) => max.or(source),
        };
        nodes.push(JobNode::new(
            "v_encode",
            &[last],
            NodeOp::EncodeVideo {
                codec: settings.codec,
                bitrate_kbps: settings.bitrate_kbps,
                width,
                height,
                fps,
                keyframe_interval_ms: settings.keyframe_interval_ms,
                target,
            },
        ));
        mux_inputs.push("v_encode");
    }

    if let (Some(settings), Some(_)) = (&preset.audio, source_audio) {
        let target = pick_audio_target(caps, settings.codec)?;
        let mut last = "a_decode";
        nodes.push(JobNode::new(
            last,
            &[],
            NodeOp::DecodeAudio {
                source: request.source.clone(),
                start_ms: None,
                end_ms: None,
            },
        ));
        if let Some(target_lufs) = settings.loudness_target_lufs {
            nodes.push(JobNode::new(
                "a_loudness",
                &[last],
                NodeOp::LoudnessNormalize {
                    target_lufs,
                    true_peak_dbtp: settings.true_peak_ceiling_dbtp,
                    output_channels: settings.channels,
                },
            ));
            last = "a_loudness";
        }
        nodes.push(JobNode::new(
            "a_encode",
            &[last],
            NodeOp::EncodeAudio {
                codec: settings.codec,
                bitrate_kbps: settings.bitrate_kbps,
                sample_rate: settings.sample_rate,
                channels: settings.channels,
                target,
            },
        ));
        mux_inputs.push("a_encode");
    }

    if mux_inputs.is_empty() {
        return Err(Error::InvalidInput(format!(
            "{} has no track this preset can encode",
            request.source
        )));
    }
    nodes.push(JobNode::new(
        "mux",
        &mux_inputs,
        NodeOp::Mux {
            container: preset.container,
            output: request.output.clone(),
        },
    ));

    if let Some(thumbs) = &request.thumbnails {
        if source_video.is_none() {
            return Err(Error::InvalidInput("thumbnails need a video track".to_owned()));
        }
        nodes.push(JobNode::new("t_decode", &[], decode_video()));
        nodes.push(JobNode::new(
            "thumbnails",
            &["t_decode"],
            NodeOp::Thumbnail {
                timestamps_ms: thumbs.timestamps_ms.clone(),
                width: thumbs.width,
                output_dir: thumbs.output_dir.clone(),
            },
        ));
    }
    JobPlan::new(request.job_id.clone(), nodes)
}

/// Plans a Studio editing proxy using the built-in proxy preset.
pub fn plan_proxy(
    job_id: &str,
    source: &str,
    output: &str,
    info: &MediaInfo,
    caps: &BackendCapabilities,
) -> Result<JobPlan> {
    plan_encode(
        &EncodeRequest {
            job_id: job_id.to_owned(),
            source: source.to_owned(),
            output: output.to_owned(),
            preset: EncodePreset::builtin(PresetKind::Proxy),
            thumbnails: None,
        },
        info,
        caps,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::backend::{AudioEncoderCapability, VideoEncoderCapability};
    use crate::job::plan::ContainerKind;
    use crate::probe::{ContainerFormat, TrackInfo};

    fn track(kind: TrackKind) -> TrackInfo {
        TrackInfo {
            index: 0,
            kind,
            codec: "h264".into(),
            duration_ms: Some(1000),
            width: (kind == TrackKind::Video).then_some(1920),
            height: (kind == TrackKind::Video).then_some(1080),
            fps: (kind == TrackKind::Video).then_some(29.97),
            sample_rate: (kind == TrackKind::Audio).then_some(44_100),
            channels: (kind == TrackKind::Audio).then_some(2),
            language: None,
        }
    }

    fn info(tracks: Vec<TrackInfo>) -> MediaInfo {
        MediaInfo {
            container: ContainerFormat::Mp4,
            duration_ms: Some(1000),
            size_bytes: 1,
            bitrate_kbps: None,
            tracks,
        }
    }

    fn caps(hardware_max: Option<u32>) -> BackendCapabilities {
        let mut video_encoders = vec![VideoEncoderCapability {
            codec: VideoCodec::H264,
            target: ExecutionTarget::Software,
            max_width: 8192,
            max_height: 8192,
        }];
        if let Some(max) = hardware_max {
            video_encoders.push(VideoEncoderCapability {
                codec: VideoCodec::H264,
                target: ExecutionTarget::Hardware,
                max_width: max,
                max_height: max,
            });
        }
        BackendCapabilities {
            name: "test".into(),
            video_encoders,
            audio_encoders: vec![AudioEncoderCapability {
                codec: AudioCodec::Aac,
                target: ExecutionTarget::Software,
            }],
            containers: vec![ContainerKind::Mp4, ContainerKind::M4a],
            supports_synthetic_sources: false,
        }
    }

    fn request(kind: PresetKind) -> EncodeRequest {
        EncodeRequest {
            job_id: "j".into(),
            source: "in.mp4".into(),
            output: "out.mp4".into(),
            preset: EncodePreset::builtin(kind),
            thumbnails: None,
        }
    }

    fn encode_target(plan: &JobPlan) -> ExecutionTarget {
        plan.nodes
            .iter()
            .find_map(|n| match n.op {
                NodeOp::EncodeVideo { target, .. } => Some(target),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn fit_keeps_aspect_and_evenness() {
        assert_eq!(fit_dimensions(1920, 1080, 960, 540), (960, 540));
        assert_eq!(fit_dimensions(1280, 720, 1920, 1080), (1280, 720));
        assert_eq!(fit_dimensions(1080, 1920, 960, 540), (304, 540));
        assert_eq!(fit_dimensions(1001, 563, 500, 500), (500, 282));
    }

    #[test]
    fn hardware_when_it_fits_software_otherwise() {
        let source = info(vec![track(TrackKind::Video), track(TrackKind::Audio)]);
        let plan = plan_encode(&request(PresetKind::Lecture), &source, &caps(Some(4096))).unwrap();
        assert_eq!(encode_target(&plan), ExecutionTarget::Hardware);
        let plan = plan_encode(&request(PresetKind::Lecture), &source, &caps(Some(1280))).unwrap();
        assert_eq!(encode_target(&plan), ExecutionTarget::Software);
    }

    #[test]
    fn missing_codec_is_unsupported() {
        let source = info(vec![track(TrackKind::Video)]);
        let req = EncodeRequest {
            preset: EncodePreset::builtin(PresetKind::Lecture).with_video_codec(VideoCodec::Hevc),
            ..request(PresetKind::Lecture)
        };
        assert!(matches!(
            plan_encode(&req, &source, &caps(None)),
            Err(Error::Unsupported(_))
        ));
    }

    #[test]
    fn proxy_scales_and_skips_loudness() {
        let source = info(vec![track(TrackKind::Video), track(TrackKind::Audio)]);
        let plan = plan_proxy("j", "in.mp4", "p.mp4", &source, &caps(None)).unwrap();
        assert!(plan.nodes.iter().any(|n| n.op
            == NodeOp::Scale {
                width: 960,
                height: 540
            }));
        assert!(
            !plan
                .nodes
                .iter()
                .any(|n| matches!(n.op, NodeOp::LoudnessNormalize { .. }))
        );
    }

    #[test]
    fn lecture_normalises_and_caps_fps() {
        let source = info(vec![track(TrackKind::Video), track(TrackKind::Audio)]);
        let plan = plan_encode(&request(PresetKind::Lecture), &source, &caps(None)).unwrap();
        assert!(plan.nodes.iter().any(|n| matches!(
            n.op,
            NodeOp::LoudnessNormalize { target_lufs, .. } if target_lufs == -16.0
        )));
        assert!(plan.nodes.iter().any(|n| matches!(
            n.op,
            NodeOp::EncodeVideo { fps: Some(f), .. } if (f - 29.97).abs() < 1e-9
        )));
        assert!(!plan.nodes.iter().any(|n| matches!(n.op, NodeOp::Scale { .. })));
    }

    #[test]
    fn audio_only_source_drops_the_video_branch() {
        let source = info(vec![track(TrackKind::Audio)]);
        let plan = plan_encode(&request(PresetKind::Lecture), &source, &caps(None)).unwrap();
        assert!(
            !plan
                .nodes
                .iter()
                .any(|n| matches!(n.op, NodeOp::EncodeVideo { .. }))
        );
    }

    #[test]
    fn nothing_to_encode_is_invalid() {
        let source = info(vec![track(TrackKind::Video)]);
        let err = plan_encode(&request(PresetKind::AudioOnly), &source, &caps(None)).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err}");
    }

    #[test]
    fn thumbnails_add_a_second_sink() {
        let source = info(vec![track(TrackKind::Video)]);
        let req = EncodeRequest {
            thumbnails: Some(ThumbnailRequest {
                timestamps_ms: vec![100],
                width: 320,
                output_dir: "thumbs".into(),
            }),
            ..request(PresetKind::Proxy)
        };
        let plan = plan_encode(&req, &source, &caps(None)).unwrap();
        assert_eq!(plan.stages().unwrap().len(), 3);
    }
}
