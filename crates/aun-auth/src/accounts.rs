//! Multi-account storage over the OS secure store (mirrors the web `SessionManager`: up to 10
//! accounts, one active, an active channel per account).
//!
//! Layout in [`SecureStorage`]:
//! * `aun/auth/account/<user id>` → JSON [`StoredAccount`] (profile + tokens)
//! * `aun/auth/active` → the active user id

use std::sync::Arc;

use aun_common::platform::SecureStorage;
use aun_common::{Error, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::oauth::TokenSet;

pub const MAX_ACCOUNTS: usize = 10;
const ACCOUNT_PREFIX: &str = "aun/auth/account/";
const ACTIVE_KEY: &str = "aun/auth/active";

/// Public profile of a signed-in account (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AccountInfo {
    /// The token subject (`sub`).
    pub user_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    /// The channel this account acts as (sent as the `channelId` header).
    pub active_channel_id: Option<String>,
    /// Set when the refresh token was rejected: the app should prompt to sign in again.
    #[serde(default)]
    pub needs_reauth: bool,
    pub added_at_ms: i64,
    pub last_used_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredAccount {
    pub info: AccountInfo,
    pub tokens: TokenSet,
}

/// Reads and writes accounts in secure storage.
#[derive(Clone)]
pub struct AccountStore {
    storage: Arc<dyn SecureStorage>,
}

impl std::fmt::Debug for AccountStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AccountStore")
    }
}

impl AccountStore {
    pub fn new(storage: Arc<dyn SecureStorage>) -> Self {
        Self { storage }
    }

    fn key(user_id: &str) -> String {
        format!("{ACCOUNT_PREFIX}{user_id}")
    }

    pub fn get(&self, user_id: &str) -> Result<Option<StoredAccount>> {
        match self.storage.get(&Self::key(user_id))? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| Error::Storage(format!("account record: {e}"))),
            None => Ok(None),
        }
    }

    pub fn put(&self, account: &StoredAccount) -> Result<()> {
        self.storage
            .set(&Self::key(&account.info.user_id), &serde_json::to_vec(account)?)
    }

    /// All accounts, most recently used first.
    pub fn list(&self) -> Result<Vec<StoredAccount>> {
        let mut out = Vec::new();
        for key in self.storage.list(ACCOUNT_PREFIX)? {
            if let Some(id) = key.strip_prefix(ACCOUNT_PREFIX)
                && let Some(a) = self.get(id)?
            {
                out.push(a);
            }
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.info.last_used_at_ms));
        Ok(out)
    }

    pub fn remove(&self, user_id: &str) -> Result<()> {
        self.storage.delete(&Self::key(user_id))?;
        if self.active()?.as_deref() == Some(user_id) {
            self.storage.delete(ACTIVE_KEY)?;
        }
        Ok(())
    }

    pub fn active(&self) -> Result<Option<String>> {
        Ok(self
            .storage
            .get(ACTIVE_KEY)?
            .and_then(|b| String::from_utf8(b).ok())
            .filter(|s| !s.is_empty()))
    }

    pub fn set_active(&self, user_id: Option<&str>) -> Result<()> {
        match user_id {
            Some(id) => self.storage.set(ACTIVE_KEY, id.as_bytes()),
            None => self.storage.delete(ACTIVE_KEY),
        }
    }
}

/// Claims we read from a JWT to label the account. The signature is NOT verified here: the token
/// came straight from the token endpoint over TLS and is only used to identify the account
/// locally; the API verifies every token it receives.
#[derive(Debug, Default, Deserialize)]
pub struct IdentityClaims {
    pub sub: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    pub picture: Option<String>,
    /// OpenAuth puts the subject's properties here (`{type, properties: {id, email}}`).
    pub properties: Option<serde_json::Value>,
}

pub fn decode_claims(jwt: &str) -> Option<IdentityClaims> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Builds the profile for a token set (id_token preferred, then the access token).
pub fn identity_from_tokens(tokens: &TokenSet) -> Result<AccountInfo> {
    let claims = tokens
        .id_token
        .as_deref()
        .and_then(decode_claims)
        .or_else(|| decode_claims(&tokens.access_token))
        .ok_or_else(|| Error::Internal("token is not a JWT; cannot identify the account".into()))?;
    let prop = |k: &str| {
        claims
            .properties
            .as_ref()
            .and_then(|p| p.get(k))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let user_id = prop("id")
        .or(claims.sub.clone())
        .ok_or_else(|| Error::Internal("token has no subject".into()))?;
    Ok(AccountInfo {
        user_id,
        email: claims.email.clone().or_else(|| prop("email")),
        display_name: claims.name.clone().or_else(|| prop("name")),
        avatar_url: claims.picture.clone(),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aun_common::platform::MemorySecureStorage;

    fn jwt(payload: serde_json::Value) -> String {
        let e = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.sig",
            e(br#"{"alg":"none"}"#),
            e(payload.to_string().as_bytes())
        )
    }

    #[test]
    fn identity_from_openauth_access_token() {
        let t = TokenSet {
            access_token: jwt(
                serde_json::json!({"sub": "user:abc", "properties": {"id": "u1", "email": "a@example.com"}}),
            ),
            refresh_token: None,
            id_token: None,
            expires_at_ms: None,
            scope: None,
        };
        let i = identity_from_tokens(&t).unwrap();
        assert_eq!(i.user_id, "u1");
        assert_eq!(i.email.as_deref(), Some("a@example.com"));
        let opaque = TokenSet {
            access_token: "opaque".into(),
            ..t
        };
        assert!(identity_from_tokens(&opaque).is_err());
    }

    #[test]
    fn store_lists_by_recency_and_tracks_active() {
        let s = AccountStore::new(Arc::new(MemorySecureStorage::new()));
        for (id, used) in [("a", 1), ("b", 3), ("c", 2)] {
            s.put(&StoredAccount {
                info: AccountInfo {
                    user_id: id.into(),
                    last_used_at_ms: used,
                    ..Default::default()
                },
                tokens: TokenSet {
                    access_token: "t".into(),
                    refresh_token: None,
                    id_token: None,
                    expires_at_ms: None,
                    scope: None,
                },
            })
            .unwrap();
        }
        let ids: Vec<_> = s.list().unwrap().into_iter().map(|a| a.info.user_id).collect();
        assert_eq!(ids, ["b", "c", "a"]);
        s.set_active(Some("c")).unwrap();
        assert_eq!(s.active().unwrap().as_deref(), Some("c"));
        s.remove("c").unwrap();
        assert_eq!(s.active().unwrap(), None);
        assert_eq!(s.list().unwrap().len(), 2);
    }
}
