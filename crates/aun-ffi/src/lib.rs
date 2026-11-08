//! UniFFI facade for `aun-core`. This is the only crate that crosses the FFI boundary
//! (docs/PLAN.md §3): it wraps the plain-Rust core crates in FFI-friendly objects and records.
//!
//! Generated bindings: Swift module `AunCore`, Kotlin package `net.alummahnow.core`,
//! C# namespace `AlUmmahNow.Core`.

uniffi::setup_scaffolding!("aun_core");

mod diagnostics;
mod error;
mod platform;

pub use diagnostics::*;
pub use error::AunError;
pub use platform::*;

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
