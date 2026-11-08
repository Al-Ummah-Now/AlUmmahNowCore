//! [`I18n`]: the stateful handle apps hold. It switches locales at runtime, looks keys up along the
//! fallback chain, formats messages, and syncs bundles from api-videos into the disk cache.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use aun_common::platform::{Clock, SystemClock};
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::bidi::Direction;
use crate::bundle::Bundle;
use crate::cache::{CachedBundle, DiskCache};
use crate::locale::{fallback_chain, locale_direction, normalize_locale};
use crate::message::{FormatArg, FormatContext, format_message};
use crate::numbers::{self, NumberFormatOptions, NumeralSystem};
use crate::plural::plural_category;
use crate::source::{LocaleInfo, RemoteConfig, TranslationSource};

/// Construction options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct I18nOptions {
    /// Initial UI locale (usually the OS preference); normalised.
    pub locale: String,
    /// Last-resort locale until the server config says otherwise.
    pub fallback_locale: String,
    /// Directory for cached bundles; `None` keeps everything in memory.
    pub cache_dir: Option<String>,
    pub numerals: NumeralSystem,
    /// Wrap text arguments in FSI…PDI when their direction differs from the message's.
    pub bidi_isolation: bool,
}

impl Default for I18nOptions {
    fn default() -> Self {
        Self {
            locale: "en".into(),
            fallback_locale: "en".into(),
            cache_dir: None,
            numerals: NumeralSystem::Latin,
            bidi_isolation: true,
        }
    }
}

/// Sent to listeners when the locale changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocaleChange {
    pub locale: String,
    pub direction: Direction,
    pub fallback_chain: Vec<String>,
    /// True when no bundle is cached for any locale of the chain except the fallback: call
    /// [`I18n::sync`] to download it.
    pub needs_sync: bool,
}

/// Result of [`I18n::sync`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SyncReport {
    pub version: String,
    /// Locales whose bundle was downloaded.
    pub updated: Vec<String>,
    /// Locales already up to date (checksum match or 304).
    pub unchanged: Vec<String>,
}

/// Locale events, implemented natively (UI refresh, `layoutDirection` flip). Called on the thread
/// that changed the locale or ran the sync; never while an internal lock is held.
pub trait LocaleListener: Send + Sync {
    fn on_locale_changed(&self, change: LocaleChange);
    fn on_bundles_updated(&self, locales: Vec<String>);
}

#[derive(Debug)]
struct State {
    locale: String,
    fallback: String,
    chain: Vec<String>,
    numerals: NumeralSystem,
    /// Downloaded (or cached) bundles, by locale.
    remote: HashMap<String, Bundle>,
    /// Checksums of the remote bundles.
    checksums: HashMap<String, Option<String>>,
    /// Bundles embedded in the app (lower priority than downloaded ones).
    builtin: HashMap<String, Bundle>,
    config: Option<RemoteConfig>,
    missing: HashSet<String>,
}

/// Translation catalog and locale state. Create one per app and share the `Arc`.
pub struct I18n {
    options: I18nOptions,
    state: RwLock<State>,
    cache: Option<DiskCache>,
    source: RwLock<Option<Arc<dyn TranslationSource>>>,
    listeners: Mutex<Vec<(u64, Arc<dyn LocaleListener>)>>,
    next_listener: std::sync::atomic::AtomicU64,
    clock: Arc<dyn Clock>,
    sync_lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for I18n {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("I18n")
            .field("locale", &self.locale())
            .field("cache", &self.cache.as_ref().map(|c| c.dir().to_path_buf()))
            .finish()
    }
}

/// Keys missing in every bundle are remembered (for QA tooling) up to this many.
const MAX_MISSING: usize = 500;

impl I18n {
    /// Creates the catalog and loads cached bundles from `cache_dir` (if any). A corrupt cache is
    /// skipped, never fatal.
    pub fn new(options: I18nOptions) -> Result<Arc<Self>> {
        Self::with_clock(options, Arc::new(SystemClock))
    }

    pub fn with_clock(options: I18nOptions, clock: Arc<dyn Clock>) -> Result<Arc<Self>> {
        let locale = normalize_locale(&options.locale)
            .ok_or_else(|| Error::InvalidInput(format!("bad locale {:?}", options.locale)))?;
        let mut fallback = normalize_locale(&options.fallback_locale).unwrap_or_else(|| "en".into());
        let cache = options.cache_dir.as_ref().map(DiskCache::new);
        let mut remote = HashMap::new();
        let mut checksums = HashMap::new();
        let mut config = None;
        if let Some(cache) = &cache {
            match cache.load_config() {
                Ok(Some(c)) => {
                    if let Some(f) = normalize_locale(&c.fallback_locale) {
                        fallback = f;
                    }
                    config = Some(c);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "ignoring unreadable i18n config cache"),
            }
            for cached in cache.load_all_bundles() {
                checksums.insert(cached.locale.clone(), cached.checksum.clone());
                remote.insert(
                    cached.locale.clone(),
                    Bundle::from_json(&cached.locale, &cached.version, &cached.translations),
                );
            }
        }
        let chain = fallback_chain(&locale, &fallback);
        Ok(Arc::new(Self {
            state: RwLock::new(State {
                locale,
                fallback,
                chain,
                numerals: options.numerals,
                remote,
                checksums,
                builtin: HashMap::new(),
                config,
                missing: HashSet::new(),
            }),
            options,
            cache,
            source: RwLock::new(None),
            listeners: Mutex::new(Vec::new()),
            next_listener: std::sync::atomic::AtomicU64::new(1),
            clock,
            sync_lock: tokio::sync::Mutex::new(()),
        }))
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|p| p.into_inner())
    }

    /// Installs the network source used by [`I18n::sync`].
    pub fn set_source(&self, source: Arc<dyn TranslationSource>) {
        *self.source.write().unwrap_or_else(|p| p.into_inner()) = Some(source);
    }

    pub fn add_listener(&self, listener: Arc<dyn LocaleListener>) -> u64 {
        let id = self
            .next_listener
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((id, listener));
        id
    }

    pub fn remove_listener(&self, id: u64) {
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(i, _)| *i != id);
    }

    fn listeners(&self) -> Vec<Arc<dyn LocaleListener>> {
        self.listeners
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(_, l)| l.clone())
            .collect()
    }

    /// Registers strings shipped inside the app (same JSON shape as the server bundle). Downloaded
    /// bundles take priority; built-ins make the first launch work offline.
    pub fn add_builtin_bundle(&self, locale: &str, version: &str, translations_json: &str) -> Result<()> {
        let locale =
            normalize_locale(locale).ok_or_else(|| Error::InvalidInput(format!("bad locale {locale:?}")))?;
        let value: serde_json::Value = serde_json::from_str(translations_json)
            .map_err(|e| Error::InvalidInput(format!("bundle JSON for {locale}: {e}")))?;
        let bundle = Bundle::from_json(&locale, version, &value);
        self.write().builtin.insert(locale, bundle);
        Ok(())
    }

    /// Switches the UI locale immediately (cached/built-in bundles) and notifies listeners. Returns
    /// the change; when `needs_sync` is set, call [`I18n::sync`] to fetch the missing bundle.
    pub fn set_locale(&self, locale: &str) -> Result<LocaleChange> {
        let norm =
            normalize_locale(locale).ok_or_else(|| Error::InvalidInput(format!("bad locale {locale:?}")))?;
        let change = {
            let mut st = self.write();
            st.locale = norm.clone();
            st.chain = fallback_chain(&norm, &st.fallback);
            st.missing.clear();
            Self::change_of(&st)
        };
        for l in self.listeners() {
            l.on_locale_changed(change.clone());
        }
        Ok(change)
    }

    fn change_of(st: &State) -> LocaleChange {
        let has_own = st
            .chain
            .iter()
            .filter(|l| **l != st.fallback)
            .any(|l| st.remote.contains_key(l) || st.builtin.contains_key(l));
        let needs_sync = st.locale != st.fallback && !has_own;
        LocaleChange {
            locale: st.locale.clone(),
            direction: locale_direction(&st.locale),
            fallback_chain: st.chain.clone(),
            needs_sync,
        }
    }

    pub fn locale(&self) -> String {
        self.read().locale.clone()
    }

    pub fn direction(&self) -> Direction {
        locale_direction(&self.read().locale)
    }

    pub fn fallback_chain(&self) -> Vec<String> {
        self.read().chain.clone()
    }

    pub fn numeral_system(&self) -> NumeralSystem {
        self.read().numerals
    }

    /// Changes the digit system (user setting "Eastern Arabic numerals"); notifies listeners as a
    /// locale change so the UI re-renders.
    pub fn set_numeral_system(&self, numerals: NumeralSystem) {
        let change = {
            let mut st = self.write();
            st.numerals = numerals;
            Self::change_of(&st)
        };
        for l in self.listeners() {
            l.on_locale_changed(change.clone());
        }
    }

    /// Locales the server offers (from the cached config), else those with a bundle.
    pub fn available_locales(&self) -> Vec<LocaleInfo> {
        let st = self.read();
        if let Some(c) = &st.config {
            return c.locales.clone();
        }
        let mut codes: Vec<&String> = st.remote.keys().chain(st.builtin.keys()).collect();
        codes.sort();
        codes.dedup();
        codes
            .into_iter()
            .map(|c| LocaleInfo {
                code: c.clone(),
                name: c.clone(),
                english_name: c.clone(),
                is_rtl: locale_direction(c) == Direction::Rtl,
                is_default: *c == st.fallback,
                is_complete: true,
            })
            .collect()
    }

    /// Whether `key` resolves in any locale of the chain.
    pub fn has(&self, key: &str) -> bool {
        let st = self.read();
        st.chain
            .iter()
            .any(|l| Self::bundle_lookup(&st, l, key).is_some())
    }

    fn bundle_lookup<'a>(st: &'a State, locale: &str, key: &str) -> Option<&'a str> {
        st.remote
            .get(locale)
            .and_then(|b| b.get(key))
            .or_else(|| st.builtin.get(locale).and_then(|b| b.get(key)))
    }

    /// Finds the pattern for `key` along the chain, with plural variants when a `count` argument
    /// is present: `key_<category>`, `key.<category>`, then `key_other`/`key.other`, then `key`.
    /// The category is computed with the rules of the locale the pattern comes from.
    fn resolve(&self, st: &State, key: &str, count: Option<&FormatArg>) -> Option<(String, String)> {
        for locale in &st.chain {
            if let Some(ops) = count.and_then(FormatArg::plural_operands) {
                let cat = plural_category(locale, ops).as_str();
                for k in [
                    format!("{key}_{cat}"),
                    format!("{key}.{cat}"),
                    format!("{key}_other"),
                    format!("{key}.other"),
                ] {
                    if let Some(p) = Self::bundle_lookup(st, locale, &k) {
                        return Some((locale.clone(), p.to_owned()));
                    }
                }
            }
            if let Some(p) = Self::bundle_lookup(st, locale, key) {
                return Some((locale.clone(), p.to_owned()));
            }
        }
        None
    }

    /// The message for `key` without arguments; the key itself when missing everywhere.
    pub fn translate(&self, key: &str) -> String {
        self.format(key, &HashMap::new())
    }

    /// The message for `key` formatted with `args` (see [`crate::message`]). A `count` argument
    /// selects i18next plural keys. Missing keys return the key and are recorded.
    pub fn format(&self, key: &str, args: &HashMap<String, FormatArg>) -> String {
        let (found, numerals) = {
            let st = self.read();
            (self.resolve(&st, key, args.get("count")), st.numerals)
        };
        match found {
            Some((locale, pattern)) => format_message(
                &pattern,
                args,
                &FormatContext {
                    locale: &locale,
                    numerals,
                    bidi_isolation: self.options.bidi_isolation,
                },
            ),
            None => {
                let mut st = self.write();
                if st.missing.len() < MAX_MISSING {
                    st.missing.insert(key.to_owned());
                }
                key.to_owned()
            }
        }
    }

    /// Formats an arbitrary pattern (not from a bundle) in the current locale.
    pub fn format_pattern(&self, pattern: &str, args: &HashMap<String, FormatArg>) -> String {
        let st = self.read();
        format_message(
            pattern,
            args,
            &FormatContext {
                locale: &st.locale,
                numerals: st.numerals,
                bidi_isolation: self.options.bidi_isolation,
            },
        )
    }

    /// Keys looked up since the last locale change that no bundle had (sorted).
    pub fn missing_keys(&self) -> Vec<String> {
        let mut v: Vec<String> = self.read().missing.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn format_number(&self, value: f64, options: NumberFormatOptions) -> String {
        let st = self.read();
        numbers::format_number(value, &st.locale, st.numerals, options)
    }

    pub fn format_integer(&self, value: i64) -> String {
        let st = self.read();
        numbers::format_integer(value, &st.locale, st.numerals)
    }

    pub fn format_duration_clock(&self, duration_ms: u64) -> String {
        numbers::format_duration_clock(duration_ms, self.read().numerals)
    }

    pub fn format_duration_compact(&self, duration_ms: u64) -> String {
        let st = self.read();
        numbers::format_duration_compact(duration_ms, &st.locale, st.numerals)
    }

    /// Refreshes the config and the bundles of the current fallback chain from the source:
    /// metadata checksums decide what to download (`If-None-Match` as a second guard). Results go to
    /// the disk cache. On failure (e.g. offline) the cached bundles stay in use and the error is
    /// returned for the caller to retry later. Concurrent calls are serialised.
    pub async fn sync(&self) -> Result<SyncReport> {
        let source = self
            .source
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| Error::Unsupported("no translation source installed".into()))?;
        let _guard = self.sync_lock.lock().await;

        let config = source.config().await?;
        let metadata = source.metadata().await?;
        if let Some(cache) = &self.cache {
            cache.save_config(&config)?;
        }
        let chain = {
            let mut st = self.write();
            if let Some(f) = normalize_locale(&config.fallback_locale) {
                st.fallback = f;
            }
            st.chain = fallback_chain(&st.locale, &st.fallback);
            st.config = Some(config);
            st.chain.clone()
        };
        let offered: HashSet<String> = metadata
            .locales
            .iter()
            .filter_map(|l| normalize_locale(l))
            .collect();

        let mut report = SyncReport {
            version: metadata.version.clone(),
            ..SyncReport::default()
        };
        for locale in chain.iter().filter(|l| offered.contains(*l)) {
            let server_sum = metadata.checksums.get(locale).cloned();
            let (cached_sum, have) = {
                let st = self.read();
                (
                    st.checksums.get(locale).cloned().flatten(),
                    st.remote.contains_key(locale),
                )
            };
            if have && server_sum.is_some() && cached_sum == server_sum {
                report.unchanged.push(locale.clone());
                continue;
            }
            let etag = if have { cached_sum.as_deref() } else { None };
            match source.translations(locale, etag).await? {
                None => report.unchanged.push(locale.clone()),
                Some(remote) => {
                    let cached = CachedBundle {
                        locale: locale.clone(),
                        version: remote.version.clone(),
                        checksum: server_sum.clone(),
                        updated_at: remote.updated_at.clone(),
                        fetched_at_ms: self.clock.now_ms(),
                        translations: remote.translations,
                    };
                    if let Some(cache) = &self.cache {
                        cache.save_bundle(&cached)?;
                    }
                    let bundle = Bundle::from_json(locale, &cached.version, &cached.translations);
                    let mut st = self.write();
                    st.remote.insert(locale.clone(), bundle);
                    st.checksums.insert(locale.clone(), server_sum);
                    st.missing.clear();
                    report.updated.push(locale.clone());
                }
            }
        }
        if !report.updated.is_empty() {
            for l in self.listeners() {
                l.on_bundles_updated(report.updated.clone());
            }
        }
        Ok(report)
    }
}
