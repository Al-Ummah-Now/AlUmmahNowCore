#!/usr/bin/env bash
# Builds build/AunCoreFFI.xcframework (static libs + C header + module map) and copies the generated
# Swift sources to apple/Sources/AunCore. macOS only.
#
# Usage: scripts/build-xcframework.sh [--macos-only] [--no-tvos]
#   --macos-only  just the macOS universal slice (fast; used by the Swift smoke test)
#   --no-tvos     skip tvOS (tier-3 targets need nightly + -Zbuild-std)
set -euo pipefail
cd "$(dirname "$0")/.."

MACOS_ONLY=0; TVOS=1
for a in "$@"; do
  case "$a" in
    --macos-only) MACOS_ONLY=1; TVOS=0 ;;
    --no-tvos) TVOS=0 ;;
    *) echo "unknown arg $a" >&2; exit 1 ;;
  esac
done

export MACOSX_DEPLOYMENT_TARGET=12.0 IPHONEOS_DEPLOYMENT_TARGET=15.0 TVOS_DEPLOYMENT_TARGET=15.0
LIB=libaun_core.a
OUT=build
rm -rf "$OUT/AunCoreFFI.xcframework" "$OUT/xcf" && mkdir -p "$OUT/xcf"

build() { cargo build -p aun-ffi --lib --release --target "$1"; }
build_tier3() { cargo +nightly build -Z build-std=std,panic_unwind -p aun-ffi --lib --release --target "$1"; }

# Headers + module map from the generated Swift bindings
scripts/gen-bindings.sh swift --release
HDR="$OUT/xcf/include/AunCoreFFI"
mkdir -p "$HDR"
cp bindings/swift/generated/AunCoreFFI.h "$HDR/"
# Module map lives in a subfolder so it can't collide with other xcframeworks' module.modulemap.
cp bindings/swift/generated/AunCoreFFI.modulemap "$HDR/module.modulemap"
mkdir -p apple/Sources/AunCore
cp bindings/swift/generated/AunCore.swift apple/Sources/AunCore/AunCore.swift

ARGS=()
build aarch64-apple-darwin; build x86_64-apple-darwin
mkdir -p "$OUT/xcf/macos"
lipo -create target/aarch64-apple-darwin/release/$LIB target/x86_64-apple-darwin/release/$LIB -output "$OUT/xcf/macos/$LIB"
ARGS+=(-library "$OUT/xcf/macos/$LIB" -headers "$OUT/xcf/include")

if [[ $MACOS_ONLY == 0 ]]; then
  build aarch64-apple-ios
  build aarch64-apple-ios-sim; build x86_64-apple-ios
  mkdir -p "$OUT/xcf/ios-sim"
  lipo -create target/aarch64-apple-ios-sim/release/$LIB target/x86_64-apple-ios/release/$LIB -output "$OUT/xcf/ios-sim/$LIB"
  ARGS+=(-library target/aarch64-apple-ios/release/$LIB -headers "$OUT/xcf/include")
  ARGS+=(-library "$OUT/xcf/ios-sim/$LIB" -headers "$OUT/xcf/include")
fi
if [[ $TVOS == 1 ]]; then
  build_tier3 aarch64-apple-tvos
  build_tier3 aarch64-apple-tvos-sim
  ARGS+=(-library target/aarch64-apple-tvos/release/$LIB -headers "$OUT/xcf/include")
  ARGS+=(-library target/aarch64-apple-tvos-sim/release/$LIB -headers "$OUT/xcf/include")
fi

xcodebuild -create-xcframework "${ARGS[@]}" -output "$OUT/AunCoreFFI.xcframework"
(cd "$OUT" && rm -f AunCoreFFI.xcframework.zip && ditto -c -k --sequesterRsrc --keepParent AunCoreFFI.xcframework AunCoreFFI.xcframework.zip)
echo "checksum: $(swift package compute-checksum "$OUT/AunCoreFFI.xcframework.zip")"
