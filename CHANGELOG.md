# Changelog

All notable changes to `aun-core` are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/). While the version is `0.x`, a minor bump may break the
FFI; such changes are listed under **Breaking**.

## [Unreleased]

### Added
- `aun-media`: `UploadHandoff` record and builder linking a finished job to `aun-upload`.
- `aun-media`: Whisper model manager (resumable, SHA-256 verified downloads), WebVTT and SRT writers,
  chapter suggestions, and whisper.cpp transcription behind the `whisper` feature.
- `aun-player`: platform-neutral player behaviour model for the Epic 13 native players. It is a
  deterministic state machine (inputs + wall-clock → commands) covering the queue with autoplay,
  seeded shuffle and repeat, resume rules (30 s / 95 %), ABR quality caps per device class, network
  and data saver, rate 0.5–2×, ±10 s skip, A–B loop, repeat-N, bookmarks, the sleep timer and chapter
  navigation. Spec in `docs/PLAYER_BEHAVIOUR.md`; 38 JSON conformance scenarios
  (`crates/aun-player/conformance/`) with a Rust runner, and native wrappers can replay them.
- `aun-media`: container/codec probing, EBU R128 loudness, silence and scene detection, waveform
  peaks, scene-aware thumbnail timestamps.
- `aun-media`: encode presets, validated `JobPlan` DAG, job runner with pause/resume/cancel, software
  reference backend and a backend conformance suite.
- `aun-media`: versioned EDL project format (`schemas/edl/v1.json`), migrations, validation, ripple
  delete/split/trim, and render planning that decides stream copy versus re-encode per segment.
- `aun-media`: bidi-aware text shaping and line breaking for titles and burned-in captions.

### Changed

- Release workflow no longer pushes to `main`: the Package.swift release commit is created on top
  of the `vX.Y.Z` tag and only the SwiftPM tag `X.Y.Z` is pushed (`scripts/swiftpm-release-tag.sh`,
  `docs/RELEASING.md`). Swift consumers depend on `from: "X.Y.Z"`.

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
