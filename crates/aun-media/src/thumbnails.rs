//! Filmstrip and thumbnail timestamp selection (Epic 30.3).

use serde::{Deserialize, Serialize};

use crate::analysis::SceneChange;

/// Tuning for [`select_thumbnail_timestamps`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThumbnailConfig {
    pub count: u32,
    /// Skips this much at each end (fades, slates).
    pub margin_ms: u64,
    /// A slot moves to a scene start this close to it.
    pub snap_window_ms: u64,
    /// How far past a cut to land, so the frame is not caught mid-transition.
    pub cut_offset_ms: u64,
}

impl Default for ThumbnailConfig {
    fn default() -> Self {
        Self {
            count: 10,
            margin_ms: 1000,
            snap_window_ms: 3000,
            cut_offset_ms: 500,
        }
    }
}

/// Chooses up to `count` ascending, distinct timestamps: evenly spaced slots, each snapped to a
/// nearby scene start. Very short media yields fewer timestamps rather than duplicates.
pub fn select_thumbnail_timestamps(
    duration_ms: u64,
    scenes: &[SceneChange],
    config: ThumbnailConfig,
) -> Vec<u64> {
    if duration_ms == 0 || config.count == 0 {
        return Vec::new();
    }
    let margin = config.margin_ms.min(duration_ms / 4);
    let last_valid = duration_ms - margin - 1;
    let span = duration_ms - 2 * margin;
    let mut cuts: Vec<u64> = scenes.iter().map(|s| s.pts_ms).collect();
    cuts.sort_unstable();
    let count = u64::from(config.count);
    let mut chosen: Vec<u64> = Vec::with_capacity(config.count as usize);
    for slot in 0..count {
        let centre = margin + span * (2 * slot + 1) / (2 * count);
        let snapped = cuts
            .iter()
            .filter(|cut| cut.abs_diff(centre) <= config.snap_window_ms)
            .min_by_key(|cut| cut.abs_diff(centre))
            .map_or(centre, |cut| cut.saturating_add(config.cut_offset_ms));
        let snapped = snapped.clamp(margin, last_valid);
        // Keep the strip ordered and distinct even when several slots snap to one cut.
        let t = match chosen.last() {
            Some(prev) if snapped <= *prev => {
                if centre > *prev && centre <= last_valid {
                    centre
                } else {
                    continue;
                }
            }
            _ => snapped,
        };
        chosen.push(t);
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(pts_ms: u64) -> SceneChange {
        SceneChange { pts_ms, score: 0.9 }
    }

    fn config(count: u32, snap: u64) -> ThumbnailConfig {
        ThumbnailConfig {
            count,
            margin_ms: 0,
            snap_window_ms: snap,
            cut_offset_ms: 500,
        }
    }

    #[test]
    fn even_slots_without_scenes() {
        assert_eq!(
            select_thumbnail_timestamps(10_000, &[], config(5, 0)),
            vec![1000, 3000, 5000, 7000, 9000]
        );
    }

    #[test]
    fn snaps_to_nearby_cut() {
        let got = select_thumbnail_timestamps(10_000, &[cut(2600)], config(5, 500));
        assert_eq!(got, vec![1000, 3100, 5000, 7000, 9000]);
    }

    #[test]
    fn slots_sharing_a_cut_stay_distinct_and_ordered() {
        let got = select_thumbnail_timestamps(10_000, &[cut(4000)], config(5, 5000));
        assert!(got.windows(2).all(|w| w[0] < w[1]), "{got:?}");
        assert!(got.len() >= 2);
    }

    #[test]
    fn margins_are_respected() {
        let cfg = ThumbnailConfig {
            margin_ms: 2000,
            ..config(4, 3000)
        };
        let got = select_thumbnail_timestamps(20_000, &[cut(100), cut(19_900)], cfg);
        assert!(got.iter().all(|t| (2000..18_000).contains(t)), "{got:?}");
    }

    #[test]
    fn tiny_duration_and_zero_count() {
        assert!(select_thumbnail_timestamps(0, &[], config(5, 0)).is_empty());
        assert!(select_thumbnail_timestamps(1000, &[], config(0, 0)).is_empty());
        let got = select_thumbnail_timestamps(1, &[], config(3, 0));
        assert_eq!(got, vec![0]);
    }
}
