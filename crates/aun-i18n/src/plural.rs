//! CLDR cardinal plural rules (CLDR 46) for the app's current and planned locales.
//!
//! Operands follow UTS #35: `n` absolute value, `i` integer digits, `v` number of visible fraction
//! digits, `f` visible fraction digits as an integer, `e` compact exponent (always 0 here).

use serde::{Deserialize, Serialize};

/// A CLDR plural category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl PluralCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            PluralCategory::Zero => "zero",
            PluralCategory::One => "one",
            PluralCategory::Two => "two",
            PluralCategory::Few => "few",
            PluralCategory::Many => "many",
            PluralCategory::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "zero" => PluralCategory::Zero,
            "one" => PluralCategory::One,
            "two" => PluralCategory::Two,
            "few" => PluralCategory::Few,
            "many" => PluralCategory::Many,
            "other" => PluralCategory::Other,
            _ => return None,
        })
    }
}

/// Plural operands of a number as displayed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PluralOperands {
    pub n: f64,
    pub i: u64,
    pub v: u32,
    pub f: u64,
}

impl PluralOperands {
    pub fn from_int(value: i64) -> Self {
        let i = value.unsigned_abs();
        Self {
            n: i as f64,
            i,
            v: 0,
            f: 0,
        }
    }

    /// Operands of a decimal string as shown to the user (`"1.50"` has v = 2). Returns `None` for
    /// anything that is not a plain decimal.
    pub fn from_decimal_str(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches(['-', '+']);
        let (int_part, frac) = match s.split_once('.') {
            Some((a, b)) => (a, b),
            None => (s, ""),
        };
        if int_part.is_empty() && frac.is_empty() {
            return None;
        }
        if !int_part.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let i: u64 = if int_part.is_empty() {
            0
        } else {
            int_part.parse().ok()?
        };
        let v = frac.len() as u32;
        let f: u64 = if frac.is_empty() { 0 } else { frac.parse().ok()? };
        let n: f64 = format!(
            "{}.{}",
            if int_part.is_empty() { "0" } else { int_part },
            if frac.is_empty() { "0" } else { frac }
        )
        .parse()
        .ok()?;
        Some(Self { n, i, v, f })
    }

    /// Operands of a float formatted with at most `max_fraction_digits` digits (trailing zeros
    /// removed), which is how [`crate::numbers`] displays it.
    pub fn from_f64(value: f64, max_fraction_digits: usize) -> Self {
        let text = crate::numbers::trim_fraction(&format!("{:.*}", max_fraction_digits, value.abs()));
        Self::from_decimal_str(&text).unwrap_or(Self {
            n: value.abs(),
            i: value.abs() as u64,
            v: 0,
            f: 0,
        })
    }

    fn is_int(&self) -> bool {
        self.v == 0
    }
}

/// The plural category of `ops` in `locale` (any tag; only the language matters).
pub fn plural_category(locale: &str, ops: PluralOperands) -> PluralCategory {
    use PluralCategory::*;
    let lang = crate::locale::language_of(locale);
    let PluralOperands { n, i, v, .. } = ops;
    let n_int = ops.is_int() && n.fract() == 0.0;
    match lang.as_str() {
        // zero: n = 0; one: n = 1; two: n = 2; few: n % 100 = 3..10; many: n % 100 = 11..99
        "ar" | "ars" => {
            if !n_int {
                return Other;
            }
            let m = i % 100;
            match i {
                0 => Zero,
                1 => One,
                2 => Two,
                _ if (3..=10).contains(&m) => Few,
                _ if (11..=99).contains(&m) => Many,
                _ => Other,
            }
        }
        // one: i = 1 and v = 0
        "en" | "ur" | "de" | "nl" | "sv" | "it" | "fi" | "et" | "ca" | "gl" => {
            if i == 1 && v == 0 {
                One
            } else {
                Other
            }
        }
        // one: n = 1
        "tr" | "az" | "uz" | "ka" | "kk" | "ky" | "sw" | "so" | "ha" | "ps" | "ug" | "ku" | "ckb" => {
            if n_int && i == 1 { One } else { Other }
        }
        // one: i = 0,1; many: e = 0 and i != 0 and i % 1000000 = 0 and v = 0
        "fr" => {
            if i == 0 || i == 1 {
                One
            } else if v == 0 && i != 0 && i % 1_000_000 == 0 {
                Many
            } else {
                Other
            }
        }
        // one: n = 1; many: i != 0 and i % 1000000 = 0 and v = 0
        "es" => {
            if n_int && i == 1 {
                One
            } else if v == 0 && i != 0 && i % 1_000_000 == 0 {
                Many
            } else {
                Other
            }
        }
        // one: i = 0 or n = 1
        "fa" | "hi" | "bn" | "gu" | "kn" | "mr" | "zu" | "am" => {
            if i == 0 || (n_int && i == 1) {
                One
            } else {
                Other
            }
        }
        // one: i = 0..1 (pt in CLDR 46 also has `many` like es)
        "pt" => {
            if i <= 1 {
                One
            } else if v == 0 && i != 0 && i % 1_000_000 == 0 {
                Many
            } else {
                Other
            }
        }
        // one: i % 10 = 1 and i % 100 != 11; few: i % 10 = 2..4 and i % 100 != 12..14; many: other ints
        "ru" | "uk" | "be" => {
            if v != 0 {
                return Other;
            }
            let (m10, m100) = (i % 10, i % 100);
            if m10 == 1 && m100 != 11 {
                One
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                Few
            } else {
                Many
            }
        }
        // no plural distinctions
        "id" | "ms" | "ja" | "zh" | "ko" | "th" | "vi" | "my" | "jv" | "su" | "yo" => Other,
        // Unknown languages: the English rule is the least surprising default.
        _ => {
            if i == 1 && v == 0 {
                One
            } else {
                Other
            }
        }
    }
}

/// The categories a locale uses, in CLDR order (useful for translators and validation).
pub fn plural_categories(locale: &str) -> Vec<PluralCategory> {
    use PluralCategory::*;
    let probes: [&str; 14] = [
        "0", "1", "2", "3", "11", "100", "1000000", "0.5", "1.5", "5", "21", "22", "25", "101",
    ];
    let mut seen = Vec::new();
    for p in probes {
        if let Some(ops) = PluralOperands::from_decimal_str(p) {
            let c = plural_category(locale, ops);
            if !seen.contains(&c) {
                seen.push(c);
            }
        }
    }
    let order = [Zero, One, Two, Few, Many, Other];
    order.into_iter().filter(|c| seen.contains(c)).collect()
}

#[cfg(test)]
mod tests {
    use super::PluralCategory::*;
    use super::*;

    fn cat(locale: &str, n: i64) -> PluralCategory {
        plural_category(locale, PluralOperands::from_int(n))
    }
    fn catd(locale: &str, s: &str) -> PluralCategory {
        plural_category(locale, PluralOperands::from_decimal_str(s).unwrap())
    }

    #[test]
    fn arabic_six_forms() {
        assert_eq!(cat("ar", 0), Zero);
        assert_eq!(cat("ar-EG", 1), One);
        assert_eq!(cat("ar", 2), Two);
        for n in [3, 7, 10, 103, 110, 1003] {
            assert_eq!(cat("ar", n), Few, "{n}");
        }
        for n in [11, 26, 99, 111, 1011] {
            assert_eq!(cat("ar", n), Many, "{n}");
        }
        for n in [100, 101, 102, 200, 1000, 100_000] {
            assert_eq!(cat("ar", n), Other, "{n}");
        }
        assert_eq!(catd("ar", "1.5"), Other);
        assert_eq!(
            catd("ar", "0.0"),
            Other,
            "visible fraction digits are not integers"
        );
        assert_eq!(cat("ar", -3), Few);
    }

    #[test]
    fn english_urdu() {
        for l in ["en", "ur"] {
            assert_eq!(cat(l, 1), One);
            assert_eq!(cat(l, 0), Other);
            assert_eq!(cat(l, 2), Other);
            assert_eq!(catd(l, "1.0"), Other);
        }
    }

    #[test]
    fn turkish_indonesian_malay() {
        assert_eq!(cat("tr", 1), One);
        assert_eq!(cat("tr", 2), Other);
        assert_eq!(catd("tr", "1.0"), Other);
        for l in ["id", "ms"] {
            for n in [0, 1, 2, 5, 100] {
                assert_eq!(cat(l, n), Other);
            }
        }
    }

    #[test]
    fn french() {
        assert_eq!(cat("fr", 0), One);
        assert_eq!(cat("fr", 1), One);
        assert_eq!(catd("fr", "1.5"), One);
        assert_eq!(cat("fr", 2), Other);
        assert_eq!(cat("fr", 1_000_000), Many);
        assert_eq!(cat("fr", 2_000_000), Many);
        assert_eq!(cat("fr", 1_000_001), Other);
    }

    #[test]
    fn others() {
        assert_eq!(cat("fa", 0), One);
        assert_eq!(cat("ru", 21), One);
        assert_eq!(cat("ru", 22), Few);
        assert_eq!(cat("ru", 25), Many);
        assert_eq!(cat("xx", 1), One);
    }

    #[test]
    fn categories_per_locale() {
        assert_eq!(plural_categories("ar"), vec![Zero, One, Two, Few, Many, Other]);
        assert_eq!(plural_categories("en"), vec![One, Other]);
        assert_eq!(plural_categories("id"), vec![Other]);
        assert_eq!(plural_categories("fr"), vec![One, Many, Other]);
    }

    #[test]
    fn operands() {
        let o = PluralOperands::from_decimal_str("1.50").unwrap();
        assert_eq!((o.i, o.v, o.f), (1, 2, 50));
        assert!(PluralOperands::from_decimal_str("abc").is_none());
        assert!(PluralOperands::from_decimal_str("").is_none());
        let o = PluralOperands::from_f64(2.0, 2);
        assert_eq!((o.i, o.v), (2, 0));
        let o = PluralOperands::from_f64(2.5, 2);
        assert_eq!((o.i, o.v, o.f), (2, 1, 5));
        assert_eq!(PluralCategory::parse("few"), Some(Few));
        assert_eq!(Few.as_str(), "few");
        assert_eq!(PluralCategory::parse("lots"), None);
    }
}
