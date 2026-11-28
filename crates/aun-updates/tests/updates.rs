#![allow(clippy::unwrap_used, clippy::expect_used)]
//! aun-updates against the shared vectors (spec/app-updates) and the rules of wiki
//! AUN-APP-UPDATES §3.5, §3.7, §5.3. Extra keys are made from fixed test seeds.

use aun_updates::decide::{UpdateAnswer, decide, offline_kind};
use aun_updates::verify::{Expect, VerifyError, accept_key_list, key_id, sha256_hex, verify_manifest};
use aun_updates::{PromptKind, UpdateState, compare, is_valid};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::Value;

fn spec(name: &str) -> Value {
    let path = format!("{}/../../spec/app-updates/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap().to_owned()
}

struct TestKey(SigningKey);
impl TestKey {
    fn new(seed: u8) -> Self {
        Self(SigningKey::from_bytes(&[seed; 32]))
    }
    fn public(&self) -> String {
        B64.encode(self.0.verifying_key().to_bytes())
    }
    fn sign(&self, bytes: &[u8]) -> String {
        let sig = self.0.sign(bytes);
        format!(
            r#"{{"signatures":[{{"keyId":"{}","sig":"{}"}}]}}"#,
            key_id(&self.public()).unwrap(),
            B64.encode(sig.to_bytes())
        )
    }
}

#[test]
fn versions_match_the_shared_table() {
    let v = spec("versions.json");
    for case in v["cases"].as_array().unwrap() {
        let (a, b, want) = (
            case[0].as_str().unwrap(),
            case[1].as_str().unwrap(),
            case[2].as_i64().unwrap(),
        );
        let got = compare(a, b).unwrap() as i64;
        assert_eq!(got, want, "{a} vs {b}");
        assert_eq!(compare(b, a).unwrap() as i64, -want, "{b} vs {a}");
    }
    for ok in v["valid"].as_array().unwrap() {
        assert!(is_valid(ok.as_str().unwrap()));
    }
    for bad in v["invalid"].as_array().unwrap() {
        assert!(!is_valid(bad.as_str().unwrap()));
    }
}

#[test]
fn the_shared_signed_vector_verifies_and_tampering_fails() {
    let v = spec("signed-vector.json");
    let root = vec![s(&v, "rootPublicKey")];
    let list = s(&v, "keyList");
    let mut state = UpdateState::default();
    assert!(
        state
            .accept_key_list(list.as_bytes(), &s(&v, "keyListSig"), &root, "AlUmmahNowTools")
            .unwrap()
    );
    assert_eq!(state.serial, 2);
    assert_eq!(state.revoked, vec!["0badc0de".to_owned()]);
    assert!(
        !state
            .accept_key_list(list.as_bytes(), &s(&v, "keyListSig"), &root, "AlUmmahNowTools")
            .unwrap(),
        "the same list twice changes nothing"
    );

    let manifest = s(&v, "manifest");
    let expect = Expect {
        repo: "AlUmmahNowTools",
        tag: Some("v2026.11.23"),
        channels: Some(&["stable"]),
        newer_than: Some("2026.11.22"),
        floor: None,
    };
    let (m, _) = verify_manifest(
        manifest.as_bytes(),
        &s(&v, "manifestSig"),
        &state.trust(),
        &expect,
    )
    .unwrap();
    assert_eq!(m.version, "2026.11.23");
    assert_eq!(m.notes.ar, "فحص أسرع للمجلدات.");
    assert_eq!(
        m.artifacts[0].sha256,
        sha256_hex(b"test package for the shared vector")
    );

    let tampered = manifest.replace("Faster", "Slower");
    assert_eq!(
        verify_manifest(
            tampered.as_bytes(),
            &s(&v, "manifestSig"),
            &state.trust(),
            &expect
        )
        .unwrap_err(),
        VerifyError::BadSignature
    );
    assert_eq!(
        verify_manifest(
            manifest.as_bytes(),
            &s(&v, "manifestSigByRoot"),
            &state.trust(),
            &expect
        )
        .unwrap_err(),
        VerifyError::UnknownKey,
        "the root key never signs releases"
    );
    let tampered_list = list.replace("\"serial\": 2", "\"serial\": 9");
    assert_eq!(
        accept_key_list(
            tampered_list.as_bytes(),
            &s(&v, "keyListSig"),
            &root,
            "AlUmmahNowTools",
            None
        )
        .unwrap_err(),
        VerifyError::BadSignature
    );
}

fn list_json(repo: &str, serial: u64, keys: &[&TestKey], revoked: &[&str], yanked: &[&str]) -> String {
    let keys: Vec<Value> = keys
        .iter()
        .map(|k| serde_json::json!({ "keyId": key_id(&k.public()).unwrap(), "publicKey": k.public() }))
        .collect();
    serde_json::json!({ "schema": 1, "repo": repo, "serial": serial, "keys": keys, "revoked": revoked, "yanked": yanked }).to_string()
}

fn manifest_json(version: &str, yanked: &[&str]) -> String {
    let tag = format!("v{version}");
    serde_json::json!({
        "schema": 1, "repo": "AlUmmahNowTools", "tag": tag, "version": version, "channel": "stable",
        "publishedAt": "2026-11-23T18:04:11Z", "notes": { "en": "New.", "ar": "جديد." },
        "artifacts": [{ "app": "tools", "platform": "windows", "arch": "x64", "kind": "velopack-full",
            "url": format!("https://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/download/{tag}/full.nupkg"), "sha256": "a".repeat(64), "size": 1 }],
        "stores": [], "yanked": yanked
    })
    .to_string()
}

#[test]
fn skip_safe_rotation_and_remembered_revocation() {
    let (root, old, new) = (TestKey::new(1), TestKey::new(2), TestKey::new(3));
    let roots = vec![root.public()];
    let mut state = UpdateState::default();
    let l1 = list_json("AlUmmahNowTools", 1, &[&old], &[], &[]);
    state
        .accept_key_list(
            l1.as_bytes(),
            &root.sign(l1.as_bytes()),
            &roots,
            "AlUmmahNowTools",
        )
        .unwrap();
    // The app skips every release; the key is rotated and the old one revoked in serial 3
    let l3 = list_json(
        "AlUmmahNowTools",
        3,
        &[&new],
        &[&key_id(&old.public()).unwrap()],
        &["2026.11.20"],
    );
    state
        .accept_key_list(
            l3.as_bytes(),
            &root.sign(l3.as_bytes()),
            &roots,
            "AlUmmahNowTools",
        )
        .unwrap();
    let m = manifest_json("2026.11.25", &[]);
    let expect = Expect {
        repo: "AlUmmahNowTools",
        ..Default::default()
    };
    assert!(verify_manifest(m.as_bytes(), &new.sign(m.as_bytes()), &state.trust(), &expect).is_ok());
    assert_eq!(
        verify_manifest(m.as_bytes(), &old.sign(m.as_bytes()), &state.trust(), &expect).unwrap_err(),
        VerifyError::UnknownKey
    );
    // A later list that lists the old key again doesn't bring it back
    let l4 = list_json("AlUmmahNowTools", 4, &[&new, &old], &[], &[]);
    state
        .accept_key_list(
            l4.as_bytes(),
            &root.sign(l4.as_bytes()),
            &roots,
            "AlUmmahNowTools",
        )
        .unwrap();
    assert_eq!(
        verify_manifest(m.as_bytes(), &old.sign(m.as_bytes()), &state.trust(), &expect).unwrap_err(),
        VerifyError::UnknownKey
    );
    // A stolen release key can't sign a key list; an older list is ignored
    let evil = list_json("AlUmmahNowTools", 9, &[&old], &[], &[]);
    assert_eq!(
        state
            .accept_key_list(
                evil.as_bytes(),
                &new.sign(evil.as_bytes()),
                &roots,
                "AlUmmahNowTools"
            )
            .unwrap_err(),
        VerifyError::UnknownKey
    );
    assert!(
        !state
            .accept_key_list(
                l3.as_bytes(),
                &root.sign(l3.as_bytes()),
                &roots,
                "AlUmmahNowTools"
            )
            .unwrap()
    );
    // Yanked versions are refused
    let y = manifest_json("2026.11.20", &[]);
    assert_eq!(
        verify_manifest(y.as_bytes(), &new.sign(y.as_bytes()), &state.trust(), &expect).unwrap_err(),
        VerifyError::Yanked
    );
}

#[test]
fn replay_floor_and_downgrade() {
    let (root, k) = (TestKey::new(4), TestKey::new(5));
    let mut state = UpdateState::default();
    let l = list_json("AlUmmahNowTools", 1, &[&k], &[], &[]);
    state
        .accept_key_list(
            l.as_bytes(),
            &root.sign(l.as_bytes()),
            &[root.public()],
            "AlUmmahNowTools",
        )
        .unwrap();
    let newer = manifest_json("2026.11.24", &["2026.11.22"]);
    let (m, _) = verify_manifest(
        newer.as_bytes(),
        &k.sign(newer.as_bytes()),
        &state.trust(),
        &Expect {
            repo: "AlUmmahNowTools",
            ..Default::default()
        },
    )
    .unwrap();
    state.remember_verified(&m);
    assert_eq!(state.floor.as_deref(), Some("2026.11.24"));
    assert!(state.yanked.contains(&"2026.11.22".to_owned()));
    // Installed 2026.11.20; an older signed 2026.11.23 is offered after 2026.11.24 was seen
    let older = manifest_json("2026.11.23", &[]);
    let e = Expect {
        repo: "AlUmmahNowTools",
        newer_than: Some("2026.11.20"),
        floor: state.floor.as_deref(),
        ..Default::default()
    };
    assert_eq!(
        verify_manifest(older.as_bytes(), &k.sign(older.as_bytes()), &state.trust(), &e).unwrap_err(),
        VerifyError::BelowFloor
    );
    let e2 = Expect {
        repo: "AlUmmahNowTools",
        newer_than: Some("2026.11.24"),
        ..Default::default()
    };
    assert_eq!(
        verify_manifest(newer.as_bytes(), &k.sign(newer.as_bytes()), &state.trust(), &e2).unwrap_err(),
        VerifyError::NotNewer
    );
    let e3 = Expect {
        repo: "AlUmmahNowTools",
        channels: Some(&["beta"]),
        ..Default::default()
    };
    assert_eq!(
        verify_manifest(newer.as_bytes(), &k.sign(newer.as_bytes()), &state.trust(), &e3).unwrap_err(),
        VerifyError::WrongChannel
    );
    let foreign = newer.replace("releases/download/v2026.11.24/", "releases/download/v2026.11.01/");
    assert!(matches!(
        verify_manifest(
            foreign.as_bytes(),
            &k.sign(foreign.as_bytes()),
            &state.trust(),
            &Expect {
                repo: "AlUmmahNowTools",
                ..Default::default()
            }
        )
        .unwrap_err(),
        VerifyError::BadContents(_)
    ));
}

#[test]
fn rejects_traversal_in_artifact_url() {
    let root = TestKey::new(10);
    let k = TestKey::new(11);
    let mut state = UpdateState::default();
    let l = list_json("AlUmmahNowTools", 1, &[&k], &[], &[]);
    state
        .accept_key_list(
            l.as_bytes(),
            &root.sign(l.as_bytes()),
            &[root.public()],
            "AlUmmahNowTools",
        )
        .unwrap();
    let base = manifest_json("2026.11.24", &[]);
    for bad_suffix in ["../v2026.11.01/evil.nupkg", "%2e%2e/evil.nupkg", "x.nupkg?sig=1"] {
        let tampered = base.replace(
            "releases/download/v2026.11.24/full.nupkg",
            &format!("releases/download/v2026.11.24/{bad_suffix}"),
        );
        assert!(
            matches!(
                verify_manifest(
                    tampered.as_bytes(),
                    &k.sign(tampered.as_bytes()),
                    &state.trust(),
                    &Expect {
                        repo: "AlUmmahNowTools",
                        ..Default::default()
                    }
                )
                .unwrap_err(),
                VerifyError::BadContents(_)
            ),
            "{bad_suffix}"
        );
    }
}

fn answer(json: &str) -> UpdateAnswer {
    serde_json::from_str(json).unwrap()
}

#[test]
fn decisions() {
    let mut st = UpdateState::default();
    let off = answer(r#"{"enabled":false,"status":"ok","minimumVersion":"9.0.0","action":"none"}"#);
    assert_eq!(decide(&off, "1.0.0", &mut st, None, true).kind, PromptKind::None);

    let rec = answer(
        r#"{"enabled":true,"status":"recommended","minimumVersion":"0.0.0","latestVersion":"1.1.0","action":"store","available":true,"notes":{"en":"New.","ar":"جديد."}}"#,
    );
    let p = decide(&rec, "1.0.0", &mut st, None, true);
    assert_eq!(p.kind, PromptKind::Notice);
    assert_eq!(p.notes.unwrap().ar, "جديد.");
    st.dismiss("1.1.0");
    assert_eq!(
        decide(&rec, "1.0.0", &mut st, None, true).kind,
        PromptKind::None,
        "closed for this version"
    );

    let play = answer(
        r#"{"enabled":true,"status":"recommended","minimumVersion":"0.0.0","latestVersion":"1.2.0","action":"store","available":null}"#,
    );
    assert_eq!(
        decide(&play, "1.0.0", &mut st, None, true).kind,
        PromptKind::None,
        "the device hasn't confirmed"
    );
    assert_eq!(
        decide(&play, "1.0.0", &mut st, Some(true), true).kind,
        PromptKind::Notice
    );

    let req = answer(
        r#"{"enabled":true,"status":"required","minimumVersion":"1.2.0","latestVersion":"1.2.0","action":"store","available":null,"message":{"en":"m","ar":"م"}}"#,
    );
    assert_eq!(
        decide(&req, "1.0.0", &mut st, Some(false), true).kind,
        PromptKind::RequiredBar,
        "store has no update yet: the app keeps working"
    );
    assert_eq!(
        decide(&req, "1.0.0", &mut st, Some(true), true).kind,
        PromptKind::Forced
    );
    assert_eq!(st.last_required_minimum.as_deref(), Some("1.2.0"));
    assert_eq!(offline_kind("1.0.0", &st), PromptKind::RequiredBar);
    assert_eq!(
        offline_kind("1.0.0", &UpdateState::default()),
        PromptKind::None,
        "never blocks on a minimum it hasn't seen"
    );

    let none = answer(
        r#"{"enabled":true,"status":"recommended","minimumVersion":"0.0.0","latestVersion":"1.2.0","action":"none","available":false}"#,
    );
    assert_eq!(
        decide(&none, "1.0.0", &mut st, None, true).kind,
        PromptKind::None,
        "nothing this build may get: no notice"
    );
}

#[test]
fn state_round_trips_and_survives_garbage() {
    let mut st = UpdateState {
        floor: Some("1.2.0".into()),
        auto_install: Some(false),
        ..Default::default()
    };
    for v in 0..15 {
        st.dismiss(&format!("1.{v}.0"));
    }
    assert_eq!(st.dismissed.len(), 10);
    let back = UpdateState::from_json(&st.to_json());
    assert_eq!(back, st);
    assert_eq!(UpdateState::from_json("not json"), UpdateState::default());
}

#[test]
fn release_tags_match_the_shared_table() {
    let t = spec("tags.json");
    for case in t["valid"].as_array().unwrap() {
        let tag = case[0].as_str().unwrap();
        let parsed = aun_updates::parse_release_tag(tag).unwrap_or_else(|| panic!("{tag}"));
        assert_eq!(parsed.prefix, case[1].as_str().unwrap(), "{tag}");
        assert_eq!(parsed.version, case[2].as_str().unwrap(), "{tag}");
        assert_eq!(parsed.admin_only, case[3].as_bool().unwrap(), "{tag}");
    }
    for bad in t["invalid"].as_array().unwrap() {
        assert!(
            aun_updates::parse_release_tag(bad.as_str().unwrap()).is_none(),
            "{bad}"
        );
    }
}

#[test]
fn admin_tags_release_only_the_admin_app_and_the_version_is_the_tags() {
    let (root, k) = (TestKey::new(6), TestKey::new(7));
    let mut state = UpdateState::default();
    let l = list_json("AlUmmahNowTools", 1, &[&k], &[], &[]);
    state
        .accept_key_list(
            l.as_bytes(),
            &root.sign(l.as_bytes()),
            &[root.public()],
            "AlUmmahNowTools",
        )
        .unwrap();
    let expect = Expect {
        repo: "AlUmmahNowTools",
        ..Default::default()
    };
    let tag = "admin-windows-v1.4.0";
    let admin = |app: &str, version: &str| {
        serde_json::json!({
            "schema": 1, "repo": "AlUmmahNowTools", "tag": tag, "version": version, "channel": "stable",
            "publishedAt": "2026-11-23T18:04:11Z", "notes": { "en": "New.", "ar": "جديد." },
            "artifacts": [{ "app": app, "platform": "windows", "kind": "msix",
                "url": format!("https://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/download/{tag}/x.msix"), "sha256": "a".repeat(64), "size": 1 }]
        })
        .to_string()
    };
    let ok = admin("admin", "1.4.0");
    assert!(verify_manifest(ok.as_bytes(), &k.sign(ok.as_bytes()), &state.trust(), &expect).is_ok());
    let mixed = admin("tools", "1.4.0");
    assert!(matches!(
        verify_manifest(
            mixed.as_bytes(),
            &k.sign(mixed.as_bytes()),
            &state.trust(),
            &expect
        )
        .unwrap_err(),
        VerifyError::BadContents(_)
    ));
    let wrong = admin("admin", "1.4.1");
    assert!(matches!(
        verify_manifest(
            wrong.as_bytes(),
            &k.sign(wrong.as_bytes()),
            &state.trust(),
            &expect
        )
        .unwrap_err(),
        VerifyError::BadContents(_)
    ));
}

#[test]
fn store_actions_open_the_store_page() {
    let mut st = UpdateState::default();
    let a = answer(
        r#"{"enabled":true,"status":"recommended","minimumVersion":"0.0.0","latestVersion":"1.3.0","action":"store","available":true,"storeUrl":"https://apps.apple.com/app/id1","url":null}"#,
    );
    assert_eq!(
        decide(&a, "1.2.0", &mut st, None, true).url.as_deref(),
        Some("https://apps.apple.com/app/id1")
    );
    let d = answer(
        r#"{"enabled":true,"status":"recommended","minimumVersion":"0.0.0","latestVersion":"1.3.0","action":"download","available":true,"storeUrl":null,"url":"https://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/tag/v1.3.0"}"#,
    );
    assert_eq!(
        decide(&d, "1.2.0", &mut st, None, true).url.as_deref(),
        Some("https://github.com/Al-Ummah-Now/AlUmmahNowTools/releases/tag/v1.3.0")
    );
}

#[test]
fn the_root_key_gate() {
    let full = |action: &str, min: &str| {
        answer(
            &serde_json::json!({
                "enabled": true, "status": "recommended", "minimumVersion": min, "latestVersion": "1.1.0", "action": action,
                "url": "https://example.com/p", "storeUrl": "https://example.com/s", "manifestUrl": "https://example.com/m",
                "tag": "v1.1.0", "available": true
            })
            .to_string(),
        )
    };
    let mut st = UpdateState::default();
    // No root key: everything but a store is the notice, with no URL
    for action in ["install", "download", "notice"] {
        let p = decide(&full(action, "0.0.0"), "1.0.0", &mut st, None, false);
        assert_eq!(p.kind, PromptKind::Notice, "{action}");
        assert_eq!(p.action, "notice");
        assert_eq!((p.url, p.manifest_url, p.tag), (None, None, None), "{action}");
        let p = decide(&full(action, "1.1.0"), "1.0.0", &mut st, None, false);
        assert_eq!(p.kind, PromptKind::Forced, "{action}");
        assert_eq!((p.url, p.manifest_url, p.tag), (None, None, None), "{action}");
    }
    let p = decide(&full("store", "0.0.0"), "1.0.0", &mut st, None, false);
    assert_eq!(
        (p.kind, p.url.as_deref()),
        (PromptKind::Notice, Some("https://example.com/s"))
    );
    // A root key: a notice answer is nothing, and never the forced screen
    let p = decide(&full("notice", "0.0.0"), "1.0.0", &mut st, None, true);
    assert_eq!((p.kind, p.url, p.tag), (PromptKind::None, None, None));
    let p = decide(&full("notice", "1.1.0"), "1.0.0", &mut st, None, true);
    assert_eq!(p.kind, PromptKind::RequiredBar);
    assert_eq!((p.action.as_str(), p.url, p.manifest_url), ("none", None, None));
    assert_eq!(
        decide(&full("install", "0.0.0"), "1.0.0", &mut st, None, true)
            .manifest_url
            .as_deref(),
        Some("https://example.com/m")
    );
    // An unknown action is nothing; a release below the minimum doesn't force anyone
    assert_eq!(
        decide(&full("run", "0.0.0"), "1.0.0", &mut st, None, true).kind,
        PromptKind::None
    );
    assert_eq!(
        decide(&full("install", "2.0.0"), "1.0.0", &mut st, None, true).kind,
        PromptKind::RequiredBar
    );
}
