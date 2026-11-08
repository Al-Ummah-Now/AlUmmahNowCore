//! Editing operations. Each runs on a copy and is committed only if the result is valid, so a
//! failed edit leaves the project untouched.

use aun_common::{Error, Result};

use super::model::{Clip, Project, Track};

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
