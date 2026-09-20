#!/bin/zsh
# Build/control the isolated synthetic-only L14 session-history QA bundle.

set -euo pipefail

REPO_ROOT="${0:A:h:h}"
REQUESTED_ROOT="${1:-}"
ACTION="${2:-build}"
OUT="$REPO_ROOT/target/luna-l14-fixture"
APP="$OUT/LUNA L14 QA.app"
EXECUTABLE="$APP/Contents/MacOS/session-history-fixture"
BUILD_EXECUTABLE="$REPO_ROOT/target/debug/examples/session-history-fixture"
BUNDLE_ID="com.novnski.luna.l14-qa"
REFERENCE_ICNS="${LUNA_L14_REFERENCE_ICNS:-$REPO_ROOT/target/luna-l13-fixture/LUNA L13 QA.app/Contents/Resources/luna.icns}"
PID_FILE=""
LOG_FILE=""

fail() {
  print -u2 -- "L14 fixture: $1"
  exit 1
}

[[ -n "$REQUESTED_ROOT" ]] || fail "usage: scripts/run-macos-l14-fixture.sh /tmp/luna-l14-<fresh-root> [build|run|stop|status]"
[[ "$REQUESTED_ROOT" == /* ]] || fail "synthetic root must be absolute"
[[ -d "$REQUESTED_ROOT" ]] || fail "create a fresh root with mktemp -d /tmp/luna-l14-native-qa.XXXXXX"
[[ ! -L "$REQUESTED_ROOT" ]] || fail "synthetic root must not be a symlink"
PROFILE="$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$REQUESTED_ROOT")"
case "$PROFILE" in
  /private/tmp/luna-l14-*) ;;
  *) fail "canonical synthetic root must be a narrow /private/tmp/luna-l14-* directory" ;;
esac
[[ "$(/usr/bin/stat -f '%Su' "$PROFILE")" == "$(/usr/bin/id -un)" ]] || fail "synthetic root must be owned by the current user"
/bin/chmod 700 "$PROFILE"
PID_FILE="$PROFILE/session-history-fixture.pid"
LOG_FILE="$PROFILE/session-history-fixture.log"

expected_command() {
  print -r -- "$EXECUTABLE $PROFILE"
}

exact_pid() {
  local pid command
  while read -r pid command; do
    [[ -n "$pid" ]] || continue
    if [[ "$command" == "$(expected_command)" ]]; then
      print -r -- "$pid"
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

validate_bundle() {
  local plist="$APP/Contents/Info.plist"
  [[ -x "$EXECUTABLE" ]] || fail "bundle executable missing"
  /usr/bin/file "$EXECUTABLE" | /usr/bin/grep -Fq "Mach-O" || fail "fixture executable is not Mach-O"
  /usr/bin/plutil -lint "$plist" >/dev/null || fail "invalid Info.plist"
  [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist")" == "$BUNDLE_ID" ]] || fail "wrong bundle identifier"
  [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")" == "session-history-fixture" ]] || fail "wrong executable"
  /usr/bin/codesign --verify --deep --strict "$APP" || fail "signature verification failed"
}

build_bundle() {
  local stage stage_app contents old_app
  cd "$REPO_ROOT"
  [[ -s "$REFERENCE_ICNS" ]] || fail "validated L13 icon ancestor is unavailable at $REFERENCE_ICNS"
  cargo build --locked -p zeron-ui --example session-history-fixture --features appshots-fixture
  /bin/mkdir -p "$OUT"
  stage="$(/usr/bin/mktemp -d "$OUT/.luna-l14-stage.XXXXXX")"
  stage_app="$stage/LUNA L14 QA.app"
  contents="$stage_app/Contents"
  /bin/mkdir -p "$contents/MacOS" "$contents/Resources"
  /usr/bin/install -m 755 "$BUILD_EXECUTABLE" "$contents/MacOS/session-history-fixture"
  /usr/bin/install -m 644 "$REPO_ROOT/dist/macos/Info-dev.plist" "$contents/Info.plist"
  /usr/bin/install -m 644 "$REFERENCE_ICNS" "$contents/Resources/luna.icns"
  /usr/bin/plutil -replace CFBundleDisplayName -string "LUNA L14 QA" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleExecutable -string "session-history-fixture" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleName -string "LUNA L14 QA" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleShortVersionString -string "0.2.73-l14-qa" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleVersion -string "1" "$contents/Info.plist"
  /usr/bin/plutil -remove CFBundleURLTypes "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/plutil -remove LSEnvironment "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/codesign --force --sign - --identifier "$BUNDLE_ID" "$stage_app"
  old_app="$OUT/.previous-luna-l14-qa.app"
  [[ ! -e "$old_app" ]] || /bin/rm -rf -- "$old_app"
  [[ ! -e "$APP" ]] || /bin/mv -- "$APP" "$old_app"
  if ! /bin/mv -- "$stage_app" "$APP"; then
    [[ ! -e "$old_app" ]] || /bin/mv -- "$old_app" "$APP"
    fail "could not install staged fixture bundle"
  fi
  [[ ! -e "$old_app" ]] || /bin/rm -rf -- "$old_app"
  /bin/rm -rf -- "$stage"
  validate_bundle
  print -- "bundle=$APP"
  print -- "identifier=$BUNDLE_ID"
  print -- "executable_sha256=$(/usr/bin/shasum -a 256 "$EXECUTABLE" | /usr/bin/awk '{print $1}')"
  print -- "safety=in-memory auth; synthetic root-scoped provider homes; native provider metadata and Keychain fallback disabled"
}

case "$ACTION" in
  build)
    [[ -z "$(exact_pid || true)" ]] || fail "fixture is running; stop it before replacing the bundle"
    build_bundle
    ;;
  run)
    validate_bundle
    [[ -z "$(exact_pid || true)" ]] || fail "fixture is already running"
    [[ -z "$(/usr/bin/find "$PROFILE" -mindepth 1 -maxdepth 1 -print -quit)" ]] || fail "first run requires a fresh empty synthetic root"
    /usr/bin/open -n --env LUNA_DISABLE_SOUND=1 --env LUNA_DISABLE_NOTIFICATIONS=1 --stdout "$LOG_FILE" --stderr "$LOG_FILE" "$APP" --args "$PROFILE"
    fixture_pid=""
    for _ in {1..100}; do
      fixture_pid="$(exact_pid || true)"
      if [[ -n "$fixture_pid" ]] && validate_pid "$fixture_pid" && [[ -s "$PROFILE/SAFETY.txt" && -s "$PROFILE/FIXTURE.json" ]]; then
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
    validate_pid "$fixture_pid" || fail "pid file does not identify the exact L14 fixture/root"
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
      validate_pid "$fixture_pid" || fail "pid file does not identify the exact L14 fixture/root"
      print -- "running pid=$fixture_pid bundle=$APP identifier=$BUNDLE_ID root=$PROFILE"
    else
      [[ -z "$(exact_pid || true)" ]] || fail "unmanaged exact fixture process is running"
      print -- "not running bundle=$APP identifier=$BUNDLE_ID root=$PROFILE"
    fi
    ;;
  *) fail "unknown action: $ACTION" ;;
esac
