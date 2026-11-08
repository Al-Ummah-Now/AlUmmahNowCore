//! Loading any known project version and upgrading it to the current one.

use aun_common::{Error, Result};
use serde_json::{Map, Value, json};

use super::model::Project;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// One upgrade step, from version `from` to `from + 1`.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub from: u32,
    pub apply: fn(Value) -> Result<Value>,
}

const MIGRATIONS: &[Migration] = &[Migration {
    from: 0,
    apply: v0_to_v1,
}];

fn invalid(detail: impl Into<String>) -> Error {
    Error::InvalidInput(format!("project: {}", detail.into()))
}

/// The document's `schemaVersion`; files from before versioning have none and count as 0.
pub fn schema_version_of(value: &Value) -> Result<u32> {
    match value.get("schemaVersion") {
        None => Ok(0),
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| invalid("schemaVersion must be a non-negative integer")),
    }
}

/// Applies `migrations` until the document reaches `target`.
pub fn migrate_value(mut value: Value, migrations: &[Migration], target: u32) -> Result<Value> {
    let mut version = schema_version_of(&value)?;
    if version > target {
        return Err(Error::Unsupported(format!(
            "project schema version {version} is newer than this app supports ({target})"
        )));
    }
    while version < target {
        let step = migrations
            .iter()
            .find(|m| m.from == version)
            .ok_or_else(|| Error::Internal(format!("no migration from version {version}")))?;
        value = (step.apply)(value)?;
        let next = schema_version_of(&value)?;
        if next != version + 1 {
            return Err(Error::Internal(format!(
                "migration from {version} produced version {next}"
            )));
        }
        version = next;
    }
    Ok(value)
}

/// Parses, migrates and validates a project document of any known version.
pub fn load_project(json: &str) -> Result<Project> {
    let value: Value = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
    if !value.is_object() {
        return Err(invalid("document must be an object"));
    }
    let migrated = migrate_value(value, MIGRATIONS, CURRENT_SCHEMA_VERSION)?;
    let project: Project = serde_json::from_value(migrated).map_err(|e| invalid(e.to_string()))?;
    project.ensure_valid()?;
    Ok(project)
}

fn seconds_to_ms(value: Option<&Value>, what: &str) -> Result<u64> {
    let seconds = value
        .and_then(Value::as_f64)
        .filter(|s| *s >= 0.0 && s.is_finite())
        .ok_or_else(|| invalid(format!("v0 field {what} must be non-negative seconds")))?;
    Ok((seconds * 1000.0).round() as u64)
}

/// v0 was a flat document: times in seconds, `assets` keyed by file name, one implicit video and one
/// implicit audio track, and `chapters` instead of markers.
fn v0_to_v1(value: Value) -> Result<Value> {
    let empty = Vec::new();
    let assets = value.get("assets").and_then(Value::as_array).unwrap_or(&empty);
    let mut sources = Vec::with_capacity(assets.len());
    let mut has_video_by_file = Map::new();
    for (index, asset) in assets.iter().enumerate() {
        let file = asset
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("v0 asset needs a file"))?;
        let id = format!("src_{index}");
        let width = asset.get("width").and_then(Value::as_u64).unwrap_or(0);
        let height = asset.get("height").and_then(Value::as_u64).unwrap_or(0);
        let mut source = Map::new();
        source.insert("id".into(), json!(id));
        source.insert("path".into(), json!(file));
        source.insert(
            "durationMs".into(),
            json!(seconds_to_ms(asset.get("duration"), "asset duration")?),
        );
        if width > 0 && height > 0 {
            source.insert(
                "video".into(),
                json!({"codec": "unknown", "width": width, "height": height, "fps": 30.0}),
            );
        }
        if asset.get("hasAudio").and_then(Value::as_bool).unwrap_or(false) {
            source.insert(
                "audio".into(),
                json!({"codec": "unknown", "sampleRate": 48000, "channels": 2}),
            );
        }
        has_video_by_file.insert(file.to_owned(), json!({"id": id, "video": width > 0}));
        sources.push(Value::Object(source));
    }

    let (mut video_clips, mut audio_clips) = (Vec::new(), Vec::new());
    let clips = value.get("clips").and_then(Value::as_array).unwrap_or(&empty);
    for (index, clip) in clips.iter().enumerate() {
        let file = clip
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("v0 clip needs a file"))?;
        let source = has_video_by_file
            .get(file)
            .ok_or_else(|| invalid(format!("v0 clip references unknown asset {file}")))?;
        let new_clip = json!({
            "id": format!("clip_{index}"),
            "sourceId": source["id"],
            "inMs": seconds_to_ms(clip.get("in"), "clip in")?,
            "outMs": seconds_to_ms(clip.get("out"), "clip out")?,
            "timelineStartMs": seconds_to_ms(clip.get("start"), "clip start")?,
        });
        if source["video"].as_bool().unwrap_or(false) {
            video_clips.push(new_clip);
        } else {
            audio_clips.push(new_clip);
        }
    }

    let chapters = value.get("chapters").and_then(Value::as_array).unwrap_or(&empty);
    let mut markers = Vec::with_capacity(chapters.len());
    for (index, chapter) in chapters.iter().enumerate() {
        let title = chapter.get("title").and_then(Value::as_str).unwrap_or("");
        markers.push(json!({
            "id": format!("marker_{index}"),
            "timeMs": seconds_to_ms(chapter.get("at"), "chapter at")?,
            "title": {"en": title},
            "chapter": true,
        }));
    }

    let name = value.get("name").and_then(Value::as_str).unwrap_or("Untitled");
    Ok(json!({
        "schemaVersion": 1,
        "id": value.get("id").and_then(Value::as_str).unwrap_or("migrated"),
        "metadata": {"title": name, "createdAtMs": 0, "updatedAtMs": 0},
        "settings": {
            "width": value.get("width").and_then(Value::as_u64).unwrap_or(1920),
            "height": value.get("height").and_then(Value::as_u64).unwrap_or(1080),
            "fps": value.get("fps").and_then(Value::as_f64).unwrap_or(30.0),
            "sampleRate": 48000,
            "channels": 2,
        },
        "sources": sources,
        "tracks": [
            {"id": "track_video", "kind": "video", "clips": video_clips},
            {"id": "track_audio", "kind": "audio", "clips": audio_clips},
        ],
        "markers": markers,
    }))
}
