# Changelog

All notable changes to `aun-core` are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/). While the version is `0.x`, a minor bump may break the
FFI; such changes are listed under **Breaking**.

## [Unreleased]

## [0.1.0] - 2026-11-08

### Added
- Cargo workspace: `aun-common`, `aun-api`, `aun-auth`, `aun-sync`, `aun-upload`, `aun-media`,
  `aun-i18n`, `aun-telemetry` and the `aun-ffi` UniFFI facade (library `aun_core`).
- FFI error model `AunError` and platform adapters `SecureStorage` and `Connectivity`.
- `core_version()` and `selftest_*` functions used by the Swift, Kotlin and C# smoke tests.
- CI: fmt, clippy, cargo-deny, tests on Linux/macOS/Windows; cross-builds for iOS, iOS simulator,
  macOS universal, tvOS, Android (3 ABIs), Windows x64/arm64, Linux x64/arm64; generated bindings
  with smoke tests in Swift, Kotlin (JVM) and C#.
- Tag-triggered release: XCFramework + Swift Package, AAR (GitHub Packages Maven), NuGet
  (GitHub Packages).
