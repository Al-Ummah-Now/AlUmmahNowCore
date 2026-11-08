# aun-core plan (Epics 11 and 30)

This repo is the shared Rust core behind every native Al Ummah Now app. UI, players, capture and
hardware encoders stay native (spec §4.1); the logic that must behave the same everywhere lives here
(spec §4.2): API client, auth, sync, uploads, the Studio media engine, i18n and telemetry.

Spec: `docs/native-apps/EPICS.md` in the monorepo (§4.1–4.3, Epic 11.1–11.9, Epic 30.1–30.7).

## 1. Consumers

| Repo | Language | How it gets the core |
|---|---|---|
| AlUmmahNowApple | Swift (iOS, iPadOS, tvOS, macOS) | Swift Package `AunCore`: binary target `AunCoreFFI.xcframework` from the GitHub release + generated Swift sources |
| AlUmmahNowAndroid | Kotlin (phone, TV, Auto) | AAR `net.alummahnow:aun-core` on GitHub Packages Maven (JNA + `.so` per ABI) |
| AlUmmahNowDesktop | C# (.NET 8+; WinUI 3, GTK 4, Tizen) | NuGet `AlUmmahNow.Core` on GitHub Packages (native libs under `runtimes/<rid>/native`) |

## 2. Workspace layout

```
Cargo.toml                 workspace, shared dependency versions, lints
crates/
  aun-common/              shared error model, platform-adapter traits, clock, ids
  aun-api/                 typed api-videos client (generated from OpenAPI) + retries, auth hook, offline queue
  aun-auth/                OAuth 2 + PKCE S256, device flow (RFC 8628), multi-account, token store trait
  aun-sync/                watch-state / history / notes / offline-library sync, LWW registers
  aun-upload/              resumable TUS engine (api-videos TUS proxy), batch queue, persistence, folder watch
  aun-media/               Studio media engine (Epic 30): probe/analyse, job graph, proxies, transcription, EDL, text shaping
  aun-i18n/                translation bundles, plural rules, locale switching, bidi helpers
  aun-telemetry/           consent-gated analytics, QoE events, crash breadcrumbs
  aun-ffi/                 the ONLY crate with UniFFI: facade objects, FFI records, AunError; builds `aun_core` cdylib/staticlib
spec/openapi/              pinned api-videos OpenAPI snapshot the client is generated from
schemas/                   JSON schemas (EDL project format, i18n bundles)
tools/codegen/             OpenAPI → Rust generator (Python 3, stdlib only)
bindings/
  swift/                   SwiftPM smoke-test package (generated sources + xcframework)
  kotlin/                  Gradle: Android library (AAR) + JVM smoke test
  csharp/                  AlUmmahNow.Core.csproj (NuGet) + xUnit smoke test
Package.swift              Swift Package consumers depend on (rewritten by the release workflow)
scripts/                   build-xcframework.sh, build-android.sh, gen-bindings.sh, …
```

## 3. FFI boundary rules

1. **Only `aun-ffi` depends on `uniffi`.** Core crates are plain Rust with plain Rust tests. `aun-ffi`
   wraps them in `#[derive(uniffi::Object)]` facades and FFI records, converting at the edge.
2. **One UniFFI namespace: `aun_core`.** Swift module `AunCore`, Kotlin package `net.alummahnow.core`,
   C# namespace `AlUmmahNow.Core`. One library, one set of generated files per language.
3. **Records cross by value, objects by handle.** Use `uniffi::Record` for plain data, `uniffi::Object`
   (behind `Arc`) for anything with state (clients, queues, engines).
4. **No panics across the boundary.** Every exported function returns `Result<_, AunError>` unless it
   is infallible; `panic = "unwind"` stays on so UniFFI turns a stray panic into an `InternalError`.
5. **Types that cross are FFI-portable:** `String`, integers, `f64`, `bool`, `Vec`, `HashMap<String,_>`,
   `Option`, records, enums, objects, `Vec<u8>` for bytes. Timestamps are `i64` Unix milliseconds;
   durations are `u64` milliseconds. No `u128`, no tuples, no generics, no lifetimes.
6. **Native implements traits via callback interfaces** (`#[uniffi::export(with_foreign)]`): secure
   storage, background transfer, hardware encoder, notifications, event listeners. Callback methods
   must be cheap and thread-safe; Rust never holds a lock while calling into native code.
7. **Additive evolution.** Adding a function, record field (with a default) or enum variant is a minor
   bump; renaming/removing is a major bump (see §8). Enum variants that may grow are matched with a
   fallback on the native side.
8. **Secrets never cross into logs.** Tokens are only handed to the `SecureStorage` adapter and HTTP
   headers; `Debug` impls redact them.

## 4. Error model

Each core crate has its own `thiserror` enum. All of them convert into one FFI error:

```rust
#[derive(uniffi::Error)]
pub enum AunError {
    Network { detail: String },                        // DNS, TLS, connection reset, timeouts
    Offline,                                           // no connectivity; queued work will retry
    Http { status: u16, code: Option<String>, detail: String },
    Unauthorized { detail: String },                  // refresh failed / sign-in required
    Forbidden { detail: String },
    NotFound { detail: String },
    Conflict { detail: String },
    RateLimited { retry_after_ms: Option<u64> },
    InvalidInput { detail: String },
    Storage { detail: String },                       // local DB / file system
    Platform { detail: String },                      // a native adapter failed
    Unsupported { detail: String },                   // feature/backend not available on this target
    Cancelled,
    Internal { detail: String },
}
```

Fields are named `detail`, not `message`, because Kotlin exceptions already have `message`.
Swift sees `throws AunError`, Kotlin `AunException` subclasses, C# `AunException` subclasses.
`aun_common::Error::is_retryable()` decides retries centrally (network, offline, 408/425/429/5xx).

## 5. Async model

- One **tokio multi-thread runtime** owned by the library (lazily created, 2–4 workers) via
  `#[uniffi::export(async_runtime = "tokio")]`. Async Rust functions become `async` (Swift),
  `suspend` (Kotlin) and `Task` (C#) natively.
- **Long-running work** (uploads, encodes, transcription, sync) is started by a call that returns
  immediately with a handle object; progress and state changes flow to native through **listener
  callback interfaces** (`UploadListener`, `JobListener`, `AuthListener`, …). Listeners are invoked
  from Rust worker threads: native code must hop to its UI thread.
- **Cancellation:** every handle has `cancel()`; work checks a `CancellationToken` at await points.
- CPU-heavy work (loudness, whisper, shaping) runs on `spawn_blocking` / a rayon pool, never on the
  async workers.

## 6. Platform-adapter traits (callback interfaces, implemented natively)

| Trait | Apple | Android | Windows | Linux |
|---|---|---|---|---|
| `SecureStorage` (get/set/delete bytes by key) | Keychain | Android Keystore + EncryptedSharedPreferences | Credential Locker / DPAPI | libsecret |
| `BackgroundTransfer` (enqueue a chunk/file transfer that survives app suspension, report completion) | `URLSession` background session | WorkManager | BackgroundTransfer | — (in-process) |
| `HardwareEncoder` (execute an encode `JobPlan` node, report progress) | VideoToolbox / AVFoundation | MediaCodec / Media3 Transformer | Media Foundation | GStreamer |
| `NotificationSink` (local notification on upload/encode/transcribe finished) | UserNotifications | NotificationManager | Toast / AppNotification | libnotify / portal |
| `Connectivity` (online/offline, metered) | NWPathMonitor | ConnectivityManager | NetworkInformation | NetworkManager / portal |
| `Logger` (structured log sink) | os_log | Logcat | ETW / file | journald |

In-process fallbacks ship in Rust where possible (in-memory storage for tests, the software encoder
backend, `reqwest` transfers) so every crate is testable without a device.

## 7. Target matrix

| Target | Triple(s) | Artifact |
|---|---|---|
| iOS device | `aarch64-apple-ios` | xcframework slice |
| iOS simulator | `aarch64-apple-ios-sim`, `x86_64-apple-ios` (lipo) | xcframework slice |
| macOS universal | `aarch64-apple-darwin`, `x86_64-apple-darwin` (lipo) | xcframework slice |
| tvOS device / simulator | `aarch64-apple-tvos`, `aarch64-apple-tvos-sim` (tier 3, nightly `-Zbuild-std`) | xcframework slices |
| Android | `aarch64-linux-android`, `armv7-linux-androideabi`, `x86_64-linux-android` (cargo-ndk, API 24) | AAR `jniLibs` |
| Windows | `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` | NuGet `runtimes/win-x64`, `win-arm64` |
| Linux | `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` | NuGet `runtimes/linux-x64`, `linux-arm64` |
| macOS (C#) | universal dylib | NuGet `runtimes/osx` |
| Tizen arm | `armv7-unknown-linux-gnueabi` | follow-up (queue issue) |

Heavy native dependencies are feature-gated so the default build stays portable:
`aun-media/whisper` (whisper.cpp via whisper-rs; cmake), `aun-media/harfbuzz` (pure-Rust HarfRust
shaping, on by default), `aun-upload/watch` (folder watching, desktop only).

## 8. Versioning and release process

- **SemVer** on the whole workspace (one version). While `0.x`, a minor bump may break the FFI;
  each break is called out in `CHANGELOG.md` under **Breaking**. `1.0.0` freezes the FFI.
- `CHANGELOG.md` follows Keep a Changelog; every PR adds a line under **Unreleased**.
- Release = push tag `vX.Y.Z` on a commit already on `main`. `release.yml`:
  1. verifies the tag is on `main` and matches `workspace.package.version`;
  2. builds all targets, generates bindings, runs the smoke tests;
  3. creates the GitHub release with `AunCoreFFI.xcframework.zip`, Swift sources, the AAR, the
     `.nupkg` and SHA-256 sums;
  4. publishes the AAR to GitHub Packages Maven and the NuGet package to GitHub Packages;
  5. commits the updated `Package.swift` (release URL + checksum) and Swift sources to `main`.
- Never release by hand; never move or delete a published tag.

## 9. Build order

1. Workspace, docs, CI (fmt, clippy, tests on 3 OSes; cross builds; bindings + smoke tests; release).
2. `aun-api` (generated client, retries, auth refresh hook, offline write queue).
3. `aun-auth` (PKCE, device flow against a mock, multi-account, token storage adapter).
4. `aun-upload` (TUS against the api-videos proxy semantics, batch queue, persistence, folder watch).
5. `aun-media` (probe/analyse, job graph + software backend, proxies/thumbnails/waveforms,
   transcription + model manager, EDL schema + migrations, Arabic shaping).
6. `aun-sync`, `aun-i18n`, `aun-telemetry`.

## 10. api-videos specifics the core must respect

- **OpenAPI:** served at `https://videos.api.qa.alummahnow.net/openapi.json` (3.1, inline schemas,
  paths currently doubled e.g. `/api/videos/api/videos/upload` — the generator normalises them until
  Epic 9.3 ships the versioned `/api/v1` contract, then we regenerate).
- **Upload flow:** `POST /api/videos/upload/initiate` → `{videoId, tusSessionId, chunkSize}`;
  TUS `POST /api/videos/tus` with `Upload-Metadata: tusSessionId …` → `Location: …/tus/{tusSessionId}`;
  `HEAD`/`PATCH` that location; finally `POST /api/videos/upload/complete`. TUS sessions live 1 h in KV;
  after expiry `GET /api/videos/upload/resume/{videoId}` re-authorises.
- **Rate limits:** upload **sessions** are limited (prod: 15/min, 20/h, 50/day, 5 concurrent; a batch
  initiate counts once per video, max 15). TUS create + complete share a looser bucket (30/min). PATCH and
  HEAD are unlimited. So the engine never re-initiates to retry: it resumes the existing session, retries
  PATCH freely, and caps concurrent sessions at 5.
- **Auth:** OpenAuth issuer `https://auth.qa.alummahnow.net` (`/authorize`, `/token`, JWKS). Device
  authorization (RFC 8628) arrives with Epic 9.2; built against the RFC with a mock server until then.

## 11. Licences

The repo's own licence is an owner decision (pending). Dependencies are checked by `cargo-deny`
(`deny.toml`: permissive licences only — MIT, Apache-2.0, BSD, ISC, Zlib, Unicode, MPL-2.0 for
file-level copyleft). Whisper model licences are listed in `docs/MODELS.md`.
