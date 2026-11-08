//! OAuth 2 sign-in for the native apps (Epic 11.3, 9.1, 9.2): authorization code + PKCE S256 in
//! the system browser, the device authorization grant (RFC 8628) for TVs and cars, multiple
//! accounts per device with an active account and channel, and tokens kept only in the OS secure
//! store through the [`aun_common::platform::SecureStorage`] adapter.
//!
//! [`AccountTokens`] (one fixed account) implements [`aun_api::AuthProvider`], so an `ApiClient`
//! built with it always sends a fresh token for that account and refreshes once on a 401
//! (single-flight, refresh-token-rotation safe).

pub mod accounts;
pub mod manager;
pub mod oauth;
pub mod pkce;

pub use accounts::{AccountInfo, AccountStore, MAX_ACCOUNTS};
pub use manager::{AccountTokens, AuthEvent, AuthListener, AuthManager, DeviceLogin, REFRESH_SKEW_MS};
pub use oauth::{DeviceAuthorization, DevicePoll, OAuthClient, OAuthConfig, PendingAuthorization, TokenSet};
