//! Semantic project validation, beyond what serde and the JSON Schema can express.

use std::collections::HashSet;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use super::model::{Project, Track, TrackKind};

/// One problem. `code` is stable for programmatic handling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationIssue {
    pub code: String,
    /// The id of the offending item.
    pub item: String,
    pub message: String,
}

struct Issues(Vec<ValidationIssue>);

impl Issues {
    fn add(&mut self, code: &str, item: &str, message: String) {
        self.0.push(ValidationIssue {
            code: code.to_owned(),
            item: item.to_owned(),
            message,
        });
    }
}

fn is_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

impl Project {
    /// Every problem found, empty when the project is valid.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Issues(Vec::new());
        self.check_settings(&mut issues);
        self.check_unique_ids(&mut issues);
        self.check_sources(&mut issues);
        for track in &self.tracks {
            self.check_track(track, &mut issues);
        }
        self.check_transitions(&mut issues);
        issues.0
    }

    /// Returns `InvalidInput` naming the first problems if the project is invalid.
    pub fn ensure_valid(&self) -> Result<()> {
        let issues = self.validate();
        if issues.is_empty() {
            return Ok(());
        }
        let summary = issues
            .iter()
            .take(5)
            .map(|i| format!("{} ({}): {}", i.code, i.item, i.message))
            .collect::<Vec<_>>()
            .join("; ");
        Err(Error::InvalidInput(format!("invalid project: {summary}")))
    }

    fn check_settings(&self, issues: &mut Issues) {
        let s = &self.settings;
        let even = |v: u32| v > 0 && v.is_multiple_of(2);
        if !even(s.width) || !even(s.height) || !(s.fps > 0.0 && s.fps.is_finite()) {
            issues.add(
                "bad_settings",
                &self.id,
                "dimensions must be positive and even, with a positive frame rate".to_owned(),
            );
        }
        if s.sample_rate == 0 || s.channels == 0 {
            issues.add("bad_settings", &self.id, "empty audio format".to_owned());
        }
    }

    fn check_unique_ids(&self, issues: &mut Issues) {
        let mut seen = HashSet::new();
        let mut visit = |id: &str| {
            if id.is_empty() {
                issues.add("empty_id", id, "ids must not be empty".to_owned());
            } else if !seen.insert(id.to_owned()) {
                issues.add("duplicate_id", id, format!("id {id} is used twice"));
            }
        };
        self.sources.iter().for_each(|s| visit(&s.id));
        for track in &self.tracks {
            visit(&track.id);
            track.clips.iter().for_each(|c| visit(&c.id));
            track.titles.iter().for_each(|t| visit(&t.id));
            track.captions.iter().for_each(|c| visit(&c.id));
        }
        self.transitions.iter().for_each(|t| visit(&t.id));
        self.markers.iter().for_each(|m| visit(&m.id));
    }

    fn check_sources(&self, issues: &mut Issues) {
        for source in &self.sources {
            if source.path.is_empty() {
                issues.add("bad_source", &source.id, "empty path".to_owned());
            }
            if source.video.is_none() && source.audio.is_none() {
                issues.add("bad_source", &source.id, "no video or audio".to_owned());
            }
        }
    }

    fn check_track(&self, track: &Track, issues: &mut Issues) {
        let (clips_ok, titles_ok, captions_ok) = match track.kind {
            TrackKind::Video | TrackKind::Audio => (true, false, false),
            TrackKind::Title => (false, true, false),
            TrackKind::Caption => (false, false, true),
        };
        if (!clips_ok && !track.clips.is_empty())
            || (!titles_ok && !track.titles.is_empty())
            || (!captions_ok && !track.captions.is_empty())
        {
            issues.add(
                "wrong_track_content",
                &track.id,
                format!("{:?} track holds items of another kind", track.kind),
            );
        }
        for clip in &track.clips {
            if !clip.speed.is_finite() || !(0.25..=4.0).contains(&clip.speed) {
                issues.add("bad_speed", &clip.id, format!("speed {}", clip.speed));
                continue;
            }
            if !clip.volume.is_finite() || !(0.0..=4.0).contains(&clip.volume) {
                issues.add("bad_volume", &clip.id, format!("volume {}", clip.volume));
            }
            if clip.in_ms >= clip.out_ms {
                issues.add("bad_range", &clip.id, "in must be before out".to_owned());
            }
            match self.source(&clip.source_id) {
                None => issues.add(
                    "unknown_source",
                    &clip.id,
                    format!("source {} does not exist", clip.source_id),
                ),
                Some(source) => {
                    if clip.out_ms > source.duration_ms {
                        issues.add(
                            "out_of_range",
                            &clip.id,
                            format!(
                                "out {} is past the source end {}",
                                clip.out_ms, source.duration_ms
                            ),
                        );
                    }
                    let usable = match track.kind {
                        TrackKind::Video => source.video.is_some(),
                        _ => source.audio.is_some(),
                    };
                    if !usable {
                        issues.add(
                            "wrong_media",
                            &clip.id,
                            format!("source lacks {:?} media", track.kind),
                        );
                    }
                }
            }
        }
        self.check_overlaps(track, issues);
        for title in &track.titles {
            if title.duration_ms == 0 || title.text.values().all(String::is_empty) {
                issues.add("bad_title", &title.id, "needs a duration and text".to_owned());
            }
            if !is_color(&title.style.color)
                || title.style.background.as_deref().is_some_and(|c| !is_color(c))
                || !(title.style.size_pt > 0.0 && title.style.size_pt.is_finite())
            {
                issues.add("bad_title_style", &title.id, "bad colour or size".to_owned());
            }
        }
        for cue in &track.captions {
            if cue.start_ms >= cue.end_ms || cue.text.is_empty() {
                issues.add("bad_caption", &cue.id, "needs a time range and text".to_owned());
            }
        }
    }

    /// Clips of one track may overlap only by exactly a declared transition.
    fn check_overlaps(&self, track: &Track, issues: &mut Issues) {
        let mut clips: Vec<_> = track.clips.iter().collect();
        clips.sort_by_key(|c| c.timeline_start_ms);
        for pair in clips.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let overlap = a.timeline_end_ms().saturating_sub(b.timeline_start_ms);
            if overlap == 0 {
                continue;
            }
            let declared = self
                .transitions
                .iter()
                .any(|t| t.from_clip_id == a.id && t.to_clip_id == b.id && t.duration_ms == overlap);
            if !declared {
                issues.add(
                    "overlap",
                    &b.id,
                    format!("overlaps {} by {overlap} ms without a transition", a.id),
                );
            }
        }
    }

    fn check_transitions(&self, issues: &mut Issues) {
        for transition in &self.transitions {
            if transition.duration_ms == 0 {
                issues.add("bad_transition", &transition.id, "zero duration".to_owned());
            }
            let on_same_track = self.tracks.iter().any(|t| {
                t.clips.iter().any(|c| c.id == transition.from_clip_id)
                    && t.clips.iter().any(|c| c.id == transition.to_clip_id)
            });
            if !on_same_track {
                issues.add(
                    "bad_transition",
                    &transition.id,
                    "both clips must exist on one track".to_owned(),
                );
                continue;
            }
            if let (Some(a), Some(b)) = (
                self.clip(&transition.from_clip_id),
                self.clip(&transition.to_clip_id),
            ) && (a.timeline_end_ms().saturating_sub(b.timeline_start_ms) != transition.duration_ms
                || b.timeline_start_ms < a.timeline_start_ms)
            {
                issues.add(
                    "bad_transition",
                    &transition.id,
                    "clips must overlap by exactly the transition duration".to_owned(),
                );
            }
        }
    }
}
