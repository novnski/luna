#!/bin/zsh
# Build/control the isolated synthetic-only L13 native-history QA bundle.

set -euo pipefail

ROOT="${0:A:h:h}"
REQUESTED_PROFILE="${1:-}"
ACTION="${2:-run}"
OUT="$ROOT/target/luna-l13-fixture"
APP="$OUT/LUNA L13 QA.app"
EXECUTABLE="$APP/Contents/MacOS/history-operations-fixture"
BUILD_EXECUTABLE="$ROOT/target/debug/examples/history-operations-fixture"
BUNDLE_ID="com.novnski.luna.l13-qa"
DISPLAY_NAME="LUNA L13 QA"
REFERENCE_ICNS="${LUNA_L13_REFERENCE_ICNS:-$ROOT/target/luna-l11-fixture/LUNA L11 QA.app/Contents/Resources/luna.icns}"
PID_FILE=""
LOG_FILE=""
ROOT_MARKER=""

fail() {
  print -u2 -- "L13 fixture: $1"
  exit 1
}

[[ -n "$REQUESTED_PROFILE" ]] || fail "usage: scripts/run-macos-l13-fixture.sh /tmp/luna-l13-<fresh-root> [build|run|stop|status]"
[[ "$REQUESTED_PROFILE" == /* ]] || fail "synthetic root must be absolute"
[[ -d "$REQUESTED_PROFILE" ]] || fail "create a fresh root with mktemp -d /tmp/luna-l13-native-qa.XXXXXX"
[[ ! -L "$REQUESTED_PROFILE" ]] || fail "synthetic root must not be a symlink"
PROFILE="$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$REQUESTED_PROFILE")"
case "$PROFILE" in
  /private/tmp/luna-l13-*) ;;
  *) fail "canonical synthetic root must be a narrow /private/tmp/luna-l13-* directory" ;;
esac
[[ "$(/usr/bin/stat -f '%Su' "$PROFILE")" == "$(/usr/bin/id -un)" ]] || fail "synthetic root must be owned by the current user"
/bin/chmod 700 "$PROFILE"
PID_FILE="$PROFILE/history-operations-fixture.pid"
LOG_FILE="$PROFILE/history-operations-fixture.log"
ROOT_MARKER="$PROFILE/.luna-l13-qa-root"

plist_value() {
  /usr/libexec/PlistBuddy -c "Print :$2" "$1"
}

validate_bundle() {
  local bundle="$1"
  local contents="$bundle/Contents"
  local plist="$contents/Info.plist"
  [[ -x "$contents/MacOS/history-operations-fixture" ]] || fail "bundle executable missing"
  /usr/bin/file "$contents/MacOS/history-operations-fixture" | /usr/bin/grep -Fq "Mach-O" || fail "fixture executable is not Mach-O"
  /usr/bin/plutil -lint "$plist" >/dev/null || fail "invalid Info.plist"
  [[ "$(plist_value "$plist" CFBundleDisplayName)" == "$DISPLAY_NAME" ]] || fail "wrong display name"
  [[ "$(plist_value "$plist" CFBundleExecutable)" == "history-operations-fixture" ]] || fail "wrong executable name"
  [[ "$(plist_value "$plist" CFBundleIdentifier)" == "$BUNDLE_ID" ]] || fail "wrong bundle id"
  [[ "$(plist_value "$plist" CFBundleShortVersionString)" == "0.2.73-l13-qa" ]] || fail "wrong fixture version"
  [[ -s "$contents/Resources/luna.icns" ]] || fail "bundle icon missing"
  /usr/bin/cmp -s "$REFERENCE_ICNS" "$contents/Resources/luna.icns" || fail "bundle icon differs from the validated LUNA ancestor"
  /usr/bin/codesign --verify --deep --strict "$bundle" || fail "signature verification failed"
}

expected_command() {
  print -r -- "$EXECUTABLE $PROFILE"
}

exact_pid() {
  local candidate_pid candidate_command
  while read -r candidate_pid candidate_command; do
    [[ -n "$candidate_pid" ]] || continue
    if [[ "$candidate_command" == "$(expected_command)" ]]; then
      print -r -- "$candidate_pid"
      return 0
    fi
  done <<<"$(/bin/ps -axo pid=,command=)"
  return 1
}

validate_pid() {
  [[ "$1" == <-> ]] || return 1
  /bin/kill -0 "$1" 2>/dev/null || return 1
  [[ "$(/bin/ps -p "$1" -o command= 2>/dev/null)" == "$(expected_command)" ]]
}

build_bundle() {
  local stage contents
  if exact_pid >/dev/null; then
    fail "refusing to replace a running L13 fixture"
  fi
  cd "$ROOT"
  [[ -s "$REFERENCE_ICNS" ]] || fail "validated LUNA icon ancestor is unavailable at $REFERENCE_ICNS"
  /usr/bin/file "$REFERENCE_ICNS" | /usr/bin/grep -Fq "Mac OS X icon" || fail "reference icon is not an icns container"
  cargo build --locked -p zeron-ui --example history-operations-fixture --features appshots-fixture
  /bin/mkdir -p "$OUT"
  stage="$(/usr/bin/mktemp -d "$OUT/.luna-l13-stage.XXXXXX")"
  trap '/bin/rm -rf -- "$stage"' EXIT
  contents="$stage/LUNA L13 QA.app/Contents"
  /bin/mkdir -p "$contents/MacOS" "$contents/Resources"
  /usr/bin/install -m 755 "$BUILD_EXECUTABLE" "$contents/MacOS/history-operations-fixture"
  /usr/bin/install -m 644 "$ROOT/dist/macos/Info-dev.plist" "$contents/Info.plist"
  /usr/bin/install -m 644 "$REFERENCE_ICNS" "$contents/Resources/luna.icns"
  /usr/bin/plutil -replace CFBundleDisplayName -string "$DISPLAY_NAME" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleExecutable -string "history-operations-fixture" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleName -string "$DISPLAY_NAME" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleShortVersionString -string "0.2.73-l13-qa" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleVersion -string "1" "$contents/Info.plist"
  /usr/bin/plutil -remove CFBundleURLTypes "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/plutil -remove LSEnvironment "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/codesign --force --sign - --identifier "$BUNDLE_ID" "$stage/LUNA L13 QA.app"
  validate_bundle "$stage/LUNA L13 QA.app"
  if [[ -e "$APP" ]]; then
    /bin/mv -- "$APP" "$OUT/LUNA L13 QA.previous.app"
  fi
  /bin/mv -- "$stage/LUNA L13 QA.app" "$APP"
  /bin/rm -rf -- "$OUT/LUNA L13 QA.previous.app"
  validate_bundle "$APP"
  trap - EXIT
  /bin/rm -rf -- "$stage"
  print -- "bundle=$APP"
  print -- "identifier=$BUNDLE_ID"
  print -- "executable_sha256=$(/usr/bin/shasum -a 256 "$EXECUTABLE" | /usr/bin/awk '{print $1}')"
  print -- "safety=synthetic in-memory provider/auth; no live provider, account, Keychain, network store, or repository"
}

case "$ACTION" in
  build)
    build_bundle
    ;;
  run)
    validate_bundle "$APP"
    if [[ -f "$PID_FILE" ]]; then
      fixture_pid="$(<"$PID_FILE")"
      validate_pid "$fixture_pid" && fail "already running as pid $fixture_pid"
      fail "stale pid file requires inspection"
    fi
    if fixture_pid="$(exact_pid)"; then
      fail "already running as unmanaged pid $fixture_pid"
    fi
    if [[ -e "$ROOT_MARKER" ]]; then
      [[ "$(<"$ROOT_MARKER")" == "$BUNDLE_ID" ]] || fail "synthetic root belongs to another fixture"
    else
      [[ -z "$(/usr/bin/find "$PROFILE" -mindepth 1 -maxdepth 1 -print -quit)" ]] || fail "first run requires a fresh empty root"
      print -r -- "$BUNDLE_ID" >"$ROOT_MARKER"
    fi
    /usr/bin/open -n --env LUNA_DISABLE_SOUND=1 --env LUNA_DISABLE_NOTIFICATIONS=1 --stdout "$LOG_FILE" --stderr "$LOG_FILE" "$APP" --args "$PROFILE"
    fixture_pid=""
    for _ in {1..100}; do
      fixture_pid="$(exact_pid || true)"
      if [[ -n "$fixture_pid" ]] && validate_pid "$fixture_pid" && [[ -s "$PROFILE/SAFETY.txt" ]]; then
        print -r -- "$fixture_pid" >"$PID_FILE"
        print -- "pid=$fixture_pid bundle=$APP identifier=$BUNDLE_ID root=$PROFILE log=$LOG_FILE"
        exit 0
      fi
      /bin/sleep 0.1
    done
    [[ -z "$fixture_pid" ]] || /bin/kill "$fixture_pid"
    fail "fixture did not reach exact-process safety readback"
    ;;
  stop)
    [[ -f "$PID_FILE" ]] || { print -- "not running"; exit 0; }
    fixture_pid="$(<"$PID_FILE")"
    validate_pid "$fixture_pid" || fail "pid file does not identify the exact L13 fixture/root"
    /bin/kill "$fixture_pid"
    for _ in {1..100}; do
      if ! /bin/kill -0 "$fixture_pid" 2>/dev/null; then
        /bin/rm -f "$PID_FILE"
        print -- "stopped pid=$fixture_pid"
        exit 0
      fi
      /bin/sleep 0.1
    done
    fail "fixture did not stop after SIGTERM"
    ;;
  status)
    if [[ -f "$PID_FILE" ]]; then
      fixture_pid="$(<"$PID_FILE")"
      validate_pid "$fixture_pid" || fail "pid file does not identify the exact L13 fixture/root"
      print -- "running pid=$fixture_pid bundle=$APP identifier=$BUNDLE_ID root=$PROFILE"
    elif fixture_pid="$(exact_pid)"; then
      fail "exact fixture is running without its pid file as pid $fixture_pid"
    else
      print -- "not running"
    fi
    ;;
  *) fail "unknown action: $ACTION" ;;
esac
