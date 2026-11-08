//! Bidirectional-text helpers: direction detection and Unicode isolates (UAX #9).
//!
//! Substituting user content (a channel name in Latin script) into an Arabic sentence, or an Arabic
//! title into an English one, can reorder neighbouring punctuation and numbers. Wrapping the
//! substitution in FSI … PDI (first-strong isolate) keeps it self-contained.

use serde::{Deserialize, Serialize};

/// LEFT-TO-RIGHT MARK.
pub const LRM: char = '\u{200E}';
/// RIGHT-TO-LEFT MARK.
pub const RLM: char = '\u{200F}';
/// ARABIC LETTER MARK.
pub const ALM: char = '\u{061C}';
/// LEFT-TO-RIGHT ISOLATE.
pub const LRI: char = '\u{2066}';
/// RIGHT-TO-LEFT ISOLATE.
pub const RLI: char = '\u{2067}';
/// FIRST STRONG ISOLATE.
pub const FSI: char = '\u{2068}';
/// POP DIRECTIONAL ISOLATE.
pub const PDI: char = '\u{2069}';

/// Text or paragraph direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Ltr,
    Rtl,
    /// No strong character (digits, punctuation, empty).
    Neutral,
}

/// Bidi class of a character, simplified to what direction detection needs.
fn strong_direction(c: char) -> Option<Direction> {
    let u = c as u32;
    // Arabic-Indic and extended Arabic-Indic digits are weak (AN/EN), combining marks are NSM.
    if matches!(u, 0x0660..=0x0669 | 0x06F0..=0x06F9 | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06ED) {
        return None;
    }
    // Arabic number signs and the Arabic letter mark are not strong letters either.
    if matches!(u, 0x0600..=0x0605 | 0x061C) {
        return None;
    }
    let rtl = matches!(
        u,
        0x0590..=0x08FF        // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic, Arabic ext.
            | 0xFB1D..=0xFDFF  // Hebrew + Arabic presentation forms A
            | 0xFE70..=0xFEFF  // Arabic presentation forms B
            | 0x10800..=0x10FFF
            | 0x1E800..=0x1EFFF
    );
    if rtl {
        return Some(Direction::Rtl);
    }
    if c.is_alphabetic() {
        return Some(Direction::Ltr);
    }
    None
}

/// Whether `c` is a strong right-to-left character.
pub fn is_rtl_char(c: char) -> bool {
    strong_direction(c) == Some(Direction::Rtl)
}

/// Direction of the first strong character (the UAX #9 P2/P3 rule), skipping isolates.
pub fn detect_direction(text: &str) -> Direction {
    let mut depth = 0u32;
    for c in text.chars() {
        match c {
            LRI | RLI | FSI => depth += 1,
            PDI => depth = depth.saturating_sub(1),
            _ if depth == 0 => {
                if let Some(d) = strong_direction(c) {
                    return d;
                }
            }
            _ => {}
        }
    }
    Direction::Neutral
}

/// Whether the text contains both strong LTR and strong RTL characters.
pub fn is_mixed_direction(text: &str) -> bool {
    let mut ltr = false;
    let mut rtl = false;
    for c in text.chars() {
        match strong_direction(c) {
            Some(Direction::Ltr) => ltr = true,
            Some(Direction::Rtl) => rtl = true,
            _ => {}
        }
        if ltr && rtl {
            return true;
        }
    }
    false
}

/// Wraps text in FSI … PDI (direction from its first strong character).
pub fn isolate(text: &str) -> String {
    let mut s = String::with_capacity(text.len() + 6);
    s.push(FSI);
    s.push_str(text);
    s.push(PDI);
    s
}

/// Wraps text in an isolate of a known direction (LRI/RLI … PDI); `Neutral` uses FSI.
pub fn isolate_as(text: &str, direction: Direction) -> String {
    let open = match direction {
        Direction::Ltr => LRI,
        Direction::Rtl => RLI,
        Direction::Neutral => FSI,
    };
    let mut s = String::with_capacity(text.len() + 6);
    s.push(open);
    s.push_str(text);
    s.push(PDI);
    s
}

/// Whether a substitution into a message of direction `context` needs an isolate: when it has
/// strong characters of the other direction, mixes directions, or starts with a neutral character
/// that could attach to the wrong side.
pub fn needs_isolation(text: &str, context: Direction) -> bool {
    if text.is_empty() {
        return false;
    }
    let dir = detect_direction(text);
    match dir {
        Direction::Neutral => context == Direction::Rtl && text.chars().any(|c| c.is_ascii_digit()),
        d => d != context || is_mixed_direction(text),
    }
}

/// Removes bidi control characters (marks, embeddings, overrides, isolates), e.g. before comparing
/// or storing user input.
pub fn strip_bidi_controls(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(*c, LRM | RLM | ALM | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_first_strong() {
        assert_eq!(detect_direction("مرحبا world"), Direction::Rtl);
        assert_eq!(detect_direction("hello مرحبا"), Direction::Ltr);
        assert_eq!(detect_direction("123 - ٤٥٦"), Direction::Neutral);
        assert_eq!(detect_direction("  (שלום)"), Direction::Rtl);
        assert_eq!(detect_direction(""), Direction::Neutral);
        // Text inside an isolate is skipped.
        assert_eq!(detect_direction(&format!("{FSI}abc{PDI} سلام")), Direction::Rtl);
    }

    #[test]
    fn mixed_and_isolation() {
        assert!(is_mixed_direction("Al Ummah الأمة"));
        assert!(!is_mixed_direction("الأمة الآن"));
        assert!(needs_isolation("Al Ummah", Direction::Rtl));
        assert!(!needs_isolation("الأمة", Direction::Rtl));
        assert!(needs_isolation("الأمة", Direction::Ltr));
        assert!(needs_isolation("Ali علي", Direction::Ltr));
        assert!(needs_isolation("+44 20", Direction::Rtl));
        assert!(!needs_isolation("!", Direction::Ltr));
        assert!(!needs_isolation("", Direction::Rtl));
    }

    #[test]
    fn isolates_and_strip() {
        assert_eq!(isolate("abc"), "\u{2068}abc\u{2069}");
        assert_eq!(isolate_as("abc", Direction::Ltr), "\u{2066}abc\u{2069}");
        assert_eq!(isolate_as("ب", Direction::Rtl), "\u{2067}ب\u{2069}");
        assert_eq!(
            strip_bidi_controls(&format!("{RLM}{FSI}abc{PDI}\u{202B}x\u{202C}")),
            "abcx"
        );
        assert!(is_rtl_char('ع'));
        assert!(!is_rtl_char('٣'));
        assert!(!is_rtl_char('a'));
    }
}
