//! Transport policy shared by every crate that sends credentials.

use crate::{Error, Result};

/// Accepts `https://` URLs, and `http://` only for loopback hosts (local development and the
/// in-process mock servers used by tests). Anything else is refused so tokens, codes and
/// uploads are never sent in cleartext.
pub fn ensure_secure_url(url: &str) -> Result<()> {
    let refuse = || {
        Error::InvalidInput(format!(
            "refusing insecure URL {url:?}: use https (http is only allowed for localhost)"
        ))
    };
    let parsed = url::Url::parse(url.trim()).map_err(|_| refuse())?;
    if is_secure(&parsed) { Ok(()) } else { Err(refuse()) }
}

/// The parsed-URL form of [`ensure_secure_url`] (used for redirects).
pub fn is_secure(url: &url::Url) -> bool {
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => match url.host() {
            Some(url::Host::Domain(d)) => {
                let d = d.to_ascii_lowercase();
                d == "localhost" || d.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
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
            "http://127.0.0.2:8080",
        ] {
            assert!(ensure_secure_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://videos.api.alummahnow.net",
            "http://127.0.0.1.evil.example/",
            "http://localhost@evil.example/",
            "http://evil.example\\@localhost/",
            "https://",
            "http://evil.example?localhost",
            "ftp://localhost",
            "//localhost",
            "",
        ] {
            assert!(ensure_secure_url(bad).is_err(), "{bad}");
        }
    }
}
