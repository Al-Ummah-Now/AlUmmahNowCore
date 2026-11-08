#!/usr/bin/env bash
# Generates Swift, Kotlin and C# bindings from the host build of aun-ffi.
# Usage: scripts/gen-bindings.sh [swift|kotlin|csharp|all] [--release]
# Output: bindings/<lang>/generated/ (git-ignored). Needs uniffi-bindgen-cs on PATH for C#.
set -euo pipefail
cd "$(dirname "$0")/.."

LANGS="${1:-all}"
PROFILE=debug
CARGO_FLAGS=()
if [[ "${2:-}" == "--release" ]]; then PROFILE=release; CARGO_FLAGS+=(--release); fi

cargo build -p aun-ffi --lib ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}
case "$(uname -s)" in
  Darwin) LIB="target/$PROFILE/libaun_core.dylib" ;;
  Linux) LIB="target/$PROFILE/libaun_core.so" ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) LIB="target/$PROFILE/aun_core.dll" ;;
  *) echo "unsupported host" >&2; exit 1 ;;
esac

bindgen() { cargo run -q -p aun-ffi --features cli --bin uniffi-bindgen -- "$@"; }
CONFIG=crates/aun-ffi/uniffi.toml

if [[ "$LANGS" == all || "$LANGS" == swift ]]; then
  rm -rf bindings/swift/generated && mkdir -p bindings/swift/generated
  bindgen generate --library "$LIB" --language swift --config "$CONFIG" --out-dir bindings/swift/generated
fi
if [[ "$LANGS" == all || "$LANGS" == kotlin ]]; then
  rm -rf bindings/kotlin/generated && mkdir -p bindings/kotlin/generated
  bindgen generate --library "$LIB" --language kotlin --no-format --config "$CONFIG" --out-dir bindings/kotlin/generated
fi
if [[ "$LANGS" == all || "$LANGS" == csharp ]]; then
  rm -rf bindings/csharp/generated && mkdir -p bindings/csharp/generated
  uniffi-bindgen-cs --library "$LIB" --config "$CONFIG" --out-dir bindings/csharp/generated
fi
echo "bindings generated from $LIB"
