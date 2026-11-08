//! `AunError`: the single error type every exported function throws (docs/PLAN.md §4).

use aun_common::Error;

/// Errors thrown to Swift (`AunError`), Kotlin (`AunException`) and C# (`AunException`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum AunError {
    #[error("network error: {detail}")]
    Network { detail: String },
    #[error("offline")]
    Offline,
    #[error("HTTP {status}: {detail}")]
    Http {
        status: u16,
        code: Option<String>,
        detail: String,
    },
    #[error("unauthorized: {detail}")]
    Unauthorized { detail: String },
    #[error("forbidden: {detail}")]
    Forbidden { detail: String },
    #[error("not found: {detail}")]
    NotFound { detail: String },
    #[error("conflict: {detail}")]
    Conflict { detail: String },
    #[error("rate limited")]
    RateLimited { retry_after_ms: Option<u64> },
    #[error("invalid input: {detail}")]
    InvalidInput { detail: String },
    #[error("storage error: {detail}")]
    Storage { detail: String },
    #[error("platform error: {detail}")]
    Platform { detail: String },
    #[error("unsupported: {detail}")]
    Unsupported { detail: String },
    #[error("cancelled")]
    Cancelled,
    #[error("internal error: {detail}")]
    Internal { detail: String },
}

impl From<Error> for AunError {
    fn from(e: Error) -> Self {
        match e {
            Error::Network(detail) => AunError::Network { detail },
            Error::Offline => AunError::Offline,
            Error::Http {
                status,
                code,
                message,
            } => AunError::Http {
                status,
                code,
                detail: message,
            },
            Error::Unauthorized(detail) => AunError::Unauthorized { detail },
            Error::Forbidden(detail) => AunError::Forbidden { detail },
            Error::NotFound(detail) => AunError::NotFound { detail },
            Error::Conflict(detail) => AunError::Conflict { detail },
            Error::RateLimited { retry_after } => AunError::RateLimited {
                retry_after_ms: retry_after.map(|d| d.as_millis() as u64),
            },
            Error::InvalidInput(detail) => AunError::InvalidInput { detail },
            Error::Storage(detail) => AunError::Storage { detail },
            Error::Platform(detail) => AunError::Platform { detail },
            Error::Unsupported(detail) => AunError::Unsupported { detail },
            Error::Cancelled => AunError::Cancelled,
            Error::Internal(detail) => AunError::Internal { detail },
        }
    }
}

impl From<AunError> for Error {
    fn from(e: AunError) -> Self {
        match e {
            AunError::Network { detail } => Error::Network(detail),
            AunError::Offline => Error::Offline,
            AunError::Http { status, code, detail } => Error::Http {
                status,
                code,
                message: detail,
            },
            AunError::Unauthorized { detail } => Error::Unauthorized(detail),
            AunError::Forbidden { detail } => Error::Forbidden(detail),
            AunError::NotFound { detail } => Error::NotFound(detail),
            AunError::Conflict { detail } => Error::Conflict(detail),
            AunError::RateLimited { retry_after_ms } => Error::RateLimited {
                retry_after: retry_after_ms.map(std::time::Duration::from_millis),
            },
            AunError::InvalidInput { detail } => Error::InvalidInput(detail),
            AunError::Storage { detail } => Error::Storage(detail),
            AunError::Platform { detail } => Error::Platform(detail),
            AunError::Unsupported { detail } => Error::Unsupported(detail),
            AunError::Cancelled => Error::Cancelled,
            AunError::Internal { detail } => Error::Internal(detail),
        }
    }
}

/// A native callback threw something that is not an `AunError` (or panicked).
impl From<uniffi::UnexpectedUniFFICallbackError> for AunError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        AunError::Platform { detail: e.reason }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_common() {
        let all = [
            Error::Network("n".into()),
            Error::Offline,
            Error::Http {
                status: 502,
                code: Some("bad_gateway".into()),
                message: "m".into(),
            },
            Error::Unauthorized("u".into()),
            Error::RateLimited {
                retry_after: Some(std::time::Duration::from_secs(3)),
            },
            Error::Cancelled,
            Error::Internal("i".into()),
        ];
        for e in all {
            let ffi: AunError = e.clone().into();
            let back: Error = ffi.into();
            assert_eq!(back, e);
        }
    }
}
