//! FFI facade for the platform display-name rule (`aun_common::display_name`).

use std::collections::{BTreeMap, HashMap};

use aun_common::display_name as rule;

/// The name fields of a profile, as the API sends them. Maps are keyed by language (`en`, `ar`).
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct NameFields {
    #[uniffi(default = None)]
    pub display_name: Option<String>,
    #[uniffi(default)]
    pub display_name_localized: HashMap<String, String>,
    #[uniffi(default = None)]
    pub first_name: Option<String>,
    #[uniffi(default)]
    pub first_name_localized: HashMap<String, String>,
    #[uniffi(default = None)]
    pub last_name: Option<String>,
    #[uniffi(default)]
    pub last_name_localized: HashMap<String, String>,
}

fn sorted(map: HashMap<String, String>) -> BTreeMap<String, String> {
    map.into_iter().collect()
}

impl From<NameFields> for rule::NameFields {
    fn from(n: NameFields) -> Self {
        Self {
            display_name: n.display_name,
            display_name_localized: sorted(n.display_name_localized),
            first_name: n.first_name,
            first_name_localized: sorted(n.first_name_localized),
            last_name: n.last_name,
            last_name_localized: sorted(n.last_name_localized),
        }
    }
}

/// The name a user is shown by, everywhere: display name in `locale`, plain display name, first +
/// last (whichever exists), then a name in another language. No `@handle` (handles belong to
/// channels). Blank values count as missing. Never the email; `""` when there is no name (the
/// caller may then show the email).
#[uniffi::export]
pub fn resolve_display_name(fields: NameFields, locale: Option<String>) -> String {
    rule::resolve_display_name(&fields.into(), locale.as_deref())
}

/// The name for one profile JSON object from the API (e.g. `result.profile`, a comment author):
/// the server's `resolvedDisplayName` when set, else [`resolve_display_name`] over its
/// `displayName`/`displayNameLocalized`/`firstName`/`lastName` (and localized) fields. `""` when the text is not
/// JSON or holds no name.
#[uniffi::export]
pub fn resolve_display_name_json(profile_json: String, locale: Option<String>) -> String {
    serde_json::from_str::<serde_json::Value>(&profile_json)
        .map(|v| rule::resolve_display_name_json(&v, locale.as_deref()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffi_rule_matches_core() {
        let fields = NameFields {
            first_name_localized: HashMap::from([("ar".to_owned(), "سارة".to_owned())]),
            last_name_localized: HashMap::from([("ar".to_owned(), "علي".to_owned())]),
            ..Default::default()
        };
        assert_eq!(resolve_display_name(fields, Some("en".into())), "سارة علي");
        assert_eq!(resolve_display_name(NameFields::default(), None), "");
        assert_eq!(
            resolve_display_name_json(
                r#"{"resolvedDisplayName":"Sara Ali","firstName":"x"}"#.into(),
                None
            ),
            "Sara Ali"
        );
        assert_eq!(
            resolve_display_name_json(r#"{"userName":"sara"}"#.into(), None),
            ""
        );
        assert_eq!(resolve_display_name_json("not json".into(), None), "");
    }
}
