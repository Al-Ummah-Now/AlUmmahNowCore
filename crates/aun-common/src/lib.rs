//! Shared foundations for `aun-core`: the error model and the platform-adapter traits that native
//! apps implement. See `docs/PLAN.md` §4 and §6.

mod error;
pub mod platform;
pub mod security;

pub use error::{Error, Result};

/// The core library version (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
