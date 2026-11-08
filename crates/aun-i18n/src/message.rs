//! Message formatting with named arguments.
//!
//! Supported syntax, a compatible subset of what the web app's bundles use:
//! - `{{name}}`: i18next interpolation.
//! - `{name}`: ICU simple argument.
//! - `{name, number}`: a localised number.
//! - `{name, plural, =0 {…} one {# video} other {# videos}}`: ICU plural with CLDR categories and
//!   exact matches. `#` is the formatted number.
//! - `{name, select, male {…} female {…} other {…}}`: ICU select.
//!
//! i18next plural keys (`key_one`, `key_other`, …) are resolved by [`crate::I18n`] before formatting.
//! Apostrophes are plain text: there is no ICU quoting, because bundles contain natural apostrophes.
//! A placeholder whose argument is missing is left as written, so the bug is visible but nothing
//! crashes. Text arguments are wrapped in FSI…PDI when their direction differs from the message
//! locale's direction (see [`crate::bidi::needs_isolation`]).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bidi::{isolate, needs_isolation};
use crate::locale::locale_direction;
use crate::numbers::{NumberFormatOptions, NumeralSystem, format_number};
use crate::plural::{PluralCategory, PluralOperands, plural_category};

/// A named argument value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FormatArg {
    Int(i64),
    Float(f64),
    Text(String),
}

impl From<&str> for FormatArg {
    fn from(s: &str) -> Self {
        FormatArg::Text(s.to_owned())
    }
}
impl From<String> for FormatArg {
    fn from(s: String) -> Self {
        FormatArg::Text(s)
    }
}
impl From<i64> for FormatArg {
    fn from(v: i64) -> Self {
        FormatArg::Int(v)
    }
}
impl From<f64> for FormatArg {
    fn from(v: f64) -> Self {
        FormatArg::Float(v)
    }
}

impl FormatArg {
    /// Plural operands of the value as it would be displayed.
    pub fn plural_operands(&self) -> Option<PluralOperands> {
        match self {
            FormatArg::Int(v) => Some(PluralOperands::from_int(*v)),
            FormatArg::Float(v) if v.is_finite() => Some(PluralOperands::from_f64(*v, 2)),
            FormatArg::Float(_) => None,
            FormatArg::Text(s) => PluralOperands::from_decimal_str(&crate::numbers::to_latin_digits(s)),
        }
    }

    fn numeric(&self) -> Option<f64> {
        match self {
            FormatArg::Int(v) => Some(*v as f64),
            FormatArg::Float(v) => Some(*v),
            FormatArg::Text(s) => crate::numbers::to_latin_digits(s).trim().parse().ok(),
        }
    }
}

/// Locale-dependent settings for formatting.
#[derive(Debug, Clone, Copy)]
pub struct FormatContext<'a> {
    pub locale: &'a str,
    pub numerals: NumeralSystem,
    pub bidi_isolation: bool,
}

/// Formats `pattern` with `args`.
pub fn format_message(pattern: &str, args: &HashMap<String, FormatArg>, ctx: &FormatContext<'_>) -> String {
    let mut out = String::with_capacity(pattern.len() + 16);
    render(pattern, args, ctx, None, &mut out);
    out
}

fn format_value(arg: &FormatArg, ctx: &FormatContext<'_>) -> String {
    match arg {
        FormatArg::Int(v) => crate::numbers::format_integer(*v, ctx.locale, ctx.numerals),
        FormatArg::Float(v) => format_number(*v, ctx.locale, ctx.numerals, NumberFormatOptions::default()),
        FormatArg::Text(s) => {
            if ctx.bidi_isolation && needs_isolation(s, locale_direction(ctx.locale)) {
                isolate(s)
            } else {
                s.clone()
            }
        }
    }
}

/// Finds the index just past the `}` matching the `{` at `open` (byte index), honouring nesting.
fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, c) in s[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn render(
    pattern: &str,
    args: &HashMap<String, FormatArg>,
    ctx: &FormatContext<'_>,
    hash: Option<&str>,
    out: &mut String,
) {
    let mut i = 0;
    while i < pattern.len() {
        let rest = &pattern[i..];
        if rest.starts_with("{{")
            && let Some(end) = rest[2..].find("}}")
        {
            let name = rest[2..2 + end].trim();
            match args.get(name) {
                Some(v) => out.push_str(&format_value(v, ctx)),
                None => out.push_str(&rest[..end + 4]),
            }
            i += end + 4;
            continue;
        }
        let Some(c) = rest.chars().next() else {
            break;
        };
        if c == '{'
            && let Some(end) = matching_brace(pattern, i)
        {
            let inner = &pattern[i + 1..end - 1];
            if !render_argument(inner, args, ctx, out) {
                out.push_str(&pattern[i..end]);
            }
            i = end;
            continue;
        }
        if c == '#'
            && let Some(h) = hash
        {
            out.push_str(h);
            i += 1;
            continue;
        }
        out.push(c);
        i += c.len_utf8();
    }
}

/// Renders `{inner}`; returns false when it cannot (missing argument or bad syntax).
fn render_argument(
    inner: &str,
    args: &HashMap<String, FormatArg>,
    ctx: &FormatContext<'_>,
    out: &mut String,
) -> bool {
    let (name, rest) = match inner.split_once(',') {
        Some((n, r)) => (n.trim(), Some(r)),
        None => (inner.trim(), None),
    };
    if name.is_empty() || name.contains(char::is_whitespace) {
        return false;
    }
    let Some(arg) = args.get(name) else {
        return false;
    };
    let Some(rest) = rest else {
        out.push_str(&format_value(arg, ctx));
        return true;
    };
    let (kind, options) = match rest.split_once(',') {
        Some((k, o)) => (k.trim(), o),
        None => (rest.trim(), ""),
    };
    match kind {
        "number" => {
            match arg.numeric() {
                Some(v) => out.push_str(&format_number(
                    v,
                    ctx.locale,
                    ctx.numerals,
                    NumberFormatOptions::default(),
                )),
                None => out.push_str(&format_value(arg, ctx)),
            }
            true
        }
        "plural" => {
            let Some(branches) = parse_branches(options) else {
                return false;
            };
            let value = arg.numeric();
            let ops = arg.plural_operands();
            let exact = value.and_then(|v| {
                branches.iter().find(|(sel, _)| {
                    sel.strip_prefix('=').and_then(|n| n.trim().parse::<f64>().ok()) == Some(v)
                })
            });
            let category = ops
                .map(|o| plural_category(ctx.locale, o))
                .unwrap_or(PluralCategory::Other);
            let chosen = exact
                .or_else(|| branches.iter().find(|(sel, _)| *sel == category.as_str()))
                .or_else(|| branches.iter().find(|(sel, _)| *sel == "other"));
            let Some((_, body)) = chosen else {
                return false;
            };
            let hash = match arg {
                FormatArg::Text(_) => match value {
                    Some(v) => format_number(v, ctx.locale, ctx.numerals, NumberFormatOptions::default()),
                    None => format_value(arg, ctx),
                },
                _ => format_value(arg, ctx),
            };
            render(body, args, ctx, Some(&hash), out);
            true
        }
        "select" => {
            let Some(branches) = parse_branches(options) else {
                return false;
            };
            let key = match arg {
                FormatArg::Text(s) => s.clone(),
                FormatArg::Int(v) => v.to_string(),
                FormatArg::Float(v) => v.to_string(),
            };
            let chosen = branches
                .iter()
                .find(|(sel, _)| *sel == key)
                .or_else(|| branches.iter().find(|(sel, _)| *sel == "other"));
            let Some((_, body)) = chosen else {
                return false;
            };
            render(body, args, ctx, None, out);
            true
        }
        _ => false,
    }
}

/// Parses `sel1 {body1} sel2 {body2} …` (an optional `offset:N` is ignored).
fn parse_branches(s: &str) -> Option<Vec<(&str, &str)>> {
    let mut branches = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            break;
        }
        i += rest.len() - trimmed.len();
        let open = s[i..].find('{')? + i;
        let selector = s[i..open].trim();
        let close = matching_brace(s, open)?;
        if selector.starts_with("offset:") {
            // `offset:1 =0 {…}`: drop the offset token, keep the selector after it.
            let sel = selector.split_whitespace().nth(1)?;
            branches.push((sel, &s[open + 1..close - 1]));
        } else {
            if selector.is_empty() || selector.contains(char::is_whitespace) {
                return None;
            }
            branches.push((selector, &s[open + 1..close - 1]));
        }
        i = close;
    }
    if branches.is_empty() { None } else { Some(branches) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bidi::{FSI, PDI};

    fn args(pairs: &[(&str, FormatArg)]) -> HashMap<String, FormatArg> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
    }
    fn fmt(locale: &str, p: &str, a: &[(&str, FormatArg)]) -> String {
        format_message(
            p,
            &args(a),
            &FormatContext {
                locale,
                numerals: NumeralSystem::Latin,
                bidi_isolation: true,
            },
        )
    }

    #[test]
    fn interpolation_styles() {
        assert_eq!(
            fmt("en", "Hello {{name}}!", &[("name", "Sara".into())]),
            "Hello Sara!"
        );
        assert_eq!(
            fmt("en", "Hello {name}!", &[("name", "Sara".into())]),
            "Hello Sara!"
        );
        assert_eq!(fmt("en", "Hi {{ name }}", &[("name", "Sara".into())]), "Hi Sara");
        assert_eq!(fmt("en", "{n} views", &[("n", 12345i64.into())]), "12,345 views");
        assert_eq!(fmt("en", "Missing {who} {{x}}", &[]), "Missing {who} {{x}}");
        assert_eq!(fmt("en", "Unbalanced { brace", &[]), "Unbalanced { brace");
        assert_eq!(fmt("en", "Don't {a b}", &[]), "Don't {a b}");
        assert_eq!(fmt("en", "# not special", &[]), "# not special");
    }

    #[test]
    fn arabic_plural_all_forms() {
        let p = "{count, plural, zero {لا فيديوهات} one {فيديو واحد} two {فيديوهان} few {# فيديوهات} many {# فيديو} other {# فيديو}}";
        let cases = [
            (0, "لا فيديوهات"),
            (1, "فيديو واحد"),
            (2, "فيديوهان"),
            (5, "5 فيديوهات"),
            (11, "11 فيديو"),
            (100, "100 فيديو"),
        ];
        for (n, want) in cases {
            assert_eq!(fmt("ar", p, &[("count", FormatArg::Int(n))]), want, "{n}");
        }
    }

    #[test]
    fn exact_match_wins_and_nesting() {
        let p = "{n, plural, =0 {No videos} one {One video by {author}} other {# videos by {author}}}";
        assert_eq!(
            fmt("en", p, &[("n", 0i64.into()), ("author", "Ali".into())]),
            "No videos"
        );
        assert_eq!(
            fmt("en", p, &[("n", 1i64.into()), ("author", "Ali".into())]),
            "One video by Ali"
        );
        assert_eq!(
            fmt("en", p, &[("n", 1500i64.into()), ("author", "Ali".into())]),
            "1,500 videos by Ali"
        );
        assert_eq!(
            fmt("en", p, &[("n", "3".into()), ("author", "Ali".into())]),
            "3 videos by Ali"
        );
        assert_eq!(
            fmt("en", "{n, plural, one {x}}", &[("n", 5i64.into())]),
            "{n, plural, one {x}}"
        );
    }

    #[test]
    fn select_and_number() {
        let p = "{g, select, male {He} female {She} other {They}} replied";
        assert_eq!(fmt("en", p, &[("g", "female".into())]), "She replied");
        assert_eq!(fmt("en", p, &[("g", "x".into())]), "They replied");
        assert_eq!(fmt("en", "{v, number}", &[("v", 1234.5.into())]), "1,234.5");
        assert_eq!(fmt("en", "{v, number}", &[("v", "abc".into())]), "abc");
        assert_eq!(fmt("en", "{v, weird}", &[("v", 1i64.into())]), "{v, weird}");
    }

    #[test]
    fn bidi_isolation_and_eastern_digits() {
        let s = fmt("ar", "قناة {name} الآن", &[("name", "Al Ummah".into())]);
        assert_eq!(s, format!("قناة {FSI}Al Ummah{PDI} الآن"));
        let s = fmt("ar", "قناة {name}", &[("name", "الأمة".into())]);
        assert_eq!(s, "قناة الأمة");
        let s = fmt("en", "By {name}", &[("name", "علي".into())]);
        assert_eq!(s, format!("By {FSI}علي{PDI}"));
        let s = format_message(
            "{count, plural, few {# فيديوهات} other {# فيديو}}",
            &args(&[("count", 5i64.into())]),
            &FormatContext {
                locale: "ar",
                numerals: NumeralSystem::ArabicIndic,
                bidi_isolation: false,
            },
        );
        assert_eq!(s, "٥ فيديوهات");
    }

    #[test]
    fn arg_from_json() {
        let a: HashMap<String, FormatArg> = serde_json::from_str(r#"{"a":1,"b":1.5,"c":"x"}"#).unwrap();
        assert_eq!(a["a"], FormatArg::Int(1));
        assert_eq!(a["b"], FormatArg::Float(1.5));
        assert_eq!(a["c"], FormatArg::Text("x".into()));
    }
}
