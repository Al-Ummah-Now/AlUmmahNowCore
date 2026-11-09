//! FFI facade for `aun-auth`.

use std::sync::Arc;

use crate::AunError;
use crate::api::{ApiClient, ApiClientConfig};
use crate::platform::{ForeignSecureStorage, SecureStorage};

/// OAuth settings for one registered public client (Epic 9.1).
#[derive(Debug, Clone, uniffi::Record)]
pub struct AuthConfig {
    /// e.g. `https://auth.qa.alummahnow.net`
    pub issuer: String,
    /// e.g. `viewer-mobile`, `viewer-tv`, `studio-desktop`
    pub client_id: String,
    /// e.g. `alummahnow://auth/callback`
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    /// Endpoints; when `None`, `{issuer}/authorize` and `{issuer}/token` are used. Prefer
    /// `auth_manager_discover` which reads them from the server.
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub device_authorization_endpoint: Option<String>,
}

/// A signed-in account (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AccountInfo {
    pub user_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub active_channel_id: Option<String>,
    /// The refresh token was rejected: prompt to sign in again.
    pub needs_reauth: bool,
    pub added_at_ms: i64,
    pub last_used_at_ms: i64,
}

impl From<aun_auth::AccountInfo> for AccountInfo {
    fn from(a: aun_auth::AccountInfo) -> Self {
        Self {
            user_id: a.user_id,
            email: a.email,
            display_name: a.display_name,
            avatar_url: a.avatar_url,
            active_channel_id: a.active_channel_id,
            needs_reauth: a.needs_reauth,
            added_at_ms: a.added_at_ms,
            last_used_at_ms: a.last_used_at_ms,
        }
    }
}

/// The `prompt` sent to `/authorize`. Use `Default` for the normal sign-in path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum LoginPrompt {
    /// No `prompt`: silent when the browser is signed in, the form otherwise.
    Default,
    /// `prompt=none`: no UI; `complete_login` throws `Unauthorized` (`login_required`) when the
    /// browser has no session. Use after `ReauthRequired` before showing sign-in.
    Silent,
    /// `prompt=login`: always the form ("Add account").
    Login,
    /// `prompt=select_account`: the account chooser (explicit "Choose account" only).
    SelectAccount,
}

impl From<LoginPrompt> for aun_auth::LoginPrompt {
    fn from(p: LoginPrompt) -> Self {
        match p {
            LoginPrompt::Default => aun_auth::LoginPrompt::Default,
            LoginPrompt::Silent => aun_auth::LoginPrompt::Silent,
            LoginPrompt::Login => aun_auth::LoginPrompt::Login,
            LoginPrompt::SelectAccount => aun_auth::LoginPrompt::SelectAccount,
        }
    }
}

/// Open `url` in ASWebAuthenticationSession / Custom Tabs / the system browser.
#[derive(Debug, Clone, uniffi::Record)]
pub struct LoginRequest {
    pub url: String,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum AuthEvent {
    SignedIn { account: AccountInfo },
    SignedOut { user_id: String },
    ActiveChanged { user_id: Option<String> },
    ReauthRequired { user_id: String },
}

impl From<aun_auth::AuthEvent> for AuthEvent {
    fn from(e: aun_auth::AuthEvent) -> Self {
        match e {
            aun_auth::AuthEvent::SignedIn(a) => AuthEvent::SignedIn { account: a.into() },
            aun_auth::AuthEvent::SignedOut { user_id } => AuthEvent::SignedOut { user_id },
            aun_auth::AuthEvent::ActiveChanged { user_id } => AuthEvent::ActiveChanged { user_id },
            aun_auth::AuthEvent::ReauthRequired { user_id } => AuthEvent::ReauthRequired { user_id },
        }
    }
}

/// Receives account events (called on a background thread).
#[uniffi::export(with_foreign)]
pub trait AuthListener: Send + Sync {
    fn on_event(&self, event: AuthEvent);
}

struct ForeignAuthListener(Arc<dyn AuthListener>);

impl aun_auth::AuthListener for ForeignAuthListener {
    fn on_event(&self, event: aun_auth::AuthEvent) {
        self.0.on_event(event.into());
    }
}

/// A device-code sign-in (TV, car, console): show `user_code` and a QR of
/// `verification_uri_complete`, then await `AuthManager.complete_device_login`.
#[derive(Debug, uniffi::Object)]
pub struct DeviceLogin {
    inner: aun_auth::DeviceLogin,
}

#[uniffi::export]
impl DeviceLogin {
    pub fn user_code(&self) -> String {
        self.inner.authorization.user_code.clone()
    }
    pub fn verification_uri(&self) -> String {
        self.inner.authorization.verification_uri.clone()
    }
    pub fn verification_uri_complete(&self) -> Option<String> {
        self.inner.authorization.verification_uri_complete.clone()
    }
    pub fn expires_in_ms(&self) -> u64 {
        self.inner.authorization.expires_in * 1000
    }
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

/// Sign-in, accounts and tokens. One per app.
#[derive(Debug, uniffi::Object)]
pub struct AuthManager {
    inner: Arc<aun_auth::AuthManager>,
}

fn http() -> Result<reqwest::Client, AunError> {
    Ok(aun_api::http_client(&aun_api::ApiConfig::new(
        "https://localhost",
    ))?)
}

/// Builds an [`AuthManager`] from the issuer's `/.well-known/oauth-authorization-server`.
#[uniffi::export(async_runtime = "tokio")]
pub async fn auth_manager_discover(
    issuer: String,
    client_id: String,
    redirect_uri: String,
    scopes: Vec<String>,
    storage: Arc<dyn SecureStorage>,
) -> Result<Arc<AuthManager>, AunError> {
    let http = http()?;
    let cfg = aun_auth::OAuthConfig::discover(&http, &issuer, client_id, redirect_uri, scopes).await?;
    Ok(Arc::new(AuthManager {
        inner: Arc::new(aun_auth::AuthManager::new(
            aun_auth::OAuthClient::new(cfg, http),
            Arc::new(ForeignSecureStorage(storage)),
        )),
    }))
}

#[uniffi::export(async_runtime = "tokio")]
impl AuthManager {
    #[uniffi::constructor]
    pub fn new(config: AuthConfig, storage: Arc<dyn SecureStorage>) -> Result<Arc<Self>, AunError> {
        let issuer = config.issuer.trim_end_matches('/').to_owned();
        let cfg = aun_auth::OAuthConfig {
            authorization_endpoint: config
                .authorization_endpoint
                .unwrap_or_else(|| format!("{issuer}/authorize")),
            token_endpoint: config.token_endpoint.unwrap_or_else(|| format!("{issuer}/token")),
            device_authorization_endpoint: config.device_authorization_endpoint,
            issuer,
            client_id: config.client_id,
            redirect_uri: config.redirect_uri,
            scopes: config.scopes,
        };
        Ok(Arc::new(Self {
            inner: Arc::new(aun_auth::AuthManager::new(
                aun_auth::OAuthClient::new(cfg, http()?),
                Arc::new(ForeignSecureStorage(storage)),
            )),
        }))
    }

    pub fn add_listener(&self, listener: Arc<dyn AuthListener>) {
        self.inner.add_listener(Arc::new(ForeignAuthListener(listener)));
    }

    /// Step 1 of browser sign-in.
    pub fn begin_login(&self) -> Result<LoginRequest, AunError> {
        let p = self.inner.begin_login()?;
        Ok(LoginRequest {
            url: p.url,
            state: p.state,
        })
    }

    /// Step 1 with an explicit `prompt` (`Silent` re-authorizes without UI, `Login` adds an account).
    pub fn begin_login_with_prompt(&self, prompt: LoginPrompt) -> Result<LoginRequest, AunError> {
        let p = self.inner.begin_login_with_prompt(prompt.into())?;
        Ok(LoginRequest {
            url: p.url,
            state: p.state,
        })
    }

    /// Step 2: pass the full redirect URL the app received.
    pub async fn complete_login(&self, redirect_url: String) -> Result<AccountInfo, AunError> {
        Ok(self.inner.complete_login(&redirect_url).await?.into())
    }

    /// Starts a device-code sign-in. Throws `Unsupported` until the auth server ships RFC 8628.
    pub async fn begin_device_login(&self) -> Result<Arc<DeviceLogin>, AunError> {
        Ok(Arc::new(DeviceLogin {
            inner: self.inner.begin_device_login().await?,
        }))
    }

    /// Polls until approved; throws `Cancelled` if denied or cancelled.
    pub async fn complete_device_login(&self, login: Arc<DeviceLogin>) -> Result<AccountInfo, AunError> {
        Ok(self.inner.complete_device_login(&login.inner).await?.into())
    }

    pub fn accounts(&self) -> Result<Vec<AccountInfo>, AunError> {
        Ok(self.inner.accounts()?.into_iter().map(Into::into).collect())
    }

    pub fn active_account(&self) -> Result<Option<AccountInfo>, AunError> {
        Ok(self.inner.active_account()?.map(Into::into))
    }

    pub fn switch_account(&self, user_id: String) -> Result<AccountInfo, AunError> {
        Ok(self.inner.switch_account(&user_id)?.into())
    }

    pub fn set_active_channel(&self, channel_id: Option<String>) -> Result<(), AunError> {
        Ok(self.inner.set_active_channel(channel_id)?)
    }

    pub fn sign_out(&self, user_id: String) -> Result<(), AunError> {
        Ok(self.inner.sign_out(&user_id)?)
    }

    /// A fresh access token for the active account (refreshes when close to expiry).
    pub async fn access_token(&self) -> Result<Option<String>, AunError> {
        Ok(self.inner.access_token().await?)
    }

    /// An API client bound to one account (`user_id`, or the active account when `None`): it
    /// keeps acting as that account after an account switch, and refreshes on 401. Create a new
    /// client after switching accounts.
    pub fn api_client(
        &self,
        config: ApiClientConfig,
        user_id: Option<String>,
    ) -> Result<Arc<ApiClient>, AunError> {
        let user_id = match user_id {
            Some(u) => u,
            None => {
                self.inner
                    .active_account()?
                    .map(|a| a.user_id)
                    .ok_or_else(|| AunError::Unauthorized {
                        detail: "no signed-in account".into(),
                    })?
            }
        };
        let client = ApiClient::new(config, None)?;
        let tokens = aun_auth::AccountTokens::new(self.inner.clone(), user_id.clone());
        Ok(Arc::new(ApiClient {
            inner: client
                .inner
                .clone()
                .with_auth(Arc::new(tokens))
                .with_account(user_id),
        }))
    }
}
