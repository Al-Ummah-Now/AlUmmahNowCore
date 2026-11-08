//! The shared error model. Every core crate converts its own errors into [`Error`], and `aun-ffi`
//! maps [`Error`] one-to-one onto the FFI `AunError`.

use std::time::Duration;

/// Errors every module can surface to the apps.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// DNS, TLS, connection reset or timeout.
    #[error("network error: {0}")]
    Network(String),
    /// No connectivity; queued work will retry when back online.
    #[error("offline")]
    Offline,
    /// A non-success HTTP status that has no more specific variant.
    #[error("HTTP {status}: {message}")]
    Http {
        status: u16,
        code: Option<String>,
        message: String,
    },
    /// Authentication is missing or could not be refreshed: sign-in required.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// 429 from the server; `retry_after` comes from the `Retry-After` header when present.
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// Local persistence (database, file system) failed.
    #[error("storage error: {0}")]
    Storage(String),
    /// A native platform adapter reported a failure.
    #[error("platform error: {0}")]
    Platform(String),
    /// The feature or backend is not available on this target or build.
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("cancelled")]
    Cancelled,
    #[error("internal error: {0}")]
    Internal(String),
}

/// Convenience alias used across the core crates.
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// Builds the right variant for an HTTP status.
    pub fn from_status(status: u16, code: Option<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        match status {
            401 => Error::Unauthorized(message),
            403 => Error::Forbidden(message),
            404 => Error::NotFound(message),
            409 => Error::Conflict(message),
            429 => Error::RateLimited { retry_after: None },
            400 | 422 => Error::InvalidInput(message),
            _ => Error::Http {
                status,
                code,
                message,
            },
        }
    }

    /// Whether retrying the same request later may succeed. This is the single place that decides
    /// retry eligibility for the API client, the upload engine and the offline queue.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Network(_) | Error::Offline | Error::RateLimited { .. } => true,
            Error::Http { status, .. } => matches!(status, 408 | 425 | 500..=599),
            _ => false,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Storage(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Internal(format!("JSON: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(
            Error::from_status(401, None, "x"),
            Error::Unauthorized("x".into())
        );
        assert_eq!(Error::from_status(404, None, "x"), Error::NotFound("x".into()));
        assert!(matches!(
            Error::from_status(429, None, ""),
            Error::RateLimited { .. }
        ));
        assert!(matches!(
            Error::from_status(503, None, ""),
            Error::Http { status: 503, .. }
        ));
    }

    #[test]
    fn retryable() {
        assert!(Error::Network("reset".into()).is_retryable());
        assert!(Error::Offline.is_retryable());
        assert!(Error::from_status(502, None, "").is_retryable());
        assert!(Error::from_status(429, None, "").is_retryable());
        assert!(!Error::from_status(400, None, "").is_retryable());
        assert!(!Error::from_status(401, None, "").is_retryable());
        assert!(!Error::Cancelled.is_retryable());
    }
}
