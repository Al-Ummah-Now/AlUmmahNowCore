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
    #[error("unauthorized: {message}")]
    Unauthorized { code: Option<String>, message: String },
    #[error("forbidden: {message}")]
    Forbidden { code: Option<String>, message: String },
    #[error("not found: {message}")]
    NotFound { code: Option<String>, message: String },
    #[error("conflict: {message}")]
    Conflict { code: Option<String>, message: String },
    /// 429 from the server; `retry_after` comes from the `Retry-After` header when present.
    #[error("rate limited")]
    RateLimited { retry_after: Option<Duration> },
    #[error("invalid input: {message}")]
    InvalidInput { code: Option<String>, message: String },
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
            401 => Error::Unauthorized { code, message },
            403 => Error::Forbidden { code, message },
            404 => Error::NotFound { code, message },
            409 => Error::Conflict { code, message },
            429 => Error::RateLimited { retry_after: None },
            400 | 422 => Error::InvalidInput { code, message },
            _ => Error::Http {
                status,
                code,
                message,
            },
        }
    }

    /// Creates a local unauthorized error, with no server code.
    pub fn unauthorized(message: String) -> Self {
        Self::Unauthorized { code: None, message }
    }

    /// Creates a local not found error, with no server code.
    pub fn not_found(message: String) -> Self {
        Self::NotFound { code: None, message }
    }

    /// Creates a local conflict error, with no server code.
    pub fn conflict(message: String) -> Self {
        Self::Conflict { code: None, message }
    }

    /// Creates a local invalid input error, with no server code.
    pub fn invalid_input(message: String) -> Self {
        Self::InvalidInput { code: None, message }
    }

    /// The server's machine-readable code, when present.
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Http { code, .. }
            | Self::Unauthorized { code, .. }
            | Self::Forbidden { code, .. }
            | Self::NotFound { code, .. }
            | Self::Conflict { code, .. }
            | Self::InvalidInput { code, .. } => code.as_deref(),
            _ => None,
        }
    }

    /// The stable, snake_case name of this error variant.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Network(_) => "network",
            Self::Offline => "offline",
            Self::Http { .. } => "http",
            Self::Unauthorized { .. } => "unauthorized",
            Self::Forbidden { .. } => "forbidden",
            Self::NotFound { .. } => "not_found",
            Self::Conflict { .. } => "conflict",
            Self::RateLimited { .. } => "rate_limited",
            Self::InvalidInput { .. } => "invalid_input",
            Self::Storage(_) => "storage",
            Self::Platform(_) => "platform",
            Self::Unsupported(_) => "unsupported",
            Self::Cancelled => "cancelled",
            Self::Internal(_) => "internal",
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
            Error::unauthorized("x".into())
        );
        assert_eq!(Error::from_status(404, None, "x"), Error::not_found("x".into()));
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
