//! UniFFI facade for `aun-player` (Epic 13 & Epic 14 S7: shared deterministic playback state
//! machine covering queue, repeat/shuffle, sleep timer, resume rules, playback rate, A–B study
//! loops, chapters, and ABR quality caps across Apple, Android, and Desktop).

use std::sync::{Arc, Mutex};

use aun_player as core;

use crate::AunError;
use crate::downloads::OfflineNetworkKind;

#[derive(Debug, Clone, uniffi::Record)]
pub struct PlayerConfig {
    pub resume_min_ms: u64,
    pub completed_fraction: f64,
    pub skip_ms: u64,
    pub min_rate: f64,
    pub max_rate: f64,
    pub restart_threshold_ms: u64,
    pub save_interval_ms: u64,
}

impl From<PlayerConfig> for core::PlayerConfig {
    fn from(c: PlayerConfig) -> Self {
        Self {
            resume_min_ms: c.resume_min_ms,
            completed_fraction: c.completed_fraction,
            skip_ms: c.skip_ms,
            min_rate: c.min_rate,
            max_rate: c.max_rate,
            restart_threshold_ms: c.restart_threshold_ms,
            save_interval_ms: c.save_interval_ms,
        }
    }
}

impl From<core::PlayerConfig> for PlayerConfig {
    fn from(c: core::PlayerConfig) -> Self {
        Self {
            resume_min_ms: c.resume_min_ms,
            completed_fraction: c.completed_fraction,
            skip_ms: c.skip_ms,
            min_rate: c.min_rate,
            max_rate: c.max_rate,
            restart_threshold_ms: c.restart_threshold_ms,
            save_interval_ms: c.save_interval_ms,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PlayerChapter {
    pub start_ms: u64,
    pub title: String,
}

impl From<PlayerChapter> for core::Chapter {
    fn from(c: PlayerChapter) -> Self {
        Self {
            start_ms: c.start_ms,
            title: c.title,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PlayerMediaItem {
    pub id: String,
    pub duration_ms: u64,
    pub resume_position_ms: Option<u64>,
    pub chapters: Vec<PlayerChapter>,
    pub is_live: bool,
}

impl From<PlayerMediaItem> for core::MediaItem {
    fn from(item: PlayerMediaItem) -> Self {
        Self {
            id: item.id,
            duration_ms: item.duration_ms,
            resume_position_ms: item.resume_position_ms,
            chapters: item.chapters.into_iter().map(Into::into).collect(),
            is_live: item.is_live,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PlayerRepeatMode {
    Off,
    One,
    All,
}

impl From<PlayerRepeatMode> for core::RepeatMode {
    fn from(m: PlayerRepeatMode) -> Self {
        match m {
            PlayerRepeatMode::Off => Self::Off,
            PlayerRepeatMode::One => Self::One,
            PlayerRepeatMode::All => Self::All,
        }
    }
}

impl From<core::RepeatMode> for PlayerRepeatMode {
    fn from(m: core::RepeatMode) -> Self {
        match m {
            core::RepeatMode::Off => Self::Off,
            core::RepeatMode::One => Self::One,
            core::RepeatMode::All => Self::All,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PlayerDeviceClass {
    Phone,
    Tablet,
    Desktop,
    Tv,
    Car,
}

impl From<PlayerDeviceClass> for core::DeviceClass {
    fn from(d: PlayerDeviceClass) -> Self {
        match d {
            PlayerDeviceClass::Phone => Self::Phone,
            PlayerDeviceClass::Tablet => Self::Tablet,
            PlayerDeviceClass::Desktop => Self::Desktop,
            PlayerDeviceClass::Tv => Self::Tv,
            PlayerDeviceClass::Car => Self::Car,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PlayerQualitySettings {
    pub data_saver: bool,
    pub user_max_height: Option<u32>,
}

impl From<PlayerQualitySettings> for core::QualitySettings {
    fn from(s: PlayerQualitySettings) -> Self {
        Self {
            data_saver: s.data_saver,
            user_max_height: s.user_max_height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PlayerQualityCap {
    pub max_height: Option<u32>,
    pub max_bitrate_bps: Option<u64>,
}

impl From<core::QualityCap> for PlayerQualityCap {
    fn from(c: core::QualityCap) -> Self {
        Self {
            max_height: c.max_height,
            max_bitrate_bps: c.max_bitrate_bps,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PlayerSleepTimer {
    Off,
    After { duration_ms: u64 },
    EndOfItem,
}

impl From<PlayerSleepTimer> for core::SleepTimer {
    fn from(t: PlayerSleepTimer) -> Self {
        match t {
            PlayerSleepTimer::Off => Self::Off,
            PlayerSleepTimer::After { duration_ms } => Self::After { duration_ms },
            PlayerSleepTimer::EndOfItem => Self::EndOfItem,
        }
    }
}

impl From<core::SleepTimer> for PlayerSleepTimer {
    fn from(t: core::SleepTimer) -> Self {
        match t {
            core::SleepTimer::Off => Self::Off,
            core::SleepTimer::After { duration_ms } => Self::After { duration_ms },
            core::SleepTimer::EndOfItem => Self::EndOfItem,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PlayerPlaybackState {
    Idle,
    Playing,
    Paused,
    Ended,
    Stopped,
    Failed,
}

impl From<core::PlaybackState> for PlayerPlaybackState {
    fn from(s: core::PlaybackState) -> Self {
        match s {
            core::PlaybackState::Idle => Self::Idle,
            core::PlaybackState::Playing => Self::Playing,
            core::PlaybackState::Paused => Self::Paused,
            core::PlaybackState::Ended => Self::Ended,
            core::PlaybackState::Stopped => Self::Stopped,
            core::PlaybackState::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct PlayerAbLoop {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl From<core::AbLoop> for PlayerAbLoop {
    fn from(ab: core::AbLoop) -> Self {
        Self {
            start_ms: ab.start_ms,
            end_ms: ab.end_ms,
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum PlayerInput {
    LoadQueue {
        items: Vec<PlayerMediaItem>,
        start_index: u32,
        play_when_ready: bool,
    },
    SelectItem {
        index: u32,
    },
    Next,
    Previous,
    SetShuffle {
        enabled: bool,
        seed: u64,
    },
    SetRepeatMode {
        mode: PlayerRepeatMode,
    },
    SetAutoplay {
        enabled: bool,
    },
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
    SetAbLoop {
        start_ms: u64,
        end_ms: u64,
    },
    ClearAbLoop,
    SetRepeatCount {
        times: u32,
    },
    AddBookmark {
        label: String,
    },
    SetSleepTimer {
        timer: PlayerSleepTimer,
    },
    SetNetwork {
        network: OfflineNetworkKind,
    },
    SetDeviceClass {
        device: PlayerDeviceClass,
    },
    SetQualitySettings {
        settings: PlayerQualitySettings,
    },
    Tick {
        position_ms: u64,
        duration_ms: u64,
    },
    PlaybackStarted,
    PlaybackPaused,
    Ended,
    Failed {
        code: String,
    },
}

impl From<PlayerInput> for core::PlayerInput {
    fn from(input: PlayerInput) -> Self {
        match input {
            PlayerInput::LoadQueue {
                items,
                start_index,
                play_when_ready,
            } => Self::LoadQueue {
                items: items.into_iter().map(Into::into).collect(),
                start_index,
                play_when_ready,
            },
            PlayerInput::SelectItem { index } => Self::SelectItem { index },
            PlayerInput::Next => Self::Next,
            PlayerInput::Previous => Self::Previous,
            PlayerInput::SetShuffle { enabled, seed } => Self::SetShuffle { enabled, seed },
            PlayerInput::SetRepeatMode { mode } => Self::SetRepeatMode { mode: mode.into() },
            PlayerInput::SetAutoplay { enabled } => Self::SetAutoplay { enabled },
            PlayerInput::Play => Self::Play,
            PlayerInput::Pause => Self::Pause,
            PlayerInput::SeekTo { position_ms } => Self::SeekTo { position_ms },
            PlayerInput::SkipForward => Self::SkipForward,
            PlayerInput::SkipBackward => Self::SkipBackward,
            PlayerInput::NextChapter => Self::NextChapter,
            PlayerInput::PreviousChapter => Self::PreviousChapter,
            PlayerInput::SetRate { rate } => Self::SetRate { rate },
            PlayerInput::Stop => Self::Stop,
            PlayerInput::SetAbLoop { start_ms, end_ms } => Self::SetAbLoop { start_ms, end_ms },
            PlayerInput::ClearAbLoop => Self::ClearAbLoop,
            PlayerInput::SetRepeatCount { times } => Self::SetRepeatCount { times },
            PlayerInput::AddBookmark { label } => Self::AddBookmark { label },
            PlayerInput::SetSleepTimer { timer } => Self::SetSleepTimer { timer: timer.into() },
            PlayerInput::SetNetwork { network } => Self::SetNetwork {
                network: network.into(),
            },
            PlayerInput::SetDeviceClass { device } => Self::SetDeviceClass {
                device: device.into(),
            },
            PlayerInput::SetQualitySettings { settings } => Self::SetQualitySettings {
                settings: settings.into(),
            },
            PlayerInput::Tick {
                position_ms,
                duration_ms,
            } => Self::Tick {
                position_ms,
                duration_ms,
            },
            PlayerInput::PlaybackStarted => Self::PlaybackStarted,
            PlayerInput::PlaybackPaused => Self::PlaybackPaused,
            PlayerInput::Ended => Self::Ended,
            PlayerInput::Failed { code } => Self::Failed { code },
        }
    }
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum PlayerCommand {
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
    SetQualityCap {
        cap: PlayerQualityCap,
    },
    Stop,
    SavePosition {
        item_id: String,
        position_ms: u64,
        duration_ms: u64,
        completed: bool,
    },
    CreateBookmark {
        item_id: String,
        position_ms: u64,
        label: String,
    },
    SleepTimerFired,
    RepeatFinished,
    QueueEnded,
}

impl From<core::PlayerCommand> for PlayerCommand {
    fn from(cmd: core::PlayerCommand) -> Self {
        match cmd {
            core::PlayerCommand::Load {
                item_id,
                start_position_ms,
                play_when_ready,
            } => Self::Load {
                item_id,
                start_position_ms,
                play_when_ready,
            },
            core::PlayerCommand::Play => Self::Play,
            core::PlayerCommand::Pause => Self::Pause,
            core::PlayerCommand::SeekTo { position_ms } => Self::SeekTo { position_ms },
            core::PlayerCommand::SetRate { rate } => Self::SetRate { rate },
            core::PlayerCommand::SetQualityCap { cap } => Self::SetQualityCap { cap: cap.into() },
            core::PlayerCommand::Stop => Self::Stop,
            core::PlayerCommand::SavePosition {
                item_id,
                position_ms,
                duration_ms,
                completed,
            } => Self::SavePosition {
                item_id,
                position_ms,
                duration_ms,
                completed,
            },
            core::PlayerCommand::CreateBookmark {
                item_id,
                position_ms,
                label,
            } => Self::CreateBookmark {
                item_id,
                position_ms,
                label,
            },
            core::PlayerCommand::SleepTimerFired => Self::SleepTimerFired,
            core::PlayerCommand::RepeatFinished => Self::RepeatFinished,
            core::PlayerCommand::QueueEnded => Self::QueueEnded,
        }
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct PlayerSnapshot {
    pub state: PlayerPlaybackState,
    pub current_index: Option<u32>,
    pub current_item_id: Option<String>,
    pub position_ms: u64,
    pub duration_ms: u64,
    pub rate: f64,
    pub shuffle: bool,
    pub repeat_mode: PlayerRepeatMode,
    pub autoplay: bool,
    pub ab_loop: Option<PlayerAbLoop>,
    pub repeat_times: u32,
    pub repeat_passes_done: u32,
    pub sleep_timer: PlayerSleepTimer,
    pub sleep_deadline_ms: Option<i64>,
    pub quality_cap: PlayerQualityCap,
    pub current_chapter: Option<u32>,
}

impl From<core::PlayerSnapshot> for PlayerSnapshot {
    fn from(s: core::PlayerSnapshot) -> Self {
        Self {
            state: s.state.into(),
            current_index: s.current_index,
            current_item_id: s.current_item_id,
            position_ms: s.position_ms,
            duration_ms: s.duration_ms,
            rate: s.rate,
            shuffle: s.shuffle,
            repeat_mode: s.repeat_mode.into(),
            autoplay: s.autoplay,
            ab_loop: s.ab_loop.map(Into::into),
            repeat_times: s.repeat_times,
            repeat_passes_done: s.repeat_passes_done,
            sleep_timer: s.sleep_timer.into(),
            sleep_deadline_ms: s.sleep_deadline_ms,
            quality_cap: s.quality_cap.into(),
            current_chapter: s.current_chapter,
        }
    }
}

/// Thread-safe UniFFI wrapper around [`aun_player::Player`].
#[derive(Debug, uniffi::Object)]
pub struct PlayerMachine {
    inner: Mutex<core::Player>,
}

#[uniffi::export]
impl PlayerMachine {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core::Player::default()),
        })
    }

    #[uniffi::constructor]
    pub fn with_config(config: PlayerConfig) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(core::Player::new(config.into())),
        })
    }

    pub fn config(&self) -> Result<PlayerConfig, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Internal {
            detail: "player lock poisoned".into(),
        })?;
        Ok(guard.config().clone().into())
    }

    pub fn handle(&self, input: PlayerInput, now_ms: i64) -> Result<Vec<PlayerCommand>, AunError> {
        let mut guard = self.inner.lock().map_err(|_| AunError::Internal {
            detail: "player lock poisoned".into(),
        })?;
        Ok(guard
            .handle(input.into(), now_ms)
            .into_iter()
            .map(Into::into)
            .collect())
    }

    pub fn snapshot(&self) -> Result<PlayerSnapshot, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Internal {
            detail: "player lock poisoned".into(),
        })?;
        Ok(guard.snapshot().into())
    }

    pub fn next_deadline_ms(&self) -> Result<Option<i64>, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Internal {
            detail: "player lock poisoned".into(),
        })?;
        Ok(guard.next_deadline_ms())
    }

    pub fn play_order(&self) -> Result<Vec<u32>, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Internal {
            detail: "player lock poisoned".into(),
        })?;
        Ok(guard.play_order().iter().map(|&i| i as u32).collect())
    }

    pub fn resume_position(&self, item: PlayerMediaItem) -> Result<u64, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Internal {
            detail: "player lock poisoned".into(),
        })?;
        Ok(guard.resume_position(&item.into()))
    }

    pub fn is_completed(&self, position_ms: u64, duration_ms: u64) -> Result<bool, AunError> {
        let guard = self.inner.lock().map_err(|_| AunError::Internal {
            detail: "player lock poisoned".into(),
        })?;
        Ok(guard.is_completed(position_ms, duration_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_machine_queue_and_sleep_timer_roundtrip() {
        let machine = PlayerMachine::new();
        let cmds = machine
            .handle(
                PlayerInput::LoadQueue {
                    items: vec![
                        PlayerMediaItem {
                            id: "v1".into(),
                            duration_ms: 600_000,
                            resume_position_ms: Some(45_000),
                            chapters: vec![],
                            is_live: false,
                        },
                        PlayerMediaItem {
                            id: "v2".into(),
                            duration_ms: 300_000,
                            resume_position_ms: None,
                            chapters: vec![],
                            is_live: false,
                        },
                    ],
                    start_index: 0,
                    play_when_ready: true,
                },
                1_000,
            )
            .unwrap();
        assert!(cmds.iter().any(|c| matches!(
            c,
            PlayerCommand::Load {
                item_id,
                start_position_ms: 45_000,
                play_when_ready: true
            } if item_id == "v1"
        )));

        machine
            .handle(
                PlayerInput::SetSleepTimer {
                    timer: PlayerSleepTimer::After { duration_ms: 60_000 },
                },
                1_000,
            )
            .unwrap();
        assert_eq!(machine.next_deadline_ms().unwrap(), Some(61_000));

        let fired = machine
            .handle(
                PlayerInput::Tick {
                    position_ms: 105_000,
                    duration_ms: 600_000,
                },
                61_000,
            )
            .unwrap();
        assert!(fired.iter().any(|c| matches!(c, PlayerCommand::SleepTimerFired)));
        assert_eq!(machine.snapshot().unwrap().state, PlayerPlaybackState::Paused);
    }
}
