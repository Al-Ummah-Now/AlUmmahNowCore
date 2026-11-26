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
    Unauthorized { code: Option<String>, detail: String },
    #[error("forbidden: {detail}")]
    Forbidden { code: Option<String>, detail: String },
    #[error("not found: {detail}")]
    NotFound { code: Option<String>, detail: String },
    #[error("conflict: {detail}")]
    Conflict { code: Option<String>, detail: String },
    #[error("rate limited")]
    RateLimited { retry_after_ms: Option<u64> },
    #[error("invalid input: {detail}")]
    InvalidInput { code: Option<String>, detail: String },
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
            Error::Unauthorized { code, message } => AunError::Unauthorized {
                code,
                detail: message,
            },
            Error::Forbidden { code, message } => AunError::Forbidden {
                code,
                detail: message,
            },
            Error::NotFound { code, message } => AunError::NotFound {
                code,
                detail: message,
            },
            Error::Conflict { code, message } => AunError::Conflict {
                code,
                detail: message,
            },
            Error::RateLimited { retry_after } => AunError::RateLimited {
                retry_after_ms: retry_after.map(|d| d.as_millis() as u64),
            },
            Error::InvalidInput { code, message } => AunError::InvalidInput {
                code,
                detail: message,
            },
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
            AunError::Unauthorized { code, detail } => Error::Unauthorized {
                code,
                message: detail,
            },
            AunError::Forbidden { code, detail } => Error::Forbidden {
                code,
                message: detail,
            },
            AunError::NotFound { code, detail } => Error::NotFound {
                code,
                message: detail,
            },
            AunError::Conflict { code, detail } => Error::Conflict {
                code,
                message: detail,
            },
            AunError::RateLimited { retry_after_ms } => Error::RateLimited {
                retry_after: retry_after_ms.map(std::time::Duration::from_millis),
            },
            AunError::InvalidInput { code, detail } => Error::InvalidInput {
                code,
                message: detail,
            },
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
            Error::unauthorized("u".into()),
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

    #[test]
    fn client_error_codes_cross_the_ffi_boundary() {
        let code = Some("server_code".to_owned());
        let detail = "error detail".to_owned();
        let cases = [
            (
                401,
                AunError::Unauthorized {
                    code: code.clone(),
                    detail: detail.clone(),
                },
            ),
            (
                403,
                AunError::Forbidden {
                    code: code.clone(),
                    detail: detail.clone(),
                },
            ),
            (
                404,
                AunError::NotFound {
                    code: code.clone(),
                    detail: detail.clone(),
                },
            ),
            (
                409,
                AunError::Conflict {
                    code: code.clone(),
                    detail: detail.clone(),
                },
            ),
            (
                400,
                AunError::InvalidInput {
                    code: code.clone(),
                    detail: detail.clone(),
                },
            ),
            (
                422,
                AunError::InvalidInput {
                    code: code.clone(),
                    detail: detail.clone(),
                },
            ),
        ];
        for (status, expected) in cases {
            let error = Error::from_status(status, code.clone(), &detail);
            let ffi = AunError::from(error.clone());
            assert_eq!(ffi, expected);
            assert_eq!(Error::from(ffi), error);
        }
    }
}
