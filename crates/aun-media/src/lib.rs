//! Studio media engine (Epic 30). Everything here is plain Rust: `aun-ffi` wraps it for the apps.
//!
//! | Module | Epic | What it does |
//! |---|---|---|
//! | [`probe`] | 30.1 | container/codec probing (ISO BMFF parser, Symphonia for audio) |
//! | [`audio`] | 30.1 | streaming audio decode |
//! | [`analysis`] | 30.1 | EBU R128 loudness, silence and scene-change detection |
//! | [`job`] | 30.2 | presets, `JobPlan` DAG, runner, backends, conformance suite |
//! | [`waveform`] | 30.3 | multi-resolution min/max peaks |
//! | [`thumbnails`] | 30.3 | scene-aware filmstrip timestamps |
//!
//! Durations and timestamps are `u64` milliseconds. Long operations take a
//! [`CancellationToken`](tokio_util::sync::CancellationToken) and are blocking: callers run them on
//! `spawn_blocking`.

pub mod analysis;
pub mod audio;
mod isobmff;
pub mod job;
pub mod probe;
mod refcontainer;
pub mod thumbnails;
pub mod waveform;

pub use tokio_util::sync::CancellationToken;
