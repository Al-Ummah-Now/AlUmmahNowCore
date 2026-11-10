//! Live search autocomplete shared by the native apps.
//!
//! - [`suggestions`]: typed client for `GET /api/search/suggestions`, decoding that tolerates new
//!   suggestion types, and grouping for display.
//! - [`normalize`]: the server's Arabic-aware search normalisation (ported from the monorepo's
//!   `shared-types/src/search/normalize.ts`), match highlighting in UTF-16 ranges, local matching.
//! - [`recent`]: recent-search list rules; the apps persist the list themselves.
//!
//! Debouncing (about 200 ms) and cancelling stale requests stay in the apps, on their own task
//! primitives; cancelling the call drops the in-flight HTTP request.

pub mod normalize;
pub mod recent;
pub mod suggestions;

pub use normalize::{
    Utf16Range, highlight_ranges, matches_query, normalize_arabic, normalize_search_text,
    strip_arabic_article, tokenize_search_text,
};
pub use recent::{DEFAULT_RECENT_CAPACITY, add_recent_search, filter_recent_searches, remove_recent_search};
pub use suggestions::{
    DEFAULT_SUGGESTIONS, MAX_QUERY_CHARS, MAX_SUGGESTIONS, SearchSuggestion, SuggestionGroup, SuggestionKind,
    SuggestionQuery, decode_suggestions, fetch_suggestions, group_suggestions, prepare_query_text,
    suggestions_request,
};
