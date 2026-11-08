//! The deterministic player state machine. Inputs go in with the current wall-clock time; commands
//! come out. No I/O, no timers, no randomness except the seeded shuffle, so every platform gets the
//! same answer for the same script (see `conformance`).

use crate::model::{
    AbLoop, DeviceClass, MediaItem, NetworkKind, PlaybackState, PlayerCommand, PlayerConfig, PlayerInput,
    PlayerSnapshot, QualityCap, QualitySettings, RepeatMode, SleepTimer,
};
use crate::quality::quality_cap;

/// The behaviour model. Not thread-safe by itself: wrap it in a `Mutex` (the FFI facade does).
#[derive(Debug, Clone)]
pub struct Player {
    config: PlayerConfig,
    items: Vec<MediaItem>,
    /// Play order: queue indices, shuffled or identity.
    order: Vec<usize>,
    /// Position of the current item inside `order`.
    order_pos: usize,
    current: Option<usize>,
    state: PlaybackState,
    position_ms: u64,
    duration_ms: u64,
    rate: f64,
    shuffle: bool,
    shuffle_seed: u64,
    repeat_mode: RepeatMode,
    autoplay: bool,
    ab_loop: Option<AbLoop>,
    repeat_times: u32,
    repeat_passes_done: u32,
    sleep_timer: SleepTimer,
    sleep_deadline_ms: Option<i64>,
    network: NetworkKind,
    device: DeviceClass,
    quality: QualitySettings,
    last_cap: Option<QualityCap>,
    last_save_ms: i64,
}

impl Default for Player {
    fn default() -> Self {
        Self::new(PlayerConfig::default())
    }
}

impl Player {
    pub fn new(config: PlayerConfig) -> Self {
        Self {
            config,
            items: Vec::new(),
            order: Vec::new(),
            order_pos: 0,
            current: None,
            state: PlaybackState::Idle,
            position_ms: 0,
            duration_ms: 0,
            rate: 1.0,
            shuffle: false,
            shuffle_seed: 0,
            repeat_mode: RepeatMode::Off,
            autoplay: true,
            ab_loop: None,
            repeat_times: 0,
            repeat_passes_done: 0,
            sleep_timer: SleepTimer::Off,
            sleep_deadline_ms: None,
            network: NetworkKind::Unknown,
            device: DeviceClass::Phone,
            quality: QualitySettings::default(),
            last_cap: None,
            last_save_ms: 0,
        }
    }

    pub fn config(&self) -> &PlayerConfig {
        &self.config
    }

    /// Feeds one input; returns the commands to execute, in order.
    pub fn handle(&mut self, input: PlayerInput, now_ms: i64) -> Vec<PlayerCommand> {
        let mut out = Vec::new();
        match input {
            PlayerInput::LoadQueue {
                items,
                start_index,
                play_when_ready,
            } => self.load_queue(items, start_index as usize, play_when_ready, now_ms, &mut out),
            PlayerInput::SelectItem { index } => {
                let index = index as usize;
                if index < self.items.len()
                    && let Some(pos) = self.order.iter().position(|&i| i == index)
                {
                    self.load_at(pos, true, now_ms, &mut out);
                }
            }
            PlayerInput::Next => {
                if let Some(pos) = self.next_pos(self.repeat_mode == RepeatMode::All) {
                    self.load_at(pos, true, now_ms, &mut out);
                }
            }
            PlayerInput::Previous => self.previous(now_ms, &mut out),
            PlayerInput::SetShuffle { enabled, seed } => self.set_shuffle(enabled, seed),
            PlayerInput::SetRepeatMode { mode } => self.repeat_mode = mode,
            PlayerInput::SetAutoplay { enabled } => self.autoplay = enabled,

            PlayerInput::Play => self.play(now_ms, &mut out),
            PlayerInput::Pause => {
                if self.state == PlaybackState::Playing {
                    self.state = PlaybackState::Paused;
                    out.push(PlayerCommand::Pause);
                    self.save(now_ms, &mut out);
                }
            }
            PlayerInput::SeekTo { position_ms } => self.seek(position_ms, now_ms, &mut out),
            PlayerInput::SkipForward => {
                let target = self.position_ms.saturating_add(self.config.skip_ms);
                self.seek(target, now_ms, &mut out);
            }
            PlayerInput::SkipBackward => {
                let target = self.position_ms.saturating_sub(self.config.skip_ms);
                self.seek(target, now_ms, &mut out);
            }
            PlayerInput::NextChapter => {
                if let Some(start) = self.next_chapter_start() {
                    self.seek(start, now_ms, &mut out);
                }
            }
            PlayerInput::PreviousChapter => {
                if let Some(start) = self.previous_chapter_start() {
                    self.seek(start, now_ms, &mut out);
                }
            }
            PlayerInput::SetRate { rate } => {
                if rate.is_finite() {
                    let rate = rate.clamp(self.config.min_rate, self.config.max_rate);
                    if rate != self.rate {
                        self.rate = rate;
                        if self.current.is_some() {
                            out.push(PlayerCommand::SetRate { rate });
                        }
                    }
                }
            }
            PlayerInput::Stop => {
                if self.current.is_some() && self.state != PlaybackState::Stopped {
                    self.save_if_active(now_ms, &mut out);
                    self.state = PlaybackState::Stopped;
                    self.sleep_timer = SleepTimer::Off;
                    self.sleep_deadline_ms = None;
                    out.push(PlayerCommand::Stop);
                }
            }

            PlayerInput::SetAbLoop { start_ms, end_ms } => {
                self.set_ab_loop(start_ms, end_ms, now_ms, &mut out)
            }
            PlayerInput::ClearAbLoop => {
                self.ab_loop = None;
                self.repeat_passes_done = 0;
            }
            PlayerInput::SetRepeatCount { times } => {
                self.repeat_times = times;
                self.repeat_passes_done = 0;
            }
            PlayerInput::AddBookmark { label } => {
                if let Some(item) = self.current_item() {
                    out.push(PlayerCommand::CreateBookmark {
                        item_id: item.id.clone(),
                        position_ms: self.position_ms,
                        label,
                    });
                }
            }
            PlayerInput::SetSleepTimer { timer } => self.set_sleep_timer(timer, now_ms),

            PlayerInput::SetNetwork { network } => {
                self.network = network;
                self.emit_cap_if_changed(&mut out);
            }
            PlayerInput::SetDeviceClass { device } => {
                self.device = device;
                self.emit_cap_if_changed(&mut out);
            }
            PlayerInput::SetQualitySettings { settings } => {
                self.quality = settings;
                self.emit_cap_if_changed(&mut out);
            }

            PlayerInput::Tick {
                position_ms,
                duration_ms,
            } => self.tick(position_ms, duration_ms, now_ms, &mut out),
            PlayerInput::PlaybackStarted => {
                if self.current.is_some()
                    && matches!(self.state, PlaybackState::Paused | PlaybackState::Ended)
                {
                    self.state = PlaybackState::Playing;
                    self.expire_sleep_timer_if_due(now_ms);
                }
            }
            PlayerInput::PlaybackPaused => {
                if self.state == PlaybackState::Playing {
                    self.state = PlaybackState::Paused;
                    self.save(now_ms, &mut out);
                }
            }
            PlayerInput::Ended => self.ended(now_ms, &mut out),
            PlayerInput::Failed { code: _ } => {
                if self.current.is_some() {
                    self.save_if_active(now_ms, &mut out);
                    self.state = PlaybackState::Failed;
                }
            }
        }
        out
    }

    /// The next wall-clock time (Unix ms) at which the model needs a [`PlayerInput::Tick`] even if
    /// the native player sends none (the sleep-timer deadline while playing).
    pub fn next_deadline_ms(&self) -> Option<i64> {
        if self.state == PlaybackState::Playing {
            self.sleep_deadline_ms
        } else {
            None
        }
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        let item = self.current_item();
        PlayerSnapshot {
            state: self.state,
            current_index: self.current.map(|i| i as u32),
            current_item_id: item.map(|i| i.id.clone()),
            position_ms: self.position_ms,
            duration_ms: self.duration_ms,
            rate: self.rate,
            shuffle: self.shuffle,
            repeat_mode: self.repeat_mode,
            autoplay: self.autoplay,
            ab_loop: self.ab_loop,
            repeat_times: self.repeat_times,
            repeat_passes_done: self.repeat_passes_done,
            sleep_timer: self.sleep_timer,
            sleep_deadline_ms: self.sleep_deadline_ms,
            quality_cap: self.current_cap(),
            current_chapter: self.current_chapter_index().map(|i| i as u32),
        }
    }

    /// Queue indices in play order (identity unless shuffled).
    pub fn play_order(&self) -> &[usize] {
        &self.order
    }

    /// The position to start `item` at, applying the resume rules.
    pub fn resume_position(&self, item: &MediaItem) -> u64 {
        let Some(saved) = item.resume_position_ms else {
            return 0;
        };
        if item.is_live || saved < self.config.resume_min_ms {
            return 0;
        }
        if item.duration_ms > 0 && self.is_completed(saved, item.duration_ms) {
            return 0;
        }
        saved
    }

    /// Whether `position_ms` counts as "watched" for an item of `duration_ms`.
    pub fn is_completed(&self, position_ms: u64, duration_ms: u64) -> bool {
        duration_ms > 0 && position_ms as f64 >= duration_ms as f64 * self.config.completed_fraction
    }

    // ---- internals -----------------------------------------------------------------------------

    fn current_item(&self) -> Option<&MediaItem> {
        self.current.and_then(|i| self.items.get(i))
    }

    fn current_cap(&self) -> QualityCap {
        quality_cap(self.device, self.network, self.quality)
    }

    fn emit_cap_if_changed(&mut self, out: &mut Vec<PlayerCommand>) {
        if self.current.is_none() {
            return;
        }
        let cap = self.current_cap();
        if self.last_cap != Some(cap) {
            self.last_cap = Some(cap);
            out.push(PlayerCommand::SetQualityCap { cap });
        }
    }

    fn load_queue(
        &mut self,
        items: Vec<MediaItem>,
        start_index: usize,
        play_when_ready: bool,
        now_ms: i64,
        out: &mut Vec<PlayerCommand>,
    ) {
        self.save_if_active(now_ms, out);
        if items.is_empty() {
            let had_item = self.current.is_some();
            *self = self.clone_settings();
            if had_item {
                out.push(PlayerCommand::Stop);
            }
            return;
        }
        let start = start_index.min(items.len() - 1);
        self.items = items;
        self.rebuild_order(start);
        let pos = self.order.iter().position(|&i| i == start).unwrap_or(0);
        // Already saved above.
        self.state = PlaybackState::Idle;
        self.load_at(pos, play_when_ready, now_ms, out);
    }

    /// Keeps user settings (rate, shuffle, repeat, autoplay, quality, sleep timer) but no queue.
    fn clone_settings(&self) -> Self {
        let mut fresh = Self::new(self.config.clone());
        fresh.rate = self.rate;
        fresh.shuffle = self.shuffle;
        fresh.shuffle_seed = self.shuffle_seed;
        fresh.repeat_mode = self.repeat_mode;
        fresh.autoplay = self.autoplay;
        fresh.network = self.network;
        fresh.device = self.device;
        fresh.quality = self.quality;
        fresh
    }

    fn rebuild_order(&mut self, first: usize) {
        self.order = if self.shuffle {
            shuffle_order(self.items.len(), first, self.shuffle_seed)
        } else {
            (0..self.items.len()).collect()
        };
    }

    fn set_shuffle(&mut self, enabled: bool, seed: u64) {
        self.shuffle = enabled;
        self.shuffle_seed = seed;
        let Some(cur) = self.current else {
            return;
        };
        self.rebuild_order(cur);
        self.order_pos = self.order.iter().position(|&i| i == cur).unwrap_or(0);
    }

    fn next_pos(&self, wrap: bool) -> Option<usize> {
        self.current?;
        if self.order_pos + 1 < self.order.len() {
            Some(self.order_pos + 1)
        } else if wrap && !self.order.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    fn previous_pos(&self, wrap: bool) -> Option<usize> {
        self.current?;
        if self.order_pos > 0 {
            Some(self.order_pos - 1)
        } else if wrap && !self.order.is_empty() {
            Some(self.order.len() - 1)
        } else {
            None
        }
    }

    fn load_at(
        &mut self,
        order_pos: usize,
        play_when_ready: bool,
        now_ms: i64,
        out: &mut Vec<PlayerCommand>,
    ) {
        let Some(&index) = self.order.get(order_pos) else {
            return;
        };
        self.save_if_active(now_ms, out);
        let Some(item) = self.items.get(index) else {
            return;
        };
        let start = self.resume_position(item);
        let item_id = item.id.clone();
        self.duration_ms = item.duration_ms;
        self.order_pos = order_pos;
        self.current = Some(index);
        self.position_ms = start;
        self.ab_loop = None;
        self.repeat_times = 0;
        self.repeat_passes_done = 0;
        self.last_save_ms = now_ms;
        self.state = if play_when_ready {
            PlaybackState::Playing
        } else {
            PlaybackState::Paused
        };
        out.push(PlayerCommand::Load {
            item_id,
            start_position_ms: start,
            play_when_ready,
        });
        let cap = self.current_cap();
        self.last_cap = Some(cap);
        out.push(PlayerCommand::SetQualityCap { cap });
        if self.rate != 1.0 {
            out.push(PlayerCommand::SetRate { rate: self.rate });
        }
        if play_when_ready {
            self.expire_sleep_timer_if_due(now_ms);
        }
    }

    fn previous(&mut self, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        if self.current.is_none() {
            return;
        }
        if self.position_ms > self.config.restart_threshold_ms {
            self.seek(0, now_ms, out);
            return;
        }
        match self.previous_pos(self.repeat_mode == RepeatMode::All) {
            Some(pos) => self.load_at(pos, true, now_ms, out),
            None => self.seek(0, now_ms, out),
        }
    }

    fn play(&mut self, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        if self.current.is_none() {
            return;
        }
        match self.state {
            PlaybackState::Idle | PlaybackState::Playing => {}
            PlaybackState::Paused => {
                self.state = PlaybackState::Playing;
                self.last_save_ms = now_ms;
                out.push(PlayerCommand::Play);
            }
            PlaybackState::Ended => {
                // Replay the finished item from the start.
                self.position_ms = 0;
                self.state = PlaybackState::Playing;
                self.last_save_ms = now_ms;
                out.push(PlayerCommand::SeekTo { position_ms: 0 });
                out.push(PlayerCommand::Play);
            }
            PlaybackState::Stopped | PlaybackState::Failed => {
                // The native player was released (or broke): reload where we were.
                let Some(item) = self.current_item() else {
                    return;
                };
                let item_id = item.id.clone();
                self.state = PlaybackState::Playing;
                self.last_save_ms = now_ms;
                out.push(PlayerCommand::Load {
                    item_id,
                    start_position_ms: self.position_ms,
                    play_when_ready: true,
                });
                let cap = self.current_cap();
                self.last_cap = Some(cap);
                out.push(PlayerCommand::SetQualityCap { cap });
                if self.rate != 1.0 {
                    out.push(PlayerCommand::SetRate { rate: self.rate });
                }
            }
        }
        self.expire_sleep_timer_if_due(now_ms);
    }

    fn seek(&mut self, target: u64, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        let Some(item) = self.current_item() else {
            return;
        };
        let live = item.is_live;
        let target = if !live && self.duration_ms > 0 {
            target.min(self.duration_ms)
        } else {
            target
        };
        self.position_ms = target;
        if self.state == PlaybackState::Ended {
            self.state = PlaybackState::Paused;
        }
        out.push(PlayerCommand::SeekTo { position_ms: target });
        self.save_if_active(now_ms, out);
    }

    fn set_ab_loop(&mut self, start_ms: u64, end_ms: u64, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        let Some(item) = self.current_item() else {
            return;
        };
        if item.is_live {
            return;
        }
        let end_ms = if self.duration_ms > 0 {
            end_ms.min(self.duration_ms)
        } else {
            end_ms
        };
        if start_ms >= end_ms {
            return;
        }
        self.ab_loop = Some(AbLoop { start_ms, end_ms });
        self.repeat_passes_done = 0;
        if self.position_ms < start_ms || self.position_ms >= end_ms {
            self.seek(start_ms, now_ms, out);
        }
    }

    fn set_sleep_timer(&mut self, timer: SleepTimer, now_ms: i64) {
        match timer {
            SleepTimer::After { duration_ms: 0 } | SleepTimer::Off => {
                self.sleep_timer = SleepTimer::Off;
                self.sleep_deadline_ms = None;
            }
            SleepTimer::After { duration_ms } => {
                self.sleep_timer = timer;
                self.sleep_deadline_ms =
                    Some(now_ms.saturating_add(i64::try_from(duration_ms).unwrap_or(i64::MAX)));
            }
            SleepTimer::EndOfItem => {
                self.sleep_timer = timer;
                self.sleep_deadline_ms = None;
            }
        }
    }

    /// A deadline that passed while paused is dropped silently when playback resumes.
    fn expire_sleep_timer_if_due(&mut self, now_ms: i64) {
        if matches!(self.sleep_deadline_ms, Some(d) if now_ms >= d) {
            self.sleep_timer = SleepTimer::Off;
            self.sleep_deadline_ms = None;
        }
    }

    fn tick(&mut self, position_ms: u64, duration_ms: u64, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        if self.current.is_none() {
            return;
        }
        if duration_ms > 0 {
            self.duration_ms = duration_ms;
        }
        let previous = self.position_ms;
        self.position_ms = position_ms;
        if self.state != PlaybackState::Playing {
            return;
        }
        if matches!(self.sleep_deadline_ms, Some(d) if now_ms >= d) {
            self.sleep_timer = SleepTimer::Off;
            self.sleep_deadline_ms = None;
            self.state = PlaybackState::Paused;
            out.push(PlayerCommand::Pause);
            self.save(now_ms, out);
            out.push(PlayerCommand::SleepTimerFired);
            return;
        }
        if let Some(ab) = self.ab_loop
            && previous < ab.end_ms
            && position_ms >= ab.end_ms
        {
            self.finish_loop_pass(ab, false, out);
            return;
        }
        if now_ms.saturating_sub(self.last_save_ms) >= self.config.save_interval_ms as i64 {
            self.save(now_ms, out);
        }
    }

    /// The A–B segment reached B: loop again or finish the repeat count.
    fn finish_loop_pass(&mut self, ab: AbLoop, after_end: bool, out: &mut Vec<PlayerCommand>) {
        self.repeat_passes_done = self.repeat_passes_done.saturating_add(1);
        if self.repeat_times == 0 || self.repeat_passes_done < self.repeat_times {
            self.position_ms = ab.start_ms;
            out.push(PlayerCommand::SeekTo {
                position_ms: ab.start_ms,
            });
            if after_end {
                self.state = PlaybackState::Playing;
                out.push(PlayerCommand::Play);
            }
        } else {
            self.ab_loop = None;
            self.repeat_times = 0;
            self.repeat_passes_done = 0;
            out.push(PlayerCommand::RepeatFinished);
        }
    }

    fn ended(&mut self, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        if self.current.is_none() || matches!(self.state, PlaybackState::Ended | PlaybackState::Stopped) {
            return;
        }
        if self.duration_ms == 0 {
            self.duration_ms = self.position_ms;
        }
        let live = self.current_item().is_some_and(|i| i.is_live);
        if let Some(ab) = self.ab_loop.filter(|_| !live) {
            self.position_ms = self.duration_ms;
            let looping = self.repeat_times == 0 || self.repeat_passes_done + 1 < self.repeat_times;
            self.finish_loop_pass(ab, true, out);
            if looping {
                return;
            }
        }
        self.position_ms = self.duration_ms;
        self.state = PlaybackState::Ended;
        self.save(now_ms, out);

        if self.sleep_timer == SleepTimer::EndOfItem {
            self.sleep_timer = SleepTimer::Off;
            out.push(PlayerCommand::SleepTimerFired);
            return;
        }
        if !live && self.repeat_times > 0 {
            self.repeat_passes_done = self.repeat_passes_done.saturating_add(1);
            if self.repeat_passes_done < self.repeat_times {
                self.restart_after_end(now_ms, out);
                return;
            }
            self.repeat_times = 0;
            self.repeat_passes_done = 0;
            out.push(PlayerCommand::RepeatFinished);
        }
        if !live && self.repeat_mode == RepeatMode::One {
            self.restart_after_end(now_ms, out);
            return;
        }
        match self.next_pos(self.repeat_mode == RepeatMode::All) {
            Some(pos) if self.autoplay => self.load_at(pos, true, now_ms, out),
            Some(_) => {}
            None => out.push(PlayerCommand::QueueEnded),
        }
    }

    fn restart_after_end(&mut self, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        self.position_ms = 0;
        self.state = PlaybackState::Playing;
        self.last_save_ms = now_ms;
        out.push(PlayerCommand::SeekTo { position_ms: 0 });
        out.push(PlayerCommand::Play);
    }

    fn save_if_active(&mut self, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        if matches!(self.state, PlaybackState::Playing | PlaybackState::Paused) {
            self.save(now_ms, out);
        }
    }

    fn save(&mut self, now_ms: i64, out: &mut Vec<PlayerCommand>) {
        let Some((item_id, live)) = self.current_item().map(|i| (i.id.clone(), i.is_live)) else {
            return;
        };
        self.last_save_ms = now_ms;
        if live {
            return;
        }
        out.push(PlayerCommand::SavePosition {
            item_id,
            position_ms: self.position_ms,
            duration_ms: self.duration_ms,
            completed: self.state == PlaybackState::Ended
                || self.is_completed(self.position_ms, self.duration_ms),
        });
    }

    fn sorted_chapter_starts(&self) -> Vec<u64> {
        let mut starts: Vec<u64> = self
            .current_item()
            .map(|i| i.chapters.iter().map(|c| c.start_ms).collect())
            .unwrap_or_default();
        starts.sort_unstable();
        starts.dedup();
        starts
    }

    fn current_chapter_index(&self) -> Option<usize> {
        let starts = self.sorted_chapter_starts();
        starts.iter().rposition(|&s| s <= self.position_ms)
    }

    fn next_chapter_start(&self) -> Option<u64> {
        self.sorted_chapter_starts()
            .into_iter()
            .find(|&s| s > self.position_ms)
    }

    fn previous_chapter_start(&self) -> Option<u64> {
        let starts = self.sorted_chapter_starts();
        if starts.is_empty() {
            return None;
        }
        let Some(cur) = starts.iter().rposition(|&s| s <= self.position_ms) else {
            return Some(0);
        };
        let into = self.position_ms - starts[cur];
        if into > self.config.restart_threshold_ms || cur == 0 {
            Some(starts[cur])
        } else {
            Some(starts[cur - 1])
        }
    }
}

/// Deterministic shuffle: `first` stays first, the rest is a Fisher–Yates permutation driven by a
/// xorshift64* generator seeded with `seed`. Native wrappers never need to reimplement it; it is
/// public so tests and tools can predict an order.
pub fn shuffle_order(len: usize, first: usize, seed: u64) -> Vec<usize> {
    let mut rest: Vec<usize> = (0..len).filter(|&i| i != first).collect();
    let mut state = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
    for i in (1..rest.len()).rev() {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let r = state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        let j = (r % (i as u64 + 1)) as usize;
        rest.swap(i, j);
    }
    let mut order = Vec::with_capacity(len);
    if first < len {
        order.push(first);
    }
    order.extend(rest);
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Chapter;

    fn item(id: &str, duration_ms: u64) -> MediaItem {
        MediaItem {
            id: id.into(),
            duration_ms,
            resume_position_ms: None,
            chapters: vec![],
            is_live: false,
        }
    }

    fn loaded(items: Vec<MediaItem>) -> Player {
        let mut p = Player::default();
        p.handle(
            PlayerInput::LoadQueue {
                items,
                start_index: 0,
                play_when_ready: true,
            },
            0,
        );
        p
    }

    fn ids(cmds: &[PlayerCommand]) -> Vec<String> {
        cmds.iter()
            .filter_map(|c| match c {
                PlayerCommand::Load { item_id, .. } => Some(item_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn resume_rules() {
        let p = Player::default();
        let mut it = item("v", 600_000);
        assert_eq!(p.resume_position(&it), 0);
        it.resume_position_ms = Some(29_999);
        assert_eq!(p.resume_position(&it), 0);
        it.resume_position_ms = Some(30_000);
        assert_eq!(p.resume_position(&it), 30_000);
        it.resume_position_ms = Some(569_999);
        assert_eq!(p.resume_position(&it), 569_999);
        it.resume_position_ms = Some(570_000);
        assert_eq!(p.resume_position(&it), 0, "95% watched restarts");
        it.duration_ms = 0;
        assert_eq!(p.resume_position(&it), 570_000, "unknown duration resumes");
        it.is_live = true;
        assert_eq!(p.resume_position(&it), 0);
    }

    #[test]
    fn shuffle_is_deterministic_permutation() {
        let a = shuffle_order(10, 3, 42);
        assert_eq!(a, shuffle_order(10, 3, 42));
        assert_eq!(a[0], 3);
        let mut sorted = a.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..10).collect::<Vec<_>>());
        assert_ne!(a, shuffle_order(10, 3, 43));
        assert_eq!(shuffle_order(0, 0, 1), Vec::<usize>::new());
        assert_eq!(shuffle_order(1, 0, 1), vec![0]);
    }

    #[test]
    fn autoplay_walks_queue_then_ends() {
        let mut p = loaded(vec![item("a", 1_000), item("b", 1_000)]);
        let cmds = p.handle(PlayerInput::Ended, 1_000);
        assert_eq!(ids(&cmds), vec!["b"]);
        let cmds = p.handle(PlayerInput::Ended, 2_000);
        assert!(cmds.contains(&PlayerCommand::QueueEnded));
        assert_eq!(p.snapshot().state, PlaybackState::Ended);
        // Ended is idempotent.
        assert!(p.handle(PlayerInput::Ended, 2_001).is_empty());
    }

    #[test]
    fn autoplay_off_stops_after_item() {
        let mut p = loaded(vec![item("a", 1_000), item("b", 1_000)]);
        p.handle(PlayerInput::SetAutoplay { enabled: false }, 0);
        let cmds = p.handle(PlayerInput::Ended, 1_000);
        assert!(ids(&cmds).is_empty());
        assert!(!cmds.contains(&PlayerCommand::QueueEnded));
        // Play after the end replays.
        let cmds = p.handle(PlayerInput::Play, 1_100);
        assert_eq!(
            cmds,
            vec![PlayerCommand::SeekTo { position_ms: 0 }, PlayerCommand::Play]
        );
    }

    #[test]
    fn repeat_all_wraps_and_previous_wraps() {
        let mut p = loaded(vec![item("a", 1_000), item("b", 1_000)]);
        p.handle(
            PlayerInput::SetRepeatMode {
                mode: RepeatMode::All,
            },
            0,
        );
        p.handle(PlayerInput::Next, 0);
        let cmds = p.handle(PlayerInput::Next, 0);
        assert_eq!(ids(&cmds), vec!["a"]);
        let cmds = p.handle(PlayerInput::Previous, 0);
        assert_eq!(ids(&cmds), vec!["b"]);
    }

    #[test]
    fn previous_restarts_after_threshold() {
        let mut p = loaded(vec![item("a", 100_000), item("b", 100_000)]);
        p.handle(PlayerInput::Next, 0);
        p.handle(
            PlayerInput::Tick {
                position_ms: 3_001,
                duration_ms: 0,
            },
            1,
        );
        let cmds = p.handle(PlayerInput::Previous, 2);
        assert_eq!(cmds[0], PlayerCommand::SeekTo { position_ms: 0 });
        let cmds = p.handle(PlayerInput::Previous, 3);
        assert_eq!(ids(&cmds), vec!["a"]);
    }

    #[test]
    fn rate_is_clamped_and_carried() {
        let mut p = loaded(vec![item("a", 1_000), item("b", 1_000)]);
        assert_eq!(
            p.handle(PlayerInput::SetRate { rate: 3.0 }, 0),
            vec![PlayerCommand::SetRate { rate: 2.0 }]
        );
        assert!(p.handle(PlayerInput::SetRate { rate: f64::NAN }, 0).is_empty());
        assert!(p.handle(PlayerInput::SetRate { rate: 2.5 }, 0).is_empty());
        let cmds = p.handle(PlayerInput::Next, 0);
        assert_eq!(cmds.last(), Some(&PlayerCommand::SetRate { rate: 2.0 }));
        assert_eq!(
            p.handle(PlayerInput::SetRate { rate: 0.1 }, 0),
            vec![PlayerCommand::SetRate { rate: 0.5 }]
        );
    }

    #[test]
    fn chapters_navigation() {
        let mut it = item("a", 600_000);
        it.chapters = vec![
            Chapter {
                start_ms: 120_000,
                title: "2".into(),
            },
            Chapter {
                start_ms: 0,
                title: "1".into(),
            },
            Chapter {
                start_ms: 300_000,
                title: "3".into(),
            },
        ];
        let mut p = loaded(vec![it]);
        assert_eq!(
            p.handle(PlayerInput::NextChapter, 0)[0],
            PlayerCommand::SeekTo { position_ms: 120_000 }
        );
        assert_eq!(p.snapshot().current_chapter, Some(1));
        // 2 s into chapter 2: previous goes to chapter 1.
        p.handle(
            PlayerInput::Tick {
                position_ms: 122_000,
                duration_ms: 0,
            },
            1,
        );
        assert_eq!(
            p.handle(PlayerInput::PreviousChapter, 2)[0],
            PlayerCommand::SeekTo { position_ms: 0 }
        );
        p.handle(PlayerInput::SeekTo { position_ms: 400_000 }, 3);
        assert_eq!(
            p.handle(PlayerInput::PreviousChapter, 4)[0],
            PlayerCommand::SeekTo { position_ms: 300_000 }
        );
        assert!(
            p.handle(PlayerInput::NextChapter, 5).is_empty(),
            "no chapter after the last"
        );
    }

    #[test]
    fn skip_clamps() {
        let mut p = loaded(vec![item("a", 15_000)]);
        assert_eq!(
            p.handle(PlayerInput::SkipBackward, 0)[0],
            PlayerCommand::SeekTo { position_ms: 0 }
        );
        p.handle(PlayerInput::SkipForward, 0);
        assert_eq!(
            p.handle(PlayerInput::SkipForward, 0)[0],
            PlayerCommand::SeekTo { position_ms: 15_000 }
        );
    }

    #[test]
    fn ab_loop_with_repeat_count() {
        let mut p = loaded(vec![item("a", 60_000)]);
        p.handle(PlayerInput::SetRepeatCount { times: 2 }, 0);
        let cmds = p.handle(
            PlayerInput::SetAbLoop {
                start_ms: 10_000,
                end_ms: 20_000,
            },
            0,
        );
        assert_eq!(cmds[0], PlayerCommand::SeekTo { position_ms: 10_000 });
        let tick = |p: &mut Player, ms| {
            p.handle(
                PlayerInput::Tick {
                    position_ms: ms,
                    duration_ms: 0,
                },
                1,
            )
        };
        assert_eq!(tick(&mut p, 19_500), vec![]);
        assert_eq!(
            tick(&mut p, 20_100),
            vec![PlayerCommand::SeekTo { position_ms: 10_000 }]
        );
        assert_eq!(tick(&mut p, 20_050), vec![PlayerCommand::RepeatFinished]);
        assert_eq!(p.snapshot().ab_loop, None);
        assert_eq!(tick(&mut p, 25_000), vec![]);
    }

    #[test]
    fn invalid_ab_loop_ignored() {
        let mut p = loaded(vec![item("a", 60_000)]);
        assert!(
            p.handle(
                PlayerInput::SetAbLoop {
                    start_ms: 5_000,
                    end_ms: 5_000
                },
                0
            )
            .is_empty()
        );
        assert_eq!(p.snapshot().ab_loop, None);
        // B beyond the end is clamped to the duration.
        p.handle(
            PlayerInput::SetAbLoop {
                start_ms: 50_000,
                end_ms: 90_000,
            },
            0,
        );
        assert_eq!(
            p.snapshot().ab_loop,
            Some(AbLoop {
                start_ms: 50_000,
                end_ms: 60_000
            })
        );
        // Ended inside the loop loops back and resumes.
        assert_eq!(
            p.handle(PlayerInput::Ended, 1),
            vec![PlayerCommand::SeekTo { position_ms: 50_000 }, PlayerCommand::Play]
        );
    }

    #[test]
    fn sleep_timer_pauses_and_deadline_is_exposed() {
        let mut p = loaded(vec![item("a", 3_600_000)]);
        p.handle(
            PlayerInput::SetSleepTimer {
                timer: SleepTimer::After { duration_ms: 60_000 },
            },
            1_000,
        );
        assert_eq!(p.next_deadline_ms(), Some(61_000));
        let cmds = p.handle(
            PlayerInput::Tick {
                position_ms: 60_000,
                duration_ms: 0,
            },
            61_000,
        );
        assert_eq!(cmds[0], PlayerCommand::Pause);
        assert_eq!(cmds.last(), Some(&PlayerCommand::SleepTimerFired));
        assert_eq!(p.next_deadline_ms(), None);
        assert_eq!(p.snapshot().sleep_timer, SleepTimer::Off);
    }

    #[test]
    fn sleep_deadline_passed_while_paused_is_dropped() {
        let mut p = loaded(vec![item("a", 3_600_000)]);
        p.handle(
            PlayerInput::SetSleepTimer {
                timer: SleepTimer::After { duration_ms: 1_000 },
            },
            0,
        );
        p.handle(PlayerInput::Pause, 10);
        assert_eq!(p.next_deadline_ms(), None);
        assert_eq!(p.handle(PlayerInput::Play, 5_000), vec![PlayerCommand::Play]);
        assert_eq!(p.snapshot().sleep_timer, SleepTimer::Off);
    }

    #[test]
    fn periodic_save_is_throttled() {
        let mut p = loaded(vec![item("a", 600_000)]);
        let mut saves = 0;
        for s in 1..=60 {
            let cmds = p.handle(
                PlayerInput::Tick {
                    position_ms: s * 1_000,
                    duration_ms: 0,
                },
                s as i64 * 1_000,
            );
            saves += cmds
                .iter()
                .filter(|c| matches!(c, PlayerCommand::SavePosition { .. }))
                .count();
        }
        assert_eq!(saves, 4, "one save per 15 s over 60 s");
    }

    #[test]
    fn quality_cap_reemitted_only_on_change() {
        let mut p = Player::default();
        assert!(
            p.handle(
                PlayerInput::SetNetwork {
                    network: NetworkKind::Cellular
                },
                0
            )
            .is_empty(),
            "nothing loaded yet"
        );
        let cmds = p.handle(
            PlayerInput::LoadQueue {
                items: vec![item("a", 1_000)],
                start_index: 0,
                play_when_ready: false,
            },
            0,
        );
        assert!(matches!(cmds[1], PlayerCommand::SetQualityCap { cap } if cap.max_height == Some(720)));
        assert!(
            p.handle(
                PlayerInput::SetNetwork {
                    network: NetworkKind::Cellular
                },
                0
            )
            .is_empty()
        );
        let cmds = p.handle(
            PlayerInput::SetNetwork {
                network: NetworkKind::Wifi,
            },
            0,
        );
        assert!(matches!(cmds[0], PlayerCommand::SetQualityCap { cap } if cap.max_height == Some(1080)));
    }

    #[test]
    fn empty_queue_stops() {
        let mut p = loaded(vec![item("a", 1_000)]);
        let cmds = p.handle(
            PlayerInput::LoadQueue {
                items: vec![],
                start_index: 0,
                play_when_ready: true,
            },
            5,
        );
        assert!(matches!(cmds[0], PlayerCommand::SavePosition { .. }));
        assert_eq!(cmds.last(), Some(&PlayerCommand::Stop));
        assert_eq!(p.snapshot().state, PlaybackState::Idle);
        assert!(p.handle(PlayerInput::Play, 6).is_empty());
    }

    #[test]
    fn stop_then_play_reloads_at_position() {
        let mut p = loaded(vec![item("a", 600_000)]);
        p.handle(
            PlayerInput::Tick {
                position_ms: 42_000,
                duration_ms: 0,
            },
            1,
        );
        let cmds = p.handle(PlayerInput::Stop, 2);
        assert_eq!(cmds.last(), Some(&PlayerCommand::Stop));
        let cmds = p.handle(PlayerInput::Play, 3);
        assert_eq!(
            cmds[0],
            PlayerCommand::Load {
                item_id: "a".into(),
                start_position_ms: 42_000,
                play_when_ready: true
            }
        );
    }

    #[test]
    fn live_items_skip_saves_and_loops() {
        let mut it = item("live", 0);
        it.is_live = true;
        it.resume_position_ms = Some(100_000);
        let mut p = Player::default();
        let cmds = p.handle(
            PlayerInput::LoadQueue {
                items: vec![it],
                start_index: 0,
                play_when_ready: true,
            },
            0,
        );
        assert!(matches!(
            cmds[0],
            PlayerCommand::Load {
                start_position_ms: 0,
                ..
            }
        ));
        assert!(
            p.handle(
                PlayerInput::SetAbLoop {
                    start_ms: 0,
                    end_ms: 10
                },
                0
            )
            .is_empty()
        );
        assert_eq!(p.handle(PlayerInput::Pause, 1), vec![PlayerCommand::Pause]);
    }
}
