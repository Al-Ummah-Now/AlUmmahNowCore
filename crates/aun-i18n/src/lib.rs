//! Translation bundles, plural rules, locale switching and bidi helpers (Epic 11.4).
//!
//! - [`I18n`] is the stateful catalog the apps hold. It provides runtime locale switching with
//!   [`LocaleListener`] callbacks, lookup along the fallback chain (`ar-EG` → `ar` → `en`),
//!   formatting with named arguments, and [`I18n::sync`] from api-videos (`/api/i18n/*`) into an
//!   on-disk cache validated by checksums.
//! - [`plural`]: CLDR cardinal rules (Arabic zero/one/two/few/many/other, English, Urdu, Turkish,
//!   Indonesian, Malay, French, …).
//! - [`message`]: `{{name}}`, `{name}`, ICU `plural`/`select`/`number`.
//! - [`numbers`]: Latin / Eastern Arabic digits, grouping, durations.
//! - [`bidi`]: direction detection and FSI/PDI isolation.

pub mod bidi;
mod bundle;
pub mod cache;
mod catalog;
pub mod locale;
pub mod message;
pub mod numbers;
pub mod plural;
pub mod source;

pub use bidi::Direction;
pub use bundle::Bundle;
pub use catalog::{I18n, I18nOptions, LocaleChange, LocaleListener, SyncReport};
pub use locale::{fallback_chain, locale_direction, normalize_locale};
pub use message::{FormatArg, FormatContext, format_message};
pub use numbers::{NumberFormatOptions, NumeralSystem};
pub use plural::{PluralCategory, PluralOperands, plural_categories, plural_category};
pub use source::{
    ApiTranslationSource, LocaleInfo, RemoteBundle, RemoteConfig, RemoteMetadata, TranslationSource,
};
