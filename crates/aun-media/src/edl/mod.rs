//! The Studio edit model (Epic 30.5): a versioned project format, its migrations, validation,
//! editing operations, and render planning into a [`crate::job::JobPlan`].

mod migrate;
mod model;
mod ops;
mod render;
mod validate;

pub use migrate::{CURRENT_SCHEMA_VERSION, Migration, load_project, migrate_value, schema_version_of};
pub use model::{
    AudioParams, CaptionCue, Clip, Marker, Project, ProjectChapter, ProjectMetadata, ProjectSettings, Source,
    TitleClip, TitlePosition, TitleStyle, Track, TrackKind, Transition, VideoParams,
};
pub use render::{RenderPlan, RenderRequest, RenderSegment, SegmentDecision, plan_render};
pub use validate::ValidationIssue;
