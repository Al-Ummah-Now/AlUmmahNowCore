//! Search text normalisation, ported from the monorepo's
//! `packages/shared-types/src/search/normalize.ts` (Story 29.9) so that local filtering and match
//! highlighting in the apps agree with what the server indexes and matches.
//!
//! Arabic
//! - tashkeel / harakat, Quranic annotation marks and superscript alef are removed
//! - tatweel (kashida) is removed
//! - alef forms أ إ آ ٱ ٲ ٳ become bare alef ا
//! - alef maqsura ى and Farsi yeh ی become yeh ي; keheh ک becomes kaf ك
//! - taa marbuta ة becomes haa ه
//! - hamza carriers ؤ → و and ئ → ي; a standalone hamza ء is dropped
//! - Arabic-Indic (٠-٩) and Extended Arabic-Indic (۰-۹) digits become 0-9
//!
//! Latin and everything else
//! - Unicode compatibility forms are folded (NFKC: ligatures, full-width, Arabic presentation forms)
//! - lower-cased, accents removed (é → e)
//! - zero-width and bidi control characters are removed
//!
//! The output of [`normalize_search_text`] contains only letters, digits and single spaces.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

fn is_arabic_mark(c: char) -> bool {
    matches!(c,
        '\u{0610}'..='\u{061A}'
        | '\u{064B}'..='\u{065F}'
        | '\u{0670}'
        | '\u{06D6}'..='\u{06DC}'
        | '\u{06DF}'..='\u{06E8}'
        | '\u{06EA}'..='\u{06ED}'
        | '\u{08D3}'..='\u{08FF}')
}

fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        | '\u{FEFF}'
        | '\u{00AD}')
}

const TATWEEL: char = '\u{0640}';

/// Folds one character after NFKC. `None` drops it.
fn fold_arabic(c: char) -> Option<char> {
    if is_invisible(c) || is_arabic_mark(c) || c == TATWEEL {
        return None;
    }
    Some(match c {
        'أ' | 'إ' | 'آ' | 'ٱ' | 'ٲ' | 'ٳ' => 'ا',
        'ى' | 'ی' => 'ي',
        'ک' => 'ك',
        'ة' => 'ه',
        'ؤ' => 'و',
        'ئ' => 'ي',
        'ء' => return None,
        '\u{0660}'..='\u{0669}' => char::from(b'0' + (c as u32 - 0x0660) as u8),
        '\u{06F0}'..='\u{06F9}' => char::from(b'0' + (c as u32 - 0x06F0) as u8),
        other => other,
    })
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric()
}

/// Normalises Arabic letters only (no case folding or punctuation removal), so display code can
/// compare Arabic strings loosely.
pub fn normalize_arabic(text: &str) -> String {
    text.nfkc().filter_map(fold_arabic).collect()
}

/// Full search normalisation: Arabic folding, case and accent folding, punctuation to single spaces.
pub fn normalize_search_text(text: &str) -> String {
    let folded: String = normalize_arabic(text)
        .to_lowercase()
        .nfd()
        .filter(|c| !is_combining_mark(*c))
        .nfc()
        .collect();
    let mut out = String::with_capacity(folded.len());
    for word in folded.split(|c: char| !is_word_char(c)).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Splits normalised text into words.
pub fn tokenize_search_text(text: &str) -> Vec<String> {
    normalize_search_text(text)
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Arabic proclitic + definite-article prefixes, longest first.
const ARTICLE_PREFIXES: [&str; 6] = ["وال", "بال", "كال", "فال", "لل", "ال"];
/// Words where the leading ال is part of the word itself.
const ARTICLE_EXCEPTIONS: [&str; 7] = ["الله", "اللهم", "الا", "الي", "التي", "الذي", "الذين"];

/// Strips a leading Arabic definite article (ال, وال, بال, كال, فال, لل) from a normalised token,
/// keeping at least two letters. Returns the token unchanged when it has no article, is too short or
/// is an exception (e.g. الله).
pub fn strip_arabic_article(token: &str) -> &str {
    if token.is_empty() || ARTICLE_EXCEPTIONS.contains(&token) {
        return token;
    }
    for prefix in ARTICLE_PREFIXES {
        if let Some(rest) = token.strip_prefix(prefix)
            && rest.chars().count() >= 2
        {
            return rest;
        }
    }
    token
}

/// A half-open range of UTF-16 code units (`start..end`), the unit Swift `NSRange`, Kotlin and
/// C# strings index by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Utf16Range {
    pub start: u32,
    pub end: u32,
}

/// One normalised character and the UTF-16 range of the original text it came from.
struct Mapped {
    ch: char,
    source: Utf16Range,
}

/// Normalises `text` character by character, remembering where each output character came from.
/// Word boundaries become a single `' '`. It matches [`normalize_search_text`] for every script that
/// does not compose across characters (Arabic, Latin, Cyrillic, Greek, CJK).
fn normalize_mapped(text: &str) -> Vec<Mapped> {
    let mut out: Vec<Mapped> = Vec::with_capacity(text.len());
    let mut offset = 0u32;
    for c in text.chars() {
        let len = c.len_utf16() as u32;
        let source = Utf16Range {
            start: offset,
            end: offset + len,
        };
        offset += len;
        let mut buf = [0u8; 4];
        let folded_char = normalize_arabic(c.encode_utf8(&mut buf)).to_lowercase();
        if folded_char.nfd().all(is_combining_mark) {
            // Marks, tatweel and invisibles belong to the letter before them: a highlight ending on
            // that letter must cover them, or it would split a grapheme.
            if let Some(last) = out.last_mut().filter(|m| m.ch != ' ') {
                last.source.end = source.end;
            }
            continue;
        }
        for folded in folded_char.nfd() {
            if is_combining_mark(folded) {
                continue;
            }
            if is_word_char(folded) {
                out.push(Mapped { ch: folded, source });
            } else if out.last().is_some_and(|m| m.ch != ' ') {
                out.push(Mapped { ch: ' ', source });
            }
        }
    }
    if out.last().is_some_and(|m| m.ch == ' ') {
        out.pop();
    }
    out
}

/// Ranges of `text` to highlight for `query`, in UTF-16 code units, sorted and merged.
///
/// Every query word matches as a prefix of a word in `text`, under the same normalisation the
/// server uses, so `احمد` highlights `أحمد` and `cafe` highlights `Café`. A word that starts with an
/// Arabic article also matches after it: `حديث` highlights the `حديث` in `الحديث`.
pub fn highlight_ranges(text: &str, query: &str) -> Vec<Utf16Range> {
    let mut tokens = tokenize_search_text(query);
    tokens.sort();
    tokens.dedup();
    if tokens.is_empty() {
        return vec![];
    }
    let mapped = normalize_mapped(text);
    let mut ranges: Vec<Utf16Range> = vec![];
    let mut i = 0;
    while i < mapped.len() {
        let mut end = i;
        while end < mapped.len() && mapped[end].ch != ' ' {
            end += 1;
        }
        let word: String = mapped[i..end].iter().map(|m| m.ch).collect();
        let stripped = strip_arabic_article(&word);
        let skip = word.chars().count() - stripped.chars().count();
        for token in &tokens {
            let token_len = token.chars().count();
            let hit = if word.starts_with(token.as_str()) {
                Some(0)
            } else if skip > 0 && stripped.starts_with(token.as_str()) {
                Some(skip)
            } else {
                None
            };
            if let Some(from) = hit {
                let first = &mapped[i + from];
                let last = &mapped[i + from + token_len - 1];
                ranges.push(Utf16Range {
                    start: first.source.start,
                    end: last.source.end,
                });
            }
        }
        i = end + 1;
    }
    ranges.sort_by_key(|r| (r.start, r.end));
    let mut merged: Vec<Utf16Range> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    merged
}

/// Whether every word of `query` is a prefix of some word of `text` (after normalisation and
/// article stripping). Apps use it to filter recent searches and cached suggestions locally.
pub fn matches_query(text: &str, query: &str) -> bool {
    let words: Vec<String> = tokenize_search_text(text);
    let query = tokenize_search_text(query);
    !query.is_empty()
        && query.iter().all(|q| {
            words
                .iter()
                .any(|w| w.starts_with(q.as_str()) || strip_arabic_article(w).starts_with(q.as_str()))
        })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn slice(text: &str, r: Utf16Range) -> String {
        let units: Vec<u16> = text.encode_utf16().collect();
        String::from_utf16(&units[r.start as usize..r.end as usize]).unwrap()
    }

    #[test]
    fn folds_arabic_letter_variants() {
        assert_eq!(normalize_search_text("أحمد"), "احمد");
        assert_eq!(normalize_search_text("إسلام"), "اسلام");
        assert_eq!(normalize_search_text("آمنة"), "امنه");
        assert_eq!(normalize_search_text("ٱلرَّحْمَٰن"), "الرحمن");
        assert_eq!(normalize_search_text("مصطفى"), "مصطفي");
        assert_eq!(normalize_search_text("مدرسة"), "مدرسه");
        assert_eq!(normalize_search_text("مؤمن"), "مومن");
        assert_eq!(normalize_search_text("مسائل"), "مسايل");
        assert_eq!(normalize_search_text("سماء"), "سما");
        assert_eq!(normalize_search_text("کتاب"), "كتاب");
        assert_eq!(normalize_search_text("فارسی"), "فارسي");
    }

    #[test]
    fn removes_tashkeel_tatweel_and_invisibles() {
        assert_eq!(normalize_search_text("بِسْمِ اللَّهِ"), "بسم الله");
        assert_eq!(normalize_search_text("محـــمد"), "محمد");
        assert_eq!(normalize_search_text("\u{200F}سلام\u{200B}"), "سلام");
    }

    #[test]
    fn folds_digits_case_accents_and_punctuation() {
        assert_eq!(normalize_search_text("سورة ١٨"), "سوره 18");
        assert_eq!(normalize_search_text("۱۲۳"), "123");
        assert_eq!(normalize_search_text("  Café — CRÈME!  "), "cafe creme");
        assert_eq!(normalize_search_text("ﬁqh"), "fiqh");
        assert_eq!(normalize_search_text("ＡＢＣ"), "abc");
        assert_eq!(normalize_search_text("ﻻ"), "لا");
        assert_eq!(normalize_search_text(""), "");
        assert_eq!(normalize_search_text("!!!"), "");
    }

    #[test]
    fn normalize_arabic_keeps_case_and_punctuation() {
        assert_eq!(normalize_arabic("Qur'an: القُرآن"), "Qur'an: القران");
    }

    #[test]
    fn tokenizes() {
        assert_eq!(
            tokenize_search_text("  تفسير، سورة البقرة "),
            ["تفسير", "سوره", "البقره"]
        );
        assert!(tokenize_search_text("—").is_empty());
    }

    #[test]
    fn strips_articles_with_exceptions() {
        assert_eq!(strip_arabic_article("الحديث"), "حديث");
        assert_eq!(strip_arabic_article("والعلم"), "علم");
        assert_eq!(strip_arabic_article("للصلاه"), "صلاه");
        assert_eq!(strip_arabic_article("الله"), "الله");
        assert_eq!(strip_arabic_article("الذين"), "الذين");
        assert_eq!(strip_arabic_article("الم"), "الم");
        assert_eq!(strip_arabic_article("كتاب"), "كتاب");
    }

    #[test]
    fn mapped_normalisation_agrees_with_normalize_search_text() {
        for s in [
            "بِسْمِ اللَّهِ الرَّحْمَٰنِ",
            "  Café — CRÈME!  ",
            "سورة ١٨: آية",
            "ﬁqh ＡＢＣ ﻻ",
            "e\u{0301}cole",
        ] {
            let mapped: String = normalize_mapped(s).iter().map(|m| m.ch).collect();
            assert_eq!(mapped, normalize_search_text(s), "{s}");
        }
    }

    #[test]
    fn highlights_prefixes_across_spelling_variants() {
        let text = "محاضرة أحمد عن الحديث";
        let r = highlight_ranges(text, "احمد حديث");
        assert_eq!(r.len(), 2);
        assert_eq!(slice(text, r[0]), "أحمد");
        assert_eq!(slice(text, r[1]), "حديث");

        let text = "Café Talks";
        let r = highlight_ranges(text, "cafe ta");
        assert_eq!(
            r.iter().map(|r| slice(text, *r)).collect::<Vec<_>>(),
            ["Café", "Ta"]
        );
    }

    #[test]
    fn highlight_keeps_diacritics_inside_the_range() {
        let text = "بِسْمِ اللَّهِ";
        let r = highlight_ranges(text, "بسم");
        assert_eq!(slice(text, r[0]), "بِسْمِ");
    }

    #[test]
    fn highlight_counts_utf16_units() {
        let text = "🎥 Quran";
        let r = highlight_ranges(text, "qu");
        assert_eq!(r, [Utf16Range { start: 3, end: 5 }]);
    }

    #[test]
    fn highlight_merges_overlaps_and_ignores_empty_queries() {
        let text = "tafsir";
        assert_eq!(
            highlight_ranges(text, "taf tafs"),
            [Utf16Range { start: 0, end: 4 }]
        );
        assert!(highlight_ranges(text, "  ").is_empty());
        assert!(highlight_ranges(text, "x").is_empty());
    }

    #[test]
    fn matches_queries_word_by_word() {
        assert!(matches_query("تفسير سورة البقرة", "سوره بقر"));
        assert!(matches_query("Tafsir Al-Baqarah", "baq taf"));
        assert!(!matches_query("Tafsir", "fsir"));
        assert!(!matches_query("Tafsir", ""));
    }
}
