//! UniFFI facade for `aun-core`. This is the only crate that crosses the FFI boundary
//! (docs/PLAN.md §3): it wraps the plain-Rust core crates in FFI-friendly objects and records.
//!
//! Generated bindings: Swift module `AunCore`, Kotlin package `net.alummahnow.core`,
//! C# namespace `AlUmmahNow.Core`.

uniffi::setup_scaffolding!("aun_core");

mod api;
mod auth;
mod diagnostics;
mod downloads;
mod error;
mod media;
mod names;
mod notifications;
mod platform;
mod player;
mod playlists;
mod runtime;
mod search;
mod short_links;
mod sync;
mod telemetry;
mod updates;
mod upload;
mod visibility;

pub use api::*;
pub use auth::*;
pub use diagnostics::*;
pub use downloads::*;
pub use error::AunError;
pub use media::*;
pub use names::*;
pub use notifications::*;
pub use platform::*;
pub use player::*;
pub use playlists::*;
pub use search::*;
pub use short_links::*;
pub use sync::*;
pub use telemetry::*;
pub use updates::*;
pub use upload::*;
pub use visibility::*;

/// The `aun-core` version, e.g. `"0.1.0"`. Apps log it and send it with telemetry.
#[uniffi::export]
pub fn core_version() -> String {
    aun_common::VERSION.to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_matches_workspace() {
        assert_eq!(super::core_version(), env!("CARGO_PKG_VERSION"));
    }
}
