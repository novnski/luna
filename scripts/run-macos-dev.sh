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
APP_NAME="${LUNA_DEV_APP_NAME-LUNA Dev}"
BUNDLE_ID="${LUNA_DEV_BUNDLE_ID-com.novnski.luna.dev}"
APP="$DEV_ROOT/$APP_NAME.app"
CONTENTS="$APP/Contents"
EXECUTABLE="$CONTENTS/MacOS/luna"
DATA_DIR="${LUNA_DEV_DATA_DIR:-$DEV_ROOT/data}"
IPC_PORT="${LUNA_DEV_IPC_PORT:-59777}"
BUILD_LOG="$DEV_ROOT/last-build.log"

# Validate before build, mkdir, signing, or any Apple Event. QA identities are
# deliberately constrained and all four isolation settings travel together.
DATA_DIR="$(python3 - "$ROOT" "$APP_NAME" "$BUNDLE_ID" "$DATA_DIR" "$IPC_PORT" "$LUNA_REVISION" <<'PY'
import os, pathlib, plistlib, re, subprocess, sys
root = pathlib.Path(sys.argv[1]).resolve()
name, bundle, data, port, revision = sys.argv[2:]
def reject(message):
    sys.exit('Invalid dev app configuration: ' + message)
if not re.fullmatch(r'[1-9][0-9]{0,4}', port) or not 1024 <= int(port) <= 65535:
    reject('IPC port must be an integer from 1024 to 65535 without leading zeros')
if not re.fullmatch(r'[1-9][0-9]{0,8}', revision):
    reject('revision must be a positive integer')
if any(c in data for c in '\n\r') or not pathlib.Path(data).is_absolute():
    reject('data directory must be an absolute single-line path')
target = root / 'target'
dev = target / 'luna-macos-dev'
if target.is_symlink() or dev.resolve() != dev:
    reject('target and bundle directory must not redirect through symlinks')
data = pathlib.Path(data).resolve()
custom = name != 'LUNA Dev' or bundle != 'com.novnski.luna.dev'
if custom:
    if not re.fullmatch(r'LUNA QA [A-Za-z0-9][A-Za-z0-9 -]{0,48}', name) or name.endswith(' '):
        reject('custom app name must be LUNA QA followed by a safe, nonempty label')
    if not re.fullmatch(r'com\.novnski\.luna\.qa\.[a-z][a-z0-9-]{0,47}', bundle):
        reject('custom bundle ID must be com.novnski.luna.qa.<unique-lowercase-slug>')
    if not os.environ.get('LUNA_DEV_DATA_DIR') or target not in data.parents or data == dev / 'data':
        reject('QA requires an explicit, separate data directory within this checkout target/')
    if data == dev or dev in data.parents:
        reject('QA data must live outside the bundle staging directory')
    if not os.environ.get('LUNA_DEV_IPC_PORT') or port == '59777':
        reject('QA requires an explicit IPC port other than main port 59777')
app = dev / (name + '.app')
if app.is_symlink() or app in data.parents or data == app:
    reject('bundle must not be a symlink or contain the data directory')
# Detect collisions against already packaged apps in every linked checkout.
listing = subprocess.check_output(['git', '-C', str(root), 'worktree', 'list', '--porcelain', '-z'])
for item in listing.split(b'\0'):
    if not item.startswith(b'worktree '):
        continue
    other_root = pathlib.Path(os.fsdecode(item[9:])) / 'target/luna-macos-dev'
    for info in other_root.glob('*.app/Contents/Info.plist'):
        if info.parent.parent == app:
            continue
        with info.open('rb') as f:
            other = plistlib.load(f)
        env = other.get('LSEnvironment', {})
        if (other.get('CFBundleIdentifier') == bundle or other.get('CFBundleDisplayName') == name
                or env.get('LUNA_IPC_PORT') == port
                or (env.get('LUNA_DATA_DIR') and pathlib.Path(env['LUNA_DATA_DIR']).resolve() == data)):
            reject('identity, IPC or data directory conflicts with ' + str(info.parent.parent))
print(data)
PY
)" || exit 2

# Two refreshes must never swap the same bundle concurrently. A stale directory
# is deliberately not auto-removed; inspect its owner before manual recovery.
mkdir -p "$DEV_ROOT"
refresh_lock="$DEV_ROOT/.refresh-lock"
if ! mkdir "$refresh_lock" 2>/dev/null; then
  echo "Another app operation owns $refresh_lock; inspect its pid before retrying" >&2
  exit 75
fi
echo "$$" > "$refresh_lock/pid"
stage_root=""
cleanup() {
  [[ -z "$stage_root" ]] || rm -rf -- "$stage_root"
  rm -f "$refresh_lock/pid"
  rmdir "$refresh_lock"
}
trap cleanup EXIT

# pgrep matches command lines as regular expressions. Confirm each candidate's
# exact executable path so the installed LUNA and another checkout are safe.
dev_pids() {
  local pid command_line
  while IFS= read -r pid; do
    command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
    if [[ "$command_line" == "$EXECUTABLE" || "$command_line" == "$EXECUTABLE "* ]]; then
      printf '%s\n' "$pid"
    fi
  done < <(pgrep -f '/Contents/MacOS/luna' 2>/dev/null || true)
}

if [[ "$MODE" != refresh && -n "$(dev_pids)" ]]; then
  echo "$APP_NAME is already running. Use --refresh to build and reopen it." >&2
  exit 1
fi

cd "$ROOT"
echo "building $APP_NAME ($BUNDLE_ID); full output: $BUILD_LOG" >&2
build_start=$SECONDS
if "$ROOT/scripts/cargo.sh" build --locked -p luna --target-dir "$ROOT/target" >"$BUILD_LOG" 2>&1; then
  echo "build succeeded in $((SECONDS - build_start))s" >&2
else
  result_code=$?
  tail -n 40 "$BUILD_LOG" >&2
  echo "build failed; the running $APP_NAME was left open ($BUILD_LOG)" >&2
  exit "$result_code"
fi

# Do the icon, plist, and signing work before asking the old app to quit.
# Staging also leaves the last working bundle intact if assembly fails.
stage_root="$(mktemp -d "$DEV_ROOT/.stage.XXXXXX")"
staged_app="$stage_root/$APP_NAME.app"
staged_contents="$staged_app/Contents"
mkdir -p "$staged_contents/MacOS" "$staged_contents/Resources" "$DATA_DIR"
install -m 755 "$ROOT/target/debug/luna" "$staged_contents/MacOS/luna"
sed -e "s/__UPSTREAM_VERSION__/$UPSTREAM_VERSION/g" \
  -e "s/__LUNA_REVISION__/$LUNA_REVISION/g" \
  "$ROOT/dist/macos/Info-dev.plist" >"$staged_contents/Info.plist"
plutil -replace LSEnvironment.LUNA_DATA_DIR -string "$DATA_DIR" "$staged_contents/Info.plist"
plutil -replace LSEnvironment.LUNA_IPC_PORT -string "$IPC_PORT" "$staged_contents/Info.plist"
plutil -replace CFBundleDisplayName -string "$APP_NAME" "$staged_contents/Info.plist"
plutil -replace CFBundleName -string "$APP_NAME" "$staged_contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$staged_contents/Info.plist"
if [[ "$BUNDLE_ID" != com.novnski.luna.dev ]]; then
  # QA launches must not register main/release deep-link schemes.
  plutil -remove CFBundleURLTypes "$staged_contents/Info.plist"
fi
iconutil -c icns "$ROOT/dist/macos/luna.iconset" \
  -o "$staged_contents/Resources/luna.icns"

# An Apple Development signature keeps macOS privacy permissions stable across
# rebuilds. Without one, the ad-hoc signature may trigger permission prompts.
IDENTITY="${LUNA_DEV_CODESIGN_IDENTITY:-}"
if [[ -z "$IDENTITY" ]]; then
  IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null | sed -n 's/.*"\(Apple Development:[^"]*\)".*/\1/p' | head -1)"
fi
if [[ -n "$IDENTITY" ]]; then
  codesign --force --sign "$IDENTITY" --identifier "$BUNDLE_ID" "$staged_app"
else
  codesign --force --sign - --identifier "$BUNDLE_ID" "$staged_app"
  echo "warning: no Apple Development identity; macOS may ask for permissions again" >&2
fi
codesign --verify --deep --strict "$staged_app"

if [[ "$MODE" == refresh && -n "$(dev_pids)" ]]; then
  echo "asking $APP_NAME to quit after the successful build" >&2
  # AppKit initially cancels the Apple Event while LUNA checks/saves open
  # documents, then performs its own quit. osascript may report -128 even when
  # the app exits cleanly; the process exit is the authoritative result.
  quit_result=0
  # NSRunningApplication targets the verified PID, avoiding LaunchServices'
  # ambiguous application-id lookup when an old duplicate bundle is registered.
  while IFS= read -r pid; do
    osascript -l JavaScript -e 'ObjC.import("AppKit"); function run(argv) {
      const app = $.NSRunningApplication.runningApplicationWithProcessIdentifier(Number(argv[0]));
      if (!app.isNil() && ObjC.unwrap(app.executableURL.path) === argv[1]) app.terminate;
    }' "$pid" "$EXECUTABLE" >/dev/null 2>&1 || quit_result=$?
  done < <(dev_pids)
  for ((attempt = 0; attempt < 45; attempt++)); do
    [[ -z "$(dev_pids)" ]] && break
    sleep 1
  done
  if [[ -n "$(dev_pids)" ]]; then
    echo "$APP_NAME did not quit (Apple Event exit $quit_result; possibly unsaved work); bundle was left intact" >&2
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
  if ! mv "$APP" "$backup_root/$APP_NAME.app"; then
    rmdir "$backup_root"
    echo "could not move the previous $APP_NAME bundle; it was left intact" >&2
    exit 1
  fi
fi
if ! mv "$staged_app" "$APP"; then
  [[ ! -d "$backup_root/$APP_NAME.app" ]] || mv "$backup_root/$APP_NAME.app" "$APP"
  rmdir "$backup_root" 2>/dev/null || true
  echo "could not replace $APP_NAME; restored the previous bundle" >&2
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
  echo "built $APP_NAME at $APP" >&2
  exit 0
fi

echo "launching $APP_NAME (data $DATA_DIR, IPC $IPC_PORT)" >&2
# LaunchServices must own the process so macOS privacy attributes Screen
# Recording to LUNA Dev instead of to the terminal.
if [[ "$MODE" == refresh ]]; then
  if open -g -n "${OPEN_ENV[@]}" "$APP" --args "$@"; then
    for ((attempt = 0; attempt < 30; attempt++)); do
      [[ -n "$(dev_pids)" ]] && break
      sleep 1
    done
  fi
  if [[ -z "$(dev_pids)" ]]; then
    mv "$APP" "$stage_root/failed.app"
    if [[ -d "$backup_root/$APP_NAME.app" ]]; then
      mv "$backup_root/$APP_NAME.app" "$APP"
    fi
    rmdir "$backup_root" 2>/dev/null || true
    echo "$APP_NAME did not launch; restored the previous bundle" >&2
    exit 1
  fi
  rm -rf "$backup_root"
  echo "$APP_NAME is running from $APP (PID $(dev_pids | paste -sd, -))" >&2
else
  rm -rf "$backup_root"
  # Keep the shell until the app exits so the stage directory is removed.
  open -W "${OPEN_ENV[@]}" "$APP" --args "$@"
fi
