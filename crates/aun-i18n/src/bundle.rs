//! Translation bundles: the web app's nested JSON flattened to dotted keys.

use std::collections::HashMap;

use serde_json::Value;

/// One locale's messages.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Bundle {
    pub locale: String,
    pub version: String,
    messages: HashMap<String, String>,
}

impl Bundle {
    /// Builds a bundle from the `translations` JSON of `GET /api/i18n/translations/{locale}` (or an
    /// embedded file of the same shape). Nested objects become dotted keys (`player.speed`), arrays
    /// use indices (`tips.0`), and numbers/booleans become text. `null` is skipped.
    pub fn from_json(locale: &str, version: &str, translations: &Value) -> Self {
        let mut messages = HashMap::new();
        flatten("", translations, &mut messages);
        Self {
            locale: locale.to_owned(),
            version: version.to_owned(),
            messages,
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.messages.get(key).map(String::as_str)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.messages.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Sorted keys (for tooling and missing-key reports).
    pub fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self.messages.keys().cloned().collect();
        k.sort();
        k
    }
}

fn flatten(prefix: &str, value: &Value, out: &mut HashMap<String, String>) {
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_owned()
        } else {
            format!("{prefix}.{k}")
        }
    };
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                flatten(&join(k), v, out);
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                flatten(&join(&i.to_string()), v, out);
            }
        }
        Value::String(s) if !prefix.is_empty() => {
            out.insert(prefix.to_owned(), s.clone());
        }
        Value::Number(n) if !prefix.is_empty() => {
            out.insert(prefix.to_owned(), n.to_string());
        }
        Value::Bool(b) if !prefix.is_empty() => {
            out.insert(prefix.to_owned(), b.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_nested_json() {
        let v = serde_json::json!({
            "common": {"save": "Save", "count_one": "{{count}} video", "count_other": "{{count}} videos"},
            "tips": ["a", "b"],
            "n": 3, "flag": true, "nothing": null
        });
        let b = Bundle::from_json("en", "1", &v);
        assert_eq!(b.get("common.save"), Some("Save"));
        assert_eq!(b.get("common.count_one"), Some("{{count}} video"));
        assert_eq!(b.get("tips.1"), Some("b"));
        assert_eq!(b.get("n"), Some("3"));
        assert_eq!(b.get("flag"), Some("true"));
        assert!(!b.contains("nothing"));
        assert_eq!(b.len(), 7);
        assert_eq!(b.keys()[0], "common.count_one");
        assert!(Bundle::from_json("en", "1", &Value::String("x".into())).is_empty());
    }
}
