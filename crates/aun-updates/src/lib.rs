//! App update checks for every native and desktop app (wiki `epics/AUN-APP-UPDATES-app-updates.md`).
//!
//! Plain Rust, no I/O: the apps fetch `GET /api/v1/app-config`, the key list and the manifest with
//! their own HTTP stack (or `aun-ffi`'s `ApiClient`), and this crate decides what is trusted and
//! what to show. The rules mirror the server's (`@al-ummah/shared-types` `app-config.ts` and
//! `app-updates.ts`); `spec/app-updates/` holds the shared test vectors both sides run.
//!
//! - [`version`]: 3- and 4-part versions with semver pre-release precedence.
//! - [`verify`]: the root-signed key list (`update-keys.json`) and the signed release manifest
//!   (`app-release.json`): signatures first, then contents, repo, tag, channel, no downgrade,
//!   replay floor, revoked keys and yanked versions.
//! - [`hosts`]: which download URLs and redirect hops are allowed.
//! - [`state`]: what an app remembers between checks.
//! - [`decide`]: the notice, the forced screen, or nothing.

pub mod decide;
pub mod hosts;
pub mod state;
pub mod verify;
pub mod version;

pub use decide::{Prompt, PromptKind, UpdateAnswer, decide, offline_kind};
pub use hosts::allowed_download_url;
pub use state::UpdateState;
pub use verify::{Expect, KeyList, Manifest, VerifyError, accept_key_list, verify_manifest};
pub use version::{compare, compare_cores, is_valid};
