//! Chapter suggestions from pauses in speech.

use serde::{Deserialize, Serialize};

use super::Transcript;
use crate::analysis::SilenceRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterConfig {
    /// Pauses shorter than this do not start a chapter.
    pub min_gap_ms: u64,
    /// Chapters shorter than this are merged away, preferring to keep the longest pauses.
    pub min_chapter_ms: u64,
    /// Words of the following speech used as a chapter title.
    pub title_words: u32,
}

impl Default for ChapterConfig {
    fn default() -> Self {
        Self {
            min_gap_ms: 2000,
            min_chapter_ms: 60_000,
            title_words: 6,
        }
    }
}

/// A suggested chapter start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChapterSuggestion {
    pub start_ms: u64,
    /// The opening words of the chapter; empty when no speech follows it.
    pub title: String,
}

/// Suggests chapters at long silences and transcript gaps. The first chapter always starts at 0.
pub fn suggest_chapters(
    silences: &[SilenceRange],
    transcript: Option<&Transcript>,
    duration_ms: u64,
    config: &ChapterConfig,
) -> Vec<ChapterSuggestion> {
    // (start of the speech after the gap, gap length)
    let mut candidates: Vec<(u64, u64)> = silences
        .iter()
        .filter(|s| s.duration_ms() >= config.min_gap_ms)
        .map(|s| (s.end_ms, s.duration_ms()))
        .collect();
    if let Some(t) = transcript {
        for pair in t.segments.windows(2) {
            let gap = pair[1].start_ms.saturating_sub(pair[0].end_ms);
            if gap >= config.min_gap_ms {
                candidates.push((pair[1].start_ms, gap));
            }
        }
    }

    // Longest pauses claim their place first; anything too close to a claimed start is dropped.
    candidates.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut starts: Vec<u64> = vec![0];
    for (start, _) in candidates {
        let far_from_edges = start >= config.min_chapter_ms && start + config.min_chapter_ms <= duration_ms;
        if far_from_edges && starts.iter().all(|s| s.abs_diff(start) >= config.min_chapter_ms) {
            starts.push(start);
        }
    }
    starts.sort_unstable();
    starts
        .into_iter()
        .map(|start_ms| ChapterSuggestion {
            start_ms,
            title: transcript
                .map(|t| title_at(t, start_ms, config.title_words as usize))
                .unwrap_or_default(),
        })
        .collect()
}

fn title_at(transcript: &Transcript, start_ms: u64, words: usize) -> String {
    transcript
        .segments
        .iter()
        .find(|s| s.end_ms > start_ms && !s.text.trim().is_empty())
        .map(|s| {
            s.text
                .split_whitespace()
                .take(words)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::TranscriptSegment;
    use super::*;

    const MIN: u64 = 60_000;

    fn silence(start_ms: u64, end_ms: u64) -> SilenceRange {
        SilenceRange { start_ms, end_ms }
    }

    fn config() -> ChapterConfig {
        ChapterConfig::default()
    }

    fn transcript(segments: Vec<(u64, u64, &str)>) -> Transcript {
        Transcript {
            language: "ar".into(),
            segments: segments
                .into_iter()
                .map(|(start_ms, end_ms, text)| TranscriptSegment {
                    start_ms,
                    end_ms,
                    text: text.into(),
                    words: vec![],
                })
                .collect(),
        }
    }

    fn starts(chapters: &[ChapterSuggestion]) -> Vec<u64> {
        chapters.iter().map(|c| c.start_ms).collect()
    }

    #[test]
    fn long_silence_starts_a_chapter_where_speech_resumes() {
        let got = suggest_chapters(&[silence(5 * MIN, 5 * MIN + 3000)], None, 20 * MIN, &config());
        assert_eq!(starts(&got), vec![0, 5 * MIN + 3000]);
    }

    #[test]
    fn short_silences_are_ignored() {
        let got = suggest_chapters(&[silence(5 * MIN, 5 * MIN + 500)], None, 20 * MIN, &config());
        assert_eq!(starts(&got), vec![0]);
    }

    #[test]
    fn longest_pause_wins_when_two_are_close() {
        let silences = [
            silence(5 * MIN, 5 * MIN + 2500),
            silence(5 * MIN + 30_000, 5 * MIN + 39_000),
        ];
        let got = suggest_chapters(&silences, None, 20 * MIN, &config());
        assert_eq!(starts(&got), vec![0, 5 * MIN + 39_000]);
    }

    #[test]
    fn nothing_right_at_the_edges() {
        let silences = [
            silence(10_000, 15_000),
            silence(19 * MIN + 30_000, 19 * MIN + 40_000),
        ];
        let got = suggest_chapters(&silences, None, 20 * MIN, &config());
        assert_eq!(starts(&got), vec![0]);
    }

    #[test]
    fn transcript_gaps_count_and_supply_arabic_titles() {
        let t = transcript(vec![
            (0, 4 * MIN, "مقدمة المحاضرة"),
            (
                4 * MIN + 5000,
                8 * MIN,
                "الفصل الثاني عن تاريخ الأمة الإسلامية في الأندلس",
            ),
        ]);
        let got = suggest_chapters(&[], Some(&t), 20 * MIN, &config());
        assert_eq!(starts(&got), vec![0, 4 * MIN + 5000]);
        assert_eq!(got[0].title, "مقدمة المحاضرة");
        assert_eq!(got[1].title, "الفصل الثاني عن تاريخ الأمة الإسلامية");
    }

    #[test]
    fn empty_input_still_has_a_first_chapter() {
        let got = suggest_chapters(&[], None, MIN, &config());
        assert_eq!(starts(&got), vec![0]);
        assert_eq!(got[0].title, "");
    }
}
