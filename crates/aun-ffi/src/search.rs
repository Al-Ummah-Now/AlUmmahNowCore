//! FFI facade for `aun-search`: live search suggestions, grouping, highlight ranges, Arabic-aware
//! normalisation and recent-search rules.
//!
//! Apps debounce typing (about 200 ms) and cancel the previous `search_suggestions` call when a new
//! one starts; cancelling the foreign task drops the in-flight request. A `RateLimited` error carries
//! `retry_after_ms`: pause suggestions for that long and keep the typed text.

use crate::{ApiClient, AunError};

/// What a suggestion points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SuggestionKind {
    /// A query completion: picking it runs a full search for its text.
    Query,
    Video,
    Channel,
    Playlist,
    Category,
}

impl From<aun_search::SuggestionKind> for SuggestionKind {
    fn from(k: aun_search::SuggestionKind) -> Self {
        use aun_search::SuggestionKind as K;
        match k {
            K::Query | K::Unknown => SuggestionKind::Query,
            K::Video => SuggestionKind::Video,
            K::Channel => SuggestionKind::Channel,
            K::Playlist => SuggestionKind::Playlist,
            K::Category => SuggestionKind::Category,
        }
    }
}

impl From<SuggestionKind> for aun_search::SuggestionKind {
    fn from(k: SuggestionKind) -> Self {
        match k {
            SuggestionKind::Query => Self::Query,
            SuggestionKind::Video => Self::Video,
            SuggestionKind::Channel => Self::Channel,
            SuggestionKind::Playlist => Self::Playlist,
            SuggestionKind::Category => Self::Category,
        }
    }
}

/// One suggestion.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SearchSuggestion {
    pub text: String,
    pub kind: SuggestionKind,
    /// Item id for videos, channels, playlists and categories.
    pub id: Option<String>,
    pub thumbnail_url: Option<String>,
    /// Secondary line, e.g. the channel name under a video title.
    pub subtitle: Option<String>,
    pub verified: bool,
    pub count: Option<f64>,
    /// Extra server fields as a JSON object, when present.
    pub metadata_json: Option<String>,
    /// True when picking it opens the item (`id` is set); false runs a full search for `text`.
    pub opens_item: bool,
}

impl From<aun_search::SearchSuggestion> for SearchSuggestion {
    fn from(s: aun_search::SearchSuggestion) -> Self {
        let opens_item = s.opens_item();
        Self {
            metadata_json: s
                .metadata
                .and_then(|m| serde_json::to_string(&serde_json::Value::Object(m)).ok()),
            text: s.text,
            kind: s.kind.into(),
            id: s.id.filter(|id| !id.is_empty()),
            thumbnail_url: s.thumbnail_url,
            subtitle: s.subtitle,
            verified: s.verified.unwrap_or(false),
            count: s.count,
            opens_item,
        }
    }
}

impl From<SearchSuggestion> for aun_search::SearchSuggestion {
    fn from(s: SearchSuggestion) -> Self {
        Self {
            text: s.text,
            kind: s.kind.into(),
            id: s.id,
            thumbnail_url: s.thumbnail_url,
            subtitle: s.subtitle,
            verified: Some(s.verified),
            count: s.count,
            metadata: s
                .metadata_json
                .and_then(|j| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&j).ok()),
        }
    }
}

/// A suggestions request.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct SuggestionQuery {
    /// What the user has typed so far. Blank returns an empty list without a request.
    pub text: String,
    /// 1-20; 8 when `None`.
    pub limit: Option<u32>,
    /// Kinds to return. Empty keeps the server default (videos, channels and categories).
    pub kinds: Vec<SuggestionKind>,
    /// Restricts suggestions to one channel.
    pub channel_id: Option<String>,
}

impl From<SuggestionQuery> for aun_search::SuggestionQuery {
    fn from(q: SuggestionQuery) -> Self {
        Self {
            text: q.text,
            limit: q.limit,
            kinds: q.kinds.into_iter().map(Into::into).collect(),
            channel_id: q.channel_id,
        }
    }
}

/// Suggestions of one kind, in server order.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct SuggestionGroup {
    pub kind: SuggestionKind,
    pub items: Vec<SearchSuggestion>,
}

/// A half-open range of UTF-16 code units, the unit `NSString`, Kotlin and C# strings index by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

#[uniffi::export(async_runtime = "tokio")]
impl ApiClient {
    /// `GET /api/search/suggestions`, made once (no retries). Throws `RateLimited` on 429.
    pub async fn search_suggestions(
        &self,
        query: SuggestionQuery,
    ) -> Result<Vec<SearchSuggestion>, AunError> {
        let items = aun_search::fetch_suggestions(&self.inner, &query.into()).await?;
        Ok(items.into_iter().map(Into::into).collect())
    }
}

/// Groups suggestions for display: completions, videos, channels, playlists, categories.
#[uniffi::export]
pub fn group_search_suggestions(suggestions: Vec<SearchSuggestion>) -> Vec<SuggestionGroup> {
    aun_search::group_suggestions(suggestions.into_iter().map(Into::into).collect())
        .into_iter()
        .map(|g| SuggestionGroup {
            kind: g.kind.into(),
            items: g.items.into_iter().map(Into::into).collect(),
        })
        .collect()
}

/// The server's search normalisation (Arabic folding, case, accents, punctuation).
#[uniffi::export]
pub fn normalize_search_text(text: String) -> String {
    aun_search::normalize_search_text(&text)
}

/// Arabic letter folding only, for loose comparison of display strings.
#[uniffi::export]
pub fn normalize_arabic(text: String) -> String {
    aun_search::normalize_arabic(&text)
}

/// Ranges of `text` matching `query` (word prefixes, Arabic-aware), in UTF-16 code units.
#[uniffi::export]
pub fn search_highlight_ranges(text: String, query: String) -> Vec<TextRange> {
    aun_search::highlight_ranges(&text, &query)
        .into_iter()
        .map(|r| TextRange {
            start: r.start,
            end: r.end,
        })
        .collect()
}

/// Whether every word of `query` starts a word of `text`, under search normalisation.
#[uniffi::export]
pub fn search_matches(text: String, query: String) -> bool {
    aun_search::matches_query(&text, &query)
}

/// Adds a submitted query to the recent list (newest first, de-duplicated, capped).
#[uniffi::export]
pub fn recent_searches_add(recent: Vec<String>, query: String, capacity: u32) -> Vec<String> {
    aun_search::add_recent_search(&recent, &query, capacity)
}

/// Removes one recent search (and its spelling variants).
#[uniffi::export]
pub fn recent_searches_remove(recent: Vec<String>, query: String) -> Vec<String> {
    aun_search::remove_recent_search(&recent, &query)
}

/// Recent searches matching the typed text; all of them when it is blank.
#[uniffi::export]
pub fn recent_searches_filter(recent: Vec<String>, text: String) -> Vec<String> {
    aun_search::filter_recent_searches(&recent, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(text: &str, kind: SuggestionKind, id: Option<&str>) -> SearchSuggestion {
        SearchSuggestion {
            text: text.into(),
            kind,
            id: id.map(Into::into),
            thumbnail_url: None,
            subtitle: None,
            verified: false,
            count: None,
            metadata_json: Some(r#"{"duration":61}"#.into()),
            opens_item: id.is_some(),
        }
    }

    #[test]
    fn groups_round_trip_through_ffi_records() {
        let groups = group_search_suggestions(vec![
            suggestion("قناة", SuggestionKind::Channel, Some("c1")),
            suggestion("تفسير", SuggestionKind::Query, None),
        ]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].kind, SuggestionKind::Query);
        assert_eq!(groups[1].items[0].id.as_deref(), Some("c1"));
        assert!(groups[1].items[0].opens_item);
        assert_eq!(
            groups[1].items[0].metadata_json.as_deref(),
            Some(r#"{"duration":61}"#)
        );
    }

    #[test]
    fn text_helpers() {
        assert_eq!(normalize_search_text("أحمد".into()), "احمد");
        assert_eq!(normalize_arabic("مدرسة!".into()), "مدرسه!");
        assert_eq!(
            search_highlight_ranges("أحمد".into(), "احم".into()),
            [TextRange { start: 0, end: 3 }]
        );
        assert!(search_matches("سورة البقرة".into(), "بقره".into()));
        let r = recent_searches_add(vec!["احمد".into()], "أحمد".into(), 20);
        assert_eq!(r, ["أحمد"]);
        assert!(recent_searches_remove(r.clone(), "احمد".into()).is_empty());
        assert_eq!(recent_searches_filter(r, "".into()), ["أحمد"]);
    }
}
