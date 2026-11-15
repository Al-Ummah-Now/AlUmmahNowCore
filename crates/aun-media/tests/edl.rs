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

// ---- join / move / append / apply / flatten (E19 19.5) ------------------------------------------

#[test]
fn join_undoes_a_split_and_keeps_transitions_on_the_joined_clip() {
    let mut p = project(vec![clip("a", 0, 8000, 0), clip("b", 0, 5000, 7000)]);
    p.transitions.push(Transition {
        id: "tr".into(),
        kind: TransitionKind::Crossfade,
        from_clip_id: "a".into(),
        to_clip_id: "b".into(),
        duration_ms: 1000,
    });
    let before = p.clone();
    p.split_clip("a", 3000, "a2").unwrap();
    assert_eq!(p.transitions[0].from_clip_id, "a2");
    p.join_clips("a", "a2").unwrap();
    assert_eq!(p, before);
}

#[test]
fn join_refuses_clips_that_are_not_halves_of_one_cut() {
    // A gap between them, another source range, and the wrong order
    let mut p = project(vec![clip("a", 0, 3000, 0), clip("b", 3000, 6000, 3500)]);
    let before = p.clone();
    assert!(matches!(p.join_clips("a", "b"), Err(Error::InvalidInput(_))));
    let mut p2 = project(vec![clip("a", 0, 3000, 0), clip("b", 4000, 6000, 3000)]);
    assert!(p2.join_clips("a", "b").is_err());
    let mut p3 = project(vec![clip("a", 0, 3000, 0), clip("b", 3000, 6000, 3000)]);
    assert!(p3.join_clips("b", "a").is_err());
    assert!(matches!(p.join_clips("a", "ghost"), Err(Error::NotFound(_))));
    assert_eq!(p, before);
}

#[test]
fn move_places_a_clip_and_refuses_landing_on_another() {
    let mut p = project(vec![clip("a", 0, 3000, 0), clip("b", 0, 3000, 5000)]);
    p.tracks.push(track("v2", TrackKind::Video, vec![]));
    p.move_clip("a", "v", 10_000).unwrap();
    assert_eq!(
        p.tracks[0]
            .clips
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(p.tracks[0].clips[1].timeline_start_ms, 10_000);
    p.move_clip("a", "v2", 0).unwrap();
    assert!(p.tracks[0].clips.iter().all(|c| c.id != "a"));
    assert_eq!(p.tracks[1].clips[0].id, "a");
    let before = p.clone();
    assert!(p.move_clip("a", "v", 6000).is_err(), "would overlap b");
    assert!(matches!(p.move_clip("a", "nope", 0), Err(Error::NotFound(_))));
    assert_eq!(p, before);
}

#[test]
fn move_onto_a_track_of_another_kind_or_a_locked_track_is_refused() {
    let mut p = project(vec![clip("a", 0, 3000, 0)]);
    p.tracks.push(track("t", TrackKind::Title, vec![]));
    let mut locked = track("v2", TrackKind::Video, vec![]);
    locked.locked = true;
    p.tracks.push(locked);
    let before = p.clone();
    assert!(p.move_clip("a", "t", 0).is_err());
    assert!(matches!(p.move_clip("a", "v2", 0), Err(Error::Conflict(_))));
    assert_eq!(p, before);
}

#[test]
fn append_goes_to_the_end_of_the_track_unless_placed() {
    let mut p = project(vec![clip("a", 0, 3000, 0)]);
    let new = |id: &str, start: Option<u64>| aun_media::edl::NewClip {
        id: id.into(),
        source_id: "cam".into(),
        in_ms: 10_000,
        out_ms: 12_000,
        timeline_start_ms: start,
        speed: None,
        volume: None,
    };
    p.append_clip("v", new("b", None)).unwrap();
    assert_eq!(p.tracks[0].clips[1].timeline_start_ms, 3000);
    p.append_clip("v", new("c", Some(20_000))).unwrap();
    assert_eq!(p.tracks[0].clips[2].timeline_start_ms, 20_000);
    let before = p.clone();
    assert!(p.append_clip("v", new("a", None)).is_err(), "duplicate id");
    let mut ghost = new("d", None);
    ghost.source_id = "ghost".into();
    assert!(p.append_clip("v", ghost).is_err());
    assert_eq!(p, before);
}

#[test]
fn edits_arrive_as_json_ops() {
    let mut p = project(vec![clip("a", 0, 9000, 0)]);
    let ops = [
        json!({"op": "split", "clipId": "a", "atMs": 3000, "newId": "a2"}),
        json!({"op": "trim", "clipId": "a2", "inMs": 4000, "outMs": 9000}),
        json!({"op": "rippleDeleteClip", "clipId": "a"}),
        json!({"op": "append", "trackId": "v", "clip": {"id": "z", "sourceId": "cam", "inMs": 0, "outMs": 1000}}),
        json!({"op": "move", "clipId": "z", "trackId": "v", "startMs": 30000}),
        json!({"op": "rippleDeleteRange", "startMs": 0, "endMs": 1000}),
    ];
    for op in ops {
        p.apply(serde_json::from_value(op).unwrap()).unwrap();
    }
    let clips = &p.tracks[0].clips;
    assert_eq!(
        clips.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["a2", "z"]
    );
    assert_eq!((clips[0].in_ms, clips[0].timeline_start_ms), (4000, 0));
    assert_eq!(clips[1].timeline_start_ms, 29_000);
    let unknown = serde_json::from_value::<aun_media::edl::EditOp>(json!({"op": "explode", "clipId": "a"}));
    assert!(unknown.is_err());
    let extra = serde_json::from_value::<aun_media::edl::EditOp>(
        json!({"op": "rippleDeleteClip", "clipId": "a", "x": 1}),
    );
    assert!(extra.is_err());
}

#[test]
fn flatten_resolves_sources_in_timeline_order() {
    let mut p = project(vec![clip("b", 5000, 8000, 4000), clip("a", 0, 4000, 0)]);
    p.tracks.push(track("t", TrackKind::Title, vec![]));
    let flat = p.flatten().unwrap();
    assert_eq!(flat.duration_ms, 7000);
    assert_eq!(flat.tracks.len(), 1, "title tracks are not composition tracks");
    let clips = &flat.tracks[0].clips;
    assert_eq!(
        clips.iter().map(|c| c.clip_id.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(clips[0].path, "/media/cam.mp4");
    let v = serde_json::to_value(&flat).unwrap();
    assert_eq!(v["tracks"][0]["clips"][1]["timelineStartMs"], 4000);
    let mut bad = project(vec![clip("a", 0, 4000, 0), clip("b", 0, 4000, 1000)]);
    assert!(bad.flatten().is_err());
    bad.tracks[0].clips.pop();
    assert!(bad.flatten().is_ok());
}

// ---- titles (E19 19.5 + 19.9: brand-kit titles and lower thirds) ---------------------------------

fn lower_third(id: &str, start_ms: u64, duration_ms: u64) -> Value {
    json!({
        "id": id, "startMs": start_ms, "durationMs": duration_ms,
        "text": {"en": "Al Noor channel", "ar": "قناة النور"},
        "style": {"fontFamily": "Noto Naskh Arabic", "sizePt": 28.0, "color": "#f7f1e8", "background": "#093041", "position": "lowerThird"}
    })
}

fn with_title_track() -> Project {
    let mut p = project(vec![clip("a", 0, 9000, 0)]);
    p.tracks.push(track("t", TrackKind::Title, vec![]));
    p
}

fn apply_json(p: &mut Project, op: Value) -> aun_common::Result<()> {
    p.apply(serde_json::from_value(op).unwrap())
}

#[test]
fn titles_are_added_moved_restyled_and_removed_as_json_ops() {
    let mut p = with_title_track();
    apply_json(
        &mut p,
        json!({"op": "addTitle", "trackId": "t", "title": lower_third("lt2", 5000, 2000)}),
    )
    .unwrap();
    apply_json(
        &mut p,
        json!({"op": "addTitle", "trackId": "t", "title": lower_third("lt1", 1000, 3000)}),
    )
    .unwrap();
    let ids = |p: &Project| {
        p.tracks[1]
            .titles
            .iter()
            .map(|t| t.id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&p), ["lt1", "lt2"], "kept in time order");

    let mut moved = lower_third("lt1", 7000, 1500);
    moved["style"]["position"] = json!("top");
    apply_json(&mut p, json!({"op": "setTitle", "title": moved})).unwrap();
    assert_eq!(ids(&p), ["lt2", "lt1"]);
    let lt1 = &p.tracks[1].titles[1];
    assert_eq!(
        (lt1.start_ms, lt1.duration_ms, lt1.style.position),
        (7000, 1500, TitlePosition::Top)
    );
    assert_eq!(lt1.text["ar"], "قناة النور");

    apply_json(&mut p, json!({"op": "removeTitle", "titleId": "lt2"})).unwrap();
    assert_eq!(ids(&p), ["lt1"]);
    assert!(matches!(
        apply_json(&mut p, json!({"op": "removeTitle", "titleId": "lt2"})),
        Err(Error::NotFound(_))
    ));
}

#[test]
fn a_bad_title_edit_is_refused_and_changes_nothing() {
    let mut p = with_title_track();
    apply_json(
        &mut p,
        json!({"op": "addTitle", "trackId": "t", "title": lower_third("lt", 0, 2000)}),
    )
    .unwrap();
    let before = p.clone();
    // not a title track
    assert!(matches!(
        apply_json(
            &mut p,
            json!({"op": "addTitle", "trackId": "v", "title": lower_third("x", 0, 1000)})
        ),
        Err(Error::InvalidInput(_))
    ));
    // duplicate id, zero length, no text, bad colour
    assert!(
        apply_json(
            &mut p,
            json!({"op": "addTitle", "trackId": "t", "title": lower_third("lt", 0, 1000)})
        )
        .is_err()
    );
    assert!(
        apply_json(
            &mut p,
            json!({"op": "addTitle", "trackId": "t", "title": lower_third("z", 0, 0)})
        )
        .is_err()
    );
    let mut empty = lower_third("lt", 0, 1000);
    empty["text"] = json!({"en": "", "ar": ""});
    assert!(apply_json(&mut p, json!({"op": "setTitle", "title": empty})).is_err());
    let mut red = lower_third("lt", 0, 1000);
    red["style"]["color"] = json!("red");
    assert!(apply_json(&mut p, json!({"op": "setTitle", "title": red})).is_err());
    // locked track
    p.tracks[1].locked = true;
    assert!(matches!(
        apply_json(&mut p, json!({"op": "removeTitle", "titleId": "lt"})),
        Err(Error::Conflict(_))
    ));
    p.tracks[1].locked = false;
    assert_eq!(p, before);
    let unknown_field = serde_json::from_value::<aun_media::edl::EditOp>(
        json!({"op": "removeTitle", "titleId": "lt", "x": 1}),
    );
    assert!(unknown_field.is_err());
}

#[test]
fn flatten_lists_titles_of_unmuted_title_tracks_in_time_order() {
    let mut p = with_title_track();
    for (id, start) in [("tb", 6000), ("ta", 500)] {
        apply_json(
            &mut p,
            json!({"op": "addTitle", "trackId": "t", "title": lower_third(id, start, 2000)}),
        )
        .unwrap();
    }
    let mut hidden = track("h", TrackKind::Title, vec![]);
    hidden.muted = true;
    hidden
        .titles
        .push(serde_json::from_value::<TitleClip>(lower_third("hidden", 0, 1000)).unwrap());
    p.tracks.push(hidden);

    let flat = p.flatten().unwrap();
    assert_eq!(flat.duration_ms, 9000, "titles do not lengthen the export");
    assert_eq!(flat.tracks.len(), 1, "title tracks are not composition tracks");
    assert_eq!(
        flat.titles
            .iter()
            .map(|t| t.title_id.as_str())
            .collect::<Vec<_>>(),
        ["ta", "tb"]
    );
    let v = serde_json::to_value(&flat).unwrap();
    assert_eq!(v["titles"][0]["trackId"], "t");
    assert_eq!(v["titles"][0]["text"]["ar"], "قناة النور");
    assert_eq!(v["titles"][0]["style"]["position"], "lowerThird");
    assert_eq!(v["titles"][1]["startMs"], 6000);
}
