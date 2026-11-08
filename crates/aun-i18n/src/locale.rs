//! Locale tags (a BCP 47 subset: `language[-Script][-REGION]`), fallback chains and direction.

use crate::bidi::Direction;

/// Normalises a tag: `ar_eg` → `ar-EG`, `zh-hant-tw` → `zh-Hant-TW`. Extensions (`-u-…`, `-x-…`)
/// and variants are dropped. Returns `None` for an empty or malformed language subtag.
pub fn normalize_locale(tag: &str) -> Option<String> {
    let mut parts = tag.trim().split(['-', '_']).filter(|p| !p.is_empty());
    let lang = parts.next()?;
    if !(2..=3).contains(&lang.len()) || !lang.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let mut out = lang.to_ascii_lowercase();
    let mut seen_script = false;
    let mut seen_region = false;
    for p in parts {
        if p.len() == 1 {
            break; // extension or private use: ignore the rest
        }
        if !seen_script && !seen_region && p.len() == 4 && p.chars().all(|c| c.is_ascii_alphabetic()) {
            let mut s = p.to_ascii_lowercase();
            if let Some(first) = s.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            out.push('-');
            out.push_str(&s);
            seen_script = true;
        } else if !seen_region
            && ((p.len() == 2 && p.chars().all(|c| c.is_ascii_alphabetic()))
                || (p.len() == 3 && p.chars().all(|c| c.is_ascii_digit())))
        {
            out.push('-');
            out.push_str(&p.to_ascii_uppercase());
            seen_region = true;
        }
    }
    Some(out)
}

/// The language subtag (`ar-EG` → `ar`).
pub fn language_of(tag: &str) -> String {
    tag.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase()
}

/// The lookup chain for `requested`: the tag itself, its truncations, then `fallback` (and its
/// truncations). `ar-EG` with fallback `en` → `["ar-EG", "ar", "en"]`.
pub fn fallback_chain(requested: &str, fallback: &str) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    for tag in [requested, fallback] {
        let Some(norm) = normalize_locale(tag) else {
            continue;
        };
        let parts: Vec<&str> = norm.split('-').collect();
        for n in (1..=parts.len()).rev() {
            let t = parts[..n].join("-");
            if !chain.contains(&t) {
                chain.push(t);
            }
        }
    }
    if chain.is_empty() {
        chain.push("en".into());
    }
    chain
}

/// Languages written right to left.
const RTL_LANGUAGES: &[&str] = &[
    "ar", "arc", "ckb", "dv", "fa", "he", "iw", "ks", "ku-Arab", "ps", "sd", "syr", "ug", "ur", "yi",
];

/// Writing direction of a locale. A script subtag wins (`az-Arab` is RTL, `ks-Deva` is LTR).
pub fn locale_direction(tag: &str) -> Direction {
    let norm = normalize_locale(tag).unwrap_or_default();
    let mut parts = norm.split('-');
    let lang = parts.next().unwrap_or("");
    if let Some(script) = parts.next().filter(|s| s.len() == 4) {
        return match script {
            "Arab" | "Hebr" | "Syrc" | "Thaa" | "Nkoo" | "Adlm" | "Rohg" => Direction::Rtl,
            _ => Direction::Ltr,
        };
    }
    if RTL_LANGUAGES.contains(&lang) {
        Direction::Rtl
    } else {
        Direction::Ltr
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize_locale("ar_eg").as_deref(), Some("ar-EG"));
        assert_eq!(normalize_locale("EN").as_deref(), Some("en"));
        assert_eq!(normalize_locale("zh-hant-tw").as_deref(), Some("zh-Hant-TW"));
        assert_eq!(normalize_locale("es-419").as_deref(), Some("es-419"));
        assert_eq!(normalize_locale("ar-EG-u-nu-arab").as_deref(), Some("ar-EG"));
        assert_eq!(normalize_locale(""), None);
        assert_eq!(normalize_locale("x"), None);
        assert_eq!(normalize_locale("1234"), None);
    }

    #[test]
    fn chains() {
        assert_eq!(fallback_chain("ar-EG", "en"), vec!["ar-EG", "ar", "en"]);
        assert_eq!(fallback_chain("en-GB", "en"), vec!["en-GB", "en"]);
        assert_eq!(
            fallback_chain("zh-Hant-TW", "en"),
            vec!["zh-Hant-TW", "zh-Hant", "zh", "en"]
        );
        assert_eq!(fallback_chain("", ""), vec!["en"]);
        assert_eq!(fallback_chain("ur", "en-US"), vec!["ur", "en-US", "en"]);
    }

    #[test]
    fn directions() {
        assert_eq!(locale_direction("ar-EG"), Direction::Rtl);
        assert_eq!(locale_direction("ur"), Direction::Rtl);
        assert_eq!(locale_direction("fa-IR"), Direction::Rtl);
        assert_eq!(locale_direction("en"), Direction::Ltr);
        assert_eq!(locale_direction("tr"), Direction::Ltr);
        assert_eq!(locale_direction("ms-Arab"), Direction::Rtl);
        assert_eq!(locale_direction("ks-Deva"), Direction::Ltr);
    }
}
