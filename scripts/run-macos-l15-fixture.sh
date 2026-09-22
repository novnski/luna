#!/bin/zsh
# Build/control the audited synthetic-only L15 Activity native QA bundle.

set -euo pipefail

REPO_ROOT="${0:A:h:h}"
REQUESTED_ROOT="${1:-}"
ACTION="${2:-build}"
OUT="$REPO_ROOT/target/luna-l15-fixture"
APP="$OUT/LUNA L15 QA.app"
EXECUTABLE="$APP/Contents/MacOS/activity-attention-fixture"
BUILD_EXECUTABLE="$REPO_ROOT/target/debug/examples/activity-attention-fixture"
BUNDLE_ID="com.novnski.luna.l15-qa"
FIXTURE_ID="luna-l15-activity-attention-v1"
EXPECTED_SAFETY="L15 synthetic fixture only. In-memory auth; fake power; sound and OS notifications disabled; native provider metadata and platform Keychain fallbacks disabled. No live provider/account/private store access."
REFERENCE_ICNS="${LUNA_L15_REFERENCE_ICNS:-$REPO_ROOT/target/luna-l13-fixture/LUNA L13 QA.app/Contents/Resources/luna.icns}"

fail() {
  print -u2 -- "L15 fixture: $1"
  exit 1
}

[[ -n "$REQUESTED_ROOT" ]] || fail "usage: scripts/run-macos-l15-fixture.sh /tmp/luna-l15-<root> [build|run|resume|resume-check|stop|status]"
[[ "$REQUESTED_ROOT" == /* ]] || fail "synthetic root must be absolute"
[[ -d "$REQUESTED_ROOT" && ! -L "$REQUESTED_ROOT" ]] || fail "synthetic root must exist and not be linked"
PROFILE="$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$REQUESTED_ROOT")"
case "$PROFILE" in
  /private/tmp/luna-l15-*) ;;
  *) fail "canonical root must be a narrow /private/tmp/luna-l15-* directory" ;;
esac
[[ "$(/usr/bin/stat -f '%Su' "$PROFILE")" == "$(/usr/bin/id -un)" ]] || fail "synthetic root must be owned by the current user"
/bin/chmod 700 "$PROFILE"
PID_FILE="$PROFILE/activity-attention-fixture.pid"
LOG_FILE="$PROFILE/activity-attention-fixture.log"

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
  [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$plist")" == "activity-attention-fixture" ]] || fail "wrong executable"
  /usr/bin/codesign --verify --deep --strict "$APP" || fail "signature verification failed"
}

validate_saved_root() {
  local fixture="$PROFILE/FIXTURE.json"
  local safety="$PROFILE/SAFETY.txt"
  local database="$PROFILE/engine/orgs/l15-fixture-org/l15-fixture-user/docs.sqlite3"
  [[ -f "$fixture" && ! -L "$fixture" ]] || fail "saved root is missing regular FIXTURE.json"
  [[ -f "$safety" && ! -L "$safety" ]] || fail "saved root is missing regular SAFETY.txt"
  [[ "$(<"$safety")" == "$EXPECTED_SAFETY" ]] || fail "saved root safety statement differs"
  [[ -d "$PROFILE/engine" && ! -L "$PROFILE/engine" ]] || fail "saved engine directory is missing or linked"
  [[ -d "$PROFILE/ui" && ! -L "$PROFILE/ui" ]] || fail "saved UI directory is missing or linked"
  [[ -s "$database" && ! -L "$database" ]] || fail "saved database is missing, empty, or linked"
  /usr/bin/python3 -c '
import json, os, sqlite3, sys
fixture_path, root, fixture_id, bundle_id, database = sys.argv[1:]
with open(fixture_path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
assert data.get("schemaVersion") == 1
assert data.get("fixtureId") == fixture_id
assert data.get("bundleIdentifier") == bundle_id
assert os.path.realpath(data.get("root", "")) == root
assert data.get("tasks") == [
    "15151515-1515-4151-8151-151515151515",
    "25252525-2525-4252-8252-252525252525",
    "35353535-3535-4353-8353-353535353535",
    "45454545-4545-4454-8454-454545454545",
]
for directory, names, files in os.walk(root, followlinks=False):
    for name in names + files:
        assert not os.path.islink(os.path.join(directory, name))
assert os.path.realpath(database).startswith(root + os.sep)
connection = sqlite3.connect(database)
assert connection.execute("PRAGMA quick_check").fetchone()[0] == "ok"
' "$fixture" "$PROFILE" "$FIXTURE_ID" "$BUNDLE_ID" "$database" || fail "saved root identity or database validation failed"
}

launch_fixture() {
  /usr/bin/open -n --env LUNA_DISABLE_SOUND=1 --env LUNA_DISABLE_NOTIFICATIONS=1 --env LUNA_FIXTURE_FAKE_POWER=1 --stdout "$LOG_FILE" --stderr "$LOG_FILE" "$APP" --args "$PROFILE"
  local fixture_pid=""
  for _ in {1..100}; do
    fixture_pid="$(exact_pid || true)"
    if [[ -n "$fixture_pid" ]] && validate_pid "$fixture_pid" && [[ -s "$PROFILE/SAFETY.txt" && -s "$PROFILE/FIXTURE.json" ]]; then
      print -r -- "$fixture_pid" >"$PID_FILE"
      print -- "pid=$fixture_pid bundle=$APP identifier=$BUNDLE_ID root=$PROFILE log=$LOG_FILE"
      return 0
    fi
    /bin/sleep 0.1
  done
  [[ -z "$fixture_pid" ]] || /bin/kill "$fixture_pid"
  fail "fixture did not reach exact-process safety readback"
}

build_bundle() {
  local stage stage_app contents old_app
  cd "$REPO_ROOT"
  [[ -s "$REFERENCE_ICNS" ]] || fail "validated L13 icon ancestor is unavailable at $REFERENCE_ICNS"
  cargo build --locked -p zeron-ui --example activity-attention-fixture --features appshots-fixture
  /bin/mkdir -p "$OUT"
  stage="$(/usr/bin/mktemp -d "$OUT/.luna-l15-stage.XXXXXX")"
  stage_app="$stage/LUNA L15 QA.app"
  contents="$stage_app/Contents"
  /bin/mkdir -p "$contents/MacOS" "$contents/Resources"
  /usr/bin/install -m 755 "$BUILD_EXECUTABLE" "$contents/MacOS/activity-attention-fixture"
  /usr/bin/install -m 644 "$REPO_ROOT/dist/macos/Info-dev.plist" "$contents/Info.plist"
  /usr/bin/install -m 644 "$REFERENCE_ICNS" "$contents/Resources/luna.icns"
  /usr/bin/plutil -replace CFBundleDisplayName -string "LUNA L15 QA" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleExecutable -string "activity-attention-fixture" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleName -string "LUNA L15 QA" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleShortVersionString -string "0.2.73-l15-qa" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleVersion -string "1" "$contents/Info.plist"
  /usr/bin/plutil -remove CFBundleURLTypes "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/plutil -remove LSEnvironment "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/codesign --force --sign - --identifier "$BUNDLE_ID" "$stage_app"
  old_app="$OUT/.previous-luna-l15-qa.app"
  [[ ! -e "$old_app" ]] || fail "unreviewed previous staging bundle exists"
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
  print -- "safety=in-memory auth; synthetic provider homes; fake power; OS sound/notifications disabled; native metadata/Keychain fallback disabled"
}

case "$ACTION" in
  build)
    [[ -z "$(exact_pid || true)" ]] || fail "fixture is running; stop it before replacing the bundle"
    build_bundle
    ;;
  run)
    validate_bundle
    [[ -z "$(exact_pid || true)" ]] || fail "fixture is already running"
    [[ -z "$(/usr/bin/find "$PROFILE" -mindepth 1 -maxdepth 1 -print -quit)" ]] || fail "first run requires an empty synthetic root"
    launch_fixture
    ;;
  resume-check)
    validate_bundle
    [[ -z "$(exact_pid || true)" ]] || fail "fixture is already running"
    validate_saved_root
    print -- "resumable bundle=$APP identifier=$BUNDLE_ID root=$PROFILE"
    ;;
  resume)
    validate_bundle
    [[ -z "$(exact_pid || true)" ]] || fail "fixture is already running"
    validate_saved_root
    launch_fixture
    ;;
  stop)
    [[ -f "$PID_FILE" ]] || { print -- "not running"; exit 0; }
    fixture_pid="$(<"$PID_FILE")"
    validate_pid "$fixture_pid" || fail "pid file does not identify the exact L15 fixture/root"
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
      validate_pid "$fixture_pid" || fail "pid file does not identify the exact L15 fixture/root"
      print -- "running pid=$fixture_pid bundle=$APP identifier=$BUNDLE_ID root=$PROFILE"
    else
      [[ -z "$(exact_pid || true)" ]] || fail "unmanaged exact fixture process is running"
      print -- "not running bundle=$APP identifier=$BUNDLE_ID root=$PROFILE"
    fi
    ;;
  *) fail "unknown action: $ACTION" ;;
esac
