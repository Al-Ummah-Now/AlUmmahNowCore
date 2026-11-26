//! Playlist payloads from ranked search.

use std::collections::BTreeMap;

use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

/// The nested playlist payload in a ranked search result.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlaylist {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_localized: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_localized: Option<BTreeMap<String, String>>,
}

/// Extracts playlists in ranked order from a full `GET /api/search` response.
pub fn decode_search_playlists(body: &str) -> Result<Vec<SearchPlaylist>> {
    #[derive(Deserialize)]
    struct Response {
        results: Vec<Hit>,
    }

    #[derive(Deserialize)]
    #[serde(tag = "type", rename_all = "lowercase")]
    enum Hit {
        Playlist {
            playlist: SearchPlaylist,
        },
        #[serde(other)]
        Other,
    }

    let response: Response = serde_json::from_str(body)
        .map_err(|error| Error::Internal(format!("decoding search results: {error}")))?;
    Ok(response
        .results
        .into_iter()
        .filter_map(|hit| match hit {
            Hit::Playlist { playlist } => Some(playlist),
            Hit::Other => None,
        })
        .collect())
}
