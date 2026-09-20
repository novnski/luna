#!/bin/zsh
# Build and control the isolated synthetic-only L12 native QA bundle.
#
# The bundle has a unique LaunchServices identity and receives one explicit,
# canonical /private/tmp/luna-l12-* profile root. The fixture itself owns the
# in-memory auth and provider-store isolation controls.

set -euo pipefail

ROOT="${0:A:h:h}"
REQUESTED_PROFILE="${1:-}"
ACTION="${2:-run}"
OUT="$ROOT/target/luna-l12-fixture"
APP="$OUT/LUNA L12 QA.app"
EXECUTABLE="$APP/Contents/MacOS/task-organization-fixture"
BUILD_EXECUTABLE="$ROOT/target/debug/examples/task-organization-fixture"
BUNDLE_ID="com.novnski.luna.l12-qa"
DISPLAY_NAME="LUNA L12 QA"
REFERENCE_ICNS="${LUNA_L12_REFERENCE_ICNS:-$ROOT/target/luna-l11-fixture/LUNA L11 QA.app/Contents/Resources/luna.icns}"
STAGE_ROOT=""
BACKUP_ROOT=""
REPLACEMENT_PENDING=false
HAD_PREVIOUS_APP=false

fail() {
  print -u2 -- "L12 fixture: $1"
  exit 1
}

[[ -n "$REQUESTED_PROFILE" ]] || fail "usage: scripts/run-macos-l12-fixture.sh /tmp/luna-l12-<fresh-root> [build|run|stop|status]"
[[ "$REQUESTED_PROFILE" == /* ]] || fail "synthetic root must be absolute"
[[ -d "$REQUESTED_PROFILE" ]] || fail "synthetic root must already exist (create it with mktemp -d /tmp/luna-l12-native-qa.XXXXXX)"
[[ ! -L "$REQUESTED_PROFILE" ]] || fail "synthetic root must not be a symlink"

PROFILE="$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$REQUESTED_PROFILE")"
case "$PROFILE" in
  /private/tmp/luna-l12-*) ;;
  *) fail "canonical synthetic root must be a narrow /private/tmp/luna-l12-* directory" ;;
esac

ROOT_OWNER="$(/usr/bin/stat -f '%Su' "$PROFILE")"
[[ "$ROOT_OWNER" == "$(/usr/bin/id -un)" ]] || fail "synthetic root must be owned by the current user"
/bin/chmod 700 "$PROFILE"

PID_FILE="$PROFILE/task-organization-fixture.pid"
LOG_FILE="$PROFILE/task-organization-fixture.log"
ROOT_MARKER="$PROFILE/.luna-l12-qa-root"

remove_generated_tree() {
  local path="$1"
  if [[ -n "$path" && "$path" == "$OUT/"* && -d "$path" ]]; then
    /bin/rm -rf -- "$path"
  fi
}

cleanup() {
  local command_exit=$?
  if [[ -n "$BACKUP_ROOT" && -e "$BACKUP_ROOT/LUNA L12 QA.app" ]]; then
    if [[ -e "$APP" ]]; then
      remove_generated_tree "$APP"
    fi
    /bin/mv -- "$BACKUP_ROOT/LUNA L12 QA.app" "$APP"
  elif $REPLACEMENT_PENDING && ! $HAD_PREVIOUS_APP && [[ -e "$APP" ]]; then
    remove_generated_tree "$APP"
  fi
  remove_generated_tree "$STAGE_ROOT"
  remove_generated_tree "$BACKUP_ROOT"
  trap - EXIT
  exit "$command_exit"
}
trap cleanup EXIT

plist_value() {
  local plist="$1"
  local key="$2"
  /usr/libexec/PlistBuddy -c "Print :$key" "$plist"
}

validate_bundle() {
  local bundle="$1"
  local contents="$bundle/Contents"
  local executable="$contents/MacOS/task-organization-fixture"
  local plist="$contents/Info.plist"
  local icon="$contents/Resources/luna.icns"
  local signature_details

  [[ -x "$executable" ]] || fail "missing bundle executable at $executable"
  /usr/bin/file "$executable" | /usr/bin/grep -Fq "Mach-O" || fail "fixture executable is not Mach-O"
  /usr/bin/plutil -lint "$plist" >/dev/null || fail "invalid Info.plist"
  [[ "$(plist_value "$plist" CFBundleDisplayName)" == "$DISPLAY_NAME" ]] || fail "unexpected CFBundleDisplayName"
  [[ "$(plist_value "$plist" CFBundleExecutable)" == "task-organization-fixture" ]] || fail "unexpected CFBundleExecutable"
  [[ "$(plist_value "$plist" CFBundleIconFile)" == "luna.icns" ]] || fail "unexpected CFBundleIconFile"
  [[ "$(plist_value "$plist" CFBundleIdentifier)" == "$BUNDLE_ID" ]] || fail "unexpected CFBundleIdentifier"
  [[ "$(plist_value "$plist" CFBundleName)" == "$DISPLAY_NAME" ]] || fail "unexpected CFBundleName"
  [[ "$(plist_value "$plist" CFBundleShortVersionString)" == "0.2.73-l12-qa" ]] || fail "unexpected CFBundleShortVersionString"
  [[ "$(plist_value "$plist" CFBundleVersion)" == "1" ]] || fail "unexpected CFBundleVersion"
  if /usr/libexec/PlistBuddy -c "Print :CFBundleURLTypes" "$plist" >/dev/null 2>&1; then
    fail "CFBundleURLTypes must not be present"
  fi
  if /usr/libexec/PlistBuddy -c "Print :LSEnvironment" "$plist" >/dev/null 2>&1; then
    fail "LSEnvironment must not be present"
  fi
  [[ -s "$icon" ]] || fail "missing or empty icon"
  /usr/bin/file "$icon" | /usr/bin/grep -Fq "Mac OS X icon" || fail "invalid macOS icon container"
  /usr/bin/cmp -s "$REFERENCE_ICNS" "$icon" || fail "bundle icon differs from the validated read-only reference"
  /usr/bin/codesign --verify --deep --strict --verbose=2 "$bundle" || fail "ad-hoc signature verification failed"
  signature_details="$(/usr/bin/codesign -d --verbose=4 "$bundle" 2>&1)" || fail "could not inspect bundle signature"
  /usr/bin/grep -Fq "Identifier=$BUNDLE_ID" <<<"$signature_details" || fail "signature has the wrong identifier"
  /usr/bin/grep -Fq "Signature=adhoc" <<<"$signature_details" || fail "bundle is not ad-hoc signed"
}

pid_command() {
  /bin/ps -p "$1" -o command= 2>/dev/null || true
}

expected_command() {
  print -r -- "$EXECUTABLE $PROFILE"
}

exact_pid_matches() {
  local fixture_pid="$1"
  local command_line
  [[ "$fixture_pid" == <-> ]] || return 1
  /bin/kill -0 "$fixture_pid" 2>/dev/null || return 1
  command_line="$(pid_command "$fixture_pid")"
  [[ "$command_line" == "$(expected_command)" ]]
}

validate_exact_pid() {
  local fixture_pid="$1"
  [[ "$fixture_pid" == <-> ]] || fail "invalid fixture pid"
  exact_pid_matches "$fixture_pid" || fail "pid $fixture_pid is not the exact running L12 bundle executable with root $PROFILE"
}

exact_root_pid() {
  local candidate_pid
  local candidate_command
  local process_rows
  process_rows="$(/bin/ps -axo pid=,command=)" || return 2
  while read -r candidate_pid candidate_command; do
    [[ -n "$candidate_pid" ]] || continue
    if [[ "$candidate_command" == "$(expected_command)" ]]; then
      print -r -- "$candidate_pid"
      return 0
    fi
  done <<<"$process_rows"
  return 1
}

refuse_any_running_bundle() {
  local candidate_pid
  local candidate_command
  local process_rows
  process_rows="$(/bin/ps -axo pid=,command=)" || fail "cannot inventory processes safely"
  while read -r candidate_pid candidate_command; do
    [[ -n "$candidate_pid" ]] || continue
    case "$candidate_command" in
      "$EXECUTABLE"|"$EXECUTABLE "*) fail "LUNA L12 QA is already running as pid $candidate_pid" ;;
    esac
  done <<<"$process_rows"
}

build_bundle() {
  refuse_any_running_bundle
  [[ -s "$REFERENCE_ICNS" ]] || fail "validated read-only icon is unavailable at $REFERENCE_ICNS"
  /usr/bin/file "$REFERENCE_ICNS" | /usr/bin/grep -Fq "Mac OS X icon" || fail "reference icon is not a macOS icon container"

  cd "$ROOT"
  cargo build --locked -p zeron-ui --example task-organization-fixture --features appshots-fixture

  /bin/mkdir -p "$OUT"
  STAGE_ROOT="$(/usr/bin/mktemp -d "$OUT/.luna-l12-stage.XXXXXX")"
  local staged_app="$STAGE_ROOT/LUNA L12 QA.app"
  local contents="$staged_app/Contents"
  /bin/mkdir -p "$contents/MacOS" "$contents/Resources"
  /usr/bin/install -m 755 "$BUILD_EXECUTABLE" "$contents/MacOS/task-organization-fixture"
  /usr/bin/install -m 644 "$ROOT/dist/macos/Info-dev.plist" "$contents/Info.plist"
  /usr/bin/install -m 644 "$REFERENCE_ICNS" "$contents/Resources/luna.icns"
  /usr/bin/plutil -replace CFBundleDisplayName -string "$DISPLAY_NAME" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleExecutable -string "task-organization-fixture" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleName -string "$DISPLAY_NAME" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleShortVersionString -string "0.2.73-l12-qa" "$contents/Info.plist"
  /usr/bin/plutil -replace CFBundleVersion -string "1" "$contents/Info.plist"
  /usr/bin/plutil -remove CFBundleURLTypes "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/plutil -remove LSEnvironment "$contents/Info.plist" 2>/dev/null || true
  /usr/bin/cmp -s "$BUILD_EXECUTABLE" "$contents/MacOS/task-organization-fixture" || fail "staged executable differs from Cargo output"
  /usr/bin/codesign --force --sign - --identifier "$BUNDLE_ID" "$staged_app"
  validate_bundle "$staged_app"

  BACKUP_ROOT="$(/usr/bin/mktemp -d "$OUT/.luna-l12-backup.XXXXXX")"
  if [[ -e "$APP" ]]; then
    /bin/mv -- "$APP" "$BACKUP_ROOT/LUNA L12 QA.app" || fail "could not preserve previous bundle"
    HAD_PREVIOUS_APP=true
  fi
  REPLACEMENT_PENDING=true
  /bin/mv -- "$staged_app" "$APP"
  validate_bundle "$APP"
  REPLACEMENT_PENDING=false
  remove_generated_tree "$BACKUP_ROOT"
  BACKUP_ROOT=""

  print -- "bundle=$APP"
  print -- "executable=$EXECUTABLE"
  print -- "identifier=$BUNDLE_ID"
  print -- "display_name=$DISPLAY_NAME"
  print -- "window_expectation=one GPUI window owned by $DISPLAY_NAME; attach by bundle path or identifier, not title text"
  print -- "profile_root=$PROFILE"
  print -- "executable_sha256=$(/usr/bin/shasum -a 256 "$EXECUTABLE" | /usr/bin/awk '{print $1}')"
  print -- "icon_sha256=$(/usr/bin/shasum -a 256 "$APP/Contents/Resources/luna.icns" | /usr/bin/awk '{print $1}')"
  print -- "executable_source_match=verified byte-identical to Cargo output before mandatory bundle signing"
}

case "$ACTION" in
  build)
    build_bundle
    ;;
  run)
    validate_bundle "$APP"
    if [[ -f "$PID_FILE" ]]; then
      fixture_pid="$(<"$PID_FILE")"
      if validate_exact_pid "$fixture_pid"; then
        fail "fixture already running as pid $fixture_pid"
      fi
      fail "stale pid file does not identify a running fixture"
    fi
    if fixture_pid="$(exact_root_pid)"; then
      fail "fixture already running as unmanaged pid $fixture_pid"
    else
      lookup_exit=$?
      [[ "$lookup_exit" == "1" ]] || fail "cannot inventory processes safely"
    fi
    if [[ -e "$ROOT_MARKER" ]]; then
      [[ "$(<"$ROOT_MARKER")" == "$BUNDLE_ID" ]] || fail "synthetic root belongs to another fixture identity"
    else
      [[ -z "$(/usr/bin/find "$PROFILE" -mindepth 1 -maxdepth 1 -print -quit)" ]] || fail "first run requires a fresh empty synthetic root"
      print -r -- "$BUNDLE_ID" >"$ROOT_MARKER"
    fi
    /bin/mkdir -p "$PROFILE/provider-state" "$PROFILE/synthetic-auth" "$PROFILE/synthetic-codex" "$PROFILE/synthetic-claude" "$PROFILE/tmp"
    # Launch through LaunchServices so native CUA receives the bundle identity.
    # `open` returns after launch; status/stop track only the exact app process.
    /usr/bin/open -n \
      --env LUNA_DISABLE_SOUND=1 \
      --env LUNA_DISABLE_NOTIFICATIONS=1 \
      --stdout "$LOG_FILE" \
      --stderr "$LOG_FILE" \
      "$APP" --args "$PROFILE"
    fixture_pid=""
    for _ in {1..100}; do
      if fixture_pid="$(exact_root_pid)"; then
        :
      else
        lookup_exit=$?
        [[ "$lookup_exit" == "1" ]] || fail "cannot inventory processes safely"
        fixture_pid=""
      fi
      if [[ -n "$fixture_pid" ]] && exact_pid_matches "$fixture_pid" && [[ -s "$PROFILE/SAFETY.txt" ]]; then
        print -r -- "$fixture_pid" >"$PID_FILE"
        print -- "pid=$fixture_pid"
        print -- "bundle=$APP"
        print -- "identifier=$BUNDLE_ID"
        print -- "executable=$EXECUTABLE"
        print -- "root=$PROFILE"
        print -- "log=$LOG_FILE"
        exit 0
      fi
      /bin/sleep 0.1
    done
    if [[ -n "$fixture_pid" ]] && exact_pid_matches "$fixture_pid"; then
      /bin/kill "$fixture_pid"
    fi
    fail "fixture did not reach a stable exact-process safety readback"
    ;;
  stop)
    [[ -f "$PID_FILE" ]] || { print -- "not running"; exit 0; }
    fixture_pid="$(<"$PID_FILE")"
    validate_exact_pid "$fixture_pid" || fail "pid file does not identify the exact L12 fixture/root"
    /bin/kill "$fixture_pid"
    for _ in {1..100}; do
      if ! /bin/kill -0 "$fixture_pid" 2>/dev/null; then
        /bin/rm -f "$PID_FILE"
        print -- "stopped pid=$fixture_pid"
        exit 0
      fi
      /bin/sleep 0.1
    done
    fail "fixture pid $fixture_pid did not stop after SIGTERM"
    ;;
  status)
    if [[ -f "$PID_FILE" ]]; then
      fixture_pid="$(<"$PID_FILE")"
      if validate_exact_pid "$fixture_pid"; then
        print -- "running pid=$fixture_pid bundle=$APP identifier=$BUNDLE_ID root=$PROFILE"
        exit 0
      fi
      fail "pid file does not identify the exact L12 fixture/root"
    fi
    if fixture_pid="$(exact_root_pid)"; then
      fail "exact fixture is running without its pid file as pid $fixture_pid"
    else
      lookup_exit=$?
      [[ "$lookup_exit" == "1" ]] || fail "cannot inventory processes safely"
    fi
    print -- "not running"
    ;;
  *) fail "unknown action: $ACTION" ;;
esac
