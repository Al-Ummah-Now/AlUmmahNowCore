//! Number and duration formatting basics: digit systems, grouping and decimal separators, clock and
//! compact durations. This is not full CLDR number formatting. It covers what the app UI shows (view
//! counts, file sizes, durations, percentages).

use serde::{Deserialize, Serialize};

/// Which digits to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumeralSystem {
    /// 0123456789 (the default, also for Arabic, matching the web app).
    #[default]
    Latin,
    /// Eastern Arabic ٠١٢٣٤٥٦٧٨٩ (Arabic `arab` numbering system).
    ArabicIndic,
    /// Extended Arabic-Indic ۰۱۲۳۴۵۶۷۸۹ (Persian/Urdu `arabext`).
    ExtendedArabicIndic,
}

/// Formatting options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NumberFormatOptions {
    pub min_fraction_digits: u32,
    pub max_fraction_digits: u32,
    pub grouping: bool,
}

impl Default for NumberFormatOptions {
    fn default() -> Self {
        Self {
            min_fraction_digits: 0,
            max_fraction_digits: 2,
            grouping: true,
        }
    }
}

/// Replaces ASCII digits with the digits of `system`.
pub fn to_numerals(text: &str, system: NumeralSystem) -> String {
    let base = match system {
        NumeralSystem::Latin => return text.to_owned(),
        NumeralSystem::ArabicIndic => 0x0660u32,
        NumeralSystem::ExtendedArabicIndic => 0x06F0u32,
    };
    text.chars()
        .map(|c| {
            if c.is_ascii_digit() {
                char::from_u32(base + (c as u32 - '0' as u32)).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// Converts Arabic-Indic and extended Arabic-Indic digits back to ASCII (for parsing user input).
pub fn to_latin_digits(text: &str) -> String {
    text.chars()
        .map(|c| match c as u32 {
            u @ 0x0660..=0x0669 => char::from_u32('0' as u32 + (u - 0x0660)).unwrap_or(c),
            u @ 0x06F0..=0x06F9 => char::from_u32('0' as u32 + (u - 0x06F0)).unwrap_or(c),
            _ => c,
        })
        .collect()
}

/// (group separator, decimal separator) for a locale and digit system.
pub fn separators(locale: &str, system: NumeralSystem) -> (&'static str, &'static str) {
    if system != NumeralSystem::Latin {
        return ("\u{066C}", "\u{066B}"); // ARABIC THOUSANDS / DECIMAL SEPARATOR
    }
    match crate::locale::language_of(locale).as_str() {
        "fr" => ("\u{202F}", ","),
        "tr" | "id" | "de" | "es" | "it" | "nl" | "pt" => (".", ","),
        "ru" | "uk" => ("\u{00A0}", ","),
        _ => (",", "."),
    }
}

/// Removes trailing fraction zeros and a dangling decimal point from an ASCII decimal.
pub(crate) fn trim_fraction(s: &str) -> String {
    if !s.contains('.') {
        return s.to_owned();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Formats a number for display.
pub fn format_number(value: f64, locale: &str, system: NumeralSystem, opts: NumberFormatOptions) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN".into()
        } else if value > 0.0 {
            "∞".into()
        } else {
            "-∞".into()
        };
    }
    let max = opts.max_fraction_digits.min(12) as usize;
    let min = (opts.min_fraction_digits as usize).min(max);
    let raw = format!("{:.*}", max, value.abs());
    let (int_part, frac_part) = raw.split_once('.').unwrap_or((raw.as_str(), ""));
    let mut frac = frac_part.trim_end_matches('0').to_owned();
    while frac.len() < min {
        frac.push('0');
    }
    let (group, decimal) = separators(locale, system);
    let mut out = String::new();
    let negative = value < 0.0 && (int_part.chars().any(|c| c != '0') || !frac.is_empty());
    if negative {
        out.push('-');
    }
    out.push_str(&group_digits(int_part, if opts.grouping { group } else { "" }));
    if !frac.is_empty() {
        out.push_str(decimal);
        out.push_str(&frac);
    }
    to_numerals(&out, system)
}

/// Formats an integer with grouping.
pub fn format_integer(value: i64, locale: &str, system: NumeralSystem) -> String {
    let (group, _) = separators(locale, system);
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    if value < 0 {
        out.push('-');
    }
    out.push_str(&group_digits(&digits, group));
    to_numerals(&out, system)
}

fn group_digits(digits: &str, sep: &str) -> String {
    if sep.is_empty() || digits.len() <= 3 {
        return digits.to_owned();
    }
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 * sep.len());
    let first = digits.len() % 3;
    if first > 0 {
        out.push_str(&digits[..first]);
    }
    for (i, chunk) in digits.as_bytes()[first..].chunks(3).enumerate() {
        if i > 0 || first > 0 {
            out.push_str(sep);
        }
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
    }
    out
}

/// A media-player clock: `m:ss` under an hour, `h:mm:ss` from an hour on.
pub fn format_duration_clock(duration_ms: u64, system: NumeralSystem) -> String {
    let total = duration_ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    let text = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    };
    to_numerals(&text, system)
}

/// A compact duration such as `1 h 5 min` / `١ س ٥ د` (seconds only under a minute). Units are
/// abbreviations, so no plural forms are needed; full sentences come from translation bundles.
pub fn format_duration_compact(duration_ms: u64, locale: &str, system: NumeralSystem) -> String {
    let total = duration_ms / 1000;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    let (uh, um, us) = match crate::locale::language_of(locale).as_str() {
        "ar" => ("س", "د", "ث"),
        "ur" => ("گھنٹے", "منٹ", "سیکنڈ"),
        "fr" => ("h", "min", "s"),
        "tr" => ("sa", "dk", "sn"),
        "id" | "ms" => ("j", "m", "d"),
        _ => ("h", "min", "s"),
    };
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(format!("{h} {uh}"));
    }
    if m > 0 {
        parts.push(format!("{m} {um}"));
    }
    if h == 0 && m == 0 {
        parts.push(format!("{s} {us}"));
    }
    to_numerals(&parts.join(" "), system)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits() {
        assert_eq!(
            to_numerals("0123456789", NumeralSystem::ArabicIndic),
            "٠١٢٣٤٥٦٧٨٩"
        );
        assert_eq!(to_numerals("10:05", NumeralSystem::ExtendedArabicIndic), "۱۰:۰۵");
        assert_eq!(to_latin_digits("٠١٢٣ ۴۵"), "0123 45");
        assert_eq!(to_numerals("abc 1", NumeralSystem::Latin), "abc 1");
    }

    #[test]
    fn numbers() {
        let o = NumberFormatOptions::default();
        assert_eq!(
            format_number(1234567.891, "en", NumeralSystem::Latin, o),
            "1,234,567.89"
        );
        assert_eq!(
            format_number(1234567.5, "ar", NumeralSystem::ArabicIndic, o),
            "١٬٢٣٤٬٥٦٧٫٥"
        );
        assert_eq!(format_number(1234.5, "tr", NumeralSystem::Latin, o), "1.234,5");
        assert_eq!(
            format_number(1234.5, "fr", NumeralSystem::Latin, o),
            "1\u{202F}234,5"
        );
        assert_eq!(format_number(-0.001, "en", NumeralSystem::Latin, o), "0");
        assert_eq!(format_number(-12.0, "en", NumeralSystem::Latin, o), "-12");
        let fixed = NumberFormatOptions {
            min_fraction_digits: 2,
            max_fraction_digits: 2,
            grouping: false,
        };
        assert_eq!(
            format_number(1234.5, "en", NumeralSystem::Latin, fixed),
            "1234.50"
        );
        assert_eq!(format_number(f64::NAN, "en", NumeralSystem::Latin, o), "NaN");
        assert_eq!(
            format_integer(-1_000_000, "en", NumeralSystem::Latin),
            "-1,000,000"
        );
        assert_eq!(format_integer(999, "ar", NumeralSystem::ArabicIndic), "٩٩٩");
        assert_eq!(format_integer(1000, "id", NumeralSystem::Latin), "1.000");
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration_clock(65_000, NumeralSystem::Latin), "1:05");
        assert_eq!(format_duration_clock(3_723_000, NumeralSystem::Latin), "1:02:03");
        assert_eq!(
            format_duration_clock(3_723_000, NumeralSystem::ArabicIndic),
            "١:٠٢:٠٣"
        );
        assert_eq!(format_duration_clock(0, NumeralSystem::Latin), "0:00");
        assert_eq!(
            format_duration_compact(3_900_000, "en", NumeralSystem::Latin),
            "1 h 5 min"
        );
        assert_eq!(
            format_duration_compact(3_900_000, "ar", NumeralSystem::ArabicIndic),
            "١ س ٥ د"
        );
        assert_eq!(
            format_duration_compact(42_000, "en", NumeralSystem::Latin),
            "42 s"
        );
        assert_eq!(
            format_duration_compact(3_600_000, "en", NumeralSystem::Latin),
            "1 h"
        );
    }
}
