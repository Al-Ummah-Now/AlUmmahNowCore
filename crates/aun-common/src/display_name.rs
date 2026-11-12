//! The one rule for the name a user is shown by, everywhere (owner rule 2026-11-12: "display name
//! should be the first and last as a fallback"). Mirrors `resolveDisplayName` in the web repo
//! (`packages/shared-types/src/display-name.ts`).
//!
//! Order:
//! 1. display name in `locale` (`displayNameLocalized[locale]`)
//! 2. plain display name
//! 3. first + last name (in `locale` when localized, else the plain ones), whichever of the two
//!    exists
//! 4. a display name, or first + last, in another language (an Arabic-only profile still has a
//!    name on an English screen)
//!
//! No `@handle`: handles belong to channels (`Channel.handle`), not to users. Where a screen shows
//! who is acting as a channel, it shows the channel's own name / `@handle`, separately.
//!
//! Empty and whitespace-only values count as missing. It never returns the email: a caller that
//! has nothing else decides on email itself. Returns `""` when the profile has no name at all.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// The name fields of a profile, as the API sends them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NameFields {
    pub display_name: Option<String>,
    pub display_name_localized: BTreeMap<String, String>,
    pub first_name: Option<String>,
    pub first_name_localized: BTreeMap<String, String>,
    pub last_name: Option<String>,
    pub last_name_localized: BTreeMap<String, String>,
}

/// Collapses runs of whitespace to one space and trims; `None` → `""`.
fn clean(value: Option<&str>) -> String {
    value
        .unwrap_or_default()
        .split(|c: char| c.is_whitespace() || c == '\u{feff}')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn base(locale: &str) -> &str {
    locale.split('-').next().unwrap_or(locale)
}

fn localized(map: &BTreeMap<String, String>, locale: Option<&str>) -> String {
    let Some(locale) = locale.filter(|l| !l.is_empty()) else {
        return String::new();
    };
    let exact = clean(map.get(locale).map(String::as_str));
    if exact.is_empty() {
        clean(map.get(base(locale)).map(String::as_str))
    } else {
        exact
    }
}

fn full_name(first: &str, last: &str) -> String {
    [first, last]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn or_else(first: String, next: impl FnOnce() -> String) -> String {
    if first.is_empty() { next() } else { first }
}

fn rank(key: &str) -> u8 {
    match key {
        "en" => 0,
        "ar" => 1,
        _ => 2,
    }
}

/// Languages other than `locale` that a localized map holds a key for, in a stable order (en, ar,
/// then the rest alphabetically).
fn other_locales(profile: &NameFields, locale: Option<&str>) -> Vec<String> {
    let locale = locale.filter(|l| !l.is_empty());
    let base = locale.map(base);
    let keys: BTreeSet<&String> = profile
        .display_name_localized
        .keys()
        .chain(profile.first_name_localized.keys())
        .chain(profile.last_name_localized.keys())
        .collect();
    let mut out: Vec<String> = keys
        .into_iter()
        .filter(|k| Some(k.as_str()) != locale && Some(k.as_str()) != base)
        .cloned()
        .collect();
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
    out
}

/// The name a user is shown by (see the module docs); `""` when there is none. Never the email.
pub fn resolve_display_name(profile: &NameFields, locale: Option<&str>) -> String {
    let display = or_else(localized(&profile.display_name_localized, locale), || {
        clean(profile.display_name.as_deref())
    });
    if !display.is_empty() {
        return display;
    }
    let first = or_else(localized(&profile.first_name_localized, locale), || {
        clean(profile.first_name.as_deref())
    });
    let last = or_else(localized(&profile.last_name_localized, locale), || {
        clean(profile.last_name.as_deref())
    });
    let full = full_name(&first, &last);
    if !full.is_empty() {
        return full;
    }
    for other in other_locales(profile, locale) {
        let other = Some(other.as_str());
        let name = or_else(localized(&profile.display_name_localized, other), || {
            full_name(
                &localized(&profile.first_name_localized, other),
                &localized(&profile.last_name_localized, other),
            )
        });
        if !name.is_empty() {
            return name;
        }
    }
    String::new()
}

fn string_map(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

impl NameFields {
    /// Reads the name fields from a profile-like JSON object (`displayName`,
    /// `displayNameLocalized`, `firstName`, `firstNameLocalized`, `lastName`,
    /// `lastNameLocalized`). Other keys, such as `userName` or `email`, are ignored.
    pub fn from_json(value: &Value) -> Self {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            display_name: text("displayName"),
            display_name_localized: string_map(value.get("displayNameLocalized")),
            first_name: text("firstName"),
            first_name_localized: string_map(value.get("firstNameLocalized")),
            last_name: text("lastName"),
            last_name_localized: string_map(value.get("lastNameLocalized")),
        }
    }
}

/// The name for a profile JSON object from the API: the server's `resolvedDisplayName` when it is
/// set, else [`resolve_display_name`] over the object's name fields.
pub fn resolve_display_name_json(value: &Value, locale: Option<&str>) -> String {
    let resolved = clean(value.get("resolvedDisplayName").and_then(Value::as_str));
    if !resolved.is_empty() {
        return resolved;
    }
    resolve_display_name(&NameFields::from_json(value), locale)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn s(v: &str) -> Option<String> {
        Some(v.to_owned())
    }

    #[test]
    fn localized_display_name_wins() {
        let p = NameFields {
            display_name: s("Plain"),
            display_name_localized: map(&[("en", "Sara"), ("ar", "سارة")]),
            first_name: s("First"),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&p, Some("ar")), "سارة");
        assert_eq!(resolve_display_name(&p, Some("en")), "Sara");
        assert_eq!(resolve_display_name(&p, Some("en-GB")), "Sara");
        assert_eq!(resolve_display_name(&p, Some("fr")), "Plain");
        assert_eq!(resolve_display_name(&p, None), "Plain");
    }

    #[test]
    fn plain_display_name_before_first_last() {
        let p = NameFields {
            display_name: s("  Sara   Ali "),
            first_name: s("First"),
            last_name: s("Last"),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&p, Some("en")), "Sara Ali");
    }

    #[test]
    fn empty_and_whitespace_display_names_are_missing() {
        for blank in ["", "   ", "\t\n", "\u{a0}"] {
            let p = NameFields {
                display_name: s(blank),
                display_name_localized: map(&[("en", blank)]),
                first_name: s("Sara"),
                last_name: s("Ali"),
                ..Default::default()
            };
            assert_eq!(resolve_display_name(&p, Some("en")), "Sara Ali", "{blank:?}");
        }
    }

    #[test]
    fn first_and_last_whichever_exists() {
        let first_only = NameFields {
            first_name: s("Sara"),
            last_name: s("  "),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&first_only, Some("en")), "Sara");
        let last_only = NameFields {
            first_name: s(""),
            last_name: s("Ali"),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&last_only, Some("en")), "Ali");
    }

    #[test]
    fn localized_first_last_preferred_per_part() {
        let p = NameFields {
            first_name: s("Sara"),
            first_name_localized: map(&[("ar", "سارة")]),
            last_name: s("Ali"),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&p, Some("ar")), "سارة Ali");
        assert_eq!(resolve_display_name(&p, Some("en")), "Sara Ali");
    }

    #[test]
    fn arabic_only_profile_viewed_in_english() {
        let display = NameFields {
            display_name_localized: map(&[("ar", "سارة علي")]),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&display, Some("en")), "سارة علي");
        let first_last = NameFields {
            first_name_localized: map(&[("ar", "سارة")]),
            last_name_localized: map(&[("ar", "علي")]),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&first_last, Some("en")), "سارة علي");
        assert_eq!(resolve_display_name(&first_last, None), "سارة علي");
    }

    #[test]
    fn other_languages_in_order_en_ar_then_rest() {
        let p = NameFields {
            display_name_localized: map(&[("fr", "Sarah"), ("ar", "سارة"), ("en", "Sara")]),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&p, Some("ur")), "Sara");
        assert_eq!(resolve_display_name(&p, Some("en")), "Sara");
        let no_en = NameFields {
            display_name_localized: map(&[("fr", "Sarah"), ("ar", "سارة"), ("de", "  ")]),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&no_en, Some("ur")), "سارة");
    }

    #[test]
    fn handle_only_has_no_name() {
        // Handles belong to channels, not users: a user with only a handle has no name.
        let v = json!({"userName": "sara", "handle": "@sara", "email": "sara@example.com"});
        assert_eq!(resolve_display_name_json(&v, Some("en")), "");
    }

    #[test]
    fn nothing_gives_empty() {
        assert_eq!(resolve_display_name(&NameFields::default(), Some("en")), "");
        let blanks = NameFields {
            display_name: s(" "),
            display_name_localized: map(&[("ar", " ")]),
            first_name: s(""),
            last_name: s("\t"),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(&blanks, Some("en")), "");
        assert_eq!(resolve_display_name(&blanks, None), "");
    }

    #[test]
    fn json_prefers_server_resolved_name() {
        let v = json!({"resolvedDisplayName": "Server Name", "displayName": "Local", "email": "a@b.co"});
        assert_eq!(resolve_display_name_json(&v, Some("en")), "Server Name");
        let blank = json!({"resolvedDisplayName": "  ", "firstName": "Sara", "lastName": "Ali"});
        assert_eq!(resolve_display_name_json(&blank, Some("en")), "Sara Ali");
    }

    #[test]
    fn json_reads_name_fields_and_never_email() {
        let v = json!({
            "displayName": null,
            "displayNameLocalized": {"ar": "سارة", "en": null},
            "email": "sara@example.com",
            "userName": "sara"
        });
        assert_eq!(resolve_display_name_json(&v, Some("en")), "سارة");
        let email_only = json!({"email": "sara@example.com"});
        assert_eq!(resolve_display_name_json(&email_only, Some("en")), "");
        assert_eq!(resolve_display_name_json(&json!("not an object"), None), "");
    }
}
