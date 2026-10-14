#!/bin/bash
# Build the LUNA mobile core (crates/mobile) for iOS and generate its Swift
# bindings, through the repository's locked Cargo wrapper and its shared
# target/ cache (build budget and lock included).
#
#   scripts/ios/build-mobile-core.sh [iphoneos|iphonesimulator] [<swift-out-dir>]
#
# In the app's Xcode build phase it follows the build being made:
#   CONFIGURATION               Debug -> Cargo dev profile, Release -> --release
#   ARCHS                       one Rust target per architecture, then lipo
#   IPHONEOS_DEPLOYMENT_TARGET  must be the app's floor (26.0)
# By hand, LUNA_IOS_CONFIGURATION and LUNA_IOS_ARCHS make the same choices
# (defaults: Debug; arm64 on a device, the Mac's architecture in Simulator).
#
# Outputs (target/ios-core/<platform>/):
#   libluna_mobile.a           link with -lluna_mobile (LIBRARY_SEARCH_PATHS)
#   include/module.modulemap   `import luna_coreFFI` (SWIFT_INCLUDE_PATHS)
#   identity.json              what was built: configuration, profile, Rust
#                              targets, toolchain, source revision and the
#                              SHA-256 of the library and its bindings
# and, when <swift-out-dir> is given, refreshes <swift-out-dir>/luna_core.swift
# only when it changed. A relative <swift-out-dir> is relative to the caller's
# working directory (Xcode passes an absolute one). The generated Swift is committed beside the app that
# links it, so its project builds from a synchronized folder; CI fails when
# the committed copy differs from what the library generates.
#
# Requires each Rust target: `rustup target add aarch64-apple-ios`, plus
# aarch64-apple-ios-sim (or x86_64-apple-ios) for the simulator.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PLATFORM="${1:-iphoneos}"
SWIFT_OUT="${2:-}"
# Resolve against the caller's directory now: the build runs from $ROOT.
if [[ -n "$SWIFT_OUT" && "$SWIFT_OUT" != /* ]]; then
  SWIFT_OUT="$PWD/$SWIFT_OUT"
fi
# The app's deployment floor. Changing it is a separate, deliberate decision
# for the project and this script together.
APP_FLOOR=26.0

case "$PLATFORM" in
  iphoneos|iphonesimulator) ;;
  *) echo "error: unsupported platform $PLATFORM" >&2; exit 64 ;;
esac

BUILD_CONFIGURATION="${CONFIGURATION:-${LUNA_IOS_CONFIGURATION:-Debug}}"
case "$BUILD_CONFIGURATION" in
  Debug) PROFILE=debug; PROFILE_FLAG="" ;;
  Release) PROFILE=release; PROFILE_FLAG="--release" ;;
  *) echo "error: no core profile for configuration $BUILD_CONFIGURATION" >&2; exit 64 ;;
esac

if [[ -n "${ARCHS:-}" ]]; then
  BUILD_ARCHS="$ARCHS"
elif [[ -n "${LUNA_IOS_ARCHS:-}" ]]; then
  BUILD_ARCHS="$LUNA_IOS_ARCHS"
elif [[ "$PLATFORM" == iphoneos ]]; then
  BUILD_ARCHS=arm64
else
  BUILD_ARCHS="$(uname -m)"
fi

FLOOR="${IPHONEOS_DEPLOYMENT_TARGET:-$APP_FLOOR}"
if [[ "$FLOOR" != "$APP_FLOOR" ]]; then
  echo "error: the app targets iOS $FLOOR but the core is built for $APP_FLOOR;" \
    "change the floor in both places deliberately" >&2
  exit 64
fi

rust_target() {
  case "$PLATFORM:$1" in
    iphoneos:arm64) echo aarch64-apple-ios ;;
    iphonesimulator:arm64|iphonesimulator:aarch64) echo aarch64-apple-ios-sim ;;
    iphonesimulator:x86_64) echo x86_64-apple-ios ;;
    *) echo "error: no Rust target for $PLATFORM architecture $1" >&2; return 64 ;;
  esac
}

OUT="$ROOT/target/ios-core/$PLATFORM"
mkdir -p "$OUT/include"

# Xcode exports SDK and deployment variables for the app; host build scripts
# and proc macros must not inherit them.
unset SDKROOT PLATFORM_NAME EFFECTIVE_PLATFORM_NAME
export IPHONEOS_DEPLOYMENT_TARGET="$APP_FLOOR"

cd "$ROOT"
# macOS /bin/bash is 3.2: never expand an empty array under `set -u`.
TARGETS=()
LIBS=()
COUNT=0
for arch in $BUILD_ARCHS; do
  target="$(rust_target "$arch")"
  scripts/cargo.sh build --locked -p luna-mobile --lib --target "$target" ${PROFILE_FLAG:+"$PROFILE_FLAG"}
  TARGETS[COUNT]="$target"
  LIBS[COUNT]="$ROOT/target/$target/$PROFILE/libluna_mobile.a"
  COUNT=$((COUNT + 1))
done
if (( COUNT == 0 )); then
  echo "error: no architectures to build" >&2
  exit 64
fi
# The bindgen tool is a host binary whose dependencies compile C (SQLite).
# cc-rs adds no sysroot for macOS, and Xcode puts its bare toolchain clang
# ahead of the /usr/bin shims, so name the macOS SDK for this build only.
SDKROOT="$(xcrun --sdk macosx --show-sdk-path)" \
  scripts/cargo.sh build --locked -p luna-mobile --bin uniffi-bindgen --features bindgen

STAGED="$OUT/libluna_mobile.a.partial"
if (( COUNT == 1 )); then
  cp -p "${LIBS[0]}" "$STAGED"
else
  xcrun lipo -create "${LIBS[@]}" -output "$STAGED"
fi
mv -f "$STAGED" "$OUT/libluna_mobile.a"

GEN="$OUT/gen"
rm -rf "$GEN"
# Every architecture carries the same metadata; bindings come from the first
# library actually linked, never from another build.
"$ROOT/target/debug/uniffi-bindgen" generate --library "${LIBS[0]}" --language swift \
  --out-dir "$GEN" >/dev/null
# UniFFI templates contain whitespace-only lines; keep committed output clean
# and stable across repeated Xcode builds without changing generated APIs.
python3 - "$GEN/luna_core.swift" <<'PY'
from pathlib import Path
import sys
source = Path(sys.argv[1])
source.write_text('\n'.join(line.rstrip() for line in source.read_text().splitlines()) + '\n')
PY
cp "$GEN/luna_coreFFI.h" "$OUT/include/luna_coreFFI.h"
cp "$GEN/luna_coreFFI.modulemap" "$OUT/include/module.modulemap"
if [[ -n "$SWIFT_OUT" ]]; then
  mkdir -p "$SWIFT_OUT"
  cmp -s "$GEN/luna_core.swift" "$SWIFT_OUT/luna_core.swift" \
    || cp "$GEN/luna_core.swift" "$SWIFT_OUT/luna_core.swift"
fi

python3 - "$ROOT" "$OUT" "$PLATFORM" "$BUILD_CONFIGURATION" "$PROFILE" "$APP_FLOOR" \
  "$BUILD_ARCHS" "${TARGETS[*]}" <<'PY'
import hashlib
import json
import os
import re
import subprocess
import sys
from pathlib import Path

root, out, platform, configuration, profile, floor, archs, targets = sys.argv[1:9]
root, out = Path(root), Path(out)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(*command):
    try:
        return subprocess.run(command, cwd=root, capture_output=True, text=True,
                              check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def locked(name):
    lock = (root / "Cargo.lock").read_text()
    match = re.search(r'\[\[package\]\]\nname = "%s"\nversion = "([^"]+)"' % re.escape(name), lock)
    return match.group(1) if match else None


inputs = ["crates/mobile", "crates/sdk", "crates/rpc", "crates/proto", "Cargo.toml", "Cargo.lock"]
status = run("git", "status", "--porcelain", "--", *inputs)
library = out / "libluna_mobile.a"
identity = {
    "schema": 1,
    "platform": platform,
    "configuration": configuration,
    "cargoProfile": profile,
    "archs": archs.split(),
    "rustTargets": targets.split(),
    "deploymentTarget": floor,
    "toolchain": {"rustc": run("rustc", "-V"), "cargo": run("cargo", "-V")},
    "packages": {name: locked(name) for name in ("luna-mobile", "luna-sdk", "luna-rpc", "uniffi")},
    "source": {"commit": run("git", "rev-parse", "HEAD"),
               "coreInputsModified": None if status is None else bool(status)},
    "library": {
        "path": str(library.relative_to(root)),
        "sha256": sha256(library),
        "lipoArchs": (run("xcrun", "lipo", "-archs", str(library)) or "").split(),
        "perTarget": {target: sha256(root / "target" / target / profile / "libluna_mobile.a")
                      for target in targets.split()},
    },
    "bindings": {
        "swift": sha256(out / "gen" / "luna_core.swift"),
        "header": sha256(out / "include" / "luna_coreFFI.h"),
        "modulemap": sha256(out / "include" / "module.modulemap"),
    },
}
staged = out / "identity.json.partial"
staged.write_text(json.dumps(identity, indent=2) + "\n")
os.replace(staged, out / "identity.json")
PY
echo "built $OUT/libluna_mobile.a (${TARGETS[*]}, $BUILD_CONFIGURATION); identity $OUT/identity.json"
