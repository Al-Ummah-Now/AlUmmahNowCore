#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The `UpdateChecker` facade end to end with the shared signed vector (spec/app-updates).

use aun_core::{UpdateChecker, UpdatePromptKind, compare_app_versions};
use serde_json::Value;

fn vector() -> Value {
    let path = format!(
        "{}/../../spec/app-updates/signed-vector.json",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap().to_owned()
}

#[test]
fn bundled_keys_answer_and_manifest() {
    let v = vector();
    let checker = UpdateChecker::new(
        "AlUmmahNowTools".into(),
        vec![s(&v, "rootPublicKey")],
        "2026.11.22".into(),
        false,
        None,
        Some(s(&v, "keyList")),
        Some(s(&v, "keyListSig")),
    )
    .unwrap();
    let answer = serde_json::json!({ "update": {
        "enabled": true, "status": "recommended", "minimumVersion": "0.0.0", "latestVersion": "2026.11.23",
        "notes": { "en": "Faster folder scans.", "ar": "فحص أسرع للمجلدات." }, "action": "install", "available": true,
        "manifestUrl": "https://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/download/v2026.11.23/app-release.json",
        "tag": "v2026.11.23", "checkHours": 100
    }});
    let prompt = checker.apply_answer(answer.to_string(), None).unwrap();
    assert_eq!(prompt.kind, UpdatePromptKind::Notice);
    assert_eq!(prompt.check_hours, 48);
    assert_eq!(prompt.notes_ar.as_deref(), Some("فحص أسرع للمجلدات."));

    let ok = checker.verify_manifest(
        s(&v, "manifest").into_bytes(),
        s(&v, "manifestSig"),
        "v2026.11.23".into(),
    );
    assert_eq!(ok.refused, None);
    assert_eq!(ok.release.unwrap().artifacts.len(), 1);
    let bad = checker.verify_manifest(
        s(&v, "manifest").replace("Faster", "Slower").into_bytes(),
        s(&v, "manifestSig"),
        "v2026.11.23".into(),
    );
    assert_eq!(bad.refused.as_deref(), Some("bad_signature"));
    assert!(bad.release.is_none());

    // The state survives a restart: the floor is now 2026.11.23, so the same manifest is a replay for a fresh install of 2026.11.20
    let restored = UpdateChecker::new(
        "AlUmmahNowTools".into(),
        vec![s(&v, "rootPublicKey")],
        "2026.11.20".into(),
        false,
        Some(checker.state_json()),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        restored
            .verify_manifest(
                s(&v, "manifest").into_bytes(),
                s(&v, "manifestSig"),
                "v2026.11.23".into()
            )
            .refused,
        None,
        "equal to the floor is fine"
    );

    checker.dismiss("2026.11.23".into());
    assert_eq!(
        checker.apply_answer(answer.to_string(), None).unwrap().kind,
        UpdatePromptKind::None
    );
    assert!(checker.allowed_download_url("https://release-assets.githubusercontent.com/x".into()));
    assert!(!checker.allowed_download_url("https://evil.example/x".into()));
    assert_eq!(compare_app_versions("1.2.3.2".into(), "1.2.3".into()), Some(1));
    assert_eq!(compare_app_versions("x".into(), "1.2.3".into()), None);
}

#[test]
fn a_bad_bundled_key_list_is_a_build_error() {
    let v = vector();
    let err = UpdateChecker::new(
        "AlUmmahNowTools".into(),
        vec![s(&v, "releasePublicKey")],
        "1.0.0".into(),
        false,
        None,
        Some(s(&v, "keyList")),
        Some(s(&v, "keyListSig")),
    );
    assert!(err.is_err());
}

#[test]
fn without_a_root_key_nothing_is_linked_or_verified() {
    let v = vector();
    // A state that already trusts the release key doesn't help a build with no root key
    let trusted = UpdateChecker::new(
        "AlUmmahNowTools".into(),
        vec![s(&v, "rootPublicKey")],
        "2026.11.22".into(),
        false,
        None,
        Some(s(&v, "keyList")),
        Some(s(&v, "keyListSig")),
    )
    .unwrap();
    let checker = UpdateChecker::new(
        "AlUmmahNowTools".into(),
        Vec::new(),
        "2026.11.22".into(),
        false,
        Some(trusted.state_json()),
        None,
        None,
    )
    .unwrap();
    let answer = serde_json::json!({ "update": {
        "enabled": true, "status": "recommended", "minimumVersion": "0.0.0", "latestVersion": "2026.11.23",
        "action": "install", "available": true, "url": "https://example.com/p",
        "manifestUrl": "https://example.com/m", "tag": "v2026.11.23",
        "keys": { "list": s(&v, "keyList"), "sig": s(&v, "keyListSig") }
    }});
    let p = checker.apply_answer(answer.to_string(), None).unwrap();
    assert_eq!(p.kind, UpdatePromptKind::Notice);
    assert_eq!(p.action, "notice");
    assert_eq!((p.url, p.manifest_url, p.tag), (None, None, None));
    let check = checker.verify_manifest(
        s(&v, "manifest").into_bytes(),
        s(&v, "manifestSig"),
        "v2026.11.23".into(),
    );
    assert_eq!(check.refused.as_deref(), Some("unknown_key"));
    assert!(!checker.allowed_download_url(
        "https://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/download/v1/x".into()
    ));
}
