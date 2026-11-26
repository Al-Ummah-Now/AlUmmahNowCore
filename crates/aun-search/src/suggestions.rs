//! Typed client for `GET /api/search/suggestions` (live search autocomplete) and the grouping the
//! apps show under the search field.
//!
//! The endpoint is public (bearer optional), returns only publicly visible items, and rate limits
//! per IP with `429` + `Retry-After`. Suggestion requests are made once, without retries: the next
//! keystroke is the retry, and a rate-limited call surfaces as [`Error::RateLimited`] so the app can
//! pause suggestions for `retry_after`. Cancelling the call (dropping the future, or cancelling the
//! foreign task over FFI) aborts the HTTP request, which is how apps drop stale requests.

use aun_api::{ApiClient, RequestSpec};
use aun_common::{Error, Result};
use serde::Deserialize;

/// Upper bound the server accepts for `limit`.
pub const MAX_SUGGESTIONS: u32 = 20;
/// `limit` used when the caller does not choose one.
pub const DEFAULT_SUGGESTIONS: u32 = 8;
/// Longest query sent; longer input is cut at a character boundary.
pub const MAX_QUERY_CHARS: usize = 100;
/// Deepest offset accepted by api-videos for an empty query.
const MAX_INITIAL_OFFSET: u32 = 500;

/// What a suggestion points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestionKind {
    /// A query completion: picking it runs a full search for `text`.
    Query,
    Video,
    Channel,
    Playlist,
    Category,
    /// A type this client does not know yet; treated like [`SuggestionKind::Query`].
    #[serde(other)]
    Unknown,
}

impl SuggestionKind {
    /// The `types` parameter value, for the kinds the server accepts there.
    pub fn as_param(self) -> Option<&'static str> {
        match self {
            SuggestionKind::Video => Some("video"),
            SuggestionKind::Channel => Some("channel"),
            SuggestionKind::Playlist => Some("playlist"),
            SuggestionKind::Category => Some("category"),
            SuggestionKind::Query | SuggestionKind::Unknown => None,
        }
    }

    /// Order of the groups under the search field: completions first, then items.
    fn group_rank(self) -> u8 {
        match self {
            SuggestionKind::Query | SuggestionKind::Unknown => 0,
            SuggestionKind::Video => 1,
            SuggestionKind::Channel => 2,
            SuggestionKind::Playlist => 3,
            SuggestionKind::Category => 4,
        }
    }
}

/// One suggestion as the server returns it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SearchSuggestion {
    pub text: String,
    #[serde(rename = "type")]
    pub kind: SuggestionKind,
    /// Item id for `video`, `channel`, `playlist` and `category`; opens the item when picked.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, rename = "thumbnailUrl")]
    pub thumbnail_url: Option<String>,
    /// Secondary line, e.g. the channel name under a video title.
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub verified: Option<bool>,
    /// Item count for channels, playlists and categories, or popularity for completions.
    #[serde(default)]
    pub count: Option<f64>,
    #[serde(default)]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

impl SearchSuggestion {
    /// Whether picking this suggestion opens an item (as opposed to running a full search).
    pub fn opens_item(&self) -> bool {
        self.kind.as_param().is_some() && self.id.as_deref().is_some_and(|id| !id.is_empty())
    }
}

/// A suggestions request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuggestionQuery {
    /// What the user has typed so far.
    pub text: String,
    /// 1-20; [`DEFAULT_SUGGESTIONS`] when `None`.
    pub limit: Option<u32>,
    /// Kinds to return. Empty keeps the server default (videos, channels and categories).
    pub kinds: Vec<SuggestionKind>,
    /// Restricts suggestions to one channel (search inside a channel page).
    pub channel_id: Option<String>,
}

/// Trims and caps the typed text. `None` when there is nothing worth asking the server about.
pub fn prepare_query_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .chars()
            .take(MAX_QUERY_CHARS)
            .collect::<String>()
            .trim_end()
            .to_owned(),
    )
}

/// Builds the request, or `None` when the text is blank (show recent searches instead).
pub fn suggestions_request(query: &SuggestionQuery) -> Result<Option<RequestSpec>> {
    let Some(text) = prepare_query_text(&query.text) else {
        return Ok(None);
    };
    build_suggestions_request(query, text, None).map(Some)
}

fn build_suggestions_request(
    query: &SuggestionQuery,
    text: String,
    cursor: Option<u32>,
) -> Result<RequestSpec> {
    let limit = query.limit.unwrap_or(DEFAULT_SUGGESTIONS);
    if !(1..=MAX_SUGGESTIONS).contains(&limit) {
        return Err(Error::invalid_input(format!(
            "limit must be 1-{MAX_SUGGESTIONS}, got {limit}"
        )));
    }
    let mut req = RequestSpec::new(
        reqwest::Method::GET,
        "/api/search/suggestions".to_owned(),
        "get_SearchSuggestionsGet",
    );
    req.max_attempts = Some(1);
    req.query.push(("q".into(), text));
    req.query.push(("limit".into(), limit.to_string()));
    if let Some(offset) = cursor {
        req.query.push(("offset".into(), offset.to_string()));
    }
    let mut kinds: Vec<&str> = query.kinds.iter().filter_map(|k| k.as_param()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    if !kinds.is_empty() {
        req.query.push(("types".into(), kinds.join(",")));
    }
    if let Some(channel) = query
        .channel_id
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        req.query.push(("channelId".into(), channel.to_owned()));
    }
    Ok(req)
}

/// Fetches suggestions. Blank text returns an empty list without a request.
pub async fn fetch_suggestions(client: &ApiClient, query: &SuggestionQuery) -> Result<Vec<SearchSuggestion>> {
    let Some(req) = suggestions_request(query)? else {
        return Ok(vec![]);
    };
    let raw = client.execute(req).await?;
    let suggestions = decode_suggestions(&raw.body)?;
    tracing::debug!(count = suggestions.len(), "search suggestions");
    Ok(suggestions)
}

/// A page of suggestions. Pass `next_cursor` back with the same query and scope.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchSuggestionsPage {
    pub items: Vec<SearchSuggestion>,
    pub next_cursor: Option<u32>,
    /// A full page can have a continuation. The following page may be empty.
    pub has_more: bool,
}

/// Fetches a picker page. Blank text loads initial results scoped by the query's kinds and channel.
/// The api-videos endpoint accepts an offset for blank queries. Typed queries use a single page.
pub async fn fetch_suggestions_page(
    client: &ApiClient,
    query: &SuggestionQuery,
    cursor: Option<u32>,
) -> Result<SearchSuggestionsPage> {
    let text = prepare_query_text(&query.text).unwrap_or_default();
    let browsing = text.is_empty();
    if let Some(offset) = cursor
        && (!browsing || offset > MAX_INITIAL_OFFSET)
    {
        return Err(Error::invalid_input(format!(
            "suggestion cursor requires a blank query and an offset of at most {MAX_INITIAL_OFFSET}"
        )));
    }
    let req = build_suggestions_request(query, text, cursor)?;
    let raw = client.execute(req).await?;
    let rows = suggestion_rows(&raw.body)?;
    let limit = query.limit.unwrap_or(DEFAULT_SUGGESTIONS);
    // Count server rows before filtering so a malformed row cannot end browsing early.
    let next_cursor = (browsing && rows.len() >= limit as usize)
        .then(|| cursor.unwrap_or_default() + limit)
        .filter(|&next| next <= MAX_INITIAL_OFFSET);
    let items = decode_rows(rows);
    tracing::debug!(count = items.len(), "search suggestions page");
    Ok(SearchSuggestionsPage {
        items,
        next_cursor,
        has_more: next_cursor.is_some(),
    })
}

/// Decodes the response body. Items that do not parse are skipped (and logged) so one bad row from
/// a newer server never empties the list.
pub fn decode_suggestions(body: &str) -> Result<Vec<SearchSuggestion>> {
    Ok(decode_rows(suggestion_rows(body)?))
}

fn suggestion_rows(body: &str) -> Result<Vec<serde_json::Value>> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| Error::Internal(format!("decoding suggestions: {e}")))?;
    match value {
        serde_json::Value::Array(items) => Ok(items),
        serde_json::Value::Object(mut o) => match o.remove("data") {
            Some(serde_json::Value::Array(items)) => Ok(items),
            _ => Err(Error::Internal("suggestions response is not a list".into())),
        },
        _ => Err(Error::Internal("suggestions response is not a list".into())),
    }
}

fn decode_rows(items: Vec<serde_json::Value>) -> Vec<SearchSuggestion> {
    items
        .into_iter()
        .filter_map(|item| match serde_json::from_value::<SearchSuggestion>(item) {
            Ok(s) if !s.text.trim().is_empty() => Some(s),
            Ok(_) => None,
            Err(e) => {
                tracing::warn!(error = %e, "skipping malformed search suggestion");
                None
            }
        })
        .collect()
}

/// Suggestions of one kind, in server order.
#[derive(Debug, Clone, PartialEq)]
pub struct SuggestionGroup {
    pub kind: SuggestionKind,
    pub items: Vec<SearchSuggestion>,
}

/// Groups suggestions for display: completions, videos, channels, playlists, categories. Server
/// order is kept inside each group; empty groups are left out; `Unknown` joins the completions.
pub fn group_suggestions(suggestions: Vec<SearchSuggestion>) -> Vec<SuggestionGroup> {
    let mut groups: Vec<SuggestionGroup> = vec![];
    for s in suggestions {
        let kind = if s.kind == SuggestionKind::Unknown {
            SuggestionKind::Query
        } else {
            s.kind
        };
        match groups.iter_mut().find(|g| g.kind == kind) {
            Some(g) => g.items.push(s),
            None => groups.push(SuggestionGroup { kind, items: vec![s] }),
        }
    }
    groups.sort_by_key(|g| g.kind.group_rank());
    groups
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {

    /// The typed query must use exactly the parameters the pinned OpenAPI snapshot declares
    /// for the operation, so a server rename shows up here when the snapshot is refreshed.
    #[test]
    fn query_parameters_match_the_openapi_snapshot() {
        let spec: serde_json::Value =
            serde_json::from_str(include_str!("../../../spec/openapi/api-videos.json")).unwrap();
        let (spec_path, op) = spec["paths"]
            .as_object()
            .unwrap()
            .iter()
            .find_map(|(p, ops)| {
                ops.as_object()?
                    .values()
                    .find(|o| o["operationId"] == "get_SearchSuggestionsGet")
                    .map(|o| (p.clone(), o.clone()))
            })
            .expect("get_SearchSuggestionsGet in the snapshot");
        let mut declared: Vec<String> = op["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["in"] == "query")
            .map(|p| p["name"].as_str().unwrap().to_owned())
            .collect();
        declared.sort();
        let req = suggestions_request(&SuggestionQuery {
            text: "x".into(),
            limit: Some(5),
            kinds: vec![SuggestionKind::Video],
            channel_id: Some("c".into()),
        })
        .unwrap()
        .unwrap();
        let mut sent: Vec<String> = req.query.iter().map(|(k, _)| k.clone()).collect();
        sent.sort();
        assert_eq!(sent, declared);
        assert_eq!(spec_path.replacen("/api/v1/", "/api/", 1), req.path);
        assert!(
            aun_api::OPERATIONS
                .iter()
                .any(|o| o.id == "get_SearchSuggestionsGet" && o.path == req.path)
        );
    }
    use super::*;

    fn query(text: &str) -> SuggestionQuery {
        SuggestionQuery {
            text: text.into(),
            ..SuggestionQuery::default()
        }
    }

    #[test]
    fn blank_text_makes_no_request() {
        assert!(suggestions_request(&query("   ")).unwrap().is_none());
    }

    #[test]
    fn builds_query_parameters() {
        let req = suggestions_request(&SuggestionQuery {
            text: "  تفسير  ".into(),
            limit: Some(5),
            kinds: vec![
                SuggestionKind::Video,
                SuggestionKind::Channel,
                SuggestionKind::Query,
                SuggestionKind::Video,
            ],
            channel_id: Some(" ch_1 ".into()),
        })
        .unwrap()
        .unwrap();
        assert_eq!(req.path, "/api/search/suggestions");
        assert_eq!(req.max_attempts, Some(1));
        assert_eq!(
            req.query,
            [
                ("q".to_owned(), "تفسير".to_owned()),
                ("limit".to_owned(), "5".to_owned()),
                ("types".to_owned(), "channel,video".to_owned()),
                ("channelId".to_owned(), "ch_1".to_owned()),
            ]
        );
    }

    #[test]
    fn defaults_limit_and_omits_types() {
        let req = suggestions_request(&query("quran")).unwrap().unwrap();
        assert_eq!(
            req.query,
            [
                ("q".to_owned(), "quran".to_owned()),
                ("limit".to_owned(), "8".to_owned())
            ]
        );
    }

    #[test]
    fn rejects_out_of_range_limits() {
        for limit in [0, 21] {
            let q = SuggestionQuery {
                limit: Some(limit),
                ..query("a")
            };
            assert!(matches!(suggestions_request(&q), Err(Error::InvalidInput { .. })));
        }
    }

    #[test]
    fn caps_long_queries_on_char_boundaries() {
        let long = "ب".repeat(150);
        assert_eq!(
            prepare_query_text(&long).unwrap().chars().count(),
            MAX_QUERY_CHARS
        );
    }

    #[test]
    fn decodes_and_skips_bad_rows() {
        let body = r#"[
            {"text":"Tafsir","type":"video","id":"v1","thumbnailUrl":"https://t/1.jpg","subtitle":"Channel","verified":true,"metadata":{"duration":61}},
            {"text":"tafsir surah","type":"query","count":12},
            {"text":"x","type":"podcast"},
            {"type":"video"},
            {"text":"  ","type":"query"}
        ]"#;
        let s = decode_suggestions(body).unwrap();
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].kind, SuggestionKind::Video);
        assert!(s[0].opens_item());
        assert_eq!(s[0].thumbnail_url.as_deref(), Some("https://t/1.jpg"));
        assert_eq!(s[0].metadata.as_ref().unwrap()["duration"], 61);
        assert!(!s[1].opens_item());
        assert_eq!(s[2].kind, SuggestionKind::Unknown);
        assert!(
            decode_suggestions(r#"{"data":[{"text":"a","type":"category","id":"c"}]}"#).unwrap()[0]
                .opens_item()
        );
        assert!(decode_suggestions(r#"{"error":"x"}"#).is_err());
    }

    #[test]
    fn groups_in_display_order() {
        let s = decode_suggestions(
            r#"[{"text":"c","type":"channel","id":"c1"},{"text":"v1","type":"video","id":"v1"},
               {"text":"q","type":"query"},{"text":"v2","type":"video","id":"v2"},{"text":"u","type":"tag"}]"#,
        )
        .unwrap();
        let g = group_suggestions(s);
        assert_eq!(
            g.iter().map(|g| (g.kind, g.items.len())).collect::<Vec<_>>(),
            [
                (SuggestionKind::Query, 2),
                (SuggestionKind::Video, 2),
                (SuggestionKind::Channel, 1)
            ]
        );
        assert_eq!(g[1].items[1].text, "v2");
    }
}
