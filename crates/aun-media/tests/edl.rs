#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use aun_common::Error;
use aun_media::edl::{
    CURRENT_SCHEMA_VERSION, Clip, Migration, Project, ProjectSettings, RenderRequest, SegmentDecision,
    Source, TitleClip, TitlePosition, TitleStyle, Track, TrackKind, Transition, VideoParams, load_project,
    migrate_value, plan_render,
};
use aun_media::job::{
    AudioCodec, BackendCapabilities, EncodeBackend, EncodePreset, NodeOp, PresetKind, SoftwareBackend,
    TransitionKind, VideoCodec,
};
use serde_json::{Value, json};

const V0: &str = include_str!("fixtures/edl/v0_khutbah.json");
const V1: &str = include_str!("fixtures/edl/v1_demo.json");
const SCHEMA: &str = include_str!("../../../schemas/edl/v1.json");

fn schema_errors(document: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    validator.iter_errors(document).map(|e| e.to_string()).collect()
}

#[test]
fn schema_accepts_the_fixture_and_serialised_projects() {
    let fixture: Value = serde_json::from_str(V1).unwrap();
    assert_eq!(schema_errors(&fixture), Vec::<String>::new());
    let project = load_project(V1).unwrap();
    let round: Value = serde_json::from_str(&project.to_json().unwrap()).unwrap();
    assert_eq!(schema_errors(&round), Vec::<String>::new());
    assert_eq!(load_project(&project.to_json().unwrap()).unwrap(), project);
}

#[test]
fn schema_rejects_unknown_fields_and_wrong_versions() {
    let mut doc: Value = serde_json::from_str(V1).unwrap();
    doc["surprise"] = json!(true);
    assert!(!schema_errors(&doc).is_empty());
    let mut doc: Value = serde_json::from_str(V1).unwrap();
    doc["schemaVersion"] = json!(2);
    assert!(!schema_errors(&doc).is_empty());
}

#[test]
fn migrates_v0_fixture() {
    let project = load_project(V0).unwrap();
    assert_eq!(project.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(project.metadata.title, "Friday khutbah");
    assert_eq!(project.settings.fps, 25.0);
    assert_eq!(project.sources.len(), 2);

    let video = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Video)
        .unwrap();
    assert_eq!(video.clips.len(), 1);
    assert_eq!(video.clips[0].in_ms, 1500);
    assert_eq!(video.clips[0].out_ms, 11_500);

    let audio = project
        .tracks
        .iter()
        .find(|t| t.kind == TrackKind::Audio)
        .unwrap();
    assert_eq!(audio.clips[0].timeline_start_ms, 2000);
    assert_eq!(audio.clips[0].out_ms, 5250);

    let chapters = project.chapters();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[1].start_ms, 6500);
    assert_eq!(chapters[1].title["en"], "Main topic");

    // A migrated document must also satisfy the published schema.
    let round: Value = serde_json::from_str(&project.to_json().unwrap()).unwrap();
    assert_eq!(schema_errors(&round), Vec::<String>::new());
}

#[test]
fn migration_framework_chains_steps() {
    fn one_to_two(mut v: Value) -> aun_common::Result<Value> {
        v["schemaVersion"] = json!(2);
        v["addedInTwo"] = json!(true);
        Ok(v)
    }
    fn two_to_three(mut v: Value) -> aun_common::Result<Value> {
        v["schemaVersion"] = json!(3);
        Ok(v)
    }
    let steps = [
        Migration {
            from: 2,
            apply: two_to_three,
        },
        Migration {
            from: 1,
            apply: one_to_two,
        },
    ];
    let out = migrate_value(json!({"schemaVersion": 1}), &steps, 3).unwrap();
    assert_eq!(out["schemaVersion"], 3);
    assert_eq!(out["addedInTwo"], true);

    // A gap in the chain.
    assert!(matches!(
        migrate_value(json!({"schemaVersion": 1}), &steps[..1], 3),
        Err(Error::Internal(_))
    ));

    // A step that forgets to bump the version.
    fn stuck(v: Value) -> aun_common::Result<Value> {
        Ok(v)
    }
    assert!(matches!(
        migrate_value(
            json!({"schemaVersion": 1}),
            &[Migration {
                from: 1,
                apply: stuck
            }],
            2
        ),
        Err(Error::Internal(_))
    ));
}

#[test]
fn newer_documents_are_unsupported() {
    assert!(matches!(
        load_project(r#"{"schemaVersion": 99}"#),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(load_project("[]"), Err(Error::InvalidInput(_))));
    assert!(matches!(load_project("{oops"), Err(Error::InvalidInput(_))));
    assert!(matches!(
        load_project(r#"{"schemaVersion": "1"}"#),
        Err(Error::InvalidInput(_))
    ));
}

fn settings() -> ProjectSettings {
    ProjectSettings {
        width: 1280,
        height: 720,
        fps: 30.0,
        sample_rate: 48_000,
        channels: 2,
    }
}

fn source(id: &str, codec: &str, keyframes: Vec<u64>) -> Source {
    Source {
        id: id.into(),
        path: format!("/media/{id}.mp4"),
        duration_ms: 60_000,
        video: Some(VideoParams {
            codec: codec.into(),
            width: 1280,
            height: 720,
            fps: 30.0,
        }),
        audio: Some(aun_media::edl::AudioParams {
            codec: "aac".into(),
            sample_rate: 48_000,
            channels: 2,
        }),
        keyframes_ms: keyframes,
    }
}

fn clip(id: &str, in_ms: u64, out_ms: u64, start: u64) -> Clip {
    Clip {
        id: id.into(),
        source_id: "cam".into(),
        in_ms,
        out_ms,
        timeline_start_ms: start,
        speed: 1.0,
        volume: 1.0,
    }
}

fn track(id: &str, kind: TrackKind, clips: Vec<Clip>) -> Track {
    Track {
        id: id.into(),
        kind,
        name: String::new(),
        muted: false,
        locked: false,
        burn_in: false,
        clips,
        titles: vec![],
        captions: vec![],
    }
}

fn keyframes() -> Vec<u64> {
    (0..=30).map(|i| i * 2000).collect()
}

fn project(clips: Vec<Clip>) -> Project {
    let mut p = Project::new("p", "t", settings());
    p.sources.push(source("cam", "h264", keyframes()));
    p.tracks.push(track("v", TrackKind::Video, clips));
    p
}

fn codes(p: &Project) -> Vec<String> {
    p.validate().into_iter().map(|i| i.code).collect()
}

#[test]
fn validation_reports_overlaps_and_ranges() {
    let p = project(vec![clip("a", 0, 5000, 0), clip("b", 0, 5000, 4000)]);
    assert_eq!(codes(&p), vec!["overlap"]);

    let p = project(vec![clip("a", 0, 90_000, 0)]);
    assert_eq!(codes(&p), vec!["out_of_range"]);

    let p = project(vec![clip("a", 5000, 5000, 0)]);
    assert_eq!(codes(&p), vec!["bad_range"]);

    let mut bad = clip("a", 0, 1000, 0);
    bad.source_id = "ghost".into();
    assert_eq!(codes(&project(vec![bad])), vec!["unknown_source"]);

    let p = project(vec![clip("a", 0, 1000, 0), clip("a", 0, 1000, 1000)]);
    assert_eq!(codes(&p), vec!["duplicate_id"]);

    let mut fast = clip("a", 0, 1000, 0);
    fast.speed = 9.0;
    assert_eq!(codes(&project(vec![fast])), vec!["bad_speed"]);

    let mut p = project(vec![]);
    p.settings.width = 641;
    assert_eq!(codes(&p), vec!["bad_settings"]);
}

#[test]
fn overlap_is_legal_only_with_a_matching_transition() {
    let mut p = project(vec![clip("a", 0, 5000, 0), clip("b", 0, 5000, 4000)]);
    p.transitions.push(Transition {
        id: "tr".into(),
        kind: TransitionKind::Crossfade,
        from_clip_id: "a".into(),
        to_clip_id: "b".into(),
        duration_ms: 1000,
    });
    assert_eq!(codes(&p), Vec::<String>::new());
    p.transitions[0].duration_ms = 500;
    let found = codes(&p);
    assert!(found.contains(&"overlap".to_owned()), "{found:?}");
    assert!(found.contains(&"bad_transition".to_owned()), "{found:?}");
}

#[test]
fn split_maps_source_time_through_speed() {
    let mut fast = clip("a", 1000, 9000, 2000);
    fast.speed = 2.0;
    let mut p = project(vec![fast]);
    p.split_clip("a", 4000, "a2").unwrap();
    let clips = &p.tracks[0].clips;
    assert_eq!(clips[0].out_ms, 5000);
    assert_eq!(clips[1].id, "a2");
    assert_eq!(clips[1].in_ms, 5000);
    assert_eq!(clips[1].timeline_start_ms, 4000);
    assert_eq!(clips[0].timeline_end_ms(), clips[1].timeline_start_ms);
}

#[test]
fn split_rejects_the_boundary_and_leaves_the_project_alone() {
    let mut p = project(vec![clip("a", 0, 5000, 0)]);
    let before = p.clone();
    assert!(p.split_clip("a", 0, "b").is_err());
    assert!(p.split_clip("a", 5000, "b").is_err());
    assert!(matches!(p.split_clip("ghost", 10, "b"), Err(Error::NotFound(_))));
    // An id collision is caught by validation and rolled back.
    assert!(p.split_clip("a", 1000, "a").is_err());
    assert_eq!(p, before);
}

#[test]
fn split_moves_outgoing_transition_to_the_right_half() {
    let mut p = project(vec![clip("a", 0, 5000, 0), clip("b", 0, 5000, 4000)]);
    p.transitions.push(Transition {
        id: "tr".into(),
        kind: TransitionKind::Crossfade,
        from_clip_id: "a".into(),
        to_clip_id: "b".into(),
        duration_ms: 1000,
    });
    p.split_clip("a", 1000, "a2").unwrap();
    assert_eq!(p.transitions[0].from_clip_id, "a2");
}

#[test]
fn trim_keeps_retained_material_in_place() {
    let mut p = project(vec![clip("a", 1000, 9000, 5000)]);
    p.trim_clip("a", 2000, 8000).unwrap();
    let c = &p.tracks[0].clips[0];
    assert_eq!((c.in_ms, c.out_ms, c.timeline_start_ms), (2000, 8000, 6000));
    assert_eq!(c.timeline_end_ms(), 12_000);
}

#[test]
fn trim_rejects_invalid_ranges() {
    let mut p = project(vec![clip("a", 1000, 9000, 0)]);
    let before = p.clone();
    assert!(p.trim_clip("a", 500, 9000).is_err(), "moves before zero");
    assert!(p.trim_clip("a", 9000, 9000).is_err());
    assert!(p.trim_clip("a", 1000, 99_000).is_err());
    assert_eq!(p, before);
}

#[test]
fn ripple_delete_clip_closes_the_gap() {
    let mut p = project(vec![
        clip("a", 0, 3000, 0),
        clip("b", 0, 2000, 3000),
        clip("c", 0, 1000, 5000),
    ]);
    p.ripple_delete_clip("b").unwrap();
    let c = &p.tracks[0].clips;
    assert_eq!(c.len(), 2);
    assert_eq!(c[1].id, "c");
    assert_eq!(c[1].timeline_start_ms, 3000);
}

#[test]
fn locked_tracks_refuse_clip_edits_and_are_skipped_by_range_ripple() {
    let mut p = project(vec![clip("a", 0, 3000, 0)]);
    p.tracks[0].locked = true;
    assert!(matches!(p.ripple_delete_clip("a"), Err(Error::Conflict(_))));
    let mut unlocked = track("v2", TrackKind::Video, vec![]);
    let mut other = clip("x", 0, 3000, 2000);
    other.source_id = "cam".into();
    unlocked.clips.push(other);
    p.tracks.push(unlocked);
    p.ripple_delete_range(0, 1000).unwrap();
    assert_eq!(p.tracks[0].clips[0].timeline_start_ms, 0);
    assert_eq!(p.tracks[1].clips[0].timeline_start_ms, 1000);
}

fn title(id: &str, start: u64, duration: u64) -> TitleClip {
    TitleClip {
        id: id.into(),
        start_ms: start,
        duration_ms: duration,
        text: BTreeMap::from([("ar".to_owned(), "عنوان".to_owned())]),
        style: TitleStyle {
            font_family: "Amiri".into(),
            size_pt: 36.0,
            color: "#FFFFFF".into(),
            background: None,
            position: TitlePosition::LowerThird,
        },
    }
}

#[test]
fn range_ripple_trims_straddlers_splits_spanners_and_shifts_the_rest() {
    let mut p = project(vec![
        clip("left", 0, 4000, 0),
        clip("span", 0, 10_000, 4000),
        clip("tail", 0, 1000, 14_000),
    ]);
    let mut titles = track("t", TrackKind::Title, vec![]);
    titles.titles.push(title("before", 0, 1000));
    titles.titles.push(title("inside", 6000, 1000));
    titles.titles.push(title("straddle", 7500, 2000));
    p.tracks.push(titles);
    p.markers.push(aun_media::edl::Marker {
        id: "keep".into(),
        time_ms: 10_000,
        title: BTreeMap::new(),
        chapter: true,
    });
    p.markers.push(aun_media::edl::Marker {
        id: "gone".into(),
        time_ms: 6000,
        title: BTreeMap::new(),
        chapter: false,
    });

    p.ripple_delete_range(5000, 8000).unwrap();

    let clips = &p.tracks[0].clips;
    let summary: Vec<_> = clips
        .iter()
        .map(|c| (c.id.as_str(), c.in_ms, c.out_ms, c.timeline_start_ms))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("left", 0, 4000, 0),
            ("span", 0, 1000, 4000),
            ("span_r", 4000, 10_000, 5000),
            ("tail", 0, 1000, 11_000),
        ]
    );
    let titles = &p.tracks[1].titles;
    let summary: Vec<_> = titles
        .iter()
        .map(|t| (t.id.as_str(), t.start_ms, t.duration_ms))
        .collect();
    assert_eq!(summary, vec![("before", 0, 1000), ("straddle", 5000, 1500)]);
    assert_eq!(p.markers.len(), 1);
    assert_eq!(p.markers[0].time_ms, 7000);
    assert_eq!(p.duration_ms(), 12_000);
}

#[test]
fn ripple_delete_drops_transitions_of_removed_clips() {
    let mut p = project(vec![
        clip("a", 0, 5000, 0),
        clip("b", 0, 5000, 4000),
        clip("c", 0, 1000, 9000),
    ]);
    p.transitions.push(Transition {
        id: "tr".into(),
        kind: TransitionKind::Crossfade,
        from_clip_id: "a".into(),
        to_clip_id: "b".into(),
        duration_ms: 1000,
    });
    p.ripple_delete_clip("a").unwrap();
    assert!(p.transitions.is_empty());
    let starts: Vec<_> = p.tracks[0].clips.iter().map(|c| c.timeline_start_ms).collect();
    assert_eq!(
        starts,
        vec![0, 5000],
        "the successor moves up to the freed position"
    );
    p.ensure_valid().unwrap();
}

// ---- Render planning ----

fn caps() -> BackendCapabilities {
    SoftwareBackend::new().capabilities()
}

fn preset(video: VideoCodec, loudness: Option<f64>) -> EncodePreset {
    let mut preset = EncodePreset::builtin(PresetKind::Lecture)
        .with_video_codec(video)
        .with_loudness_target(loudness);
    if let Some(v) = &mut preset.video {
        v.max_width = 1280;
        v.max_height = 720;
    }
    preset
}

fn request(preset: EncodePreset) -> RenderRequest {
    RenderRequest {
        job_id: "render".into(),
        output: "/out/final.mp4".into(),
        preset,
    }
}

fn count(plan: &aun_media::edl::RenderPlan, pred: fn(&NodeOp) -> bool) -> usize {
    plan.job.nodes.iter().filter(|n| pred(&n.op)).count()
}

fn decisions(plan: &aun_media::edl::RenderPlan) -> Vec<(u64, u64, SegmentDecision)> {
    plan.segments
        .iter()
        .map(|s| (s.start_ms, s.end_ms, s.decision))
        .collect()
}

#[test]
fn keyframe_aligned_same_codec_is_all_copy() {
    let p = project(vec![clip("a", 2000, 6000, 0), clip("b", 8000, 12_000, 4000)]);
    let plan = plan_render(&p, &request(preset(VideoCodec::H264, None)), &caps()).unwrap();
    assert_eq!(
        decisions(&plan),
        vec![
            (0, 4000, SegmentDecision::Copy),
            (4000, 8000, SegmentDecision::Copy)
        ]
    );
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::EncodeVideo { .. })), 0);
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::CopyVideo { .. })), 2);
    assert!(plan.audio_copied);
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::EncodeAudio { .. })), 0);
    plan.job.stages().unwrap();
}

#[test]
fn mid_gop_cut_reencodes_only_the_head() {
    let p = project(vec![clip("a", 1000, 6000, 0)]);
    let plan = plan_render(&p, &request(preset(VideoCodec::H264, None)), &caps()).unwrap();
    assert_eq!(
        decisions(&plan),
        vec![
            (0, 1000, SegmentDecision::ReEncode),
            (1000, 5000, SegmentDecision::Copy)
        ]
    );
    assert_eq!(plan.segments[0].reasons, vec!["cut is not on a keyframe"]);
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::EncodeVideo { .. })), 1);
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::Concat)), 1);
}

#[test]
fn codec_mismatch_forces_reencode() {
    let p = project(vec![clip("a", 0, 4000, 0)]);
    let plan = plan_render(&p, &request(preset(VideoCodec::Hevc, None)), &caps()).unwrap();
    assert_eq!(decisions(&plan), vec![(0, 4000, SegmentDecision::ReEncode)]);
    assert_eq!(plan.segments[0].reasons, vec!["codec h264 differs"]);
}

#[test]
fn speed_transition_and_title_force_reencode_locally() {
    let mut fast = clip("fast", 0, 4000, 0);
    fast.speed = 2.0;
    let mut p = project(vec![fast, clip("plain", 0, 8000, 2000)]);
    let mut titles = track("t", TrackKind::Title, vec![]);
    titles.titles.push(title("title", 4000, 2000));
    p.tracks.push(titles);
    let plan = plan_render(&p, &request(preset(VideoCodec::H264, None)), &caps()).unwrap();
    assert_eq!(
        decisions(&plan),
        vec![
            (0, 2000, SegmentDecision::ReEncode),
            (2000, 4000, SegmentDecision::Copy),
            (4000, 6000, SegmentDecision::ReEncode),
            (6000, 10_000, SegmentDecision::Copy),
        ]
    );
    assert!(plan.segments[0].reasons.contains(&"speed change".to_owned()));
    assert!(plan.segments[2].reasons.contains(&"burned-in text".to_owned()));
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::BurnText { .. })), 1);
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::ChangeSpeed { .. })), 2);
    assert!(!plan.audio_copied, "speed changes rule out audio copy");
}

#[test]
fn transition_decodes_both_clips_over_the_overlap() {
    let mut p = project(vec![clip("a", 0, 6000, 0), clip("b", 2000, 8000, 5000)]);
    p.transitions.push(Transition {
        id: "tr".into(),
        kind: TransitionKind::Crossfade,
        from_clip_id: "a".into(),
        to_clip_id: "b".into(),
        duration_ms: 1000,
    });
    let plan = plan_render(&p, &request(preset(VideoCodec::H264, None)), &caps()).unwrap();
    assert_eq!(
        decisions(&plan),
        vec![
            (0, 5000, SegmentDecision::Copy),
            (5000, 6000, SegmentDecision::ReEncode),
            // After the blend, source position 3000 is mid-GOP until the keyframe at 4000.
            (6000, 7000, SegmentDecision::ReEncode),
            (7000, 11_000, SegmentDecision::Copy),
        ]
    );
    let decode_ranges: Vec<_> = plan
        .job
        .nodes
        .iter()
        .filter_map(|n| match &n.op {
            NodeOp::DecodeVideo { start_ms, end_ms, .. } => Some((*start_ms, *end_ms)),
            _ => None,
        })
        .collect();
    assert!(
        decode_ranges.contains(&(Some(5000), Some(6000))),
        "{decode_ranges:?}"
    );
    assert!(
        decode_ranges.contains(&(Some(2000), Some(3000))),
        "{decode_ranges:?}"
    );
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::Transition { .. })), 1);
}

#[test]
fn gaps_render_black_and_audio_is_offset() {
    let p = project(vec![clip("a", 0, 2000, 3000)]);
    let plan = plan_render(&p, &request(preset(VideoCodec::H264, Some(-16.0))), &caps()).unwrap();
    assert_eq!(plan.segments[0].clip_id, None);
    assert_eq!(plan.segments[0].reasons, vec!["gap"]);
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::GenerateVideo { .. })), 1);
    assert!(plan.job.nodes.iter().any(|n| matches!(
        n.op,
        NodeOp::GenerateAudio {
            duration_ms: 3000,
            ..
        }
    )));
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::LoudnessNormalize { .. })), 1);
    assert!(!plan.audio_copied);
}

#[test]
fn volume_and_overlapping_audio_mix() {
    let mut music = clip("music", 0, 4000, 0);
    music.volume = 0.5;
    let mut p = project(vec![clip("a", 0, 4000, 0)]);
    p.tracks.push(track("mus", TrackKind::Audio, vec![music]));
    let plan = plan_render(&p, &request(preset(VideoCodec::H264, None)), &caps()).unwrap();
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::MixAudio)), 1);
    assert!(plan.job.nodes.iter().any(|n| matches!(
        n.op,
        NodeOp::Gain { db } if (db + 6.02).abs() < 0.01
    )));
}

#[test]
fn audio_only_preset_ignores_picture() {
    let p = project(vec![clip("a", 0, 4000, 0)]);
    let audio = EncodePreset::builtin(PresetKind::AudioOnly)
        .with_audio_codec(AudioCodec::Aac)
        .with_loudness_target(None);
    let plan = plan_render(&p, &request(audio), &caps()).unwrap();
    assert!(plan.segments.is_empty());
    assert_eq!(count(&plan, |o| matches!(o, NodeOp::CopyVideo { .. })), 0);
}

#[test]
fn nothing_to_render_is_invalid() {
    let p = project(vec![]);
    assert!(matches!(
        plan_render(&p, &request(preset(VideoCodec::H264, None)), &caps()),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn render_plans_survive_json() {
    let p = project(vec![clip("a", 1000, 6000, 0)]);
    let plan = plan_render(&p, &request(preset(VideoCodec::H264, Some(-16.0))), &caps()).unwrap();
    let json = plan.job.to_json().unwrap();
    assert_eq!(aun_media::job::JobPlan::from_json(&json).unwrap(), plan.job);
}
