//! Editing operations. Each runs on a copy and is committed only if the result is valid, so a
//! failed edit leaves the project untouched.

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;

use super::model::{Clip, Project, TitleClip, TitleStyle, Track, TrackKind};

fn not_found(id: &str) -> Error {
    Error::NotFound(format!("clip {id}"))
}

impl Project {
    fn transact<T>(&mut self, edit: impl FnOnce(&mut Project) -> Result<T>) -> Result<T> {
        let mut draft = self.clone();
        let result = edit(&mut draft)?;
        draft.ensure_valid()?;
        *self = draft;
        Ok(result)
    }

    fn unlocked_track_with_clip(&mut self, clip_id: &str) -> Result<&mut Track> {
        let track = self
            .tracks
            .iter_mut()
            .find(|t| t.clips.iter().any(|c| c.id == clip_id))
            .ok_or_else(|| not_found(clip_id))?;
        if track.locked {
            return Err(Error::Conflict(format!("track {} is locked", track.id)));
        }
        Ok(track)
    }

    /// Cuts a clip in two at a timeline position strictly inside it. The right half gets `new_id`
    /// and inherits transitions leaving the original clip.
    pub fn split_clip(&mut self, clip_id: &str, at_ms: u64, new_id: &str) -> Result<()> {
        self.transact(|project| {
            let track = project.unlocked_track_with_clip(clip_id)?;
            let index = track
                .clips
                .iter()
                .position(|c| c.id == clip_id)
                .ok_or_else(|| not_found(clip_id))?;
            let clip = track.clips[index].clone();
            if at_ms <= clip.timeline_start_ms || at_ms >= clip.timeline_end_ms() {
                return Err(Error::InvalidInput(format!(
                    "split point {at_ms} is outside clip {clip_id}"
                )));
            }
            let source_cut = clip.source_at(at_ms);
            let right = Clip {
                id: new_id.to_owned(),
                in_ms: source_cut,
                timeline_start_ms: at_ms,
                ..clip
            };
            track.clips[index].out_ms = source_cut;
            track.clips.insert(index + 1, right);
            for transition in &mut project.transitions {
                if transition.from_clip_id == clip_id {
                    transition.from_clip_id = new_id.to_owned();
                }
            }
            Ok(())
        })
    }

    /// Sets a clip's source range. Trimming the head moves the clip's start, so the material
    /// that stays does not shift on the timeline.
    pub fn trim_clip(&mut self, clip_id: &str, new_in_ms: u64, new_out_ms: u64) -> Result<()> {
        self.transact(|project| {
            let track = project.unlocked_track_with_clip(clip_id)?;
            let clip = track
                .clips
                .iter_mut()
                .find(|c| c.id == clip_id)
                .ok_or_else(|| not_found(clip_id))?;
            let delta = (new_in_ms as f64 - clip.in_ms as f64) / clip.speed;
            let start = clip.timeline_start_ms as f64 + delta;
            if start < 0.0 {
                return Err(Error::InvalidInput(
                    "trim would move the clip before the timeline start".to_owned(),
                ));
            }
            clip.timeline_start_ms = start.round() as u64;
            clip.in_ms = new_in_ms;
            clip.out_ms = new_out_ms;
            project
                .transitions
                .retain(|t| t.from_clip_id != clip_id && t.to_clip_id != clip_id);
            Ok(())
        })
    }

    /// Removes a clip and closes the gap on its own track.
    pub fn ripple_delete_clip(&mut self, clip_id: &str) -> Result<()> {
        self.transact(|project| {
            let track = project.unlocked_track_with_clip(clip_id)?;
            let index = track
                .clips
                .iter()
                .position(|c| c.id == clip_id)
                .ok_or_else(|| not_found(clip_id))?;
            let removed = track.clips.remove(index);
            // A clip overlapped by its successor (a transition) only frees up to where that
            // successor begins.
            let next_start = track
                .clips
                .iter()
                .map(|c| c.timeline_start_ms)
                .filter(|s| *s > removed.timeline_start_ms)
                .min();
            let shift = next_start.map_or(removed.duration_ms(), |next| {
                (next - removed.timeline_start_ms).min(removed.duration_ms())
            });
            for clip in &mut track.clips {
                if clip.timeline_start_ms > removed.timeline_start_ms {
                    clip.timeline_start_ms -= shift;
                }
            }
            project
                .transitions
                .retain(|t| t.from_clip_id != clip_id && t.to_clip_id != clip_id);
            Ok(())
        })
    }

    /// Removes `[start_ms, end_ms)` from every unlocked track and closes the gap: straddling items
    /// are trimmed, spanning clips are split, and markers inside the range are dropped.
    pub fn ripple_delete_range(&mut self, start_ms: u64, end_ms: u64) -> Result<()> {
        if start_ms >= end_ms {
            return Err(Error::InvalidInput("start must be before end".to_owned()));
        }
        self.transact(|project| {
            let shift = end_ms - start_ms;
            let mut touched: Vec<String> = Vec::new();
            for track in project.tracks.iter_mut().filter(|t| !t.locked) {
                let mut clips = Vec::with_capacity(track.clips.len());
                for clip in track.clips.drain(..) {
                    ripple_clip(clip, start_ms, end_ms, &mut clips, &mut touched);
                }
                track.clips = clips;
                track.titles.retain_mut(|title| {
                    let end = title.start_ms.saturating_add(title.duration_ms);
                    match cut_span(title.start_ms, end, start_ms, end_ms) {
                        Some((s, e)) => {
                            title.start_ms = s;
                            title.duration_ms = e - s;
                            true
                        }
                        None => false,
                    }
                });
                track
                    .captions
                    .retain_mut(|cue| match cut_span(cue.start_ms, cue.end_ms, start_ms, end_ms) {
                        Some((s, e)) => {
                            cue.start_ms = s;
                            cue.end_ms = e;
                            true
                        }
                        None => false,
                    });
            }
            project.markers.retain_mut(|marker| {
                if marker.time_ms < start_ms {
                    true
                } else if marker.time_ms < end_ms {
                    false
                } else {
                    marker.time_ms -= shift;
                    true
                }
            });
            project
                .transitions
                .retain(|t| !touched.contains(&t.from_clip_id) && !touched.contains(&t.to_clip_id));
            Ok(())
        })
    }

    /// Joins two clips back into one: `right` must directly follow `left` on the same track and
    /// continue the same source at the same speed and volume (the inverse of [`Self::split_clip`]).
    pub fn join_clips(&mut self, left_id: &str, right_id: &str) -> Result<()> {
        self.transact(|project| {
            let track = project.unlocked_track_with_clip(left_id)?;
            let li = track
                .clips
                .iter()
                .position(|c| c.id == left_id)
                .ok_or_else(|| not_found(left_id))?;
            let ri = track
                .clips
                .iter()
                .position(|c| c.id == right_id)
                .ok_or_else(|| not_found(right_id))?;
            let (left, right) = (&track.clips[li], &track.clips[ri]);
            let right_out = right.out_ms;
            if left.source_id != right.source_id
                || left.out_ms != right.in_ms
                || left.timeline_end_ms() != right.timeline_start_ms
                || left.speed != right.speed
                || left.volume != right.volume
            {
                return Err(Error::InvalidInput(format!(
                    "{right_id} does not continue {left_id}: only the two halves of a cut can be joined"
                )));
            }
            track.clips[li].out_ms = right_out;
            track.clips.remove(ri);
            project.transitions.retain(|t| {
                !(t.from_clip_id == left_id && t.to_clip_id == right_id) && t.to_clip_id != right_id
            });
            for transition in &mut project.transitions {
                if transition.from_clip_id == right_id {
                    transition.from_clip_id = left_id.to_owned();
                }
            }
            Ok(())
        })
    }

    /// Moves a clip to `start_ms` on `track_id` (its own track or another of the same kind).
    /// Its transitions are dropped; landing on another clip is refused.
    pub fn move_clip(&mut self, clip_id: &str, track_id: &str, start_ms: u64) -> Result<()> {
        self.transact(|project| {
            let from = project.unlocked_track_with_clip(clip_id)?;
            let index = from
                .clips
                .iter()
                .position(|c| c.id == clip_id)
                .ok_or_else(|| not_found(clip_id))?;
            let mut clip = from.clips.remove(index);
            clip.timeline_start_ms = start_ms;
            let to = unlocked_track(project, track_id)?;
            insert_sorted(to, clip);
            project
                .transitions
                .retain(|t| t.from_clip_id != clip_id && t.to_clip_id != clip_id);
            Ok(())
        })
    }

    /// Adds a clip to a track, at `clip.timeline_start_ms` or, when that is `None`, at the end of
    /// the track.
    pub fn append_clip(&mut self, track_id: &str, clip: NewClip) -> Result<()> {
        self.transact(|project| {
            let track = unlocked_track(project, track_id)?;
            let end = track.clips.iter().map(Clip::timeline_end_ms).max().unwrap_or(0);
            let clip = Clip {
                id: clip.id,
                source_id: clip.source_id,
                in_ms: clip.in_ms,
                out_ms: clip.out_ms,
                timeline_start_ms: clip.timeline_start_ms.unwrap_or(end),
                speed: clip.speed.unwrap_or(1.0),
                volume: clip.volume.unwrap_or(1.0),
            };
            insert_sorted(track, clip);
            Ok(())
        })
    }

    /// Adds an on-screen title (for example a brand-kit lower third) to a title track.
    pub fn add_title(&mut self, track_id: &str, title: TitleClip) -> Result<()> {
        self.transact(|project| {
            let track = unlocked_track(project, track_id)?;
            if track.kind != TrackKind::Title {
                return Err(Error::InvalidInput(format!(
                    "track {track_id} is not a title track"
                )));
            }
            let at = track
                .titles
                .iter()
                .position(|t| t.start_ms > title.start_ms)
                .unwrap_or(track.titles.len());
            track.titles.insert(at, title);
            Ok(())
        })
    }

    /// Replaces the title with the same id: moves, retimes, rewords or restyles it.
    pub fn set_title(&mut self, title: TitleClip) -> Result<()> {
        self.transact(|project| {
            let track = unlocked_track_with_title(project, &title.id)?;
            track.titles.retain(|t| t.id != title.id);
            let at = track
                .titles
                .iter()
                .position(|t| t.start_ms > title.start_ms)
                .unwrap_or(track.titles.len());
            track.titles.insert(at, title);
            Ok(())
        })
    }

    pub fn remove_title(&mut self, title_id: &str) -> Result<()> {
        self.transact(|project| {
            let track = unlocked_track_with_title(project, title_id)?;
            track.titles.retain(|t| t.id != title_id);
            Ok(())
        })
    }

    /// Applies one edit. Every app sends edits this way, so the rules match everywhere.
    pub fn apply(&mut self, op: EditOp) -> Result<()> {
        match op {
            EditOp::Split {
                clip_id,
                at_ms,
                new_id,
            } => self.split_clip(&clip_id, at_ms, &new_id),
            EditOp::Trim {
                clip_id,
                in_ms,
                out_ms,
            } => self.trim_clip(&clip_id, in_ms, out_ms),
            EditOp::RippleDeleteClip { clip_id } => self.ripple_delete_clip(&clip_id),
            EditOp::RippleDeleteRange { start_ms, end_ms } => self.ripple_delete_range(start_ms, end_ms),
            EditOp::Join { left_id, right_id } => self.join_clips(&left_id, &right_id),
            EditOp::Move {
                clip_id,
                track_id,
                start_ms,
            } => self.move_clip(&clip_id, &track_id, start_ms),
            EditOp::Append { track_id, clip } => self.append_clip(&track_id, clip),
            EditOp::AddTitle { track_id, title } => self.add_title(&track_id, title),
            EditOp::SetTitle { title } => self.set_title(title),
            EditOp::RemoveTitle { title_id } => self.remove_title(&title_id),
        }
    }
}

impl Project {
    /// The clip tracks in timeline order with each clip's source path resolved, for the platform
    /// composition APIs (AVMutableComposition, MediaComposition, GES) that render an export, plus
    /// the titles of every title track that isn't muted (muted hides a title track), in time order.
    /// Titles don't lengthen the export: `duration_ms` comes from the clips.
    pub fn flatten(&self) -> Result<FlatTimeline> {
        self.ensure_valid()?;
        let mut duration_ms = 0;
        let mut tracks = Vec::new();
        for track in self
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Video | TrackKind::Audio))
        {
            let mut clips = Vec::with_capacity(track.clips.len());
            let mut ordered: Vec<&Clip> = track.clips.iter().collect();
            ordered.sort_by_key(|c| c.timeline_start_ms);
            for clip in ordered {
                let source = self
                    .sources
                    .iter()
                    .find(|s| s.id == clip.source_id)
                    .ok_or_else(|| Error::NotFound(format!("source {}", clip.source_id)))?;
                duration_ms = duration_ms.max(clip.timeline_end_ms());
                clips.push(FlatClip {
                    clip_id: clip.id.clone(),
                    source_id: clip.source_id.clone(),
                    path: source.path.clone(),
                    in_ms: clip.in_ms,
                    out_ms: clip.out_ms,
                    timeline_start_ms: clip.timeline_start_ms,
                    speed: clip.speed,
                    volume: clip.volume,
                });
            }
            tracks.push(FlatTrack {
                id: track.id.clone(),
                kind: track.kind,
                muted: track.muted,
                clips,
            });
        }
        let mut titles: Vec<FlatTitle> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Title && !t.muted)
            .flat_map(|track| {
                track.titles.iter().map(|title| FlatTitle {
                    title_id: title.id.clone(),
                    track_id: track.id.clone(),
                    start_ms: title.start_ms,
                    duration_ms: title.duration_ms,
                    text: title.text.clone(),
                    style: title.style.clone(),
                })
            })
            .collect();
        titles.sort_by_key(|t| t.start_ms);
        Ok(FlatTimeline {
            duration_ms,
            tracks,
            titles,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatTimeline {
    pub duration_ms: u64,
    pub tracks: Vec<FlatTrack>,
    pub titles: Vec<FlatTitle>,
}

/// A title to draw over the picture from `start_ms` for `duration_ms`. `text` maps locale → string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatTitle {
    pub title_id: String,
    pub track_id: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub text: BTreeMap<String, String>,
    pub style: TitleStyle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatTrack {
    pub id: String,
    pub kind: TrackKind,
    pub muted: bool,
    pub clips: Vec<FlatClip>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatClip {
    pub clip_id: String,
    pub source_id: String,
    pub path: String,
    pub in_ms: u64,
    pub out_ms: u64,
    pub timeline_start_ms: u64,
    pub speed: f64,
    pub volume: f64,
}

fn unlocked_track<'a>(project: &'a mut Project, track_id: &str) -> Result<&'a mut Track> {
    let track = project
        .tracks
        .iter_mut()
        .find(|t| t.id == track_id)
        .ok_or_else(|| Error::NotFound(format!("track {track_id}")))?;
    if track.locked {
        return Err(Error::Conflict(format!("track {} is locked", track.id)));
    }
    Ok(track)
}

fn unlocked_track_with_title<'a>(project: &'a mut Project, title_id: &str) -> Result<&'a mut Track> {
    let track = project
        .tracks
        .iter_mut()
        .find(|t| t.titles.iter().any(|x| x.id == title_id))
        .ok_or_else(|| Error::NotFound(format!("title {title_id}")))?;
    if track.locked {
        return Err(Error::Conflict(format!("track {} is locked", track.id)));
    }
    Ok(track)
}

fn insert_sorted(track: &mut Track, clip: Clip) {
    let at = track
        .clips
        .iter()
        .position(|c| c.timeline_start_ms > clip.timeline_start_ms)
        .unwrap_or(track.clips.len());
    track.clips.insert(at, clip);
}

/// A clip to add; omitted fields take their defaults (end of track, speed 1, volume 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewClip {
    pub id: String,
    pub source_id: String,
    pub in_ms: u64,
    pub out_ms: u64,
    #[serde(default)]
    pub timeline_start_ms: Option<u64>,
    #[serde(default)]
    pub speed: Option<f64>,
    #[serde(default)]
    pub volume: Option<f64>,
}

/// One edit as the apps send it: `{"op":"split","clipId":"c1","atMs":5000,"newId":"c2"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "op",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EditOp {
    Split {
        clip_id: String,
        at_ms: u64,
        new_id: String,
    },
    Trim {
        clip_id: String,
        in_ms: u64,
        out_ms: u64,
    },
    RippleDeleteClip {
        clip_id: String,
    },
    RippleDeleteRange {
        start_ms: u64,
        end_ms: u64,
    },
    Join {
        left_id: String,
        right_id: String,
    },
    Move {
        clip_id: String,
        track_id: String,
        start_ms: u64,
    },
    Append {
        track_id: String,
        clip: NewClip,
    },
    AddTitle {
        track_id: String,
        title: TitleClip,
    },
    SetTitle {
        title: TitleClip,
    },
    RemoveTitle {
        title_id: String,
    },
}

/// The span after removing `[cut_start, cut_end)` and closing the gap; `None` if nothing remains.
fn cut_span(start: u64, end: u64, cut_start: u64, cut_end: u64) -> Option<(u64, u64)> {
    let shift = cut_end - cut_start;
    let map = |t: u64| {
        if t <= cut_start {
            t
        } else if t >= cut_end {
            t - shift
        } else {
            cut_start
        }
    };
    let (s, e) = (map(start), map(end));
    (s < e).then_some((s, e))
}

fn ripple_clip(clip: Clip, cut_start: u64, cut_end: u64, out: &mut Vec<Clip>, touched: &mut Vec<String>) {
    let shift = cut_end - cut_start;
    let (start, end) = (clip.timeline_start_ms, clip.timeline_end_ms());
    if end <= cut_start {
        out.push(clip);
        return;
    }
    if start >= cut_end {
        out.push(Clip {
            timeline_start_ms: start - shift,
            ..clip
        });
        return;
    }
    touched.push(clip.id.clone());
    if start < cut_start {
        out.push(Clip {
            out_ms: clip.source_at(cut_start),
            ..clip.clone()
        });
    }
    if end > cut_end {
        let id = if start < cut_start {
            format!("{}_r", clip.id)
        } else {
            clip.id.clone()
        };
        out.push(Clip {
            id,
            in_ms: clip.source_at(cut_end),
            timeline_start_ms: cut_start,
            ..clip
        });
    }
}
