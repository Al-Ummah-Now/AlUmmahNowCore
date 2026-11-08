//! Platform-neutral player behaviour model for the native players (AVKit, ExoPlayer/Media3,
//! Windows.Media.Playback, GStreamer, AVPlay).
//!
//! The model is a deterministic state machine: the native wrapper feeds it [`PlayerInput`]s (user
//! intents and reports from the platform player) together with the wall-clock time, and executes
//! the [`PlayerCommand`]s it returns. Queue/autoplay, resume rules, quality caps, playback rate,
//! skip, A–B loop, repeat-N, bookmarks, the sleep timer and chapter navigation all live here so every
//! platform behaves the same. The normative spec is `docs/PLAYER_BEHAVIOUR.md`; the conformance
//! scenarios in `conformance/*.json` pin it down.

pub mod conformance;
mod machine;
mod model;
pub mod quality;

pub use machine::{Player, shuffle_order};
pub use model::{
    AbLoop, Chapter, DeviceClass, MediaItem, NetworkKind, PlaybackState, PlayerCommand, PlayerConfig,
    PlayerInput, PlayerSnapshot, QualityCap, QualitySettings, RepeatMode, SleepTimer,
};
