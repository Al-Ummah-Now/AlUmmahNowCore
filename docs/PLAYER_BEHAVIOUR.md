# Player behaviour spec (Epic 11.5)

Normative behaviour shared by every native player in Epic 13: AVKit (iOS, iPadOS, tvOS, macOS),
ExoPlayer/Media3 (Android phone, TV, Auto), Windows.Media.Playback (WinUI 3), GStreamer (GTK 4 on
Linux) and AVPlay (Tizen). The rules are implemented once in `crates/aun-player` and pinned down by
the conformance scenarios in `crates/aun-player/conformance/*.json`.

## 1. Architecture

```
 user intents ──┐                         ┌──► commands ──► native player (AVPlayer, ExoPlayer, …)
                ├──► Player.handle(input, now_ms) ──┤
 native reports ┘                         └──► SavePosition / CreateBookmark ──► aun-sync
```

- `aun_player::Player` is a **deterministic state machine**. It owns no timers, does no I/O and
  uses no randomness except the seeded shuffle. Same inputs at the same times give the same commands
  on every platform.
- Each input is passed with the current wall-clock time (`now_ms`, Unix ms). Native wrappers send
  user intents (`play`, `seek_to`, `set_rate`, …) and platform reports (`tick`, `ended`,
  `playback_paused`, `failed`, …) and **execute the returned commands in order**.
- The model is optimistic: after `play` it reports `playing` without waiting for the platform. If the
  platform cannot follow, it reports `failed` or `playback_paused`.
- `tick` is the position report the platform sends while playing (every 250–1000 ms). It also drives
  the sleep timer and periodic saves. When nothing else will produce a tick (e.g. audio in the
  background), the wrapper schedules a wake-up at `Player::next_deadline_ms()`.
- The model is not thread-safe on its own. The FFI facade wraps it in a mutex. Call it from one
  queue/thread or through the facade.

## 2. States

| State | Meaning |
|---|---|
| `idle` | nothing loaded (initial, or after loading an empty queue) |
| `playing` | playing, or about to after a `load` with `play_when_ready` |
| `paused` | paused by the user or the system |
| `ended` | the item finished and nothing else was started |
| `stopped` | the native player was released (`stop`) |
| `failed` | the native player reported an error |

`play` from `ended` replays the item from 0. `play` from `stopped`/`failed` reloads the item at the
last known position (retry).

## 3. Queue, autoplay, shuffle and repeat

- `load_queue{items, start_index, play_when_ready}` replaces the queue and loads `items[start_index]`
  (clamped). An empty queue stops and returns to `idle`. Rate, shuffle, repeat, autoplay and quality
  settings survive a new queue. A–B loop and repeat count do not.
- **Autoplay is on by default.** On `ended` the next item in play order loads automatically. With
  autoplay off, the player stays on the finished item (`ended`). The user can still press Next.
- **Shuffle is off by default.** When it is turned on, the current item stays current and the rest
  follow a seeded Fisher–Yates permutation (`shuffle_order(len, first, seed)`). Apps pass a random
  seed; fixtures pass a fixed one. Turning shuffle off goes back to queue order from the current item.
- **Repeat** `off` (default) | `one` (restart the item at its end) | `all` (wrap last → first for
  Ended, Next and Previous).
- **Next** loads the next item in play order. At the end of the queue it does nothing unless repeat
  is `all`.
- **Previous** restarts the current item when more than **3 s** in. Otherwise it loads the previous
  item, or restarts the item when it is the first one and repeat is not `all`.
- When the last item ends with nothing after it, the model emits `queue_ended`. The wrapper may show
  end-screen suggestions; the core does not pick recommendations.

## 4. Resume and "watched" rules

| Saved position *p* for an item of duration *d* | Start at |
|---|---|
| no saved position | 0 |
| *p* < **30 s** | 0 (barely started) |
| *p* ≥ **95 %** of *d* | 0 (already watched) |
| otherwise (or *d* unknown and *p* ≥ 30 s) | *p* |
| live items | live edge (0), never resumed |

- An item is **completed** when its position reaches 95 % of the duration or it ends. Every
  `save_position` carries `completed`. aun-sync uses it to drop the item from "Continue watching".
- A duration of 0 means "unknown". The model learns it from `tick.duration_ms` and, at the latest,
  from the position at `ended`.

## 5. Quality policy (ABR caps)

`set_quality_cap{cap: {max_height, max_bitrate_bps}}` is emitted after every `load` and whenever the
cap changes. `null` means unlimited. The smallest limit wins:

| Input | Max height | Max bitrate |
|---|---|---|
| device class: phone | 1080 | – |
| tablet | 1440 | – |
| desktop, TV | 2160 | – |
| car (parked video only) | 480 | – |
| network cellular (data saver off) | 720 | 2.5 Mbps |
| data saver on (any network) | 480 | 1 Mbps |
| user picked a fixed quality | that height (it can only lower the cap) | – |

The cap is an upper bound for the platform ABR, never a fixed rendition. Native mapping: AVPlayerItem
`preferredMaximumResolution` + `preferredPeakBitRate`; Media3 `TrackSelectionParameters`
`setMaxVideoSize` + `setMaxVideoBitrate`; MediaPlaybackItem `AdaptiveMediaSource.DesiredMaxBitrate`;
GStreamer `adaptivedemux` `max-video-height` / `max-bitrate`; AVPlay `setStreamingProperty`. Offline
downloads play the downloaded rendition regardless of the cap.

## 6. Transport

- **Rate** is clamped to **0.5×–2×**. It is re-applied after every load (`set_rate` follows
  `load`/`set_quality_cap` when the rate is not 1). NaN/∞ are ignored. The UI offers
  0.5, 0.75, 1, 1.25, 1.5, 1.75, 2.
- **Skip** moves ±**10 s** and is clamped to [0, duration]. In RTL layouts the *buttons* are mirrored
  by the UI, but `skip_forward` always means later in the media. Mirroring is a UI concern and is
  never reflected in the model.
- **Seek** is clamped to the duration (not for live). Seeking after the end leaves the player
  `paused`.
- **Pause/Play** emit `pause`/`play`. A pause, from the user or the system, saves the position.
- **Stop** saves the position, cancels the sleep timer and emits `stop` (release the decoder).

## 7. Position saves (feed for aun-sync)

`save_position{item_id, position_ms, duration_ms, completed}` is emitted on pause (user or system),
seek, stop, failure, item change, end, and periodically while playing at most every **15 s**.
Live items never save. aun-sync debounces these again before anything reaches the network.

## 8. Study tools

- **A–B loop** `set_ab_loop{start_ms, end_ms}`: B is clamped to the duration. A ≥ B and live items
  are ignored. If the position is outside [A, B), the player seeks to A. When a tick *crosses* B
  (previous position < B ≤ new position), the player seeks to A. A seek that lands beyond B does not
  trigger the loop. `ended` inside a loop counts as reaching B and resumes playback at A.
  `clear_ab_loop` removes the loop.
- **Repeat-N** `set_repeat_count{times}`: the total number of passes, counting the current one. It
  applies to the A–B segment when one is set, otherwise to the whole item. After the last pass the
  model emits `repeat_finished`, clears the loop and playback continues normally (past B, or
  autoplay). `times = 0` clears the count; a bare A–B loop then repeats forever.
- **Bookmark** `add_bookmark{label}` emits `create_bookmark{item_id, position_ms, label}` at the
  current position. aun-sync stores it as a timestamped note.
- Loading another item clears the A–B loop and the repeat count.

## 9. Sleep timer

- `set_sleep_timer{timer: {kind: "after", duration_ms}}` pauses playback when a tick arrives at or
  after the wall-clock deadline (`now + duration`). The model emits `pause`, `save_position` and
  `sleep_timer_fired`, and the timer turns off. Wrappers schedule a wake-up at `next_deadline_ms()`
  so the timer fires even without ticks. If the deadline passes while paused, the timer is dropped
  silently on the next play.
- `{kind: "end_of_item"}` stops at the end of the current item: no autoplay, no repeat. It emits
  `sleep_timer_fired`.
- `{kind: "off"}` (or a zero duration) cancels the timer. `stop` cancels it too.
- Fade-out before pausing is optional platform polish and is not modelled.

## 10. Chapters

Chapters are sorted by start time. **Next chapter** seeks to the first chapter start after the
position; after the last chapter it does nothing. **Previous chapter** restarts the current chapter
when more than 3 s into it, otherwise seeks to the previous chapter's start (the first chapter
restarts). The snapshot exposes `current_chapter`.

## 11. Conformance suite

- Fixtures: `crates/aun-player/conformance/*.json`, format
  `{"scenarios": [{"name", "description", "config"?, "steps": [{"at_ms", "input", "expect", "expect_state"?}]}]}`.
  `input` and `expect` use the serde form of `PlayerInput` / `PlayerCommand` (`{"type": "snake_case", …}`).
  `expect` is matched **exactly and in order**. An omitted `expect` means no commands. `expect_state`
  checks parts of the snapshot (`state`, `current_item_id`, `position_ms`, `rate`, `repeat_mode`,
  `sleep_timer`, `has_ab_loop`, `current_chapter`). `config` overrides `PlayerConfig` fields.
- The Rust runner (`aun_player::conformance::run_scenario`) replays every scenario in CI
  (`crates/aun-player/tests/conformance.rs`). New fixture files must be added to `SCENARIO_FILES`;
  a test checks that.
- **Native wrappers** get all scenarios as one JSON array (`builtin_scenarios_json()`, exposed over
  FFI) and replay them through their wrapper, with the platform player replaced by a recording fake.
  They assert that each emitted command maps to the expected platform call (seek, rate, ABR cap,
  load, stop) and that wrapper-generated inputs (ticks, ended) produce the scripted results.
- Changing a threshold or a rule means changing this document, the model and the fixtures in the same
  PR. A change to the commands a scenario expects is a behaviour change and goes in `CHANGELOG.md`.
