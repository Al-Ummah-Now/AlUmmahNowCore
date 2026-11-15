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

/// One caption cue as the server's on-device transcript upload takes it
/// (`POST /api/videos/:id/captions/drafts/:language/transcript`, D10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Cues of at most two `max_line_chars` lines each. Longer speech is split at word timings when
/// the model gave them, otherwise in proportion to the characters.
pub fn caption_segments(transcript: &Transcript, max_line_chars: u32) -> Vec<CaptionSegment> {
    let budget = (max_line_chars.max(1) as usize) * 2;
    let mut out = Vec::new();
    for segment in cues(transcript) {
        let (start, end) = cue_times(segment);
        let text = segment.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if wrap(&text, max_line_chars).len() <= 2 {
            out.push(CaptionSegment {
                start_ms: start,
                end_ms: end,
                text,
            });
            continue;
        }
        let words: Vec<_> = segment
            .words
            .iter()
            .filter(|w| !w.text.trim().is_empty())
            .collect();
        if !words.is_empty() {
            let mut group: Vec<&super::TranscriptWord> = Vec::new();
            let mut chars = 0usize;
            let flush = |group: &mut Vec<&super::TranscriptWord>, out: &mut Vec<CaptionSegment>| {
                if let (Some(first), Some(last)) = (group.first(), group.last()) {
                    let s = first.start_ms.max(start);
                    out.push(CaptionSegment {
                        start_ms: s,
                        end_ms: last.end_ms.min(end).max(s + 1),
                        text: group.iter().map(|w| w.text.trim()).collect::<Vec<_>>().join(" "),
                    });
                }
                group.clear();
            };
            for word in words {
                let len = word.text.trim().chars().count();
                if !group.is_empty() && chars + 1 + len > budget {
                    flush(&mut group, &mut out);
                    chars = 0;
                }
                chars += if group.is_empty() { len } else { len + 1 };
                group.push(word);
            }
            flush(&mut group, &mut out);
            continue;
        }
        // No word timings: share the time out by characters
        let pieces = split_text(&text, budget);
        let total: usize = pieces.iter().map(|p| p.chars().count()).sum::<usize>().max(1);
        let span = end - start;
        let mut done = 0usize;
        for piece in pieces {
            let s = start + (span as u128 * done as u128 / total as u128) as u64;
            done += piece.chars().count();
            let e = start + (span as u128 * done as u128 / total as u128) as u64;
            out.push(CaptionSegment {
                start_ms: s,
                end_ms: e.max(s + 1),
                text: piece,
            });
        }
    }
    out
}

fn split_text(text: &str, budget: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let len = word.chars().count();
        if !current.is_empty() && current.chars().count() + 1 + len > budget {
            pieces.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
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
    #[test]
    fn caption_segments_keep_short_cues_and_split_long_ones_at_word_times() {
        let words = |list: &[(u64, u64, &str)]| {
            list.iter()
                .map(|(s, e, t)| TranscriptWord {
                    start_ms: *s,
                    end_ms: *e,
                    text: (*t).to_owned(),
                    confidence: 0.9,
                })
                .collect::<Vec<_>>()
        };
        let mut long = segment(
            0,
            9000,
            "بسم الله الرحمن الرحيم الحمد لله رب العالمين الرحمن الرحيم مالك يوم الدين",
        );
        long.words = words(&[
            (0, 500, "بسم"),
            (500, 1000, "الله"),
            (1000, 1800, "الرحمن"),
            (1800, 2600, "الرحيم"),
            (2600, 3200, "الحمد"),
            (3200, 3600, "لله"),
            (3600, 4000, "رب"),
            (4000, 5000, "العالمين"),
            (5000, 6000, "الرحمن"),
            (6000, 7000, "الرحيم"),
            (7000, 7600, "مالك"),
            (7600, 8200, "يوم"),
            (8200, 9000, "الدين"),
        ]);
        let t = transcript(
            "ar",
            vec![segment(0, 0, "  hi   there "), segment(10, 20, "   "), long],
        );
        let cues = caption_segments(&t, 20);
        assert_eq!(
            cues[0],
            CaptionSegment {
                start_ms: 0,
                end_ms: 1,
                text: "hi there".into()
            }
        );
        assert!(cues.len() > 2, "the long segment is split");
        for cue in &cues[1..] {
            assert!(cue.text.chars().count() <= 40, "{cue:?}");
            assert!(cue.end_ms > cue.start_ms);
        }
        assert_eq!(cues[1].start_ms, 0);
        assert_eq!(cues.last().unwrap().end_ms, 9000);
        let joined = cues[1..]
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, t.segments[2].text);
        let json = serde_json::to_value(&cues[0]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"startMs": 0, "endMs": 1, "text": "hi there"})
        );
    }

    #[test]
    fn caption_segments_share_time_by_characters_without_word_times() {
        let t = transcript(
            "en",
            vec![segment(1000, 5000, "aaaa bbbb cccc dddd eeee ffff gggg hhhh")],
        );
        let cues = caption_segments(&t, 5);
        assert_eq!(cues.len(), 4);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[3].end_ms, 5000);
        assert!(cues.windows(2).all(|w| w[0].end_ms == w[1].start_ms));
    }
}
