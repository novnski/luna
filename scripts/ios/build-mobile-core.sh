#!/bin/bash
# Build the LUNA mobile core (crates/mobile) for iOS and generate its Swift
# bindings.
#
#   scripts/ios/build-mobile-core.sh [iphoneos|iphonesimulator] [<swift-out-dir>]
#
# Outputs (target/ios-core/<platform>/):
#   libluna_mobile.a           link with -lluna_mobile (LIBRARY_SEARCH_PATHS)
#   include/module.modulemap   `import luna_coreFFI` (SWIFT_INCLUDE_PATHS)
# and, when <swift-out-dir> is given, refreshes <swift-out-dir>/luna_core.swift
# only when it changed. The generated Swift is committed beside the app that
# links it, so its project builds from a synchronized folder.
#
# Requires the Rust target (`rustup target add aarch64-apple-ios`, or
# aarch64-apple-ios-sim for the simulator).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PLATFORM="${1:-iphoneos}"
SWIFT_OUT="${2:-}"
case "$PLATFORM" in
  iphoneos) TARGET=aarch64-apple-ios ;;
  iphonesimulator) TARGET=aarch64-apple-ios-sim ;;
  *) echo "error: unsupported platform $PLATFORM" >&2; exit 64 ;;
esac

OUT="$ROOT/target/ios-core/$PLATFORM"
mkdir -p "$OUT/include"

# Xcode exports SDK and deployment variables for the app; host build scripts
# and proc macros must not inherit them.
unset SDKROOT PLATFORM_NAME EFFECTIVE_PLATFORM_NAME
export IPHONEOS_DEPLOYMENT_TARGET=26.0

cd "$ROOT"
scripts/cargo.sh build --locked -p luna-mobile --lib --target "$TARGET"
scripts/cargo.sh build --locked -p luna-mobile --bin uniffi-bindgen --features bindgen

LIB="$ROOT/target/$TARGET/debug/libluna_mobile.a"
cp -p "$LIB" "$OUT/libluna_mobile.a"

GEN="$OUT/gen"
rm -rf "$GEN"
"$ROOT/target/debug/uniffi-bindgen" generate --library "$LIB" --language swift \
  --out-dir "$GEN" >/dev/null
cp "$GEN/luna_coreFFI.h" "$OUT/include/luna_coreFFI.h"
cp "$GEN/luna_coreFFI.modulemap" "$OUT/include/module.modulemap"
if [[ -n "$SWIFT_OUT" ]]; then
  mkdir -p "$SWIFT_OUT"
  cmp -s "$GEN/luna_core.swift" "$SWIFT_OUT/luna_core.swift" \
    || cp "$GEN/luna_core.swift" "$SWIFT_OUT/luna_core.swift"
fi
echo "built $OUT/libluna_mobile.a"
