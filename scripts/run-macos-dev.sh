#!/usr/bin/env bash
# Build and run an isolated macOS development bundle. --refresh is the agent's
# edit-to-app path: build while the old app runs, quit it safely, then replace
# and relaunch the one development bundle.

set -euo pipefail

MODE=run
case "${1:-}" in
  --build-only) MODE=build-only; shift ;;
  --refresh) MODE=refresh; shift ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
UPSTREAM_VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
LUNA_REVISION="${LUNA_DEV_REVISION:-1}"
DEV_ROOT="$ROOT/target/luna-macos-dev"
APP="$DEV_ROOT/LUNA Dev.app"
CONTENTS="$APP/Contents"
EXECUTABLE="$CONTENTS/MacOS/luna"
DATA_DIR="${LUNA_DEV_DATA_DIR:-$DEV_ROOT/data}"
IPC_PORT="${LUNA_DEV_IPC_PORT:-59777}"
BUILD_LOG="$DEV_ROOT/last-build.log"

# pgrep matches command lines as regular expressions. Confirm each candidate's
# exact executable path so the installed LUNA and another checkout are safe.
dev_pids() {
  local pid command_line
  while IFS= read -r pid; do
    command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    if [[ "$command_line" == "$EXECUTABLE" || "$command_line" == "$EXECUTABLE "* ]]; then
      printf '%s\n' "$pid"
    fi
  done < <(pgrep -f "$EXECUTABLE" 2>/dev/null || true)
}

if [[ "$MODE" != refresh && -n "$(dev_pids)" ]]; then
  echo "LUNA Dev is already running. Use --refresh to build and reopen it." >&2
  exit 1
fi

mkdir -p "$DEV_ROOT"
cd "$ROOT"
echo "building LUNA Dev; full output: $BUILD_LOG" >&2
build_start=$SECONDS
if cargo build --locked -p luna >"$BUILD_LOG" 2>&1; then
  echo "build succeeded in $((SECONDS - build_start))s" >&2
else
  result_code=$?
  tail -n 40 "$BUILD_LOG" >&2
  echo "build failed; the running LUNA Dev was left open ($BUILD_LOG)" >&2
  exit "$result_code"
fi

# Do the icon, plist, and signing work before asking the old app to quit.
# Staging also leaves the last working bundle intact if assembly fails.
stage_root="$(mktemp -d "$DEV_ROOT/.stage.XXXXXX")"
trap 'rm -rf "$stage_root"' EXIT
staged_app="$stage_root/LUNA Dev.app"
staged_contents="$staged_app/Contents"
mkdir -p "$staged_contents/MacOS" "$staged_contents/Resources" "$DATA_DIR"
install -m 755 "$ROOT/target/debug/luna" "$staged_contents/MacOS/luna"
sed -e "s/__UPSTREAM_VERSION__/$UPSTREAM_VERSION/g" \
  -e "s/__LUNA_REVISION__/$LUNA_REVISION/g" \
  "$ROOT/dist/macos/Info-dev.plist" >"$staged_contents/Info.plist"
plutil -replace LSEnvironment.LUNA_DATA_DIR -string "$DATA_DIR" "$staged_contents/Info.plist"
plutil -replace LSEnvironment.LUNA_IPC_PORT -string "$IPC_PORT" "$staged_contents/Info.plist"
iconutil -c icns "$ROOT/dist/macos/luna.iconset" \
  -o "$staged_contents/Resources/luna.icns"

# An Apple Development signature keeps macOS privacy permissions stable across
# rebuilds. Without one, the ad-hoc signature may trigger permission prompts.
IDENTITY="${LUNA_DEV_CODESIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
  IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development:[^"]*\)".*/\1/p' | head -1)"
fi
if [[ -n "$IDENTITY" ]]; then
  codesign --force --sign "$IDENTITY" --identifier com.novnski.luna.dev "$staged_app"
else
  codesign --force --sign - --identifier com.novnski.luna.dev "$staged_app"
  echo "warning: no Apple Development identity; macOS may ask for permissions again" >&2
fi
codesign --verify --deep --strict "$staged_app"

if [[ "$MODE" == refresh && -n "$(dev_pids)" ]]; then
  echo "asking LUNA Dev to quit after the successful build" >&2
  # AppKit initially cancels the Apple Event while LUNA checks/saves open
  # documents, then performs its own quit. osascript may report -128 even when
  # the app exits cleanly; the process exit is the authoritative result.
  quit_result=0
  osascript -e 'tell application id "com.novnski.luna.dev" to quit' >/dev/null 2>&1 || quit_result=$?
  for ((attempt = 0; attempt < 45; attempt++)); do
    [[ -z "$(dev_pids)" ]] && break
    sleep 1
  done
  if [[ -n "$(dev_pids)" ]]; then
    echo "LUNA Dev did not quit (Apple Event exit $quit_result; possibly unsaved work); bundle was left intact" >&2
    exit 1
  fi
fi

if [[ "$MODE" != build-only ]] && lsof -nP -iTCP:"$IPC_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "IPC port $IPC_PORT is in use; bundle was left intact" >&2
  exit 1
fi

# Keep one rollback copy only during the swap. Successful refreshes leave one
# LUNA Dev.app and one overwritten build log, not a series of installed apps.
backup_root="$(mktemp -d "$DEV_ROOT/.previous.XXXXXX")"
if [[ -d "$APP" ]]; then
  if ! mv "$APP" "$backup_root/LUNA Dev.app"; then
    rmdir "$backup_root"
    echo "could not move the previous LUNA Dev bundle; it was left intact" >&2
    exit 1
  fi
fi
if ! mv "$staged_app" "$APP"; then
  [[ ! -d "$backup_root/LUNA Dev.app" ]] || mv "$backup_root/LUNA Dev.app" "$APP"
  rmdir "$backup_root" 2>/dev/null || true
  echo "could not replace LUNA Dev; restored the previous bundle" >&2
  exit 1
fi

OPEN_ENV=(
  --env "LUNA_DATA_DIR=$DATA_DIR"
  --env "LUNA_IPC_PORT=$IPC_PORT"
)
if [[ -n "${LUNA_OPEN_ROUTE:-}" ]]; then
  OPEN_ENV+=(--env "LUNA_OPEN_ROUTE=$LUNA_OPEN_ROUTE")
fi

if [[ "$MODE" == build-only ]]; then
  rm -rf "$backup_root"
  echo "built LUNA Dev at $APP" >&2
  exit 0
fi

echo "launching LUNA Dev (data $DATA_DIR, IPC $IPC_PORT)" >&2
# LaunchServices must own the process so macOS privacy attributes Screen
# Recording to LUNA Dev instead of to the terminal.
if [[ "$MODE" == refresh ]]; then
  if open -n "${OPEN_ENV[@]}" "$APP" --args "$@"; then
    for ((attempt = 0; attempt < 30; attempt++)); do
      [[ -n "$(dev_pids)" ]] && break
      sleep 1
    done
  fi
  if [[ -z "$(dev_pids)" ]]; then
    mv "$APP" "$stage_root/failed.app"
    if [[ -d "$backup_root/LUNA Dev.app" ]]; then
      mv "$backup_root/LUNA Dev.app" "$APP"
    fi
    rmdir "$backup_root" 2>/dev/null || true
    echo "LUNA Dev did not launch; restored the previous bundle" >&2
    exit 1
  fi
  rm -rf "$backup_root"
  echo "LUNA Dev is running from $APP (PID $(dev_pids | paste -sd, -))" >&2
else
  rm -rf "$backup_root"
  # Keep the shell until the app exits so the stage directory is removed.
  open -W "${OPEN_ENV[@]}" "$APP" --args "$@"
fi
