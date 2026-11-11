//! aun-auth against a mock OAuth server (RFC 6749 / 7636 / 8628 behaviour).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use aun_auth::{AuthEvent, AuthListener, AuthManager, LoginPrompt, OAuthClient, OAuthConfig};
use aun_common::Error;
use aun_common::platform::{ManualClock, MemorySecureStorage};
use base64::Engine;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn jwt(sub: &str, email: &str) -> String {
    let e = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
    let payload =
        serde_json::json!({"sub": format!("user:{sub}"), "properties": {"id": sub, "email": email}});
    format!(
        "{}.{}.sig",
        e(br#"{"alg":"ES256"}"#),
        e(payload.to_string().as_bytes())
    )
}

/// No timeouts: tokio's paused clock would fire them instantly while waiting on real sockets.
fn http_untimed() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder().build().unwrap()
}

fn manager_untimed(server: &MockServer) -> AuthManager {
    let oauth = OAuthClient::new(config(server, true), http_untimed());
    AuthManager::with_clock(
        oauth,
        Arc::new(MemorySecureStorage::new()),
        Arc::new(ManualClock::new(0)),
    )
}

fn http() -> reqwest::Client {
    aun_api::http_client(&aun_api::ApiConfig::new("http://localhost")).unwrap()
}

fn config(server: &MockServer, device: bool) -> OAuthConfig {
    OAuthConfig {
        issuer: server.uri(),
        authorization_endpoint: format!("{}/authorize", server.uri()),
        token_endpoint: format!("{}/token", server.uri()),
        device_authorization_endpoint: device.then(|| format!("{}/device/code", server.uri())),
        revocation_endpoint: None,
        client_id: "viewer-mobile".into(),
        redirect_uri: "alummahnow://auth/callback".into(),
        scopes: vec!["openid".into(), "profile".into()],
    }
}

fn manager(server: &MockServer, clock: Arc<ManualClock>) -> AuthManager {
    let oauth = OAuthClient::new(config(server, true), http());
    AuthManager::with_clock(oauth, Arc::new(MemorySecureStorage::new()), clock)
}

fn token_body(sub: &str, access: &str, refresh: Option<&str>, expires_in: i64) -> serde_json::Value {
    let mut v = serde_json::json!({
        "access_token": access, "token_type": "bearer", "expires_in": expires_in,
        "id_token": jwt(sub, &format!("{sub}@example.com")),
    });
    if let Some(r) = refresh {
        v["refresh_token"] = r.into();
    }
    v
}

#[derive(Default)]
struct Events(Mutex<Vec<AuthEvent>>);
impl AuthListener for Events {
    fn on_event(&self, e: AuthEvent) {
        self.0.lock().unwrap().push(e);
    }
}

async fn sign_in(m: &AuthManager, server: &MockServer, sub: &str) {
    let pending = m.begin_login().unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(format!(
            "code_verifier={}",
            pending.pkce.verifier
        )))
        .and(body_string_contains(format!("code=code-{sub}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_body(
            sub,
            &format!("at-{sub}"),
            Some(&format!("rt-{sub}")),
            3600,
        )))
        .up_to_n_times(1)
        .mount(server)
        .await;
    let redirect = format!(
        "alummahnow://auth/callback?code=code-{sub}&state={}",
        pending.state
    );
    let info = m.complete_login(&redirect).await.unwrap();
    assert_eq!(info.user_id, sub);
}

#[tokio::test]
async fn discovery_reads_metadata() {
    let server = MockServer::start().await;
    Mock::given(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": server.uri(), "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()), "jwks_uri": "x",
        })))
        .mount(&server)
        .await;
    let c = OAuthConfig::discover(&http(), &server.uri(), "tv", "alummahnow://cb", vec![])
        .await
        .unwrap();
    assert_eq!(c.token_endpoint, format!("{}/token", server.uri()));
    assert_eq!(c.device_authorization_endpoint, None);
}

#[tokio::test]
async fn authorization_url_has_pkce_s256_and_state() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    let p = m.begin_login().unwrap();
    let url = reqwest::Url::parse(&p.url).unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["response_type"], "code");
    assert_eq!(q["client_id"], "viewer-mobile");
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["code_challenge"], aun_auth::pkce::s256(&p.pkce.verifier));
    assert_eq!(q["state"], p.state);
    assert_eq!(q["scope"], "openid profile");
}

#[tokio::test]
async fn normal_sign_in_sends_no_prompt_and_explicit_prompts_are_passed() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    let prompt_of = |url: &str| {
        reqwest::Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "prompt")
            .map(|(_, v)| v.into_owned())
    };
    assert_eq!(prompt_of(&m.begin_login().unwrap().url), None);
    assert_eq!(
        prompt_of(&m.begin_login_with_prompt(LoginPrompt::Default).unwrap().url),
        None
    );
    assert_eq!(
        prompt_of(&m.begin_login_with_prompt(LoginPrompt::Silent).unwrap().url).as_deref(),
        Some("none")
    );
    assert_eq!(
        prompt_of(&m.begin_login_with_prompt(LoginPrompt::Login).unwrap().url).as_deref(),
        Some("login")
    );
    assert_eq!(
        prompt_of(&m.begin_login_with_prompt(LoginPrompt::SelectAccount).unwrap().url).as_deref(),
        Some("select_account")
    );
}

#[tokio::test]
async fn silent_reauthorize_after_rejected_refresh() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    sign_in(&m, &server, "u1").await;
    Mock::given(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "invalid_grant"})))
        .mount(&server)
        .await;
    assert_eq!(m.force_refresh().await.unwrap(), None);
    assert!(m.active_account().unwrap().unwrap().needs_reauth);

    // No browser session: prompt=none comes back with login_required and nothing changes.
    let p = m.begin_login_with_prompt(LoginPrompt::Silent).unwrap();
    let e = m
        .complete_login(&format!(
            "alummahnow://auth/callback?error=login_required&state={}",
            p.state
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, Error::Unauthorized(d) if d.starts_with("login_required")),
        "{e:?}"
    );
    assert!(m.active_account().unwrap().unwrap().needs_reauth);

    // Browser still signed in: prompt=none returns a code and the account is usable again.
    let p = m.begin_login_with_prompt(LoginPrompt::Silent).unwrap();
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains(format!("code_verifier={}", p.pkce.verifier)))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_body(
            "u1",
            "at-u1-2",
            Some("rt-u1-2"),
            900,
        )))
        .mount(&server)
        .await;
    let info = m
        .complete_login(&format!("alummahnow://auth/callback?code=c2&state={}", p.state))
        .await
        .unwrap();
    assert_eq!(info.user_id, "u1");
    assert!(!info.needs_reauth);
    assert_eq!(m.accounts().unwrap().len(), 1);
    assert_eq!(m.access_token().await.unwrap().as_deref(), Some("at-u1-2"));
}

#[tokio::test]
async fn login_rejects_wrong_state_error_and_foreign_redirect() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    let p = m.begin_login().unwrap();
    let e = m
        .complete_login("alummahnow://auth/callback?code=x&state=forged")
        .await
        .unwrap_err();
    assert!(matches!(e, Error::InvalidInput(_)));
    let e = m
        .complete_login(&format!("https://evil.example/cb?code=x&state={}", p.state))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::InvalidInput(_)), "{e:?}");
    // The attempt is single-use even after a failure.
    let p = m.begin_login().unwrap();
    let e = m
        .complete_login(&format!(
            "alummahnow://auth/callback?error=access_denied&state={}",
            p.state
        ))
        .await
        .unwrap_err();
    assert_eq!(e, Error::Cancelled);
}

#[tokio::test]
async fn multi_account_switch_and_sign_out() {
    let server = MockServer::start().await;
    let clock = Arc::new(ManualClock::new(1_000));
    let m = manager(&server, clock.clone());
    let events = Arc::new(Events::default());
    m.add_listener(events.clone());
    sign_in(&m, &server, "u1").await;
    clock.advance(10);
    sign_in(&m, &server, "u2").await;
    assert_eq!(m.active_account().unwrap().unwrap().user_id, "u2");
    assert_eq!(m.accounts().unwrap().len(), 2);
    assert_eq!(m.access_token().await.unwrap().as_deref(), Some("at-u2"));

    clock.advance(10);
    m.switch_account("u1").unwrap();
    m.set_active_channel(Some("ch-9".into())).unwrap();
    assert_eq!(m.access_token().await.unwrap().as_deref(), Some("at-u1"));
    assert_eq!(
        m.active_account().unwrap().unwrap().active_channel_id.as_deref(),
        Some("ch-9")
    );

    m.sign_out("u1").unwrap();
    assert_eq!(m.active_account().unwrap().unwrap().user_id, "u2");
    m.sign_out("u2").unwrap();
    assert_eq!(m.active_account().unwrap(), None);
    assert_eq!(m.access_token().await.unwrap(), None);
    let ev = events.0.lock().unwrap();
    assert!(ev.contains(&AuthEvent::SignedOut { user_id: "u1".into() }));
    assert!(ev.contains(&AuthEvent::ActiveChanged { user_id: None }));
}

/// Rotating refresh endpoint: each refresh token works once.
struct Rotating {
    calls: AtomicUsize,
}
impl Respond for Rotating {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body = String::from_utf8_lossy(&req.body);
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 && body.contains("refresh_token=rt-u1&") {
            ResponseTemplate::new(200).set_body_json(token_body(
                "u1",
                &format!("at-u1-r{}", n + 1),
                Some(&format!("rt-u1-{}", n + 1)),
                3600,
            ))
        } else {
            ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "invalid_grant"}))
        }
    }
}

#[tokio::test]
async fn refreshes_near_expiry_once_for_concurrent_callers() {
    let server = MockServer::start().await;
    let clock = Arc::new(ManualClock::new(0));
    let m = Arc::new(manager(&server, clock.clone()));
    sign_in(&m, &server, "u1").await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(Rotating {
            calls: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    clock.advance(3_600_000 - 30_000); // inside the 60 s skew
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let m = m.clone();
            tokio::spawn(async move { m.access_token().await.unwrap() })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.unwrap().as_deref(), Some("at-u1-r1"));
    }
    let refreshes = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| String::from_utf8_lossy(&r.body).contains("grant_type=refresh_token"))
        .count();
    assert_eq!(refreshes, 1, "single-flight refresh");
}

#[tokio::test]
async fn sign_out_during_refresh_does_not_resurrect_the_account() {
    let server = MockServer::start().await;
    let clock = Arc::new(ManualClock::new(0));
    let m = Arc::new(manager(&server, clock.clone()));
    sign_in(&m, &server, "u1").await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(token_body("u1", "at-new", Some("rt-new"), 3600))
                .set_delay(std::time::Duration::from_millis(300)),
        )
        .mount(&server)
        .await;
    clock.advance(3_600_000 - 30_000);
    let refreshing = {
        let m = m.clone();
        tokio::spawn(async move { m.access_token().await.unwrap() })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await; // refresh is in flight
    m.sign_out("u1").unwrap();
    assert_eq!(refreshing.await.unwrap(), None);
    assert!(m.accounts().unwrap().is_empty(), "signed-out account came back");
    assert_eq!(m.token_for_account("u1", false).await.unwrap(), None);
}

#[tokio::test]
async fn rejected_refresh_marks_reauth() {
    let server = MockServer::start().await;
    let clock = Arc::new(ManualClock::new(0));
    let m = manager(&server, clock.clone());
    let events = Arc::new(Events::default());
    m.add_listener(events.clone());
    sign_in(&m, &server, "u1").await;
    Mock::given(body_string_contains("grant_type=refresh_token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"error": "invalid_grant", "error_description": "revoked"})),
        )
        .mount(&server)
        .await;
    assert_eq!(m.force_refresh().await.unwrap(), None);
    assert!(m.active_account().unwrap().unwrap().needs_reauth);
    assert_eq!(m.access_token().await.unwrap(), None);
    assert!(
        events
            .0
            .lock()
            .unwrap()
            .contains(&AuthEvent::ReauthRequired { user_id: "u1".into() })
    );
}

#[tokio::test]
async fn api_client_uses_auth_manager_and_refreshes_on_401() {
    let server = MockServer::start().await;
    let m = Arc::new(manager(&server, Arc::new(ManualClock::new(0))));
    sign_in(&m, &server, "u1").await;
    Mock::given(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_body("u1", "at-new", None, 3600)))
        .mount(&server)
        .await;
    Mock::given(path("/api/profile"))
        .and(wiremock::matchers::header("authorization", "Bearer at-u1"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    Mock::given(path("/api/profile"))
        .and(wiremock::matchers::header("authorization", "Bearer at-new"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;
    let client = aun_api::ApiClient::new(aun_api::ApiConfig::new(server.uri()))
        .unwrap()
        .with_auth(m.clone());
    client
        .execute(aun_api::RequestSpec::new(
            reqwest::Method::GET,
            "/api/profile".into(),
            "t",
        ))
        .await
        .unwrap();
    // Rotation kept the old refresh token because the server sent none.
    assert_eq!(m.access_token().await.unwrap().as_deref(), Some("at-new"));
}

/// Device flow: pending, slow_down, then granted.
struct DeviceScript {
    n: AtomicUsize,
}
impl Respond for DeviceScript {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        match self.n.fetch_add(1, Ordering::SeqCst) {
            0 => ResponseTemplate::new(400)
                .set_body_json(serde_json::json!({"error": "authorization_pending"})),
            1 => ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "slow_down"})),
            _ => {
                ResponseTemplate::new(200).set_body_json(token_body("tv-user", "at-tv", Some("rt-tv"), 3600))
            }
        }
    }
}

async fn mount_device_code(server: &MockServer, expires_in: u64) {
    Mock::given(path("/device/code"))
        .and(body_string_contains("client_id=viewer-mobile"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_code": "dc-1", "user_code": "WDJB-MJHT",
            "verification_uri": "https://alummahnow.net/activate",
            "verification_uri_complete": "https://alummahnow.net/activate?user_code=WDJB-MJHT",
            "expires_in": expires_in, "interval": 5
        })))
        .mount(server)
        .await;
}

#[tokio::test(start_paused = true)]
async fn device_flow_pending_slow_down_granted() {
    let server = MockServer::start().await;
    mount_device_code(&server, 600).await;
    Mock::given(path("/token"))
        .and(body_string_contains("device_code=dc-1"))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .respond_with(DeviceScript {
            n: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let m = manager_untimed(&server);
    let login = m.begin_device_login().await.unwrap();
    assert_eq!(login.authorization.user_code, "WDJB-MJHT");
    let info = m.complete_device_login(&login).await.unwrap();
    assert_eq!(info.user_id, "tv-user");
    let polls = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/token")
        .count();
    assert_eq!(polls, 3, "pending, slow_down, granted");
    assert_eq!(m.access_token().await.unwrap().as_deref(), Some("at-tv"));
}

#[tokio::test(start_paused = true)]
async fn device_flow_denied_expired_and_cancelled() {
    let server = MockServer::start().await;
    mount_device_code(&server, 600).await;
    Mock::given(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "access_denied"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let m = manager_untimed(&server);
    let login = m.begin_device_login().await.unwrap();
    assert_eq!(
        m.complete_device_login(&login).await.unwrap_err(),
        Error::Cancelled
    );

    Mock::given(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "expired_token"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    let login = m.begin_device_login().await.unwrap();
    assert!(matches!(
        m.complete_device_login(&login).await.unwrap_err(),
        Error::Unauthorized(_)
    ));

    Mock::given(path("/token"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(serde_json::json!({"error": "authorization_pending"})),
        )
        .mount(&server)
        .await;
    let login = m.begin_device_login().await.unwrap();
    let l2 = login.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(12)).await;
        l2.cancel();
    });
    assert_eq!(
        m.complete_device_login(&login).await.unwrap_err(),
        Error::Cancelled
    );
}

#[tokio::test]
async fn device_flow_unsupported_without_endpoint() {
    let server = MockServer::start().await;
    let oauth = OAuthClient::new(config(&server, false), http());
    let m = AuthManager::new(oauth, Arc::new(MemorySecureStorage::new()));
    assert!(matches!(
        m.begin_device_login().await.unwrap_err(),
        Error::Unsupported(_)
    ));
}

#[tokio::test]
async fn account_bound_tokens_ignore_account_switches() {
    let server = MockServer::start().await;
    let m = Arc::new(manager(&server, Arc::new(ManualClock::new(0))));
    sign_in(&m, &server, "u1").await;
    sign_in(&m, &server, "u2").await; // u2 is now active
    let u1 = aun_auth::AccountTokens::new(m.clone(), "u1");
    use aun_api::AuthProvider;
    assert_eq!(u1.access_token().await.as_deref(), Some("at-u1"));
    m.switch_account("u2").unwrap();
    assert_eq!(u1.access_token().await.as_deref(), Some("at-u1"));
    m.sign_out("u1").unwrap();
    assert_eq!(u1.access_token().await, None);
}

#[tokio::test]
async fn refuses_cleartext_endpoints() {
    let server = MockServer::start().await;
    let mut cfg = config(&server, true);
    cfg.authorization_endpoint = "http://auth.example.com/authorize".into();
    cfg.token_endpoint = "http://auth.example.com/token".into();
    cfg.device_authorization_endpoint = Some("http://auth.example.com/device".into());
    let c = OAuthClient::new(cfg, http());
    assert!(matches!(c.authorize(&[]), Err(Error::InvalidInput(_))));
    assert!(matches!(c.refresh("rt", 0).await, Err(Error::InvalidInput(_))));
    assert!(matches!(
        c.device_authorization().await,
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(
        OAuthConfig::discover(&http(), "http://auth.example.com", "c", "x://cb", vec![]).await,
        Err(Error::InvalidInput(_))
    ));
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "nothing was sent"
    );
}

#[tokio::test]
async fn sign_out_and_revoke_revokes_the_refresh_token() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    sign_in(&m, &server, "u1").await;
    Mock::given(method("POST"))
        .and(path("/token/revoke"))
        .and(body_string_contains("token=rt-u1"))
        .and(body_string_contains("token_type_hint=refresh_token"))
        .and(body_string_contains("client_id=viewer-mobile"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    m.sign_out_and_revoke("u1").await.unwrap();
    assert!(m.accounts().unwrap().is_empty());
    assert_eq!(m.active_account().unwrap(), None);
}

#[tokio::test]
async fn failed_revocation_never_blocks_sign_out() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    sign_in(&m, &server, "u1").await;
    sign_in(&m, &server, "u2").await;
    Mock::given(method("POST"))
        .and(path("/token/revoke"))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&server)
        .await;
    m.sign_out_and_revoke("u2").await.unwrap();
    let ids: Vec<_> = m.accounts().unwrap().into_iter().map(|a| a.user_id).collect();
    assert_eq!(ids, vec!["u1".to_string()]);
    assert_eq!(m.active_account().unwrap().unwrap().user_id, "u1");
}

#[tokio::test]
async fn sign_out_of_an_unknown_account_sends_nothing() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    Mock::given(path("/token/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    m.sign_out_and_revoke("nobody").await.unwrap();
    assert_eq!(m.sign_out_taking_refresh_token("nobody").unwrap(), None);
}

#[tokio::test]
async fn discovery_reads_the_revocation_endpoint() {
    let server = MockServer::start().await;
    Mock::given(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": server.uri(), "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
            "revocation_endpoint": format!("{}/oauth/revoke", server.uri()),
        })))
        .mount(&server)
        .await;
    let c = OAuthConfig::discover(&http(), &server.uri(), "tv", "alummahnow://cb", vec![])
        .await
        .unwrap();
    assert_eq!(c.revocation_url(), format!("{}/oauth/revoke", server.uri()));
    let mut d = c.clone();
    d.revocation_endpoint = None;
    assert_eq!(d.revocation_url(), format!("{}/token/revoke", server.uri()));
}

#[tokio::test]
async fn loopback_redirect_is_used_for_authorize_and_the_code_exchange() {
    let server = MockServer::start().await;
    let m = manager(&server, Arc::new(ManualClock::new(0)));
    let loopback = "http://127.0.0.1:53111/auth/callback";
    let p = m
        .begin_login_with_redirect(LoginPrompt::Login, Some(loopback))
        .unwrap();
    let url = reqwest::Url::parse(&p.url).unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["redirect_uri"], loopback);
    assert_eq!(q["prompt"], "login");
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains(
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A53111%2Fauth%2Fcallback",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_body("u1", "at", Some("rt"), 3600)))
        .expect(1)
        .mount(&server)
        .await;
    // Another port is a different redirect: refused before any network call.
    let other = m
        .begin_login_with_redirect(LoginPrompt::Default, Some(loopback))
        .unwrap();
    let wrong_port = format!(
        "http://127.0.0.1:53112/auth/callback?code=c&state={}",
        other.state
    );
    assert!(matches!(
        m.complete_login(&wrong_port).await,
        Err(Error::InvalidInput(_))
    ));
    let info = m
        .complete_login(&format!("{loopback}?code=c&state={}", p.state))
        .await
        .unwrap();
    assert_eq!(info.user_id, "u1");
    // Only loopback or the registered redirect may be used.
    assert!(matches!(
        m.begin_login_with_redirect(LoginPrompt::Default, Some("https://evil.example/cb")),
        Err(Error::InvalidInput(_))
    ));
}
