#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `I18n` end to end: built-ins, fallback, plurals, locale switching, sync and the disk cache.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use aun_common::platform::ManualClock;
use aun_common::{Error, Result};
use aun_i18n::bidi::{FSI, PDI};
use aun_i18n::{
    Direction, FormatArg, I18n, I18nOptions, LocaleChange, LocaleInfo, LocaleListener, NumeralSystem,
    RemoteBundle, RemoteConfig, RemoteMetadata, TranslationSource,
};

const EN: &str = r#"{
  "common": {"save": "Save", "welcome": "Welcome, {{name}}!"},
  "videos": {"count_one": "{{count}} video", "count_other": "{{count}} videos"},
  "only_en": "English only"
}"#;
const AR: &str = r#"{
  "common": {"save": "حفظ", "welcome": "أهلاً {{name}}!"},
  "videos": {
    "count_zero": "لا توجد فيديوهات", "count_one": "فيديو واحد", "count_two": "فيديوهان",
    "count_few": "{{count}} فيديوهات", "count_many": "{{count}} فيديو", "count_other": "{{count}} فيديو"
  }
}"#;

fn args(pairs: &[(&str, FormatArg)]) -> HashMap<String, FormatArg> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn catalog(locale: &str) -> Arc<I18n> {
    let i18n = I18n::new(I18nOptions {
        locale: locale.into(),
        ..I18nOptions::default()
    })
    .unwrap();
    i18n.add_builtin_bundle("en", "builtin", EN).unwrap();
    i18n.add_builtin_bundle("ar", "builtin", AR).unwrap();
    i18n
}

#[test]
fn fallback_chain_and_missing_keys() {
    let i18n = catalog("ar-EG");
    assert_eq!(i18n.fallback_chain(), vec!["ar-EG", "ar", "en"]);
    assert_eq!(i18n.direction(), Direction::Rtl);
    assert_eq!(i18n.translate("common.save"), "حفظ");
    assert_eq!(i18n.translate("only_en"), "English only");
    assert_eq!(i18n.translate("nope.key"), "nope.key");
    assert!(i18n.has("only_en"));
    assert!(!i18n.has("nope.key"));
    assert_eq!(i18n.missing_keys(), vec!["nope.key"]);
}

#[test]
fn plural_keys_follow_each_locale_rules() {
    let ar = catalog("ar");
    let want = [
        (0, "لا توجد فيديوهات"),
        (1, "فيديو واحد"),
        (2, "فيديوهان"),
        (3, "3 فيديوهات"),
        (11, "11 فيديو"),
        (102, "102 فيديو"),
    ];
    for (n, w) in want {
        assert_eq!(
            ar.format("videos.count", &args(&[("count", FormatArg::Int(n))])),
            w
        );
    }
    let en = catalog("en");
    assert_eq!(
        en.format("videos.count", &args(&[("count", 1i64.into())])),
        "1 video"
    );
    assert_eq!(
        en.format("videos.count", &args(&[("count", 0i64.into())])),
        "0 videos"
    );
    assert_eq!(
        en.format("videos.count", &args(&[("count", 2500i64.into())])),
        "2,500 videos"
    );
}

#[test]
fn eastern_arabic_digits_and_bidi() {
    let i18n = catalog("ar");
    i18n.set_numeral_system(NumeralSystem::ArabicIndic);
    assert_eq!(
        i18n.format("videos.count", &args(&[("count", 5i64.into())])),
        "٥ فيديوهات"
    );
    assert_eq!(
        i18n.format("common.welcome", &args(&[("name", "Sara".into())])),
        format!("أهلاً {FSI}Sara{PDI}!")
    );
    assert_eq!(i18n.format_integer(1_234_567), "١٬٢٣٤٬٥٦٧");
    assert_eq!(i18n.format_duration_clock(3_723_000), "١:٠٢:٠٣");
    assert_eq!(i18n.format_duration_compact(300_000), "٥ د");
    assert_eq!(
        i18n.format_pattern("{n, plural, two {اثنان} other {#}}", &args(&[("n", 2i64.into())])),
        "اثنان"
    );
}

#[derive(Default)]
struct Recorder {
    changes: Mutex<Vec<LocaleChange>>,
    updates: Mutex<Vec<Vec<String>>>,
}

impl LocaleListener for Recorder {
    fn on_locale_changed(&self, change: LocaleChange) {
        self.changes.lock().unwrap().push(change);
    }
    fn on_bundles_updated(&self, locales: Vec<String>) {
        self.updates.lock().unwrap().push(locales);
    }
}

#[test]
fn locale_switching_notifies_listeners() {
    let i18n = catalog("en");
    let rec = Arc::new(Recorder::default());
    let id = i18n.add_listener(rec.clone());
    let change = i18n.set_locale("ar_EG").unwrap();
    assert_eq!(change.locale, "ar-EG");
    assert_eq!(change.direction, Direction::Rtl);
    assert!(!change.needs_sync, "built-in ar exists");
    assert_eq!(i18n.translate("common.save"), "حفظ");
    let change = i18n.set_locale("tr").unwrap();
    assert!(change.needs_sync, "no Turkish bundle yet");
    assert_eq!(i18n.translate("common.save"), "Save");
    assert!(matches!(i18n.set_locale("!!"), Err(Error::InvalidInput(_))));
    i18n.remove_listener(id);
    i18n.set_locale("en").unwrap();
    assert_eq!(rec.changes.lock().unwrap().len(), 2);
}

/// In-memory server with per-locale bundles and checksums.
struct FakeSource {
    bundles: Mutex<HashMap<String, (String, serde_json::Value)>>,
    fetches: AtomicUsize,
    offline: std::sync::atomic::AtomicBool,
    etags_seen: Mutex<Vec<Option<String>>>,
}

impl FakeSource {
    fn new() -> Arc<Self> {
        let mut m = HashMap::new();
        m.insert(
            "en".to_owned(),
            (
                "sum-en-1".to_owned(),
                serde_json::json!({"common": {"save": "Save!"}}),
            ),
        );
        m.insert(
            "ar".to_owned(),
            (
                "sum-ar-1".to_owned(),
                serde_json::json!({"common": {"save": "احفظ"}}),
            ),
        );
        Arc::new(Self {
            bundles: Mutex::new(m),
            fetches: AtomicUsize::new(0),
            offline: false.into(),
            etags_seen: Mutex::new(vec![]),
        })
    }
    fn check(&self) -> Result<()> {
        if self.offline.load(Ordering::SeqCst) {
            Err(Error::Network("offline".into()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl TranslationSource for FakeSource {
    async fn config(&self) -> Result<RemoteConfig> {
        self.check()?;
        Ok(RemoteConfig {
            locales: vec![
                LocaleInfo {
                    code: "en".into(),
                    name: "English".into(),
                    english_name: "English".into(),
                    is_rtl: false,
                    is_default: true,
                    is_complete: true,
                },
                LocaleInfo {
                    code: "ar".into(),
                    name: "العربية".into(),
                    english_name: "Arabic".into(),
                    is_rtl: true,
                    is_default: false,
                    is_complete: true,
                },
            ],
            fallback_locale: "en".into(),
            cache_max_age_s: 3600,
            version: "cfg-1".into(),
        })
    }
    async fn metadata(&self) -> Result<RemoteMetadata> {
        self.check()?;
        let b = self.bundles.lock().unwrap();
        Ok(RemoteMetadata {
            version: "meta-1".into(),
            locales: b.keys().cloned().collect(),
            default_locale: "en".into(),
            updated_at: "2026-11-01T00:00:00Z".into(),
            checksums: b
                .iter()
                .map(|(k, (s, _))| (k.clone(), s.clone()))
                .collect::<BTreeMap<_, _>>(),
        })
    }
    async fn translations(&self, locale: &str, if_none_match: Option<&str>) -> Result<Option<RemoteBundle>> {
        self.check()?;
        self.etags_seen
            .lock()
            .unwrap()
            .push(if_none_match.map(str::to_owned));
        let b = self.bundles.lock().unwrap();
        let (sum, json) = b.get(locale).ok_or_else(|| Error::NotFound(locale.into()))?;
        if if_none_match == Some(sum.as_str()) {
            return Ok(None);
        }
        self.fetches.fetch_add(1, Ordering::SeqCst);
        Ok(Some(RemoteBundle {
            locale: locale.into(),
            version: sum.clone(),
            updated_at: "2026-11-01T00:00:00Z".into(),
            translations: json.clone(),
        }))
    }
}

#[tokio::test]
async fn sync_downloads_caches_and_revalidates() {
    let dir = tempfile::tempdir().unwrap();
    let opts = I18nOptions {
        locale: "ar-EG".into(),
        cache_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..I18nOptions::default()
    };
    let i18n = I18n::with_clock(opts.clone(), Arc::new(ManualClock::new(1_000))).unwrap();
    assert!(matches!(i18n.sync().await, Err(Error::Unsupported(_))));

    let source = FakeSource::new();
    i18n.set_source(source.clone());
    let rec = Arc::new(Recorder::default());
    i18n.add_listener(rec.clone());

    let report = i18n.sync().await.unwrap();
    assert_eq!(report.updated, vec!["ar", "en"], "chain order, ar-EG not offered");
    assert_eq!(i18n.translate("common.save"), "احفظ");
    assert_eq!(rec.updates.lock().unwrap().len(), 1);
    assert_eq!(i18n.available_locales().len(), 2);

    // Second sync: checksums match, nothing downloaded.
    let report = i18n.sync().await.unwrap();
    assert_eq!(report.unchanged, vec!["ar", "en"]);
    assert_eq!(source.fetches.load(Ordering::SeqCst), 2);

    // Server changes Arabic: only Arabic is refetched, with the old checksum as If-None-Match.
    source.bundles.lock().unwrap().insert(
        "ar".into(),
        (
            "sum-ar-2".into(),
            serde_json::json!({"common": {"save": "حفظ الآن"}}),
        ),
    );
    let report = i18n.sync().await.unwrap();
    assert_eq!(report.updated, vec!["ar"]);
    assert_eq!(
        source
            .etags_seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
            .as_deref(),
        Some("sum-ar-1")
    );
    assert_eq!(i18n.translate("common.save"), "حفظ الآن");

    // Offline: error, cached bundles still served.
    source.offline.store(true, Ordering::SeqCst);
    let err = i18n.sync().await.unwrap_err();
    assert!(err.is_retryable());
    assert_eq!(i18n.translate("common.save"), "حفظ الآن");

    // A new process starts from the disk cache without any network.
    let cold = I18n::new(opts).unwrap();
    assert_eq!(cold.translate("common.save"), "حفظ الآن");
    assert_eq!(cold.available_locales()[1].name, "العربية");
    cold.set_locale("en").unwrap();
    assert_eq!(cold.translate("common.save"), "Save!");
}

#[tokio::test]
async fn downloaded_bundles_override_builtins() {
    let i18n = catalog("en");
    assert_eq!(i18n.translate("common.save"), "Save");
    i18n.set_source(FakeSource::new());
    i18n.sync().await.unwrap();
    assert_eq!(i18n.translate("common.save"), "Save!");
    // Keys only in the built-in bundle still resolve.
    assert_eq!(i18n.translate("only_en"), "English only");
}

#[test]
fn corrupt_cache_is_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("bundles")).unwrap();
    std::fs::write(dir.path().join("bundles/ar.json"), b"garbage").unwrap();
    std::fs::write(dir.path().join("config.json"), b"garbage").unwrap();
    let i18n = I18n::new(I18nOptions {
        locale: "ar".into(),
        cache_dir: Some(dir.path().to_string_lossy().into_owned()),
        ..I18nOptions::default()
    })
    .unwrap();
    assert_eq!(i18n.translate("x"), "x");
    assert!(
        I18n::new(I18nOptions {
            locale: "".into(),
            ..I18nOptions::default()
        })
        .is_err()
    );
}
