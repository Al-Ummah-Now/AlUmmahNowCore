//! TUS 1.0.0 client calls (creation, HEAD, PATCH) as used against the api-videos proxy
//! (`/api/videos/tus`, authorised by our bearer token + `tusSessionId` metadata) or, after a
//! proxy session expired, directly against the endpoint returned by `/upload/resume/{videoId}`.

use std::collections::BTreeMap;

use aun_common::security::ensure_secure_url;
use aun_common::{Error, Result};
use base64::Engine;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

pub const TUS_VERSION: &str = "1.0.0";

/// Where TUS requests go and which extra headers they carry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TusTarget {
    /// The api-videos proxy; requests carry `Authorization: Bearer` from the token source.
    Proxy { endpoint: String },
    /// Direct upload with server-issued headers (signature, expiry, library, video id).
    Direct {
        endpoint: String,
        headers: BTreeMap<String, String>,
    },
}

impl TusTarget {
    pub fn endpoint(&self) -> &str {
        match self {
            TusTarget::Proxy { endpoint } | TusTarget::Direct { endpoint, .. } => endpoint,
        }
    }
}

/// `Upload-Metadata`: comma-separated `key base64(value)` pairs, keys sorted for determinism.
pub fn encode_metadata(meta: &BTreeMap<String, String>) -> String {
    meta.iter()
        .map(|(k, v)| {
            format!(
                "{k} {}",
                base64::engine::general_purpose::STANDARD.encode(v.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Thin TUS protocol client.
#[derive(Debug, Clone)]
pub struct TusClient {
    http: reqwest::Client,
}

fn net(e: reqwest::Error) -> Error {
    if e.is_timeout() {
        Error::Network(format!("timeout: {e}"))
    } else {
        Error::Network(e.to_string())
    }
}

fn status_error(
    resp_status: reqwest::StatusCode,
    what: &str,
    retry_after: Option<std::time::Duration>,
) -> Error {
    match Error::from_status(resp_status.as_u16(), None, format!("TUS {what}: {resp_status}")) {
        Error::RateLimited { .. } => Error::RateLimited { retry_after },
        e => e,
    }
}

fn retry_after(h: &HeaderMap) -> Option<std::time::Duration> {
    h.get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .map(std::time::Duration::from_secs)
}

fn header_u64(h: &HeaderMap, name: &str) -> Option<u64> {
    h.get(name)?.to_str().ok()?.trim().parse().ok()
}

impl TusClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// Every TUS request goes through here first. The upload URL must be https (http only for
    /// localhost). On the proxy target, where the account's bearer token is attached, it must
    /// also share the endpoint's origin, so a hostile `Location` can't collect the token.
    fn check_url(target: &TusTarget, url: &str) -> Result<()> {
        ensure_secure_url(url)?;
        if let TusTarget::Proxy { endpoint } = target {
            let parse = |u: &str| {
                reqwest::Url::parse(u.trim()).map_err(|_| Error::InvalidInput(format!("bad TUS URL {u:?}")))
            };
            if parse(endpoint)?.origin() != parse(url)?.origin() {
                return Err(Error::InvalidInput(format!(
                    "refusing TUS upload URL {url:?}: not on the endpoint's origin"
                )));
            }
        }
        Ok(())
    }

    fn headers(target: &TusTarget, bearer: Option<&str>) -> Result<HeaderMap> {
        let mut h = HeaderMap::new();
        h.insert("Tus-Resumable", HeaderValue::from_static(TUS_VERSION));
        match target {
            TusTarget::Proxy { .. } => {
                if let Some(t) = bearer {
                    let v = HeaderValue::from_str(&format!("Bearer {t}"))
                        .map_err(|_| Error::InvalidInput("token has invalid characters".into()))?;
                    h.insert(reqwest::header::AUTHORIZATION, v);
                }
            }
            TusTarget::Direct { headers, .. } => {
                for (k, v) in headers {
                    let name = HeaderName::from_bytes(k.as_bytes())
                        .map_err(|_| Error::InvalidInput(format!("bad header {k}")))?;
                    let value = HeaderValue::from_str(v)
                        .map_err(|_| Error::InvalidInput(format!("bad header {k}")))?;
                    h.insert(name, value);
                }
            }
        }
        Ok(h)
    }

    /// Creation extension: returns the absolute upload URL.
    pub async fn create(
        &self,
        target: &TusTarget,
        bearer: Option<&str>,
        length: u64,
        metadata: &BTreeMap<String, String>,
    ) -> Result<String> {
        ensure_secure_url(target.endpoint())?;
        let mut h = Self::headers(target, bearer)?;
        h.insert("Upload-Length", HeaderValue::from(length));
        if let Ok(v) = HeaderValue::from_str(&encode_metadata(metadata)) {
            h.insert("Upload-Metadata", v);
        }
        let resp = self
            .http
            .post(target.endpoint())
            .headers(h)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status.as_u16() != 201 && !status.is_success() {
            return Err(status_error(status, "create", retry_after(resp.headers())));
        }
        let loc = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Http {
                status: status.as_u16(),
                code: None,
                message: "TUS create returned no Location".into(),
            })?;
        let base = reqwest::Url::parse(target.endpoint()).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let abs = base
            .join(loc)
            .map_err(|e| Error::Internal(format!("bad Location {loc}: {e}")))?;
        let abs: String = abs.into();
        Self::check_url(target, &abs)?;
        Ok(abs)
    }

    /// HEAD: the server's current offset.
    pub async fn offset(&self, target: &TusTarget, bearer: Option<&str>, upload_url: &str) -> Result<u64> {
        Self::check_url(target, upload_url)?;
        let resp = self
            .http
            .head(upload_url)
            .headers(Self::headers(target, bearer)?)
            .send()
            .await
            .map_err(net)?;
        if !resp.status().is_success() {
            return Err(status_error(resp.status(), "HEAD", retry_after(resp.headers())));
        }
        header_u64(resp.headers(), "Upload-Offset").ok_or_else(|| Error::Http {
            status: resp.status().as_u16(),
            code: None,
            message: "TUS HEAD returned no Upload-Offset".into(),
        })
    }

    /// PATCH one chunk at `offset`; returns the new offset.
    pub async fn patch(
        &self,
        target: &TusTarget,
        bearer: Option<&str>,
        upload_url: &str,
        offset: u64,
        body: reqwest::Body,
        len: u64,
    ) -> Result<u64> {
        Self::check_url(target, upload_url)?;
        let mut h = Self::headers(target, bearer)?;
        h.insert("Upload-Offset", HeaderValue::from(offset));
        h.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/offset+octet-stream"),
        );
        h.insert(reqwest::header::CONTENT_LENGTH, HeaderValue::from(len));
        let resp = self
            .http
            .patch(upload_url)
            .headers(h)
            .body(body)
            .send()
            .await
            .map_err(net)?;
        let status = resp.status();
        if status.as_u16() == 409 {
            return Err(Error::Conflict("TUS offset mismatch".into()));
        }
        if !status.is_success() {
            return Err(status_error(status, "PATCH", retry_after(resp.headers())));
        }
        Ok(header_u64(resp.headers(), "Upload-Offset").unwrap_or(offset + len))
    }

    /// Termination extension (best effort when the user cancels).
    pub async fn terminate(&self, target: &TusTarget, bearer: Option<&str>, upload_url: &str) -> Result<()> {
        Self::check_url(target, upload_url)?;
        let resp = self
            .http
            .delete(upload_url)
            .headers(Self::headers(target, bearer)?)
            .send()
            .await
            .map_err(net)?;
        if resp.status().is_success() || resp.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(status_error(resp.status(), "DELETE", None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(endpoint: &str) -> TusTarget {
        TusTarget::Proxy {
            endpoint: endpoint.into(),
        }
    }

    #[test]
    fn upload_urls_must_be_secure_and_same_origin_on_the_proxy() {
        let p = proxy("https://api.example.com/api/videos/tus");
        assert!(TusClient::check_url(&p, "https://api.example.com/api/videos/tus/abc").is_ok());
        for bad in [
            "http://api.example.com/api/videos/tus/abc",
            "https://evil.example/api/videos/tus/abc",
            "https://api.example.com:8443/tus/abc",
            "ftp://api.example.com/x",
            "not a url",
        ] {
            assert!(TusClient::check_url(&p, bad).is_err(), "{bad}");
        }
        assert!(TusClient::check_url(&proxy("http://127.0.0.1:9/tus"), "http://127.0.0.1:9/tus/a").is_ok());

        // Direct targets carry per-upload signatures, not the bearer: any https host is fine.
        let d = TusTarget::Direct {
            endpoint: "https://video.example.net/tusupload".into(),
            headers: BTreeMap::new(),
        };
        assert!(TusClient::check_url(&d, "https://cdn.example.org/tusupload/xyz").is_ok());
        assert!(TusClient::check_url(&d, "http://cdn.example.org/tusupload/xyz").is_err());
    }

    #[tokio::test]
    async fn create_refuses_insecure_endpoints_and_foreign_locations() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let client = TusClient::new(
            aun_api::http_client(&aun_api::ApiConfig::new("https://api.example.com")).unwrap(),
        );
        let meta = BTreeMap::new();

        let err = client
            .create(&proxy("http://uploads.example.com/tus"), Some("t"), 10, &meta)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(201).insert_header("Location", "https://evil.example/tus/abc"),
            )
            .mount(&server)
            .await;
        let err = client
            .create(&proxy(&format!("{}/tus", server.uri())), Some("t"), 10, &meta)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    }

    #[test]
    fn metadata_is_base64_and_sorted() {
        let mut m = BTreeMap::new();
        m.insert("tusSessionId".to_string(), "s1".to_string());
        m.insert("filename".to_string(), "درس.mp4".to_string());
        assert_eq!(encode_metadata(&m), "filename 2K/YsdizLm1wNA==,tusSessionId czE=");
    }
}
