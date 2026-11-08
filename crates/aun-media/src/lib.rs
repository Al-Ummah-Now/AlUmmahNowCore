//! Studio media engine (Epic 30). Everything here is plain Rust: `aun-ffi` wraps it for the apps.
//!
//! | Module | Epic | What it does |
//! |---|---|---|
//! | [`probe`] | 30.1 | container/codec probing (ISO BMFF parser, Symphonia for audio) |
//! | [`audio`] | 30.1 | streaming audio decode |
//! | [`analysis`] | 30.1 | EBU R128 loudness, silence and scene-change detection |
//! | [`edl`] | 30.5 | versioned edit project, operations, render planning |
//! | [`job`] | 30.2 | presets, `JobPlan` DAG, runner, backends, conformance suite |
//! | [`transcribe`] | 30.4 | model management, subtitles, chapter suggestions, whisper (feature `whisper`) |
//! | [`waveform`] | 30.3 | multi-resolution min/max peaks |
//! | [`thumbnails`] | 30.3 | scene-aware filmstrip timestamps |
//! | [`text`] | 30.6 | bidi, line breaking and Arabic-correct shaping (feature `harfbuzz`) |
//!
//! Durations and timestamps are `u64` milliseconds. Long operations take a
//! [`CancellationToken`](tokio_util::sync::CancellationToken) and are blocking: callers run them on
//! `spawn_blocking`.

pub mod analysis;
pub mod audio;
pub mod edl;
mod isobmff;
pub mod job;
pub mod probe;
mod refcontainer;
#[cfg(feature = "harfbuzz")]
pub mod text;
pub mod thumbnails;
pub mod transcribe;
pub mod waveform;

pub use tokio_util::sync::CancellationToken;
