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

/// Shared HTTP client used by every module that talks to our servers.
pub fn http_client(config: &ApiConfig) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .tls_backend_preconfigured(rustls_config()?)
        .user_agent(config.user_agent.clone())
        .timeout(config.timeout)
        .connect_timeout(std::time::Duration::from_secs(15))
        .pool_idle_timeout(std::time::Duration::from_secs(60))
        // Follow at most 5 redirects and never to a cleartext URL (no https -> http downgrade).
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                attempt.error("too many redirects")
            } else if aun_common::security::is_secure(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("refusing redirect to an insecure URL")
            }
        }))
        .build()
        .map_err(|e| Error::Internal(format!("HTTP client: {e}")))
}
