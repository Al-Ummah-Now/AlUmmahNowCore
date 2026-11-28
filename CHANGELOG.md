# Changelog

All notable changes to `aun-core` are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/). While the version is `0.x`, a minor bump may break the
FFI; such changes are listed under **Breaking**.

## [Unreleased]

### Fixed

- Telemetry scrubbing replaces account names in local file paths (`/Users/…`, `/home/…`, `C:\Users\…`, `file:///…`) with `[user]`, and UNC hosts and shares with `[host]`/`[share]`.

## [0.4.0-rc.13] - 2026-11-27

### Added

- `aun-telemetry` FFI (`TelemetryClient`, `ConsentState`, `CrashReport`, `Breadcrumb`, `BreadcrumbLevel`, `EventKind`, `TelemetryConfig`, `AppInfo`, `FlushReport`, `HttpSink`) exposed across Swift, Kotlin, and C# via UniFFI (`AlUmmahNowCore#76`). Opt-in by default (`ConsentState::Undecided`), offline disk persistence with atomic revocation/purge, crash-safe flush gating, and automatic PII scrubbing.

## [0.4.0-rc.12] - 2026-11-26

### Added

- Resumable uploads from platform streams, such as Android content URIs, without a staging copy.

### Breaking

- Rust `UploadManifest` struct literals now require a `source: UploadSourceKind` field.
  Existing persisted manifests default to `UploadSourceKind::Path`.

## [0.4.0-rc.11] - 2026-11-26

### Added

- Bilingual playlist display text and localized playlist fields in native search.

### Breaking

- Rust `SearchSuggestion` struct literals now take optional `title_localized` and
  `description_localized` fields.
  The Kotlin constructor adds two trailing nullable parameters that default to `null`.
  Recompile Kotlin binaries against the updated bindings.

## [0.4.0-rc.10] - 2026-11-26

### Added

- Public, unlisted, and private visibility for video uploads and edits, including bulk edits.
- Native playlist operations for personal and channel playlists, including the personal `mine`
  filter, visibility, and adding or removing videos.

### Changed

- Refreshed the pinned api-videos OpenAPI contract from v1.0.0 to v1.4.0, covering 261 paths
  (up from 132) and 344 generated Rust operations (up from 191).

### Breaking

- Rust `aun_upload::UploadRequest` struct literals now take `visibility`. Set it to `None` to
  keep using the legacy `is_public` field. Existing saved uploads and native constructors still
  accept an omitted visibility.
- Channel membership now starts with an invitation. Migrate `post_channel_member_add` and the
  `post_ChannelMemberAdd` operation ID to `post_channel_invite_create` / `post_ChannelInviteCreate`.
  The invitation takes `handle` or `email` with `role` and returns both members and invitations.
- `get_profile_current_fetch` now takes `GetProfileCurrentFetchParams` with optional `lang`.
  Pass `&Default::default()` to omit it.
- Comment dislikes now return HTTP 410 with code `dislikes_disabled`. The generated
  `post_comment_dislike_create` has no typed success response and returns `Result<serde_json::Value>`.
- `DeleteVideoDeleteResponse` drops `series`.
- The following generated Rust fields now use `Option`.
  - `header_channel_id` in playlist create, update, delete, add-video and remove-video params,
    comment create, update and delete params, and subscription create and delete params.
  - `channel_id` on comment records and their authors in comment list, replies and create responses.
  - `GetSearchSuggestionsGetParams.q`, `GetTrendingVideosSectionResponseDataItem.trending_metadata`,
    `PostPlaylistCreateBody.title`, `PostVideoBulkUpdateBody.video_ids` and `changes`, and
    `PutNotificationSettingsUpdateBody.settings`.
- Generated struct literals must account for added fields, such as watch-history `q`,
  search-suggestion `offset`, and channel member `invites`.
  Use `..Default::default()` in literals that can use defaults for newly added fields.

## [0.4.0-rc.9] - 2026-11-26

### Added

- `LoginPrompt::StepUp { max_age_seconds }` in Rust and UniFFI sends `prompt=login` and `max_age`.
  Completion requires a fresh `auth_time` before saving tokens and allows up to 60 seconds of
  future clock skew. When an account is already active, the callback must be for that account.
- `search_suggestions_page(query, cursor)` over UniFFI (`fetch_suggestions_page` in Rust) loads scoped
  results when a picker opens with blank text. Pass `next_cursor` back with the same query and scope
  while `has_more` is true. Typed queries use one page. The list-returning API still skips blank text.

### Fixed

- Raw API requests return HTTP 304 successfully and expose response headers. Translation sync saves
  the bundle's ETag and sends it back as `If-None-Match`. Metadata checksums still determine when
  a bundle needs revalidation. Existing on-disk caches load without an ETag.

### Breaking

- Rust `RawResponse`, `RemoteBundle` and `CachedBundle` struct literals must specify the new
  `headers` or `etag` field. UniFFI `ApiResponse` constructors now take `headers`.
- Custom implementations of the generated `ApiClient` interface need to implement the new
  `search_suggestions_page` method.
- `LoginPrompt` adds a payload-bearing `StepUp` case. Update exhaustive matches and regenerate
  native bindings. C# uses a record hierarchy, so use `new LoginPrompt.Login()` or
  `new LoginPrompt.Default()` for the existing cases.

## [0.4.0-rc.8] - 2026-11-26

### Added

- `AuthManager.token_for_account(user_id, force_refresh)` is available through UniFFI for clients
  that need a token for a specific signed-in account.

### Fixed

- Matching channel and playlist auto-download rules share their newest eligible episodes.
  Files from a removed rule stay while another rule still needs them and free storage when
  they roll out of every active rule's window.
- Uploads keep the server's failure code across restarts and clear failure metadata when retried.

### Breaking

- `Unauthorized`, `Forbidden`, `NotFound`, `Conflict` and `InvalidInput` errors now carry an
  optional server `code` in Rust and UniFFI. Update native patterns and constructors that bind
  the associated fields.
- `UploadInfo` adds optional `lastErrorKind` and `lastErrorCode` fields.

## [0.4.0-rc.7] - 2026-11-24

### Breaking

- `aun-api`: the profile and channel `email` is optional (`Option<String>`), as in the api-videos contract:
  the API takes a profile's email from the signed-in account, and channels no longer return one.
- `aun-updates`: `decide` takes `has_root_key` (Rust only; the FFI is unchanged).

### Changed

- App updates (`AUN-APP-UPDATES`): the root-key gate. A build without root keys gets `action: notice` with no URL,
  manifest or tag instead of `install` and `download`, verifies no manifest and allows no download URL. A build with
  root keys takes a `notice` answer as nothing. Below the minimum, a release that is itself below it forces no one.

## [0.4.0-rc.6] - 2026-11-24

### Added

- Epic 14 (`E14-S7`, `Core#53`):
  - New crate `aun-downloads` for plain progressive R2 `video.mp4` offline downloads (per E14-D1/D2/D5/D8 — no encryption, DRM, quality ladder, audio-only downloads, or license expiry): `(user_id, video_id)`-keyed JSON manifest with exclusive `StoreLock`, fsync + atomic rename with rollback on failed save, relative-only local paths, crash-safe two-phase deletion (`Removed` → `confirm_removed`), revocations feed cursor & purge, corrupt/zero-byte quarantine & per-record salvage with container-safe `reconcile_orphaned_paths`, atomic `try_reserve` with monotonic `reservation_id` tokens, free-space reserve accounting across unfinished downloads, and per-rule (`AutoDownloadSource` channel/playlist) `max_episodes` auto-download selection & eviction (`AutoDownloadPlan`).
  - UniFFI exports in `aun-ffi` (`AunCore.swift`) for `OfflineDownloadStore`, `offline_default_download_settings`, `offline_can_start_download`, `offline_select_auto_downloads`, `PlayerMachine` (`aun-player`), and `OfflineSyncEngine` (`aun-sync` CRDT watch state, history, Continue watching, likes, Watch later, timestamped study notes, change listeners, `flush`, and `sync`).
  - Hardened `aun-sync` `sanitize_thumbnail_url` to strip signed/private thumbnail URLs (`/priv/`, `token=`, `sig=`, `X-Amz-Signature`, `bcdn_token=`, `access_token=`, `jwt=`).

### Breaking

- Epic 14 (`E14-S7`, `Core#53`): removed legacy download-manifest helpers from `aun-sync` (`upsert_download`, `remove_download`, `downloads`, `DownloadRecord`, and `SyncChange::Downloads`); offline downloads now live exclusively in the dedicated `aun-downloads` crate (`DownloadStore` / FFI `OfflineDownloadStore`).

## [0.4.0-rc.5] - 2026-11-23

### Added

- Epic 22 (`E22-S7`, `Core#40`):
  - `aun-common::short_links` & UniFFI `short_link_origin_for`, `short_link_for`, `share_short_link`, and `parse_short_link_path` matching `packages/shared-types/src/short-links.ts` (`/v/`, `/s/`, `/@`, `/c/`, `/p/`, `/l/`, `/e/`, `?list=`, `?index=`, `?t=`, and `?lang=ar`).
  - `aun-api::notifications` & UniFFI `ApiClient` methods for the notification bell (`unseenCount` with `9+` / `٩+` formatting, `POST /api/notifications/seen`, `menuOpen` mode, realtime `notifications_state` reducer per `Core#40`), daily quiet hours & prayer-time quiet windows (`GET` / `PUT /api/notifications/settings`), and native push device registration (`PUT` / `DELETE /api/devices/:installId`).
  - `aun-api::events` & UniFFI `ApiClient` methods for `GET /api/me/features` (`eventCalendar`), `GET /api/events`, and live-stream reminder toggles (`POST` / `DELETE /api/live-streams/:id/reminders`).
- App update checks (wiki AUN-APP-UPDATES, APPUPD-5): new crate `aun-updates` (plain Rust, no I/O) with app versions (3 or 4 numeric parts, semver pre-release precedence), the root-signed update key list (`update-keys.json`: serial must rise, revoked keys and yanked versions remembered for good), signed release manifests (`app-release.json`: any valid signature under a trusted, non-revoked key, then repo, tag, channel, no downgrade, replay floor, yanked), the download host rule (every redirect hop: this repo's GitHub release downloads, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`; HTTPS only), and the decision (nothing, a notice closed per version, a bar that can't be closed, or the forced screen only when the build's source confirms an update).
- FFI `UpdateChecker` (state kept as JSON by the app; `apply_answer`, `verify_manifest`, `allowed_download_url`, `dismiss`, `auto_install`, `offline_prompt_kind`, `mark_checked`), records `UpdatePrompt`, `VerifiedRelease`, `ReleaseArtifact`, enum `UpdatePromptKind`, and `compare_app_versions`.
- `spec/app-updates/`: the version table and a signed vector shared with the server (`@al-ummah/shared-types`).

## [0.4.0-rc.4] - 2026-11-22

### Added

- On-device transcription on the GPU (D10, AlUmmahNow TA-13), with the CPU as the automatic fallback. New features `whisper-metal` (macOS: Metal) and `whisper-vulkan` (Windows, Linux: Vulkan) on `aun-media` and `aun-ffi`; the desktop builds (macOS universal dylib, the XCFramework's macOS slice, Windows x64/ARM64, Linux x64/arm64) now use them. Without a usable GPU, whisper runs on the CPU as before. When the GPU cannot load the model or a transcription on it fails (e.g. too little video memory), the model is loaded on the CPU once and used from then on. Progress never goes back.
- FFI `transcription_device()` → `TranscriptionDevice { gpu, backend, name }` (`backend` is `"Metal"`, `"Vulkan"` or `"CPU"`; `None` in builds without on-device transcription); `aun-media`: `transcribe::inference_device()`, `InferenceDevice`.
- `AUN_VULKAN_LOADER` (environment): unset opens the system's loader (Windows: from System32 only); an absolute path picks another loader; any other value, e.g. `none`, keeps Vulkan builds on the CPU.

### Changed

- Vulkan builds never link the Vulkan loader (`vulkan-1.dll`, `libvulkan.so.1`): `aun_core` opens it at run time (`crates/aun-media/build.rs`, `src/transcribe/vulkan_loader.c`), so the library still loads on computers without a GPU driver. CI checks the built DLL and `.so` for it.
- Building `whisper-vulkan` needs the Vulkan headers and `glslc` (Windows: the LunarG Vulkan SDK with `VULKAN_SDK` set; Linux: `libvulkan-dev` and `glslc`).

### Fixed

- whisper.cpp was compiled for the build machine's own CPU (ggml's default `-march=native`), so a shipped desktop library could crash with an illegal instruction (SIGILL) on a user's older CPU. `.cargo/config.toml` now sets a portable baseline for every build: x86-64 needs AVX2, FMA and F16C (Intel Haswell / AMD Excavator or newer); ARM keeps the compiler's default.

## [0.4.0-rc.3] - 2026-11-19

### Added

- `X-AUN-Client` on every API request (`ApiConfig.client`, FFI `ApiClientConfig.client`, default `aun-core/<version> (<os>)`): which app, version and OS is calling, for the servers' request origin (errors and analytics; no identifier). Apps pass e.g. `aun-ios/1.4.0 (iPadOS 18.1)`; the field defaults to `None`, so existing callers keep compiling.

### Changed

- Arabic always formats with Eastern Arabic numerals (٠–٩): the catalog formats messages, numbers and durations for Arabic locales with `NumeralSystem::ArabicIndic` whatever `numerals` says (owner decision 2026-11-15, AlUmmahNow #1488, #1496). Other languages keep the setting (default 0–9).

## [0.4.0-rc.2] - 2026-11-15

### Added
- EDL titles (E19 19.5 + 19.9, brand-kit titles and lower thirds): `edl_apply_json` takes
  `addTitle` (`trackId`, `title`), `setTitle` (`title`, replaced by id: move, retime, reword,
  restyle) and `removeTitle` (`titleId`), with the same all-or-nothing validation as clip edits.
  `edl_flatten_json` adds `titles` (titles of unmuted title tracks in time order: `titleId`,
  `trackId`, `startMs`, `durationMs`, `text` by locale, `style`); `tracks` is unchanged.

### Added
- `AuthConfig.install_id` (FFI, default `None`) and `OAuthConfig::with_install_id`: the install's stable id is sent as `install_id` on the code exchange. Auth stores it on that install's refresh tokens, so "Sign out this device" in the videos admin ends exactly that install's sign-in (AlUmmahNow#442, AlUmmahNowAuth#363). The Admin apps pass their push-device install id.

## [0.4.0-rc.1] - 2026-11-14

### Added
- Desktop Studio (E19, decisions D10/D11 of 2026-11-14), FFI:
  - EDL editing: `edl_apply_json(project, op)` applies one edit (`split`, `trim`,
    `rippleDeleteClip`, `rippleDeleteRange`, and new `join`, `move`, `append`) atomically;
    `edl_flatten_json(project)` lists clip tracks in timeline order with source paths for the
    platform composition APIs. `aun-media`: `Project::{join_clips, move_clip, append_clip, apply,
    flatten}`, `EditOp`, `NewClip`.
  - On-device transcription (D10): `ModelStore` (list, resumable checksum-verified download,
    path, delete), `transcription_on_device_available()`, `transcribe_on_device_json(...)` with a
    `MediaProgress` callback and `MediaCancel`, and `transcript_to_caption_segments_json` for the
    server's on-device transcript upload (`[{startMs,endMs,text}]`, ≤ two lines per cue).
    Desktop builds (Windows, Linux, macOS dylib and the XCFramework's macOS slice) are built with
    `whisper`; the Swift package links libc++ and Accelerate on macOS.
  - Pre-encoded renditions (D11): `hls_ladder(width, height, rotation)` and `hls_encode_fps`
    mirror the server's ladder (checked against a fixture generated from AlUmmahNow
    `apps/media/container/ladder.mjs`); `hls_package_mp4` remuxes one rung's progressive MP4 into
    fMP4 HLS cut like the server's encode (no re-encoding); `renditions_files` lists the upload.
- `aun-common`/FFI: one display-name rule for every native app (owner rule 2026-11-12, mirrors the
  web `resolveDisplayName`): display name in the locale → plain display name → first + last
  (whichever exists) → a name in another language → `""`. No `@handle` (handles belong to
  channels). Blank values count as missing; never the email. FFI:
  `resolve_display_name(NameFields, locale)` and `resolve_display_name_json(profile_json, locale)`,
  which prefers the API's `resolvedDisplayName` and falls back to the rule.

### Changed
- `aun-auth`: `AccountInfo.display_name` follows the rule over the token claims (`name`, then
  `given_name` + `family_name`; OpenAuth `properties` too). A blank `name` is now `None` instead
  of whitespace; it is never a handle or the email.

## [0.3.0-rc.1] - 2026-11-11

### Security
- `aun-auth`/FFI: sign-out revokes the refresh token (RFC 7009, AlUmmahNow#720). `AuthManager.sign_out`
  removes the account from the device at once and revokes its refresh token in the background;
  the new async `sign_out_and_revoke` waits for the revocation (at most 10 s). A failed or slow
  revocation never fails or undoes sign-out. The endpoint is the discovered `revocation_endpoint`,
  else `{issuer}/token/revoke`; `AuthConfig.revocation_endpoint` (default `None`) overrides it.
  The browser session is not ended: apps that want single sign-out open `{issuer}/logout` themselves.

### Added
- `aun-auth`/FFI: `AuthManager.begin_login_with_redirect(prompt, redirect_uri)` for desktop apps that
  receive the code on a loopback listener with an OS-assigned port (RFC 8252 §7.3). The redirect
  must be `http://127.0.0.1:{port}/…`, `http://[::1]:{port}/…` or `http://localhost:{port}/…` (or
  the registered redirect); the code exchange sends the same redirect, and a callback on another
  port is refused.

### Fixed
- `aun-upload`: resuming after the proxy lost an upload (404/410) follows the server's current
  `upload/resume` contract. It returns a new proxy `tusSessionId` and, when the server still holds
  the earlier upload of the same size, its `tusUploadId`: the engine continues at
  `{tus}/{tusUploadId}` from the server's offset, or creates a new upload with the new session.
  Every request goes through the proxy with the bearer token. The direct-to-storage fallback
  (`tusEndpoint`/`tusHeaders`) is gone from the server, so uploads whose proxy session expired
  failed before this fix. Manifests saved with a `Direct` target switch to the proxy on their
  next 404 or 401.

### Changed
- `aun-api`: OpenAPI snapshot refreshed from QA (132 paths; #25). New operations include
  `get_ChannelAccessList`, `post_VideoBulkUpdate`, `post_PlaylistVideosBulkAdd`/`Remove`,
  `put_PlaylistVideosReorder`, `get_PlaylistUploadAddsStatus`/`post_PlaylistUploadAddsCreate` and
  the playlist job routes; `GetSearchSuggestionsGetParams` gains `types` and `channel_id`. The
  codegen maps the documented `/api/v1/…` routes to the `/api/…` paths clients already call.
- `aun-search`: a test pins the suggestion query parameters to the snapshot.

### Changed
- `aun-sync`/`aun-upload`: an open store is now locked against other processes. `SyncEngine::open`
  takes an exclusive lock on `<store>.lock`, and `UploadEngine::open` one per account in the store
  directory. A second open while the first is alive returns `Conflict` instead of both processes
  overwriting each other's state or resuming the same TUS upload. The upload lock is released when
  the engine is dropped, or after `shutdown()` once its transfers have stopped.
- `aun-upload`: manifests are written through a temp file unique to each write, so concurrent saves
  never share a temp file. A throttled transfer now stops promptly on pause, cancel or shutdown.

### Added
- FFI: `ApiClient.request_path(PathCall)` calls an `/api/` path that is not in the pinned OpenAPI
  document yet (for example the Creator Studio routes: `/api/channels/access`,
  `/api/channels/{id}/content`, `/api/videos/bulk-update`), with the same auth, refresh on 401,
  retries and errors as `request`. `headers` carries extras such as `channelId`
  (`Authorization`, `Cookie`, `Host` and `Content-Length` are refused). The template must start with `/api/`; `{name}` parameters are
  percent-encoded, and dot segments, queries, `//` and `%` in the template are refused.
- `aun-search` (new crate) and FFI: live search autocomplete for the native apps.
  `ApiClient.search_suggestions(SuggestionQuery)` calls `GET /api/search/suggestions` with `q`,
  `limit` (1-20, default 8), `types` and `channelId`, once and without retries (a 429 throws
  `RateLimited` with `retry_after_ms`); unknown suggestion types decode as `Query` and malformed rows
  are skipped. `group_search_suggestions` orders groups as completions, videos, channels, playlists,
  categories. `normalize_search_text`/`normalize_arabic` port the server's Arabic-aware search
  normalisation (tashkeel, alef forms, taa marbuta, yeh/alef maqsura, digits, NFKC, accents);
  `search_highlight_ranges` returns match ranges in UTF-16 units; `search_matches` and
  `recent_searches_add`/`_remove`/`_filter` hold the recent-search rules (apps persist the list).
- `aun-api`: `RequestSpec::max_attempts` caps retries for a single request.
- `aun-auth`/FFI: `LoginPrompt` and `AuthManager.begin_login_with_prompt`. `Silent` sends `prompt=none`
  (re-authorize without UI after `ReauthRequired`; `login_required` comes back as `Unauthorized`), `Login`
  sends `prompt=login` ("Add account"), `SelectAccount` sends `prompt=select_account`. `begin_login` still
  sends no `prompt`.

## [0.2.0] - 2026-11-08

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
- `aun-i18n`: the `I18n` catalog. It switches locales at runtime with `LocaleListener` callbacks,
  resolves keys along the fallback chain (`ar-EG` → `ar` → `en`), formats messages with named
  arguments (`{{name}}`, `{name}`, ICU `plural`/`select`/`number`) and supports i18next plural keys.
  CLDR plural rules cover Arabic (six forms), English, Urdu, Turkish, Indonesian, Malay, French and
  more. Also: Eastern Arabic / Persian digits, number and duration formatting, and bidi helpers
  (direction detection, FSI/PDI isolation of mixed-direction arguments). Bundles sync from
  `/api/i18n/config|metadata|translations/{locale}` into an atomic on-disk cache validated by
  checksums. Built-in bundles cover offline first launch. Bundle schema in `schemas/i18n/`.
- `aun-telemetry`: consent-gated `Telemetry` recorder. With consent `unknown` or `denied` nothing
  is recorded or stored; `crash_only` keeps crash reports and breadcrumbs only; narrowing consent
  purges the memory and disk buffer. It scrubs PII (sensitive keys, e-mails, tokens/JWTs, URL
  queries, opaque secrets, phone numbers), samples by session (never crashes), caps size, keeps an
  atomic persistent buffer, a breadcrumb ring buffer attached to the next crash report, and batched
  flush with retry/drop semantics. `QoeTracker` measures startup time, rebuffer count/duration/ratio,
  bitrate switches and stable playback error codes. Sinks: `HttpSink` (JSON batches to a
  configurable URL), `ApiPerformanceSink` (QoE to `POST /api/analytics/events`), `NoopSink`.
  `HttpSink` refuses non-HTTPS URLs (plain `http` only for loopback).
- `aun-sync`: cross-device watch state, history, notes/bookmarks, Watch later and likes as LWW
  registers with tombstones (commutative, idempotent `join`, property-tested). Also: a Lamport
  floor for local writes, an atomic JSON store with outbox and download records, debounced
  position pushes, continue-watching and history queries, change listeners, and
  `ApiSyncTransport` over `aun-api` (server gaps in `docs/SYNC.md`, #13).
- `aun-api`: client generated from the api-videos OpenAPI (`spec/openapi/api-videos.json`,
  `tools/codegen/openapi_to_rust.py`): typed params/bodies/responses for every operation,
  request builders, retries with jittered backoff (idempotent requests or an `Idempotency-Key`),
  `Retry-After` handling, refresh-once-on-401 via `AuthProvider`, error-body mapping, and a
  persistent FIFO offline write queue with coalescing.
- FFI: `ApiClient` (generic `request(ApiCall)` over every operation id), `api_operations()`,
  `TokenSource` callback interface, `OfflineQueue` with `QueueFlushReport`.
- `aun-auth`: OAuth 2 authorization code + PKCE S256 (state/CSRF and redirect checks),
  discovery (RFC 8414), refresh with rotation and single-flight, RFC 8628 device flow
  (pending/slow_down/denied/expired, cancellable), multi-account store over `SecureStorage`
  (max 10, active account + active channel, re-auth flag), `AuthManager` implementing
  `aun_api::AuthProvider`.
- FFI: `AuthManager` (+ `auth_manager_discover`), `DeviceLogin`, `AuthListener`/`AuthEvent`,
  `AccountInfo`, `AuthManager.api_client()`.
- Security: API clients and OAuth endpoints refuse non-HTTPS URLs (plain `http` only for
  loopback), so tokens and codes are never sent in cleartext (`aun_common::security`).

### Changed

- Release workflow no longer pushes to `main`: the Package.swift release commit is created on top
  of the `vX.Y.Z` tag and only the SwiftPM tag `X.Y.Z` is pushed (`scripts/swiftpm-release-tag.sh`,
  `docs/RELEASING.md`). Swift consumers depend on `from: "X.Y.Z"`.
- `aun-upload`: resumable TUS uploads for api-videos. Persistent per-account queue (survives
  restarts, resumes from the server's offset via HEAD), priority + FIFO, 1–5 parallel uploads,
  shared runtime-adjustable bandwidth cap, Wi-Fi-only/offline waiting, SHA-256 checksums with
  duplicate lookup, pause/resume/retry/cancel (TUS terminate), changed-file detection, rate-limit
  aware (one upload session per file, never re-initiated; `Retry-After` honoured), fallback to
  direct upload via `/upload/resume/{videoId}` when the proxy session expired. Feature `watch`:
  desktop folder watching (settle detection, symlinks outside the folder ignored).
  Every TUS request is checked first: endpoints and upload URLs must be https (http only for
  localhost), and on the proxy, where the bearer token is attached, the upload URL must share the
  endpoint's origin, so a hostile `Location` can't collect the token.
- FFI: `UploadEngine`, `UploadListener`, `UploadRequest`/`UploadConfig`/`UploadInfo`/`UploadState`,
  `upload_config_default()`; `UploadEngine.watch_folder` on macOS/Windows/Linux (throws
  `Unsupported` on mobile/TV).
- FFI for `aun-media`: `media_probe_json`, `media_keyframes`, `media_measure_loudness`,
  `media_gain_to_target_db`, `media_detect_silence`, `media_generate_waveform`,
  `media_select_thumbnail_timestamps`, `transcription_model_catalog`, `transcript_to_webvtt`/`_srt`,
  `edl_schema_version`, `edl_load_project` (migrates to the current schema), `edl_validate`,
  `edl_chapters_json`, `MediaCancel`; config defaults. Long operations run off the caller's thread.

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
