//! Recent searches: the list the apps show when the search field is focused and empty, or filter
//! while typing. The core owns the rules (order, de-duplication, cap); each app stores the list
//! locally (UserDefaults, DataStore, settings file) and offers "Clear recent searches".

use crate::normalize::{matches_query, normalize_search_text};

/// How many recent searches apps keep by default.
pub const DEFAULT_RECENT_CAPACITY: u32 = 20;

/// Adds `query` to the front of `recent`, newest first. A query that normalises to the same text
/// as an existing entry replaces it (so `أحمد` and `احمد` are one entry, shown as last typed).
/// Blank queries leave the list unchanged. The result holds at most `capacity` entries.
pub fn add_recent_search(recent: &[String], query: &str, capacity: u32) -> Vec<String> {
    let capacity = capacity.max(1) as usize;
    let trimmed = query.trim();
    let key = normalize_search_text(trimmed);
    if key.is_empty() {
        return recent.iter().take(capacity).cloned().collect();
    }
    std::iter::once(trimmed.to_owned())
        .chain(recent.iter().filter(|r| normalize_search_text(r) != key).cloned())
        .take(capacity)
        .collect()
}

/// Removes every entry that normalises to the same text as `query` (swipe-to-delete one entry).
pub fn remove_recent_search(recent: &[String], query: &str) -> Vec<String> {
    let key = normalize_search_text(query);
    recent
        .iter()
        .filter(|r| normalize_search_text(r) != key)
        .cloned()
        .collect()
}

/// Recent searches to show for the typed text: all of them when it is blank, otherwise those whose
/// words start with every typed word (same normalisation as the server).
pub fn filter_recent_searches(recent: &[String], text: &str) -> Vec<String> {
    if normalize_search_text(text).is_empty() {
        return recent.to_vec();
    }
    recent
        .iter()
        .filter(|r| matches_query(r, text))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn adds_newest_first_and_dedupes_by_normalised_text() {
        let r = add_recent_search(&list(&["quran", "احمد"]), "  أحمد ", 20);
        assert_eq!(r, list(&["أحمد", "quran"]));
        let r = add_recent_search(&r, "QURAN", 20);
        assert_eq!(r, list(&["QURAN", "أحمد"]));
    }

    #[test]
    fn caps_and_ignores_blank() {
        let r = add_recent_search(&list(&["a", "b", "c"]), "d", 3);
        assert_eq!(r, list(&["d", "a", "b"]));
        assert_eq!(add_recent_search(&r, " ؟ ", 3), r);
        assert_eq!(add_recent_search(&r, "x", 0), list(&["x"]));
    }

    #[test]
    fn removes_and_filters() {
        let r = list(&["تفسير سورة البقرة", "Tafsir", "fiqh"]);
        assert_eq!(
            remove_recent_search(&r, "tafsír"),
            list(&["تفسير سورة البقرة", "fiqh"])
        );
        assert_eq!(filter_recent_searches(&r, "بقره"), list(&["تفسير سورة البقرة"]));
        assert_eq!(filter_recent_searches(&r, "TAF"), list(&["Tafsir"]));
        assert_eq!(filter_recent_searches(&r, " "), r);
    }
}
