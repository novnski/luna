#!/usr/bin/env bash
# Build and launch the isolated, mock-only L10 native QA bundle.
#
# The bundle has its own LaunchServices identity and receives an explicit
# synthetic profile root. drafts-fixture disables native credential fallbacks
# and places every provider home below that root.

set -euo pipefail

BUILD_ONLY=false
if [[ "${1:-}" == "--build-only" ]]; then
  BUILD_ONLY=true
  shift
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="$ROOT/target/luna-l10-fixture"
APP="$OUT/LUNA L10 QA.app"
CONTENTS="$APP/Contents"
PROFILE="${LUNA_L10_FIXTURE_ROOT:-$OUT/profile}"
EXECUTABLE="$CONTENTS/MacOS/drafts-fixture"

if pgrep -f -x "$EXECUTABLE" >/dev/null 2>&1; then
  echo "LUNA L10 QA is already running. Quit it before rebuilding the bundle." >&2
  exit 1
fi

cd "$ROOT"
cargo build -p zeron-ui --example drafts-fixture --features appshots-fixture

rm -rf "$APP"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources" "$PROFILE"
install -m 755 "$ROOT/target/debug/examples/drafts-fixture" "$EXECUTABLE"
install -m 644 "$ROOT/dist/macos/Info-dev.plist" "$CONTENTS/Info.plist"
plutil -replace CFBundleDisplayName -string "LUNA L10 QA" "$CONTENTS/Info.plist"
plutil -replace CFBundleExecutable -string "drafts-fixture" "$CONTENTS/Info.plist"
plutil -replace CFBundleIdentifier -string "com.novnski.luna.l10-qa" "$CONTENTS/Info.plist"
plutil -replace CFBundleName -string "LUNA L10 QA" "$CONTENTS/Info.plist"
plutil -replace CFBundleShortVersionString -string "0.2.73-l10-qa" "$CONTENTS/Info.plist"
plutil -replace CFBundleVersion -string "1" "$CONTENTS/Info.plist"
plutil -remove CFBundleURLTypes "$CONTENTS/Info.plist"
plutil -remove LSEnvironment "$CONTENTS/Info.plist"
iconutil -c icns "$ROOT/dist/macos/luna.iconset" \
  -o "$CONTENTS/Resources/luna.icns"
codesign --force --sign - --identifier com.novnski.luna.l10-qa "$APP"

if $BUILD_ONLY; then
  echo "Built mock-only LUNA L10 QA at $APP" >&2
  echo "Profile: $PROFILE" >&2
  exit 0
fi

echo "Launching mock-only LUNA L10 QA" >&2
echo "Profile: $PROFILE" >&2
echo "Control: scripts/l10-fixture-control.sh '$PROFILE' subscription <reset-at-ms>" >&2
exec open -W "$APP" --args "$PROFILE" turn-recovery
