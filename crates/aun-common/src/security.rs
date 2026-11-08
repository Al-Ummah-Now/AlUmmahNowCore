//! Transport policy shared by every crate that sends credentials.

use crate::{Error, Result};

/// Accepts `https://` URLs, and `http://` only for loopback hosts (local development and the
/// in-process mock servers used by tests). Anything else is refused so tokens, codes and
/// uploads are never sent in cleartext.
pub fn ensure_secure_url(url: &str) -> Result<()> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("http://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host_port = authority.rsplit('@').next().unwrap_or("");
        let host = if let Some(v6) = host_port.strip_prefix('[') {
            v6.split(']').next().unwrap_or("")
        } else {
            host_port.split(':').next().unwrap_or("")
        };
        if matches!(host, "localhost" | "127.0.0.1" | "::1") || host.ends_with(".localhost") {
            return Ok(());
        }
    }
    Err(Error::InvalidInput(format!(
        "refusing insecure URL {url:?}: use https (http is only allowed for localhost)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_and_loopback_only() {
        for ok in [
            "https://videos.api.alummahnow.net",
            "HTTPS://Auth.qa.alummahnow.net/token",
            "http://127.0.0.1:8787/api",
            "http://localhost",
            "http://[::1]:9/x",
            "http://app.localhost:3000",
        ] {
            assert!(ensure_secure_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://videos.api.alummahnow.net",
            "http://127.0.0.1.evil.example/",
            "http://localhost@evil.example/",
            "http://evil.example?localhost",
            "ftp://localhost",
            "//localhost",
            "",
        ] {
            assert!(ensure_secure_url(bad).is_err(), "{bad}");
        }
    }
}
