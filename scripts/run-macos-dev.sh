#!/usr/bin/env bash
# Build and run an isolated macOS development bundle. Zeron.app may remain
# open: this bundle has a separate LaunchServices/TCC identity, data directory,
# and engine IPC port.

set -euo pipefail

BUILD_ONLY=false
if [[ "${1:-}" == "--build-only" ]]; then
  BUILD_ONLY=true
  shift
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
UPSTREAM_VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
LUNA_REVISION="${LUNA_DEV_REVISION:-1}"
DEV_ROOT="$ROOT/target/luna-macos-dev"
APP="$DEV_ROOT/LUNA Dev.app"
CONTENTS="$APP/Contents"
DATA_DIR="${LUNA_DEV_DATA_DIR:-$DEV_ROOT/data}"
IPC_PORT="${LUNA_DEV_IPC_PORT:-59777}"

if pgrep -f -x "$CONTENTS/MacOS/luna" >/dev/null 2>&1; then
  echo "LUNA Dev is already running. Quit it before rebuilding the signed bundle." >&2
  exit 1
fi

cd "$ROOT"
cargo build -p zeron

mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources" "$DATA_DIR"
install -m 755 "$ROOT/target/debug/luna" "$CONTENTS/MacOS/luna"
sed -e "s/__UPSTREAM_VERSION__/$UPSTREAM_VERSION/g" \
  -e "s/__LUNA_REVISION__/$LUNA_REVISION/g" \
  "$ROOT/dist/macos/Info-dev.plist" >"$CONTENTS/Info.plist"
plutil -replace LSEnvironment.LUNA_DATA_DIR -string "$DATA_DIR" "$CONTENTS/Info.plist"
plutil -replace LSEnvironment.LUNA_IPC_PORT -string "$IPC_PORT" "$CONTENTS/Info.plist"

# Rebuild on every assembly so a previously built development bundle cannot
# retain stale icon artwork.
iconutil -c icns "$ROOT/dist/macos/luna.iconset" \
  -o "$CONTENTS/Resources/luna.icns"

# A real Apple Development identity gives TCC a stable signing requirement
# across rebuilds. Set LUNA_DEV_CODESIGN_IDENTITY explicitly when more than
# one identity is installed; otherwise fall back to an ad-hoc signature.
IDENTITY="${LUNA_DEV_CODESIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
  IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development:[^"]*\)".*/\1/p' | head -1)"
fi
if [[ -n "$IDENTITY" ]]; then
  codesign --force --sign "$IDENTITY" --identifier com.novnski.luna.dev "$APP"
else
  codesign --force --sign - --identifier com.novnski.luna.dev "$APP"
  echo "warning: no Apple Development signing identity found; macOS may ask for permissions again after a rebuild" >&2
fi

echo "assembled LUNA Dev (bundle com.novnski.luna.dev, data $DATA_DIR, IPC $IPC_PORT)" >&2
if $BUILD_ONLY; then
  echo "built LUNA Dev at $APP" >&2
  exit 0
fi
if lsof -nP -iTCP:"$IPC_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "IPC port $IPC_PORT is already in use; choose an unused LUNA_DEV_IPC_PORT" >&2
  exit 1
fi
echo "launching LUNA Dev" >&2
# LaunchServices must own the process. Launching Contents/MacOS/luna directly
# makes TCC attribute Screen Recording to the terminal (Warp, Terminal, etc.).
# -W keeps the script attached until the app exits. Runtime logs remain in the
# isolated data directory (`target/luna-macos-dev/data/logs/luna-headed.log`).
OPEN_ENV=(
  --env "LUNA_DATA_DIR=$DATA_DIR"
  --env "LUNA_IPC_PORT=$IPC_PORT"
)
if [[ -n "${ZERON_OPEN_ROUTE:-}" ]]; then
  OPEN_ENV+=(--env "ZERON_OPEN_ROUTE=$ZERON_OPEN_ROUTE")
fi
exec open -W "${OPEN_ENV[@]}" "$APP" --args "$@"
