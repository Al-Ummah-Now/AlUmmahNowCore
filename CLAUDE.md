# CLAUDE.md — AlUmmahNowCore

Shared Rust core (`aun-core`) and Studio media engine (`aun-media`) for the native Al Ummah Now apps.
Read `docs/PLAN.md` first: layout, FFI rules, error and async model, adapters, targets, releases.

## Hard rules

- **Public repo.** Never commit secrets, tokens, `.env` files, signing material, real user data or
  downloaded models. Test fixtures are synthetic (generated tones/noise, our own short clips).
- **Only `crates/aun-ffi` uses UniFFI.** Core crates stay plain Rust; convert at the FFI edge.
- **uniffi is pinned to `=0.31.0`** to match `uniffi-bindgen-cs v0.11.1+v0.31.0`. Bump both together.
- Exported functions return `Result<_, AunError>`; no panics across FFI; no `unwrap()` outside tests.
- Releases only via tag `vX.Y.Z` on `main` → `release.yml`. Never publish by hand. Update
  `CHANGELOG.md` (Unreleased) in every PR; a breaking FFI change goes under **Breaking**.
- Small PRs into `main`, CI green, then `gh pr merge --squash --admin`.
- Out-of-scope work: open a GitHub issue labelled `queue`.

## Commands

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check licenses            # licence policy (deny.toml)
python3 tools/codegen/openapi_to_rust.py spec/openapi/api-videos.json crates/aun-api/src/generated.rs
scripts/gen-bindings.sh              # Swift/Kotlin/C# bindings into bindings/*/generated
scripts/build-xcframework.sh         # macOS only
```

If Homebrew's `cargo` shadows rustup's: `export PATH="$(dirname "$(rustup which cargo)"):$PATH"`.

## Conventions

- Timestamps `i64` Unix ms, durations `u64` ms at the FFI boundary.
- Every crate has unit tests; network code is tested against `wiremock`/in-process servers, never
  against QA/prod in CI.
- Feature flags: `aun-media/whisper` (cmake, whisper.cpp), `aun-upload/watch` (desktop folder watch).
- Arabic is a first-class locale: test RTL/shaping paths with Arabic strings.
