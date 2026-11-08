//! Serde types for the project format. `schemas/edl/v1.json` is the published description.

use std::collections::BTreeMap;

use aun_common::Result;
use serde::{Deserialize, Serialize};

use super::migrate::CURRENT_SCHEMA_VERSION;
use crate::job::TransitionKind;

fn one() -> f64 {
    1.0
}

fn is_one(v: &f64) -> bool {
    *v == 1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Project {
    pub schema_version: u32,
    pub id: String,
    pub metadata: ProjectMetadata,
    pub settings: ProjectSettings,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
    #[serde(default)]
    pub markers: Vec<Marker>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectMetadata {
    pub title: String,
    /// Locale → description.
    #[serde(default)]
    pub description: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectSettings {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub sample_rate: u32,
    pub channels: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VideoParams {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioParams {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
}

/// A media file the project references.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub path: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoParams>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioParams>,
    /// Keyframe positions, ascending. Empty when unknown, which rules out stream copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes_ms: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    Title,
    Caption,
}

/// A timeline lane. Later video tracks are drawn over earlier ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub locked: bool,
    /// Caption tracks only: render into the picture instead of shipping as a sidecar.
    #[serde(default)]
    pub burn_in: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<Clip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<TitleClip>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub captions: Vec<CaptionCue>,
}

/// A span of a source on the timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Clip {
    pub id: String,
    pub source_id: String,
    pub in_ms: u64,
    pub out_ms: u64,
    pub timeline_start_ms: u64,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub speed: f64,
    /// Linear gain: 1.0 is unchanged, 0.0 is mute.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub volume: f64,
}

impl Clip {
    /// Length on the timeline after speed.
    pub fn duration_ms(&self) -> u64 {
        let source = self.out_ms.saturating_sub(self.in_ms) as f64;
        (source / self.speed).round() as u64
    }

    pub fn timeline_end_ms(&self) -> u64 {
        self.timeline_start_ms.saturating_add(self.duration_ms())
    }

    /// The source position shown at a timeline position inside this clip.
    pub fn source_at(&self, timeline_ms: u64) -> u64 {
        let offset = timeline_ms.saturating_sub(self.timeline_start_ms) as f64;
        (self.in_ms + (offset * self.speed).round() as u64).min(self.out_ms)
    }
}

/// A blend between two overlapping clips of one track. The overlap must equal `duration_ms`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transition {
    pub id: String,
    pub kind: TransitionKind,
    pub from_clip_id: String,
    pub to_clip_id: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TitlePosition {
    Top,
    Center,
    LowerThird,
}

impl TitlePosition {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Center => "center",
            Self::LowerThird => "lower_third",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TitleStyle {
    pub font_family: String,
    pub size_pt: f64,
    /// `#RRGGBB`.
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    pub position: TitlePosition,
}

/// An on-screen title. `text` maps locale → string, for bilingual titles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TitleClip {
    pub id: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub text: BTreeMap<String, String>,
    pub style: TitleStyle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptionCue {
    pub id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub locale: String,
}

/// A timeline marker; chapter markers become player chapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Marker {
    pub id: String,
    pub time_ms: u64,
    pub title: BTreeMap<String, String>,
    #[serde(default)]
    pub chapter: bool,
}

/// A chapter derived from the markers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectChapter {
    pub start_ms: u64,
    pub title: BTreeMap<String, String>,
}

impl Project {
    /// An empty project at the current schema version.
    pub fn new(id: impl Into<String>, title: impl Into<String>, settings: ProjectSettings) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            id: id.into(),
            metadata: ProjectMetadata {
                title: title.into(),
                description: BTreeMap::new(),
                created_at_ms: 0,
                updated_at_ms: 0,
            },
            settings,
            sources: Vec::new(),
            tracks: Vec::new(),
            transitions: Vec::new(),
            markers: Vec::new(),
        }
    }

    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn clip(&self, id: &str) -> Option<&Clip> {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == id)
    }

    /// The end of the last item on any track.
    pub fn duration_ms(&self) -> u64 {
        let mut end = 0;
        for track in &self.tracks {
            for clip in &track.clips {
                end = end.max(clip.timeline_end_ms());
            }
            for title in &track.titles {
                end = end.max(title.start_ms.saturating_add(title.duration_ms));
            }
            for cue in &track.captions {
                end = end.max(cue.end_ms);
            }
        }
        end
    }

    /// Chapter markers in time order.
    pub fn chapters(&self) -> Vec<ProjectChapter> {
        let mut chapters: Vec<ProjectChapter> = self
            .markers
            .iter()
            .filter(|m| m.chapter)
            .map(|m| ProjectChapter {
                start_ms: m.time_ms,
                title: m.title.clone(),
            })
            .collect();
        chapters.sort_by_key(|c| c.start_ms);
        chapters
    }
}
