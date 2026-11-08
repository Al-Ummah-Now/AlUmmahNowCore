//! `AuthManager`: sign-in (browser PKCE or device code), the account list, switching, sign-out,
//! and always-fresh access tokens for `aun-api`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aun_common::platform::{Clock, SecureStorage, SystemClock};
use aun_common::{Error, Result};
use tokio_util::sync::CancellationToken;

use crate::accounts::{AccountInfo, AccountStore, MAX_ACCOUNTS, StoredAccount, identity_from_tokens};
use crate::oauth::{DeviceAuthorization, OAuthClient, PendingAuthorization, TokenSet, poll_until_granted};

/// Refresh when the access token has less than this left.
pub const REFRESH_SKEW_MS: i64 = 60_000;

/// Events for the app (update the account switcher, prompt to sign in again).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthEvent {
    SignedIn(AccountInfo),
    SignedOut {
        user_id: String,
    },
    ActiveChanged {
        user_id: Option<String>,
    },
    /// The refresh token was rejected; the account stays listed with `needs_reauth`.
    ReauthRequired {
        user_id: String,
    },
}

pub trait AuthListener: Send + Sync {
    fn on_event(&self, event: AuthEvent);
}

/// A device-code sign-in in progress (TVs, cars, consoles).
#[derive(Debug, Clone)]
pub struct DeviceLogin {
    pub authorization: DeviceAuthorization,
    cancel: CancellationToken,
}

impl DeviceLogin {
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

pub struct AuthManager {
    oauth: OAuthClient,
    store: AccountStore,
    clock: Arc<dyn Clock>,
    pending: Mutex<HashMap<String, PendingAuthorization>>,
    /// Serialises refreshes so concurrent API calls trigger one token request (rotation-safe).
    refresh_lock: tokio::sync::Mutex<()>,
    /// Orders sign-out against writing refreshed tokens back, so a refresh that was in flight
    /// during sign-out can't resurrect the account.
    store_lock: Mutex<()>,
    listeners: Mutex<Vec<Arc<dyn AuthListener>>>,
}

impl std::fmt::Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager")
            .field("client_id", &self.oauth.config.client_id)
            .finish()
    }
}

impl AuthManager {
    pub fn new(oauth: OAuthClient, storage: Arc<dyn SecureStorage>) -> Self {
        Self::with_clock(oauth, storage, Arc::new(SystemClock))
    }

    pub fn with_clock(oauth: OAuthClient, storage: Arc<dyn SecureStorage>, clock: Arc<dyn Clock>) -> Self {
        Self {
            oauth,
            store: AccountStore::new(storage),
            clock,
            pending: Mutex::new(HashMap::new()),
            refresh_lock: tokio::sync::Mutex::new(()),
            store_lock: Mutex::new(()),
            listeners: Mutex::new(Vec::new()),
        }
    }

    pub fn add_listener(&self, l: Arc<dyn AuthListener>) {
        if let Ok(mut v) = self.listeners.lock() {
            v.push(l);
        }
    }

    fn emit(&self, e: AuthEvent) {
        let ls = self.listeners.lock().map(|v| v.clone()).unwrap_or_default();
        for l in ls {
            l.on_event(e.clone());
        }
    }

    /// Step 1 of browser sign-in: returns the URL to open. Keep the returned `state`.
    pub fn begin_login(&self) -> Result<PendingAuthorization> {
        let p = self.oauth.authorize(&[])?;
        self.pending
            .lock()
            .map_err(|_| Error::Internal("poisoned".into()))?
            .insert(p.state.clone(), p.clone());
        Ok(p)
    }

    /// Step 2: the app received the redirect (`alummahnow://auth/callback?code=…&state=…`).
    pub async fn complete_login(&self, redirect_url: &str) -> Result<AccountInfo> {
        let state = reqwest::Url::parse(redirect_url)
            .ok()
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, _)| k == "state")
                    .map(|(_, v)| v.into_owned())
            })
            .ok_or_else(|| Error::InvalidInput("redirect has no state".into()))?;
        let pending = self
            .pending
            .lock()
            .map_err(|_| Error::Internal("poisoned".into()))?
            .remove(&state)
            .ok_or_else(|| Error::InvalidInput("unknown or already used sign-in attempt".into()))?;
        let tokens = self
            .oauth
            .exchange_code(&pending, redirect_url, self.clock.now_ms())
            .await?;
        self.add_account(tokens)
    }

    /// Starts a device-code sign-in: show `user_code` and a QR of `verification_uri_complete`.
    pub async fn begin_device_login(&self) -> Result<DeviceLogin> {
        Ok(DeviceLogin {
            authorization: self.oauth.device_authorization().await?,
            cancel: CancellationToken::new(),
        })
    }

    /// Polls until the user approves (or denies, or the code expires, or `cancel()`).
    pub async fn complete_device_login(&self, login: &DeviceLogin) -> Result<AccountInfo> {
        let tokens =
            poll_until_granted(&self.oauth, &login.authorization, &*self.clock, &login.cancel).await?;
        self.add_account(tokens)
    }

    fn add_account(&self, tokens: TokenSet) -> Result<AccountInfo> {
        let now = self.clock.now_ms();
        let mut info = identity_from_tokens(&tokens)?;
        let existing = self.store.get(&info.user_id)?;
        if existing.is_none() && self.store.list()?.len() >= MAX_ACCOUNTS {
            return Err(Error::InvalidInput(format!(
                "at most {MAX_ACCOUNTS} accounts per device"
            )));
        }
        info.added_at_ms = existing.as_ref().map_or(now, |e| e.info.added_at_ms);
        info.active_channel_id = existing.as_ref().and_then(|e| e.info.active_channel_id.clone());
        info.last_used_at_ms = now;
        info.needs_reauth = false;
        self.store.put(&StoredAccount {
            info: info.clone(),
            tokens,
        })?;
        self.store.set_active(Some(&info.user_id))?;
        self.emit(AuthEvent::SignedIn(info.clone()));
        self.emit(AuthEvent::ActiveChanged {
            user_id: Some(info.user_id.clone()),
        });
        Ok(info)
    }

    pub fn accounts(&self) -> Result<Vec<AccountInfo>> {
        Ok(self.store.list()?.into_iter().map(|a| a.info).collect())
    }

    pub fn active_account(&self) -> Result<Option<AccountInfo>> {
        match self.store.active()? {
            Some(id) => Ok(self.store.get(&id)?.map(|a| a.info)),
            None => Ok(None),
        }
    }

    pub fn switch_account(&self, user_id: &str) -> Result<AccountInfo> {
        let mut a = self
            .store
            .get(user_id)?
            .ok_or_else(|| Error::NotFound(format!("account {user_id}")))?;
        a.info.last_used_at_ms = self.clock.now_ms();
        self.store.put(&a)?;
        self.store.set_active(Some(user_id))?;
        self.emit(AuthEvent::ActiveChanged {
            user_id: Some(user_id.into()),
        });
        Ok(a.info)
    }

    /// Selects the channel the active account acts as.
    pub fn set_active_channel(&self, channel_id: Option<String>) -> Result<()> {
        let id = self
            .store
            .active()?
            .ok_or_else(|| Error::Unauthorized("no active account".into()))?;
        let mut a = self
            .store
            .get(&id)?
            .ok_or_else(|| Error::NotFound(format!("account {id}")))?;
        a.info.active_channel_id = channel_id;
        self.store.put(&a)
    }

    /// Removes the account and its tokens from this device. If it was active, the most recently
    /// used remaining account becomes active.
    pub fn sign_out(&self, user_id: &str) -> Result<()> {
        let was_active = self.store.active()?.as_deref() == Some(user_id);
        {
            let _g = self
                .store_lock
                .lock()
                .map_err(|_| Error::Internal("auth lock poisoned".into()))?;
            self.store.remove(user_id)?;
        }
        self.emit(AuthEvent::SignedOut {
            user_id: user_id.into(),
        });
        if was_active {
            let next = self.store.list()?.into_iter().next().map(|a| a.info.user_id);
            self.store.set_active(next.as_deref())?;
            self.emit(AuthEvent::ActiveChanged { user_id: next });
        }
        Ok(())
    }

    /// A valid access token for the active account, refreshing first if it expires soon.
    pub async fn access_token(&self) -> Result<Option<String>> {
        let Some(id) = self.store.active()? else {
            return Ok(None);
        };
        self.token_for(&id, false).await
    }

    /// Forces a refresh (after a 401) for the active account.
    pub async fn force_refresh(&self) -> Result<Option<String>> {
        let Some(id) = self.store.active()? else {
            return Ok(None);
        };
        self.token_for(&id, true).await
    }

    /// A valid access token for a specific account (not necessarily the active one).
    pub async fn token_for_account(&self, user_id: &str, force_refresh: bool) -> Result<Option<String>> {
        self.token_for(user_id, force_refresh).await
    }

    async fn token_for(&self, user_id: &str, force: bool) -> Result<Option<String>> {
        let snapshot = self.store.get(user_id)?;
        let Some(a) = snapshot else { return Ok(None) };
        if a.info.needs_reauth {
            return Ok(None);
        }
        let now = self.clock.now_ms();
        if !force && !a.tokens.expires_within(now, REFRESH_SKEW_MS) {
            return Ok(Some(a.tokens.access_token));
        }
        let seen = a.tokens.access_token;
        let _g = self.refresh_lock.lock().await;
        // Another caller may have refreshed while we waited: use its token (also after a 401,
        // which was for the old token) instead of spending the rotated refresh token again.
        let Some(mut a) = self.store.get(user_id)? else {
            return Ok(None);
        };
        if a.info.needs_reauth {
            return Ok(None);
        }
        if a.tokens.access_token != seen {
            return Ok(Some(a.tokens.access_token));
        }
        let Some(rt) = a.tokens.refresh_token.clone() else {
            return self.mark_reauth(a).map(|_| None);
        };
        match self.oauth.refresh(&rt, self.clock.now_ms()).await {
            Ok(t) => {
                a.tokens = t;
                if !self.put_if_present(&a)? {
                    return Ok(None); // signed out while refreshing
                }
                Ok(Some(a.tokens.access_token))
            }
            Err(Error::Unauthorized(_)) => self.mark_reauth(a).map(|_| None),
            Err(e) => Err(e),
        }
    }

    /// Saves `a` only if the account still exists (it may have been signed out meanwhile).
    fn put_if_present(&self, a: &StoredAccount) -> Result<bool> {
        let _g = self
            .store_lock
            .lock()
            .map_err(|_| Error::Internal("auth lock poisoned".into()))?;
        if self.store.get(&a.info.user_id)?.is_none() {
            return Ok(false);
        }
        self.store.put(a)?;
        Ok(true)
    }

    fn mark_reauth(&self, mut a: StoredAccount) -> Result<()> {
        a.info.needs_reauth = true;
        if !self.put_if_present(&a)? {
            return Ok(());
        }
        self.emit(AuthEvent::ReauthRequired {
            user_id: a.info.user_id,
        });
        Ok(())
    }
}

/// Tokens for one fixed account. API clients use this so an account switch elsewhere in the app
/// can never change which user a client (or its offline queue) acts as.
#[derive(Debug, Clone)]
pub struct AccountTokens {
    manager: Arc<AuthManager>,
    user_id: String,
}

impl AccountTokens {
    pub fn new(manager: Arc<AuthManager>, user_id: impl Into<String>) -> Self {
        Self {
            manager,
            user_id: user_id.into(),
        }
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

#[async_trait::async_trait]
impl aun_api::AuthProvider for AccountTokens {
    async fn access_token(&self) -> Option<String> {
        self.manager.token_for(&self.user_id, false).await.ok().flatten()
    }
    async fn refresh(&self) -> Result<Option<String>> {
        self.manager.token_for(&self.user_id, true).await
    }
}

/// Follows the *active* account. Prefer [`AccountTokens`] for API clients.
#[async_trait::async_trait]
impl aun_api::AuthProvider for AuthManager {
    async fn access_token(&self) -> Option<String> {
        AuthManager::access_token(self).await.ok().flatten()
    }
    async fn refresh(&self) -> Result<Option<String>> {
        self.force_refresh().await
    }
}
