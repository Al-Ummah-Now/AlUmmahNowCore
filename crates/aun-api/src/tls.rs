//! HTTP client construction. TLS is rustls with the `ring` provider and Mozilla's root store
//! (webpki-roots): it builds for every target without cmake/NASM (aws-lc) and without the
//! Android JNI initialisation the platform verifier needs.

use std::sync::Arc;

use aun_common::Error;

use crate::Result;
use crate::client::ApiConfig;

pub(crate) fn rustls_config() -> Result<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Internal(format!("TLS config: {e}")))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

/// `X-AUN-Client` on every request (the servers' request origin: surface, app, version).
fn client_headers(config: &ApiConfig) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    let value = crate::client::sanitize_client(&config.client);
    if let Ok(v) = reqwest::header::HeaderValue::from_str(&value) {
        headers.insert("x-aun-client", v);
    }
    headers
}

/// Shared HTTP client used by every module that talks to our servers.
pub fn http_client(config: &ApiConfig) -> Result<reqwest::Client> {
    http_client_with_redirect(
        config,
        // Follow at most 5 same-origin secure redirects (no https -> http downgrade and no
        // cross-origin redirects that could leak headers or turn API/OAuth calls into SSRF).
        reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if !aun_common::security::is_secure(attempt.url()) {
                attempt.error("refusing redirect to an insecure URL")
            } else if attempt
                .previous()
                .first()
                .is_some_and(|origin| origin.origin() != attempt.url().origin())
            {
                attempt.error("refusing cross-origin redirect")
            } else {
                attempt.follow()
            }
        }),
    )
}

/// Shared HTTP client that refuses all HTTP redirects (used by telemetry ingest POSTs so a
/// 307/308 redirect cannot forward diagnostic batches to a different origin).
pub fn http_client_no_redirects(config: &ApiConfig) -> Result<reqwest::Client> {
    http_client_with_redirect(config, reqwest::redirect::Policy::none())
}

fn http_client_with_redirect(
    config: &ApiConfig,
    policy: reqwest::redirect::Policy,
) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .tls_backend_preconfigured(rustls_config()?)
        .user_agent(config.user_agent.clone())
        .default_headers(client_headers(config))
        .timeout(config.timeout)
        .connect_timeout(std::time::Duration::from_secs(15))
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        .redirect(policy)
        .build()
        .map_err(|e| Error::Internal(format!("HTTP client: {e}")))
}
