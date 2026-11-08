//! WebVTT and SubRip writers.

use serde::{Deserialize, Serialize};

use super::{Transcript, TranscriptSegment};

const RLM: char = '\u{200F}';

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleOptions {
    /// Wraps cue text at this many characters.
    pub max_line_chars: u32,
    /// WebVTT only: adds `<00:00:01.000>` tags so players can highlight word by word.
    pub word_timestamps: bool,
}

impl Default for SubtitleOptions {
    fn default() -> Self {
        Self {
            max_line_chars: 42,
            word_timestamps: false,
        }
    }
}

/// Renders a transcript as a WebVTT document.
pub fn to_webvtt(transcript: &Transcript, options: &SubtitleOptions) -> String {
    let mut out = String::from("WEBVTT\n");
    if !transcript.language.is_empty() {
        out.push_str(&format!("Language: {}\n", transcript.language));
    }
    out.push('\n');
    for segment in cues(transcript) {
        let (start, end) = cue_times(segment);
        out.push_str(&format!("{} --> {}\n", vtt_time(start), vtt_time(end)));
        if options.word_timestamps && !segment.words.is_empty() {
            out.push_str(&word_timed_line(segment));
        } else {
            for line in wrap(&segment.text, options.max_line_chars) {
                out.push_str(&mark_rtl(&escape_vtt(&line)));
                out.push('\n');
            }
        }
        out.push('\n');
    }
    out
}

/// Renders a transcript as SubRip.
pub fn to_srt(transcript: &Transcript, options: &SubtitleOptions) -> String {
    let mut out = String::new();
    for (index, segment) in cues(transcript).enumerate() {
        let (start, end) = cue_times(segment);
        out.push_str(&format!(
            "{}\n{} --> {}\n",
            index + 1,
            srt_time(start),
            srt_time(end)
        ));
        for line in wrap(&segment.text, options.max_line_chars) {
            out.push_str(&mark_rtl(&line));
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

fn cues(transcript: &Transcript) -> impl Iterator<Item = &TranscriptSegment> {
    transcript.segments.iter().filter(|s| !s.text.trim().is_empty())
}

/// A cue must have positive length to be valid.
fn cue_times(segment: &TranscriptSegment) -> (u64, u64) {
    (segment.start_ms, segment.end_ms.max(segment.start_ms + 1))
}

fn clock(ms: u64) -> (u64, u64, u64, u64) {
    (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

fn vtt_time(ms: u64) -> String {
    let (h, m, s, f) = clock(ms);
    format!("{h:02}:{m:02}:{s:02}.{f:03}")
}

fn srt_time(ms: u64) -> String {
    let (h, m, s, f) = clock(ms);
    format!("{h:02}:{m:02}:{s:02},{f:03}")
}

fn escape_vtt(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Whether the first strong character is right-to-left.
fn is_rtl(text: &str) -> bool {
    text.chars()
        .find(|c| c.is_alphabetic())
        .is_some_and(|c| matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF))
}

/// Prefixes RTL lines with a right-to-left mark, so players that infer direction from the first
/// character, or start a line with punctuation or digits, still lay the line out correctly.
fn mark_rtl(line: &str) -> String {
    if is_rtl(line) {
        format!("{RLM}{line}")
    } else {
        line.to_owned()
    }
}

fn word_timed_line(segment: &TranscriptSegment) -> String {
    let mut line = String::new();
    for (index, word) in segment.words.iter().enumerate() {
        if index > 0 {
            line.push_str(&format!(" <{}>", vtt_time(word.start_ms)));
        }
        line.push_str(&escape_vtt(word.text.trim()));
    }
    format!("{}\n", mark_rtl(&line))
}

fn wrap(text: &str, max_chars: u32) -> Vec<String> {
    let max_chars = max_chars.max(1) as usize;
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if count > 0 && count + 1 + len > max_chars {
            lines.push(std::mem::take(&mut current));
            count = 0;
        }
        if count > 0 {
            current.push(' ');
            count += 1;
        }
        current.push_str(word);
        count += len;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::TranscriptWord;
    use super::*;

    fn segment(start_ms: u64, end_ms: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            start_ms,
            end_ms,
            text: text.to_owned(),
            words: vec![],
        }
    }

    fn transcript(language: &str, segments: Vec<TranscriptSegment>) -> Transcript {
        Transcript {
            language: language.to_owned(),
            segments,
        }
    }

    #[test]
    fn webvtt_structure_and_escaping() {
        let t = transcript("en", vec![segment(1500, 4000, "Fish & chips <b>now</b>")]);
        assert_eq!(
            to_webvtt(&t, &SubtitleOptions::default()),
            "WEBVTT\nLanguage: en\n\n00:00:01.500 --> 00:00:04.000\nFish &amp; chips &lt;b&gt;now&lt;/b&gt;\n\n"
        );
    }

    #[test]
    fn srt_structure_and_numbering() {
        let t = transcript(
            "en",
            vec![segment(0, 1000, "One"), segment(3_661_001, 3_662_000, "Two")],
        );
        assert_eq!(
            to_srt(&t, &SubtitleOptions::default()),
            "1\n00:00:00,000 --> 00:00:01,000\nOne\n\n2\n01:01:01,001 --> 01:01:02,000\nTwo\n\n"
        );
    }

    #[test]
    fn arabic_lines_get_a_direction_mark_and_keep_their_text() {
        let text = "بسم الله الرحمن الرحيم";
        let t = transcript("ar", vec![segment(0, 2000, text)]);
        let vtt = to_webvtt(&t, &SubtitleOptions::default());
        assert!(vtt.contains(&format!("{RLM}{text}\n")), "{vtt}");
        assert!(vtt.contains("Language: ar\n"));
        let srt = to_srt(&t, &SubtitleOptions::default());
        assert!(srt.contains(&format!("{RLM}{text}\n")), "{srt}");
    }

    #[test]
    fn mixed_line_direction_follows_the_first_strong_character() {
        let t = transcript("ar", vec![segment(0, 1000, "2024 سنة Hello")]);
        let vtt = to_webvtt(&t, &SubtitleOptions::default());
        assert!(vtt.contains(&format!("{RLM}2024")), "{vtt}");
        let t = transcript("en", vec![segment(0, 1000, "Hello مرحبا")]);
        assert!(!to_webvtt(&t, &SubtitleOptions::default()).contains(RLM));
    }

    #[test]
    fn wraps_by_characters_not_bytes() {
        let text = "مرحبا بكم في هذه المحاضرة الجديدة عن تاريخ الأمة الإسلامية";
        let lines = wrap(text, 30);
        assert!(lines.len() >= 2);
        assert!(lines.iter().all(|l| l.chars().count() <= 30), "{lines:?}");
        assert_eq!(lines.join(" "), text);
    }

    #[test]
    fn an_overlong_word_is_not_split() {
        assert_eq!(
            wrap("short verylongwordhere x", 5),
            vec!["short", "verylongwordhere", "x"]
        );
    }

    #[test]
    fn empty_segments_are_skipped_and_numbering_stays_contiguous() {
        let t = transcript(
            "en",
            vec![segment(0, 10, "a"), segment(20, 30, "   "), segment(40, 50, "b")],
        );
        let srt = to_srt(&t, &SubtitleOptions::default());
        assert!(srt.contains("2\n00:00:00,040"), "{srt}");
        assert!(!srt.contains("3\n"));
    }

    #[test]
    fn zero_length_cues_are_made_valid() {
        let t = transcript("en", vec![segment(100, 100, "x")]);
        assert!(to_srt(&t, &SubtitleOptions::default()).contains("00:00:00,100 --> 00:00:00,101"));
    }

    #[test]
    fn word_timestamps_tag_each_following_word() {
        let word = |start_ms, end_ms, text: &str| TranscriptWord {
            start_ms,
            end_ms,
            text: text.to_owned(),
            confidence: 0.9,
        };
        let mut s = segment(0, 2000, "مرحبا بكم");
        s.words = vec![word(0, 900, " مرحبا"), word(1000, 2000, " بكم")];
        let t = transcript("ar", vec![s]);
        let options = SubtitleOptions {
            word_timestamps: true,
            ..Default::default()
        };
        let vtt = to_webvtt(&t, &options);
        assert!(vtt.contains(&format!("{RLM}مرحبا <00:00:01.000>بكم\n")), "{vtt}");
    }

    #[test]
    fn empty_transcript_is_a_valid_document() {
        let t = transcript("", vec![]);
        assert_eq!(to_webvtt(&t, &SubtitleOptions::default()), "WEBVTT\n\n");
        assert_eq!(to_srt(&t, &SubtitleOptions::default()), "");
    }
}
