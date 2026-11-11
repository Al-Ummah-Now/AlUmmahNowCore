//! OAuth 2 protocol calls: discovery (RFC 8414), authorization-code + PKCE (RFC 6749 / 7636),
//! refresh with rotation, and the device authorization grant (RFC 8628).

use std::time::Duration;

use aun_common::security::ensure_secure_url;
use aun_common::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::pkce::{Pkce, random_token};

/// A registered public client (Epic 9.1: one per app, PKCE S256 required).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    /// e.g. `https://auth.qa.alummahnow.net`
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// RFC 8628 endpoint; `None` until the auth server ships it (Epic 9.2).
    pub device_authorization_endpoint: Option<String>,
    /// e.g. `viewer-mobile`, `studio-desktop`
    pub client_id: String,
    /// e.g. `alummahnow://auth/callback`
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Metadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    device_authorization_endpoint: Option<String>,
}

impl OAuthConfig {
    /// RFC 7009 revocation endpoint: `{issuer}/token/revoke` (the auth server does not advertise
    /// it in discovery metadata).
    pub fn revocation_endpoint(&self) -> String {
        format!("{}/token/revoke", self.issuer.trim_end_matches('/'))
    }

    /// The auth server's sign-out page (`{issuer}/logout`), which ends the browser session and
    /// signs every web app out. `return_to` must be an allowed Al Ummah Now page.
    pub fn logout_url(&self, return_to: Option<&str>) -> Result<String> {
        let base = format!("{}/logout", self.issuer.trim_end_matches('/'));
        ensure_secure_url(&base)?;
        let url = match return_to {
            Some(r) => reqwest::Url::parse_with_params(&base, &[("return", r)]),
            None => reqwest::Url::parse(&base),
        }
        .map_err(|e| Error::InvalidInput(format!("logout url: {e}")))?;
        Ok(url.into())
    }

    /// Reads `/.well-known/oauth-authorization-server` from the issuer.
    pub async fn discover(
        http: &reqwest::Client,
        issuer: &str,
        client_id: impl Into<String>,
        redirect_uri: impl Into<String>,
        scopes: Vec<String>,
    ) -> Result<Self> {
        ensure_secure_url(issuer)?;
        let url = format!(
            "{}/.well-known/oauth-authorization-server",
            issuer.trim_end_matches('/')
        );
        let resp = http.get(&url).send().await.map_err(net)?;
        if !resp.status().is_success() {
            return Err(Error::from_status(
                resp.status().as_u16(),
                None,
                format!("discovery: {url}"),
            ));
        }
        let m: Metadata = resp.json().await.map_err(net)?;
        ensure_secure_url(&m.authorization_endpoint)?;
        ensure_secure_url(&m.token_endpoint)?;
        if let Some(d) = &m.device_authorization_endpoint {
            ensure_secure_url(d)?;
        }
        Ok(Self {
            issuer: m.issuer,
            authorization_endpoint: m.authorization_endpoint,
            token_endpoint: m.token_endpoint,
            device_authorization_endpoint: m.device_authorization_endpoint,
            client_id: client_id.into(),
            redirect_uri: redirect_uri.into(),
            scopes,
        })
    }
}

/// Tokens from the token endpoint. `Debug` never prints secrets.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    /// Unix ms; `None` when the server didn't say.
    pub expires_at_ms: Option<i64>,
    pub scope: Option<String>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at_ms", &self.expires_at_ms)
            .field("scope", &self.scope)
            .finish()
    }
}

impl TokenSet {
    /// Whether the access token expires within `skew_ms` of `now_ms`.
    pub fn expires_within(&self, now_ms: i64, skew_ms: i64) -> bool {
        self.expires_at_ms.is_some_and(|exp| exp - now_ms <= skew_ms)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    error: String,
    error_description: Option<String>,
}

/// The OIDC `prompt` sent to `/authorize`.
///
/// The normal sign-in path uses [`LoginPrompt::Default`] (no `prompt`): the auth server signs the
/// user in silently as the browser session's active account and shows the form only when there is
/// no session. Never use `Login` or `SelectAccount` for the normal path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoginPrompt {
    /// No `prompt` parameter: silent when the browser has a session, the form otherwise.
    #[default]
    Default,
    /// `prompt=none`: never shows UI. Without a browser session the redirect carries
    /// `error=login_required`, which `exchange_code` returns as [`Error::Unauthorized`]. Use it to
    /// re-authorize after a rejected refresh token before asking the user to sign in.
    Silent,
    /// `prompt=login`: always shows the sign-in form ("Add account").
    Login,
    /// `prompt=select_account`: shows the account chooser (an explicit "Choose account").
    SelectAccount,
}

impl LoginPrompt {
    /// The `prompt` value, or `None` when no parameter is sent.
    pub fn as_param(self) -> Option<&'static str> {
        match self {
            LoginPrompt::Default => None,
            LoginPrompt::Silent => Some("none"),
            LoginPrompt::Login => Some("login"),
            LoginPrompt::SelectAccount => Some("select_account"),
        }
    }
}

/// An authorization request in flight: open `url` in the system browser
/// (ASWebAuthenticationSession / Custom Tabs / the default browser), then pass the redirect to
/// [`OAuthClient::exchange_code`].
#[derive(Clone)]
pub struct PendingAuthorization {
    pub url: String,
    pub state: String,
    pub pkce: Pkce,
}

impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingAuthorization")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// RFC 8628 §3.2 response.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DeviceAuthorization {
    pub device_code: String,
    /// Shown on the TV, e.g. `WDJB-MJHT`.
    pub user_code: String,
    /// e.g. `https://alummahnow.net/activate`
    pub verification_uri: String,
    /// The same with the code filled in: render as a QR code.
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// The outcome of one device-token poll.
#[derive(Debug, Clone, PartialEq)]
pub enum DevicePoll {
    Pending,
    /// The server asked us to back off: the new interval in seconds.
    SlowDown(u64),
    Granted(TokenSet),
}

/// Protocol client for one [`OAuthConfig`].
#[derive(Debug, Clone)]
pub struct OAuthClient {
    pub config: OAuthConfig,
    http: reqwest::Client,
}

fn net(e: reqwest::Error) -> Error {
    Error::Network(e.to_string())
}

impl OAuthClient {
    pub fn new(config: OAuthConfig, http: reqwest::Client) -> Self {
        Self { config, http }
    }

    /// Builds the authorization URL with a fresh `state` and PKCE S256 challenge.
    pub fn authorize(&self, extra: &[(&str, &str)]) -> Result<PendingAuthorization> {
        ensure_secure_url(&self.config.authorization_endpoint)?;
        let pkce = Pkce::generate()?;
        let state = random_token(16)?;
        let scope = self.config.scopes.join(" ");
        let mut params: Vec<(&str, &str)> = vec![
            ("response_type", "code"),
            ("client_id", &self.config.client_id),
            ("redirect_uri", &self.config.redirect_uri),
            ("state", &state),
            ("code_challenge", &pkce.challenge),
            ("code_challenge_method", Pkce::METHOD),
        ];
        if !scope.is_empty() {
            params.push(("scope", &scope));
        }
        params.extend_from_slice(extra);
        let url = reqwest::Url::parse_with_params(&self.config.authorization_endpoint, &params)
            .map_err(|e| Error::InvalidInput(format!("authorization endpoint: {e}")))?;
        Ok(PendingAuthorization {
            url: url.into(),
            state,
            pkce,
        })
    }

    /// Validates the redirect (`state`, `error`) and exchanges the code for tokens.
    pub async fn exchange_code(
        &self,
        pending: &PendingAuthorization,
        redirect_url: &str,
        now_ms: i64,
    ) -> Result<TokenSet> {
        let url =
            reqwest::Url::parse(redirect_url).map_err(|e| Error::InvalidInput(format!("redirect: {e}")))?;
        let expected = reqwest::Url::parse(&self.config.redirect_uri).ok();
        if let Some(exp) = expected
            && (exp.scheme() != url.scheme() || exp.host_str() != url.host_str() || exp.path() != url.path())
        {
            return Err(Error::InvalidInput(
                "redirect does not match the registered redirect_uri".into(),
            ));
        }
        let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        if let Some(err) = q.get("error") {
            let desc = q.get("error_description").cloned().unwrap_or_default();
            return Err(if err == "access_denied" {
                Error::Cancelled
            } else {
                Error::Unauthorized(format!("{err}: {desc}"))
            });
        }
        if q.get("state") != Some(&pending.state) {
            return Err(Error::InvalidInput("state mismatch (possible CSRF)".into()));
        }
        let code = q
            .get("code")
            .ok_or_else(|| Error::InvalidInput("redirect has no code".into()))?;
        self.token_request(
            &[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.config.redirect_uri),
                ("client_id", &self.config.client_id),
                ("code_verifier", &pending.pkce.verifier),
            ],
            now_ms,
        )
        .await
    }

    /// Refreshes. Servers rotate refresh tokens: when the response has none, the old one is kept.
    pub async fn refresh(&self, refresh_token: &str, now_ms: i64) -> Result<TokenSet> {
        let mut t = self
            .token_request(
                &[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", refresh_token),
                    ("client_id", &self.config.client_id),
                ],
                now_ms,
            )
            .await?;
        if t.refresh_token.is_none() {
            t.refresh_token = Some(refresh_token.to_owned());
        }
        Ok(t)
    }

    /// RFC 7009: revokes a refresh token (the server deletes its token family). The server answers
    /// 200 for unknown tokens too, so only transport errors and 5xx fail.
    pub async fn revoke_refresh_token(&self, refresh_token: &str) -> Result<()> {
        let endpoint = self.config.revocation_endpoint();
        ensure_secure_url(&endpoint)?;
        let form = [
            ("token", refresh_token),
            ("token_type_hint", "refresh_token"),
            ("client_id", self.config.client_id.as_str()),
        ];
        let resp = self.http.post(&endpoint).form(&form).send().await.map_err(net)?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        let body = resp.text().await.map_err(net)?;
        Err(oauth_error(status.as_u16(), &body))
    }

    /// RFC 8628 §3.1: asks for a device and user code.
    pub async fn device_authorization(&self) -> Result<DeviceAuthorization> {
        let endpoint = self
            .config
            .device_authorization_endpoint
            .as_deref()
            .ok_or_else(|| {
                Error::Unsupported(
                    "the auth server has no device_authorization_endpoint yet (Epic 9.2)".into(),
                )
            })?;
        ensure_secure_url(endpoint)?;
        let scope = self.config.scopes.join(" ");
        let mut form = vec![("client_id", self.config.client_id.as_str())];
        if !scope.is_empty() {
            form.push(("scope", &scope));
        }
        let resp = self.http.post(endpoint).form(&form).send().await.map_err(net)?;
        let status = resp.status();
        let body = resp.text().await.map_err(net)?;
        if !status.is_success() {
            return Err(oauth_error(status.as_u16(), &body));
        }
        serde_json::from_str(&body).map_err(|e| Error::Internal(format!("device authorization: {e}")))
    }

    /// RFC 8628 §3.4/3.5: one poll of the token endpoint.
    pub async fn poll_device_token(&self, device_code: &str, now_ms: i64) -> Result<DevicePoll> {
        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
            ("client_id", self.config.client_id.as_str()),
        ];
        ensure_secure_url(&self.config.token_endpoint)?;
        let resp = self
            .http
            .post(&self.config.token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        let body = resp.text().await.map_err(net)?;
        if status.is_success() {
            return parse_tokens(&body, now_ms).map(DevicePoll::Granted);
        }
        match serde_json::from_str::<OAuthErrorBody>(&body) {
            Ok(e) if e.error == "authorization_pending" => Ok(DevicePoll::Pending),
            Ok(e) if e.error == "slow_down" => Ok(DevicePoll::SlowDown(0)),
            Ok(e) if e.error == "access_denied" => Err(Error::Cancelled),
            Ok(e) if e.error == "expired_token" => Err(Error::Unauthorized(
                "the code expired before it was approved; start again".into(),
            )),
            _ => Err(oauth_error(status.as_u16(), &body)),
        }
    }

    async fn token_request(&self, form: &[(&str, &str)], now_ms: i64) -> Result<TokenSet> {
        ensure_secure_url(&self.config.token_endpoint)?;
        let resp = self
            .http
            .post(&self.config.token_endpoint)
            .form(form)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        let body = resp.text().await.map_err(net)?;
        if !status.is_success() {
            return Err(oauth_error(status.as_u16(), &body));
        }
        parse_tokens(&body, now_ms)
    }
}

fn parse_tokens(body: &str, now_ms: i64) -> Result<TokenSet> {
    let t: TokenResponse =
        serde_json::from_str(body).map_err(|e| Error::Internal(format!("token response: {e}")))?;
    Ok(TokenSet {
        access_token: t.access_token,
        refresh_token: t.refresh_token,
        id_token: t.id_token,
        expires_at_ms: t.expires_in.map(|s| now_ms + s * 1000),
        scope: t.scope,
    })
}

/// Maps RFC 6749 §5.2 errors. `invalid_grant` (expired/revoked refresh token) means sign-in again.
fn oauth_error(status: u16, body: &str) -> Error {
    match serde_json::from_str::<OAuthErrorBody>(body) {
        Ok(e) => {
            let msg = match e.error_description {
                Some(d) => format!("{}: {d}", e.error),
                None => e.error.clone(),
            };
            match e.error.as_str() {
                "invalid_grant" | "invalid_token" | "unauthorized_client" | "invalid_client" => {
                    Error::Unauthorized(msg)
                }
                "invalid_request" | "invalid_scope" | "unsupported_grant_type" => Error::InvalidInput(msg),
                _ => Error::from_status(status, Some(e.error), msg),
            }
        }
        Err(_) => Error::from_status(status, None, body.chars().take(200).collect::<String>()),
    }
}

/// Seconds to wait before the next device poll after a `slow_down` (RFC 8628 §3.5: +5 s).
pub fn slowed_interval(current: u64) -> u64 {
    current + 5
}

/// Polling helper used by [`crate::AuthManager`]; separate for testing with paused time.
pub async fn poll_until_granted(
    client: &OAuthClient,
    auth: &DeviceAuthorization,
    clock: &dyn aun_common::platform::Clock,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<TokenSet> {
    let mut interval = auth.interval.max(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(auth.expires_in);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Err(Error::Cancelled),
            _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Unauthorized(
                "the code expired before it was approved; start again".into(),
            ));
        }
        match client.poll_device_token(&auth.device_code, clock.now_ms()).await {
            Ok(DevicePoll::Granted(t)) => return Ok(t),
            Ok(DevicePoll::Pending) => {}
            Ok(DevicePoll::SlowDown(_)) => interval = slowed_interval(interval),
            // Transient network trouble while the user approves on their phone: keep polling.
            Err(e) if e.is_retryable() => interval = slowed_interval(interval).min(30),
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_prompt_params() {
        assert_eq!(LoginPrompt::default(), LoginPrompt::Default);
        assert_eq!(LoginPrompt::Default.as_param(), None);
        assert_eq!(LoginPrompt::Silent.as_param(), Some("none"));
        assert_eq!(LoginPrompt::Login.as_param(), Some("login"));
        assert_eq!(LoginPrompt::SelectAccount.as_param(), Some("select_account"));
    }

    #[test]
    fn slow_down_adds_five_seconds() {
        assert_eq!(slowed_interval(5), 10);
    }

    #[test]
    fn maps_oauth_errors() {
        assert!(matches!(
            oauth_error(400, r#"{"error":"invalid_grant"}"#),
            Error::Unauthorized(_)
        ));
        assert!(matches!(
            oauth_error(400, r#"{"error":"invalid_scope"}"#),
            Error::InvalidInput(_)
        ));
        assert!(matches!(
            oauth_error(503, "down"),
            Error::Http { status: 503, .. }
        ));
    }

    #[test]
    fn token_set_debug_redacts_and_expiry() {
        let t = parse_tokens(
            r#"{"access_token":"secret-at","refresh_token":"secret-rt","expires_in":60}"#,
            1_000,
        )
        .unwrap();
        assert_eq!(t.expires_at_ms, Some(61_000));
        assert!(t.expires_within(30_000, 60_000));
        assert!(!t.expires_within(0, 60_000));
        let d = format!("{t:?}");
        assert!(!d.contains("secret"));
    }
}
