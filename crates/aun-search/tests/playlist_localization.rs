#![allow(clippy::unwrap_used)]

use aun_search::{decode_search_playlists, decode_suggestions};
use serde_json::{json, to_value};

#[test]
fn reads_nested_ranked_playlists_and_top_level_suggestion_localizations() {
    let fields = json!({
        "titleLocalized": {"ar": "دروس"},
        "descriptionLocalized": {"en": "Notes"}
    });
    let mut playlist = fields.clone();
    playlist["id"] = json!("p1");
    playlist["name"] = json!("Lessons");
    let response = json!({"results": [
        {"type": "video", "video": {"id": "v1"}},
        {"type": "playlist", "playlist": playlist},
        {"type": "playlist", "playlist": {"id": "p2", "name": "Original"}}
    ]});
    let playlists = decode_search_playlists(&response.to_string()).unwrap();
    assert_eq!(playlists.len(), 2);
    assert_eq!(to_value(&playlists[0]).unwrap(), playlist);
    assert_eq!(
        to_value(&playlists[1]).unwrap(),
        json!({"id": "p2", "name": "Original"})
    );

    let mut suggestion = fields.clone();
    suggestion["text"] = json!("Lessons");
    suggestion["type"] = json!("playlist");
    suggestion["id"] = json!("p1");
    let response = json!([suggestion, {"text": "Original", "type": "playlist"}]);
    let suggestions = decode_suggestions(&response.to_string()).unwrap();
    assert_eq!(suggestions.len(), 2);
    for key in ["titleLocalized", "descriptionLocalized"] {
        assert_eq!(to_value(&suggestions[0]).unwrap()[key], fields[key]);
        assert!(to_value(&suggestions[1]).unwrap().get(key).is_none());
    }
}
