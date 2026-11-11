# Changelog

All notable changes to `aun-core` are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/). While the version is `0.x`, a minor bump may break the
FFI; such changes are listed under **Breaking**.

## [Unreleased]

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
