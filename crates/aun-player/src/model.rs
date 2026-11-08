//! Plain data types of the behaviour model: inputs, commands, configuration and the snapshot.
//! Every type is FFI-portable (strings, integers, `f64`, `bool`, `Vec`, `Option`, enums) and
//! serialises to the JSON used by the conformance fixtures (`conformance/*.json`).

use serde::{Deserialize, Serialize};

/// Tunable thresholds. The defaults are the product behaviour described in `docs/PLAYER_BEHAVIOUR.md`;
/// apps should not change them except in tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerConfig {
    /// Resume only if the saved position is at least this far in (default 30 s).
    pub resume_min_ms: u64,
    /// Resume only if less than this fraction was watched (default 0.95). At or beyond it the item
    /// counts as completed and starts from the beginning.
    pub completed_fraction: f64,
    /// Skip forward/backward step (default 10 s).
    pub skip_ms: u64,
    /// Playback-rate bounds (default 0.5×–2×).
    pub min_rate: f64,
    pub max_rate: f64,
    /// "Previous" restarts the current item when more than this far in (default 3 s); the same rule
    /// applies to "previous chapter".
    pub restart_threshold_ms: u64,
    /// While playing, emit [`PlayerCommand::SavePosition`] at most this often (default 15 s).
    /// `aun-sync` debounces again before anything goes over the network.
    pub save_interval_ms: u64,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            resume_min_ms: 30_000,
            completed_fraction: 0.95,
            skip_ms: 10_000,
            min_rate: 0.5,
            max_rate: 2.0,
            restart_threshold_ms: 3_000,
            save_interval_ms: 15_000,
        }
    }
}

/// A chapter marker inside an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chapter {
    pub start_ms: u64,
    #[serde(default)]
    pub title: String,
}

/// One playable item in the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaItem {
    /// Video id (api-videos).
    pub id: String,
    /// Duration in ms; 0 when unknown until the native player reports it.
    #[serde(default)]
    pub duration_ms: u64,
    /// Saved position from watch-state sync, if any.
    #[serde(default)]
    pub resume_position_ms: Option<u64>,
    /// Chapters in any order; the model sorts them.
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    /// Live streams never resume, have no "completed" state and ignore repeat/A–B loop.
    #[serde(default)]
    pub is_live: bool,
}

/// Queue repeat mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    #[default]
    Off,
    /// Repeat the current item forever.
    One,
    /// Wrap from the last item back to the first.
    All,
}

/// Network type reported by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkKind {
    #[default]
    Unknown,
    Wifi,
    Ethernet,
    Cellular,
    Offline,
}

/// Device class, which bounds the maximum useful resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    #[default]
    Phone,
    Tablet,
    Desktop,
    Tv,
    /// In-car head unit (video only while parked); kept small on purpose.
    Car,
}

/// User quality preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct QualitySettings {
    /// "Data saver" switch: low cap on every network.
    pub data_saver: bool,
    /// The user picked a fixed maximum (e.g. 720) in the quality menu; `None` = Auto.
    pub user_max_height: Option<u32>,
}

/// Limits handed to the native ABR. `None` means unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityCap {
    pub max_height: Option<u32>,
    pub max_bitrate_bps: Option<u64>,
}

/// Sleep timer setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SleepTimer {
    #[default]
    Off,
    /// Pause after this much wall-clock time (the UI offers minutes).
    After { duration_ms: u64 },
    /// Stop at the end of the current item (no autoplay).
    EndOfItem,
}

/// Coarse playback state of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackState {
    /// Nothing loaded.
    #[default]
    Idle,
    Playing,
    Paused,
    /// The current item finished and nothing was started after it.
    Ended,
    Stopped,
    Failed,
}

/// Everything that drives the model: user intents and reports from the native player.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlayerInput {
    // ---- queue ---------------------------------------------------------------------------
    /// Replace the queue and load `items[start_index]`.
    LoadQueue {
        items: Vec<MediaItem>,
        #[serde(default)]
        start_index: u32,
        /// Start playing as soon as the item is ready (default true).
        #[serde(default = "default_true")]
        play_when_ready: bool,
    },
    /// Jump to an item of the queue (in queue order, not shuffle order).
    SelectItem {
        index: u32,
    },
    Next,
    Previous,
    SetShuffle {
        enabled: bool,
        /// Seed for the deterministic shuffle (apps pass a random value; fixtures a fixed one).
        #[serde(default)]
        seed: u64,
    },
    SetRepeatMode {
        mode: RepeatMode,
    },
    SetAutoplay {
        enabled: bool,
    },

    // ---- transport -----------------------------------------------------------------------
    Play,
    Pause,
    SeekTo {
        position_ms: u64,
    },
    SkipForward,
    SkipBackward,
    NextChapter,
    PreviousChapter,
    SetRate {
        rate: f64,
    },
    Stop,

    // ---- study tools ---------------------------------------------------------------------
    /// Loop between A and B (ms). Replaces any previous loop.
    SetAbLoop {
        start_ms: u64,
        end_ms: u64,
    },
    ClearAbLoop,
    /// Total passes of the A–B segment (or of the whole item when no loop is set), counting the
    /// current pass. 0 clears it: an A–B loop then repeats forever, an item plays once.
    SetRepeatCount {
        times: u32,
    },
    AddBookmark {
        #[serde(default)]
        label: String,
    },
    SetSleepTimer {
        timer: SleepTimer,
    },

    // ---- environment ---------------------------------------------------------------------
    SetNetwork {
        network: NetworkKind,
    },
    SetDeviceClass {
        device: DeviceClass,
    },
    SetQualitySettings {
        settings: QualitySettings,
    },

    // ---- native reports ------------------------------------------------------------------
    /// Periodic position report while playing (every 250–1000 ms), also used as the timer tick.
    Tick {
        position_ms: u64,
        /// Duration if the native player knows it (0 = unknown / unchanged).
        #[serde(default)]
        duration_ms: u64,
    },
    /// The native player started or resumed playback on its own (remote control, AirPlay, …).
    PlaybackStarted,
    /// The native player paused on its own (audio interruption, headphones unplugged, …).
    PlaybackPaused,
    /// The current item played to its end.
    Ended,
    /// The native player failed. `code` is a stable string (e.g. `network`, `decode`, `drm`).
    Failed {
        code: String,
    },
}

fn default_true() -> bool {
    true
}

/// What the native wrapper must do (or may surface to the UI), in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlayerCommand {
    /// Load an item and seek to `start_position_ms` before the first frame.
    Load {
        item_id: String,
        start_position_ms: u64,
        play_when_ready: bool,
    },
    Play,
    Pause,
    SeekTo {
        position_ms: u64,
    },
    SetRate {
        rate: f64,
    },
    /// Apply ABR limits (AVPlayerItem.preferredPeakBitRate / preferredMaximumResolution,
    /// ExoPlayer track-selection parameters, …).
    SetQualityCap {
        cap: QualityCap,
    },
    /// Release the player (queue finished, user stopped).
    Stop,
    /// Persist the watch position (feed into `aun-sync`).
    SavePosition {
        item_id: String,
        position_ms: u64,
        duration_ms: u64,
        completed: bool,
    },
    /// Create a bookmark/note at a timestamp (feed into `aun-sync`).
    CreateBookmark {
        item_id: String,
        position_ms: u64,
        label: String,
    },
    /// The sleep timer fired; the UI may show a "Sleep timer ended" toast.
    SleepTimerFired,
    /// The requested repeat count of the A–B segment / item is done.
    RepeatFinished,
    /// The end of the queue was reached and nothing else will play.
    QueueEnded,
}

/// A–B loop state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbLoop {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Read-only view of the model for the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    pub state: PlaybackState,
    /// Index into the queue (queue order), `None` when idle.
    pub current_index: Option<u32>,
    pub current_item_id: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub rate: f64,
    pub shuffle: bool,
    pub repeat_mode: RepeatMode,
    pub autoplay: bool,
    pub ab_loop: Option<AbLoop>,
    /// Requested passes (0 = not set) and passes completed so far.
    pub repeat_times: u32,
    pub repeat_passes_done: u32,
    pub sleep_timer: SleepTimer,
    /// Wall-clock deadline of an `After` sleep timer, Unix ms.
    pub sleep_deadline_ms: Option<i64>,
    pub quality_cap: QualityCap,
    /// Index of the chapter containing the position, if the item has chapters.
    pub current_chapter: Option<u32>,
}
