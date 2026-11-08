//! Decides, segment by segment, what can be stream-copied and what must be re-encoded, and builds
//! the [`JobPlan`] that does it.

use std::collections::BTreeSet;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use super::model::{Clip, Project, Source, TrackKind, Transition};
use crate::job::{
    BackendCapabilities, EncodePreset, JobNode, JobPlan, NodeOp, VideoSettings, fit_dimensions,
};

/// One render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderRequest {
    pub job_id: String,
    pub output: String,
    pub preset: EncodePreset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentDecision {
    Copy,
    ReEncode,
}

/// A span of the output video timeline and how it will be produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    /// `None` for a gap, which renders as black.
    pub clip_id: Option<String>,
    pub decision: SegmentDecision,
    /// Why a segment is re-encoded; empty for copies.
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderPlan {
    pub job: JobPlan,
    pub segments: Vec<RenderSegment>,
    pub audio_copied: bool,
}

/// Text drawn over a span of the timeline.
#[derive(Clone)]
struct Overlay {
    start_ms: u64,
    end_ms: u64,
    text: String,
    font_family: String,
    size_pt: f64,
    color: String,
    position: String,
}

/// What occupies one timeline interval.
struct Interval<'a> {
    start_ms: u64,
    end_ms: u64,
    clip: Option<&'a Clip>,
    transition: Option<(&'a Transition, &'a Clip, &'a Clip)>,
    overlays: Vec<Overlay>,
}

/// Plans the render of a valid project.
pub fn plan_render(
    project: &Project,
    request: &RenderRequest,
    caps: &BackendCapabilities,
) -> Result<RenderPlan> {
    project.ensure_valid()?;
    request.preset.validate()?;
    if !caps.containers.contains(&request.preset.container) {
        return Err(Error::Unsupported(format!(
            "{} cannot write {:?}",
            caps.name, request.preset.container
        )));
    }
    let mut builder = Builder {
        project,
        request,
        caps,
        nodes: Vec::new(),
        next_id: 0,
    };
    let mut mux_inputs = Vec::new();
    let mut segments = Vec::new();
    if let Some(settings) = &request.preset.video {
        let has_video = project
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Video && !t.muted && !t.clips.is_empty());
        if has_video {
            let (id, planned) = builder.plan_video(settings)?;
            mux_inputs.push(id);
            segments = planned;
        }
    }
    let mut audio_copied = false;
    if request.preset.audio.is_some()
        && let Some((id, copied)) = builder.plan_audio()?
    {
        mux_inputs.push(id);
        audio_copied = copied;
    }
    if mux_inputs.is_empty() {
        return Err(Error::InvalidInput("project has nothing to render".to_owned()));
    }
    let inputs: Vec<&str> = mux_inputs.iter().map(String::as_str).collect();
    builder.nodes.push(JobNode::new(
        "mux",
        &inputs,
        NodeOp::Mux {
            container: request.preset.container,
            output: request.output.clone(),
        },
    ));
    let job = JobPlan::new(request.job_id.clone(), builder.nodes)?;
    Ok(RenderPlan {
        job,
        segments,
        audio_copied,
    })
}

struct Builder<'a> {
    project: &'a Project,
    request: &'a RenderRequest,
    caps: &'a BackendCapabilities,
    nodes: Vec<JobNode>,
    next_id: u32,
}

/// A contiguous run of one decision, before its nodes exist.
struct Piece<'a> {
    start_ms: u64,
    end_ms: u64,
    interval_index: usize,
    decision: SegmentDecision,
    reasons: Vec<String>,
    clip: Option<&'a Clip>,
}

impl<'a> Builder<'a> {
    fn fresh(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}_{}", self.next_id)
    }

    fn source(&self, clip: &Clip) -> Result<&'a Source> {
        self.project
            .source(&clip.source_id)
            .ok_or_else(|| Error::Internal(format!("source {} vanished", clip.source_id)))
    }

    fn intervals(&self) -> Vec<Interval<'a>> {
        let project = self.project;
        let video_tracks: Vec<_> = project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video && !t.muted)
            .collect();
        let transitions: Vec<(&Transition, &Clip, &Clip, u64, u64)> = project
            .transitions
            .iter()
            .filter_map(|t| {
                let from = project.clip(&t.from_clip_id)?;
                let to = project.clip(&t.to_clip_id)?;
                let on_video = video_tracks
                    .iter()
                    .any(|track| track.clips.iter().any(|c| c.id == from.id));
                on_video.then(|| (t, from, to, to.timeline_start_ms, from.timeline_end_ms()))
            })
            .collect();

        let mut overlays = Vec::new();
        for track in project.tracks.iter().filter(|t| !t.muted) {
            match track.kind {
                TrackKind::Title => {
                    for title in &track.titles {
                        let text = preferred_text(&title.text);
                        overlays.push(Overlay {
                            start_ms: title.start_ms,
                            end_ms: title.start_ms + title.duration_ms,
                            text,
                            font_family: title.style.font_family.clone(),
                            size_pt: title.style.size_pt,
                            color: title.style.color.clone(),
                            position: title.style.position.name().to_owned(),
                        });
                    }
                }
                TrackKind::Caption if track.burn_in => {
                    for cue in &track.captions {
                        overlays.push(Overlay {
                            start_ms: cue.start_ms,
                            end_ms: cue.end_ms,
                            text: cue.text.clone(),
                            font_family: "sans-serif".to_owned(),
                            size_pt: 28.0,
                            color: "#FFFFFF".to_owned(),
                            position: "bottom".to_owned(),
                        });
                    }
                }
                _ => {}
            }
        }

        let total = video_tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(Clip::timeline_end_ms)
            .max()
            .unwrap_or(0);
        let mut points: BTreeSet<u64> = BTreeSet::from([0, total]);
        for track in &video_tracks {
            for clip in &track.clips {
                points.insert(clip.timeline_start_ms);
                points.insert(clip.timeline_end_ms());
            }
        }
        for overlay in &overlays {
            points.insert(overlay.start_ms);
            points.insert(overlay.end_ms);
        }
        for (_, _, _, start, end) in &transitions {
            points.insert(*start);
            points.insert(*end);
        }
        // A transition stays whole, even if something else starts inside it.
        points.retain(|p| *p <= total && !transitions.iter().any(|(_, _, _, s, e)| s < p && p < e));

        let points: Vec<u64> = points.into_iter().collect();
        points
            .windows(2)
            .map(|w| {
                let (start_ms, end_ms) = (w[0], w[1]);
                let covers = |c: &&Clip| c.timeline_start_ms <= start_ms && start_ms < c.timeline_end_ms();
                // The latest track wins; within a track a transition is handled below.
                let clip = video_tracks
                    .iter()
                    .rev()
                    .find_map(|t| t.clips.iter().filter(covers).max_by_key(|c| c.timeline_start_ms));
                let transition = transitions
                    .iter()
                    .find(|(_, _, _, s, e)| *s <= start_ms && start_ms < *e)
                    .map(|(t, from, to, _, _)| (*t, *from, *to));
                let active = overlays
                    .iter()
                    .filter(|o| o.start_ms < end_ms && start_ms < o.end_ms)
                    .cloned()
                    .collect();
                Interval {
                    start_ms,
                    end_ms,
                    clip,
                    transition,
                    overlays: active,
                }
            })
            .collect()
    }

    /// Why this interval cannot be copied; empty means it can.
    fn blockers(
        &self,
        interval: &Interval<'_>,
        settings: &VideoSettings,
        size: (u32, u32),
    ) -> Result<Vec<String>> {
        let mut reasons = Vec::new();
        if interval.transition.is_some() {
            reasons.push("transition".to_owned());
        }
        if !interval.overlays.is_empty() {
            reasons.push("burned-in text".to_owned());
        }
        let Some(clip) = interval.clip else {
            reasons.push("gap".to_owned());
            return Ok(reasons);
        };
        if clip.speed != 1.0 {
            reasons.push("speed change".to_owned());
        }
        if settings.deinterlace {
            reasons.push("deinterlace".to_owned());
        }
        let source = self.source(clip)?;
        match &source.video {
            None => reasons.push("source has no video".to_owned()),
            Some(video) => {
                if video.codec != settings.codec.name() {
                    reasons.push(format!("codec {} differs", video.codec));
                }
                if (video.width, video.height) != size {
                    reasons.push("resolution differs".to_owned());
                }
                if (video.fps - self.project.settings.fps).abs() > 0.01 {
                    reasons.push("frame rate differs".to_owned());
                }
            }
        }
        if source.keyframes_ms.is_empty() {
            reasons.push("keyframes unknown".to_owned());
        }
        Ok(reasons)
    }

    fn pieces(
        &self,
        intervals: &[Interval<'a>],
        settings: &VideoSettings,
        size: (u32, u32),
    ) -> Result<Vec<Piece<'a>>> {
        let mut pieces: Vec<Piece<'a>> = Vec::new();
        for (index, interval) in intervals.iter().enumerate() {
            let reasons = self.blockers(interval, settings, size)?;
            let mut push = |start_ms, end_ms, decision, reasons: Vec<String>| {
                pieces.push(Piece {
                    start_ms,
                    end_ms,
                    interval_index: index,
                    decision,
                    reasons,
                    clip: interval.clip,
                });
            };
            if !reasons.is_empty() {
                push(
                    interval.start_ms,
                    interval.end_ms,
                    SegmentDecision::ReEncode,
                    reasons,
                );
                continue;
            }
            let clip = interval
                .clip
                .ok_or_else(|| Error::Internal("copy without a clip".to_owned()))?;
            let source = self.source(clip)?;
            let source_start = clip.source_at(interval.start_ms);
            let source_end = clip.source_at(interval.end_ms);
            let next_key = source
                .keyframes_ms
                .iter()
                .copied()
                .find(|k| *k >= source_start && *k < source_end);
            match next_key {
                Some(key) if key == source_start => {
                    push(
                        interval.start_ms,
                        interval.end_ms,
                        SegmentDecision::Copy,
                        Vec::new(),
                    );
                }
                Some(key) => {
                    // Re-encode up to the first keyframe, then copy the rest.
                    let split = interval.start_ms + (key - source_start);
                    push(
                        interval.start_ms,
                        split,
                        SegmentDecision::ReEncode,
                        vec!["cut is not on a keyframe".to_owned()],
                    );
                    push(split, interval.end_ms, SegmentDecision::Copy, Vec::new());
                }
                None => push(
                    interval.start_ms,
                    interval.end_ms,
                    SegmentDecision::ReEncode,
                    vec!["no keyframe in segment".to_owned()],
                ),
            }
        }
        Ok(pieces)
    }

    /// Plans the video branch; returns the id of its encoded stream and the segment report.
    fn plan_video(&mut self, settings: &VideoSettings) -> Result<(String, Vec<RenderSegment>)> {
        let project_settings = self.project.settings;
        let size = fit_dimensions(
            project_settings.width,
            project_settings.height,
            settings.max_width,
            settings.max_height,
        );
        let intervals = self.intervals();
        let pieces = self.pieces(&intervals, settings, size)?;
        let target = crate::job::pick_video_target(self.caps, settings.codec, size.0, size.1)?;

        let mut encoded_parts: Vec<String> = Vec::new();
        let mut raw_run: Vec<String> = Vec::new();
        for piece in &pieces {
            match piece.decision {
                SegmentDecision::Copy => {
                    self.flush_raw(&mut raw_run, &mut encoded_parts, settings, size, target);
                    let clip = piece
                        .clip
                        .ok_or_else(|| Error::Internal("copy without a clip".to_owned()))?;
                    let id = self.fresh("vcopy");
                    let source = self.source(clip)?.path.clone();
                    self.nodes.push(JobNode::new(
                        id.as_str(),
                        &[],
                        NodeOp::CopyVideo {
                            source,
                            start_ms: Some(clip.source_at(piece.start_ms)),
                            end_ms: Some(clip.source_at(piece.end_ms)),
                        },
                    ));
                    encoded_parts.push(id);
                }
                SegmentDecision::ReEncode => {
                    let interval = &intervals[piece.interval_index];
                    let id = self.raw_segment(piece, interval, size)?;
                    raw_run.push(id);
                }
            }
        }
        self.flush_raw(&mut raw_run, &mut encoded_parts, settings, size, target);
        let stream = self.join(encoded_parts, "vjoin")?;
        let segments = pieces
            .into_iter()
            .map(|p| RenderSegment {
                start_ms: p.start_ms,
                end_ms: p.end_ms,
                clip_id: p.clip.map(|c| c.id.clone()),
                decision: p.decision,
                reasons: p.reasons,
            })
            .collect();
        Ok((stream, segments))
    }

    fn join(&mut self, parts: Vec<String>, prefix: &str) -> Result<String> {
        let mut parts = parts;
        match parts.len() {
            0 => Err(Error::Internal("nothing to join".to_owned())),
            1 => parts.pop().ok_or_else(|| Error::Internal("no part".to_owned())),
            _ => {
                let id = self.fresh(prefix);
                let inputs: Vec<&str> = parts.iter().map(String::as_str).collect();
                self.nodes
                    .push(JobNode::new(id.as_str(), &inputs, NodeOp::Concat));
                Ok(id)
            }
        }
    }

    /// Encodes a run of adjacent raw segments as one stream.
    fn flush_raw(
        &mut self,
        run: &mut Vec<String>,
        encoded: &mut Vec<String>,
        settings: &VideoSettings,
        size: (u32, u32),
        target: crate::job::ExecutionTarget,
    ) {
        if run.is_empty() {
            return;
        }
        let parts = std::mem::take(run);
        let Ok(raw) = self.join(parts, "vraw") else {
            return;
        };
        let id = self.fresh("vencode");
        let fps = Some(settings.max_fps.map_or(self.project.settings.fps, |max| {
            max.min(self.project.settings.fps)
        }));
        self.nodes.push(JobNode::new(
            id.as_str(),
            &[raw.as_str()],
            NodeOp::EncodeVideo {
                codec: settings.codec,
                bitrate_kbps: settings.bitrate_kbps,
                width: size.0,
                height: size.1,
                fps,
                keyframe_interval_ms: settings.keyframe_interval_ms,
                target,
            },
        ));
        encoded.push(id);
    }

    /// Decodes one clip span at the output size.
    fn decode_clip(&mut self, clip: &Clip, start_ms: u64, end_ms: u64, size: (u32, u32)) -> Result<String> {
        let source = self.source(clip)?;
        let path = source.path.clone();
        let native = source.video.as_ref().map(|v| (v.width, v.height));
        let id = self.fresh("vdecode");
        self.nodes.push(JobNode::new(
            id.as_str(),
            &[],
            NodeOp::DecodeVideo {
                source: path,
                start_ms: Some(clip.source_at(start_ms)),
                end_ms: Some(clip.source_at(end_ms)),
            },
        ));
        let mut last = id;
        if clip.speed != 1.0 {
            let id = self.fresh("vspeed");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[last.as_str()],
                NodeOp::ChangeSpeed { factor: clip.speed },
            ));
            last = id;
        }
        if native != Some(size) {
            let id = self.fresh("vscale");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[last.as_str()],
                NodeOp::Scale {
                    width: size.0,
                    height: size.1,
                },
            ));
            last = id;
        }
        Ok(last)
    }

    fn raw_segment(
        &mut self,
        piece: &Piece<'a>,
        interval: &Interval<'a>,
        size: (u32, u32),
    ) -> Result<String> {
        let (start, end) = (piece.start_ms, piece.end_ms);
        let mut last = if let Some((transition, from, to)) = interval.transition {
            let a = self.decode_clip(from, start, end, size)?;
            let b = self.decode_clip(to, start, end, size)?;
            let id = self.fresh("vtransition");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[a.as_str(), b.as_str()],
                NodeOp::Transition {
                    kind: transition.kind,
                    duration_ms: transition.duration_ms,
                },
            ));
            id
        } else if let Some(clip) = piece.clip {
            self.decode_clip(clip, start, end, size)?
        } else {
            let id = self.fresh("vblack");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[],
                NodeOp::GenerateVideo {
                    duration_ms: end - start,
                    width: size.0,
                    height: size.1,
                    fps: self.project.settings.fps,
                },
            ));
            id
        };
        for overlay in &interval.overlays {
            let (from, to) = (overlay.start_ms.max(start), overlay.end_ms.min(end));
            if from >= to {
                continue;
            }
            let id = self.fresh("vtext");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[last.as_str()],
                NodeOp::BurnText {
                    text: overlay.text.clone(),
                    start_ms: from - start,
                    end_ms: to - start,
                    font_family: overlay.font_family.clone(),
                    size_pt: overlay.size_pt,
                    color: overlay.color.clone(),
                    position: overlay.position.clone(),
                },
            ));
            last = id;
        }
        Ok(last)
    }

    /// The clips that contribute sound, in timeline order.
    fn audio_items(&self) -> Result<Vec<(&'a Clip, &'a Source)>> {
        let mut items = Vec::new();
        for track in self.project.tracks.iter().filter(|t| !t.muted) {
            if !matches!(track.kind, TrackKind::Video | TrackKind::Audio) {
                continue;
            }
            for clip in &track.clips {
                let source = self.source(clip)?;
                if source.audio.is_some() && clip.volume > 0.0 {
                    items.push((clip, source));
                }
            }
        }
        items.sort_by_key(|(c, _)| c.timeline_start_ms);
        Ok(items)
    }

    /// Plans the audio branch; `None` if the project is silent.
    fn plan_audio(&mut self) -> Result<Option<(String, bool)>> {
        let Some(settings) = self.request.preset.audio.clone() else {
            return Ok(None);
        };
        let items = self.audio_items()?;
        if items.is_empty() {
            return Ok(None);
        }
        let copyable = settings.loudness_target_lufs.is_none()
            && self.project.transitions.is_empty()
            && items.first().is_some_and(|(c, _)| c.timeline_start_ms == 0)
            && items
                .windows(2)
                .all(|w| w[0].0.timeline_end_ms() == w[1].0.timeline_start_ms)
            && items.iter().all(|(clip, source)| {
                clip.speed == 1.0
                    && clip.volume == 1.0
                    && source.audio.as_ref().is_some_and(|a| {
                        a.codec == settings.codec.name()
                            && a.sample_rate == settings.sample_rate
                            && a.channels == settings.channels
                    })
            });
        if copyable {
            let mut parts = Vec::with_capacity(items.len());
            for (clip, source) in items {
                let id = self.fresh("acopy");
                self.nodes.push(JobNode::new(
                    id.as_str(),
                    &[],
                    NodeOp::CopyAudio {
                        source: source.path.clone(),
                        start_ms: Some(clip.in_ms),
                        end_ms: Some(clip.out_ms),
                    },
                ));
                parts.push(id);
            }
            return Ok(Some((self.join(parts, "ajoin")?, true)));
        }

        let target = crate::job::pick_audio_target(self.caps, settings.codec)?;
        let mut lanes = Vec::with_capacity(items.len());
        for (clip, source) in items {
            lanes.push(self.audio_lane(clip, source, settings.sample_rate, settings.channels));
        }
        let mut last = if lanes.len() == 1 {
            lanes.pop().ok_or_else(|| Error::Internal("no lane".to_owned()))?
        } else {
            let id = self.fresh("amix");
            let inputs: Vec<&str> = lanes.iter().map(String::as_str).collect();
            self.nodes
                .push(JobNode::new(id.as_str(), &inputs, NodeOp::MixAudio));
            id
        };
        if let Some(target_lufs) = settings.loudness_target_lufs {
            let id = self.fresh("aloudness");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[last.as_str()],
                NodeOp::LoudnessNormalize {
                    target_lufs,
                    true_peak_dbtp: settings.true_peak_ceiling_dbtp,
                    output_channels: settings.channels,
                },
            ));
            last = id;
        }
        let id = self.fresh("aencode");
        self.nodes.push(JobNode::new(
            id.as_str(),
            &[last.as_str()],
            NodeOp::EncodeAudio {
                codec: settings.codec,
                bitrate_kbps: settings.bitrate_kbps,
                sample_rate: settings.sample_rate,
                channels: settings.channels,
                target,
            },
        ));
        Ok(Some((id, false)))
    }

    /// One clip as raw audio, offset to its timeline position with leading silence.
    fn audio_lane(&mut self, clip: &Clip, source: &Source, sample_rate: u32, channels: u32) -> String {
        let id = self.fresh("adecode");
        self.nodes.push(JobNode::new(
            id.as_str(),
            &[],
            NodeOp::DecodeAudio {
                source: source.path.clone(),
                start_ms: Some(clip.in_ms),
                end_ms: Some(clip.out_ms),
            },
        ));
        let mut last = id;
        if clip.speed != 1.0 {
            let id = self.fresh("aspeed");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[last.as_str()],
                NodeOp::ChangeSpeed { factor: clip.speed },
            ));
            last = id;
        }
        if clip.volume != 1.0 {
            let id = self.fresh("again");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[last.as_str()],
                NodeOp::Gain {
                    db: 20.0 * clip.volume.log10(),
                },
            ));
            last = id;
        }
        if clip.timeline_start_ms > 0 {
            let silence = self.fresh("asilence");
            self.nodes.push(JobNode::new(
                silence.as_str(),
                &[],
                NodeOp::GenerateAudio {
                    duration_ms: clip.timeline_start_ms,
                    sample_rate,
                    channels,
                },
            ));
            let id = self.fresh("alane");
            self.nodes.push(JobNode::new(
                id.as_str(),
                &[silence.as_str(), last.as_str()],
                NodeOp::Concat,
            ));
            last = id;
        }
        last
    }
}

/// Arabic text if present, else English, else whichever comes first.
fn preferred_text(text: &std::collections::BTreeMap<String, String>) -> String {
    text.get("ar")
        .or_else(|| text.get("en"))
        .or_else(|| text.values().next())
        .cloned()
        .unwrap_or_default()
}
