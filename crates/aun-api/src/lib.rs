//! Typed client for api-videos, generated from its OpenAPI document (`spec/openapi/api-videos.json`
//! via `tools/codegen/openapi_to_rust.py`), plus the hand-written runtime: retries with backoff,
//! the auth refresh hook ([`AuthProvider`]) and the persistent offline write queue ([`queue`]).

pub mod client;
pub mod generated;
pub mod queue;
mod tls;

pub use aun_common::{Error, Result};
pub use client::{ApiClient, ApiConfig, AuthMode, AuthProvider, Body, RawResponse, RequestSpec, RetryPolicy};
pub use generated::*;
pub use tls::http_client;
