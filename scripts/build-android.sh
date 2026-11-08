#!/usr/bin/env bash
# Builds libaun_core.so for each Android ABI into the AAR's jniLibs (needs cargo-ndk + an NDK).
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to an NDK (r26+)}"
OUT=bindings/kotlin/aun-core/src/main/jniLibs
rm -rf "$OUT"
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 --platform 24 -o "$OUT" build -p aun-ffi --lib --release
find "$OUT" -name '*.so' -exec ls -l {} \;
