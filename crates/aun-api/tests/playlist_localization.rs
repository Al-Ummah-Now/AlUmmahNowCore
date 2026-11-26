#![allow(clippy::unwrap_used)]

use aun_api::playlists::{Playlist, localized_playlist_text};
use aun_api::{PatchPlaylistUpdateBody, PostPlaylistCreateBody};
use serde_json::{json, to_value};

#[test]
fn playlist_reads_preserve_localized_and_legacy_text() {
    let mut body = json!({
        "id": "p1", "title": "Original", "description": "Notes",
        "userId": "u1", "channelId": "",
        "titleLocalized": {"en": "Lessons", "ar": "دروس"},
        "descriptionLocalized": {"ar": "نبذة"}
    });
    let playlist: Playlist = serde_json::from_value(body.clone()).unwrap();
    for key in ["titleLocalized", "descriptionLocalized"] {
        assert_eq!(to_value(&playlist).unwrap()[key], body[key]);
        body.as_object_mut().unwrap().remove(key);
    }
    let legacy: Playlist = serde_json::from_value(body).unwrap();
    assert!(legacy.title_localized.is_none());
    assert!(legacy.description_localized.is_none());
}

#[test]
fn requests_distinguish_localized_only_create_empty_and_omitted_maps() {
    let create = json!({"titleLocalized": {"ar": "دروس"}, "descriptionLocalized": {"en": "Notes"}});
    assert_eq!(
        to_value(serde_json::from_value::<PostPlaylistCreateBody>(create.clone()).unwrap()).unwrap(),
        create
    );
    for update in [
        json!({"titleLocalized": {}, "descriptionLocalized": {}}),
        json!({"titleLocalized": {}, "description": "Original"}),
        json!({"descriptionLocalized": {"ar": "نبذة"}}),
        json!({}),
    ] {
        assert_eq!(
            to_value(serde_json::from_value::<PatchPlaylistUpdateBody>(update.clone()).unwrap()).unwrap(),
            update
        );
    }
}

#[test]
fn display_uses_the_requested_language_then_the_original_text() {
    let names = [
        ("en".into(), " Lessons ".into()),
        ("ar".into(), "\u{feff} دروس ".into()),
    ]
    .into();
    for (locale, expected) in [
        ("en", "Lessons"),
        ("en-US", "Lessons"),
        ("en_US", "Lessons"),
        ("ar", "دروس"),
        ("ar-EG", "دروس"),
        ("ar_SA", "دروس"),
        (" AR-sa ", "دروس"),
        ("fr", "Original"),
    ] {
        assert_eq!(
            localized_playlist_text(Some("Original"), Some(&names), locale),
            expected
        );
    }
    let descriptions = [("ar".into(), " \t ".into())].into();
    assert_eq!(
        localized_playlist_text(Some(" Notes "), Some(&descriptions), "ar"),
        " Notes "
    );
    assert_eq!(localized_playlist_text(Some("Original"), None, "ar"), "Original");
    assert_eq!(localized_playlist_text(None, Some(&descriptions), "en"), "");
}
