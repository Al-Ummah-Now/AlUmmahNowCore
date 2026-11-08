//! Scene-change detection from frames supplied by native decoders.

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const HISTOGRAM_BINS: usize = 64;

/// A decoded frame as an 8-bit luma plane, tightly packed (`width * height` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub pts_ms: u64,
    pub width: u32,
    pub height: u32,
    pub luma: Vec<u8>,
}

/// Supplies frames in presentation order, usually downscaled (for example to 160×90). Implemented
/// by the native decoders.
pub trait FrameSource: Send {
    /// The next frame, or `None` at the end.
    fn next_frame(&mut self) -> Result<Option<VideoFrame>>;
}

/// Tuning for [`SceneDetector`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SceneConfig {
    /// A change is reported when the score reaches this value in `0.0..=1.0`.
    pub threshold: f64,
    /// Weight of the histogram distance; the mean absolute difference gets `1 - weight`.
    pub histogram_weight: f64,
    /// Minimum distance between reported changes, to suppress flashes.
    pub min_gap_ms: u64,
}

impl Default for SceneConfig {
    fn default() -> Self {
        Self {
            threshold: 0.35,
            histogram_weight: 0.5,
            min_gap_ms: 1000,
        }
    }
}

/// A detected cut.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SceneChange {
    pub pts_ms: u64,
    /// Strength of the cut in `0.0..=1.0`.
    pub score: f64,
}

/// Scores consecutive frames with a luma histogram distance blended with their mean absolute
/// difference. The histogram resists motion; the difference catches cuts between similar palettes.
#[derive(Debug)]
pub struct SceneDetector {
    config: SceneConfig,
    previous: Option<VideoFrame>,
    previous_histogram: [u32; HISTOGRAM_BINS],
    last_change_ms: Option<u64>,
}

impl SceneDetector {
    pub fn new(config: SceneConfig) -> Result<Self> {
        if !(0.0..=1.0).contains(&config.threshold) || !(0.0..=1.0).contains(&config.histogram_weight) {
            return Err(Error::InvalidInput(
                "scene threshold and weight must be between 0 and 1".to_owned(),
            ));
        }
        Ok(Self {
            config,
            previous: None,
            previous_histogram: [0; HISTOGRAM_BINS],
            last_change_ms: None,
        })
    }

    /// Feeds one frame; returns the change it starts, if any.
    pub fn push(&mut self, frame: VideoFrame) -> Result<Option<SceneChange>> {
        let expected = u64::from(frame.width) * u64::from(frame.height);
        if expected == 0 || frame.luma.len() as u64 != expected {
            return Err(Error::InvalidInput(format!(
                "frame at {} ms: luma plane is {} bytes, expected {expected}",
                frame.pts_ms,
                frame.luma.len()
            )));
        }
        let histogram = histogram(&frame.luma);
        let mut change = None;
        if let Some(prev) = &self.previous {
            if prev.width != frame.width || prev.height != frame.height {
                return Err(Error::InvalidInput("frame size changed mid-stream".to_owned()));
            }
            let hist = histogram_distance(&self.previous_histogram, &histogram, frame.luma.len());
            let sad = mean_absolute_difference(&prev.luma, &frame.luma);
            let weight = self.config.histogram_weight;
            let score = (hist * weight + sad * (1.0 - weight)).clamp(0.0, 1.0);
            let far_enough = self
                .last_change_ms
                .is_none_or(|t| frame.pts_ms.saturating_sub(t) >= self.config.min_gap_ms);
            if score >= self.config.threshold && far_enough {
                self.last_change_ms = Some(frame.pts_ms);
                change = Some(SceneChange {
                    pts_ms: frame.pts_ms,
                    score,
                });
            }
        }
        self.previous = Some(frame);
        self.previous_histogram = histogram;
        Ok(change)
    }
}

/// Runs a detector over a whole frame source. Blocking.
pub fn detect_scenes(
    source: &mut dyn FrameSource,
    config: SceneConfig,
    cancel: &CancellationToken,
) -> Result<Vec<SceneChange>> {
    let mut detector = SceneDetector::new(config)?;
    let mut changes = Vec::new();
    while let Some(frame) = source.next_frame()? {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(change) = detector.push(frame)? {
            changes.push(change);
        }
    }
    Ok(changes)
}

fn histogram(luma: &[u8]) -> [u32; HISTOGRAM_BINS] {
    let mut bins = [0u32; HISTOGRAM_BINS];
    for value in luma {
        bins[usize::from(*value) * HISTOGRAM_BINS / 256] += 1;
    }
    bins
}

/// Normalised L1 distance: 0 for identical distributions, 1 for disjoint ones.
fn histogram_distance(a: &[u32; HISTOGRAM_BINS], b: &[u32; HISTOGRAM_BINS], pixels: usize) -> f64 {
    let total: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    total as f64 / (2.0 * pixels as f64)
}

fn mean_absolute_difference(a: &[u8], b: &[u8]) -> f64 {
    let total: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    total as f64 / (a.len() as f64 * 255.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(pts_ms: u64, value: u8) -> VideoFrame {
        VideoFrame {
            pts_ms,
            width: 16,
            height: 9,
            luma: vec![value; 144],
        }
    }

    /// A gradient that drifts sideways, like a slow pan.
    fn pan(pts_ms: u64, shift: usize) -> VideoFrame {
        let luma = (0..144).map(|i| ((i + shift) % 144 * 255 / 144) as u8).collect();
        VideoFrame {
            pts_ms,
            width: 16,
            height: 9,
            luma,
        }
    }

    struct Frames(std::vec::IntoIter<VideoFrame>);
    impl FrameSource for Frames {
        fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
            Ok(self.0.next())
        }
    }

    #[test]
    fn hard_cut_is_detected_once() {
        let mut frames: Vec<_> = (0..30).map(|i| flat(i * 33, 20)).collect();
        frames.extend((30..60).map(|i| flat(i * 33, 220)));
        let found = detect_scenes(
            &mut Frames(frames.into_iter()),
            SceneConfig::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pts_ms, 990);
        assert!(found[0].score > 0.8);
    }

    #[test]
    fn gentle_pan_is_not_a_cut() {
        let frames: Vec<_> = (0..60).map(|i| pan(i as u64 * 33, i)).collect();
        let found = detect_scenes(
            &mut Frames(frames.into_iter()),
            SceneConfig::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn min_gap_suppresses_flashes() {
        let mut frames = vec![flat(0, 20), flat(33, 220), flat(66, 20)];
        frames.push(flat(1200, 220));
        let found = detect_scenes(
            &mut Frames(frames.into_iter()),
            SceneConfig::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        let times: Vec<_> = found.iter().map(|c| c.pts_ms).collect();
        assert_eq!(times, vec![33, 1200]);
    }

    #[test]
    fn rejects_bad_frames() {
        let mut d = SceneDetector::new(SceneConfig::default()).unwrap();
        let mut bad = flat(0, 1);
        bad.luma.pop();
        assert!(d.push(bad).is_err());
        d.push(flat(0, 1)).unwrap();
        let other = VideoFrame {
            pts_ms: 1,
            width: 8,
            height: 18,
            luma: vec![0; 144],
        };
        assert!(d.push(other).is_err());
    }

    #[test]
    fn rejects_bad_config() {
        let config = SceneConfig {
            threshold: 2.0,
            ..Default::default()
        };
        assert!(SceneDetector::new(config).is_err());
    }

    #[test]
    fn cancellation_stops_the_scan() {
        let token = CancellationToken::new();
        token.cancel();
        let err = detect_scenes(
            &mut Frames(vec![flat(0, 1)].into_iter()),
            SceneConfig::default(),
            &token,
        )
        .unwrap_err();
        assert_eq!(err, Error::Cancelled);
    }
}
