#!/bin/zsh
set -euo pipefail

ROOT="${0:A:h:h}"
PROFILE="${1:-}"
ACTION="${2:-run}"
EXECUTABLE="$ROOT/target/debug/examples/task-organization-fixture"

fail() {
  print -u2 -- "L12 fixture: $1"
  exit 1
}

[[ -n "$PROFILE" ]] || fail "usage: scripts/run-macos-l12-fixture.sh /absolute/synthetic-root [build|run|stop|status]"
[[ "$PROFILE" == /* ]] || fail "synthetic root must be absolute"
[[ "$PROFILE" != "/" && "$PROFILE" != "/Users" && "$PROFILE" != "/Users/iulian" ]] || fail "refusing broad root"

/bin/mkdir -p "$PROFILE"
PROFILE="$(/usr/bin/python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$PROFILE")"
PID_FILE="$PROFILE/task-organization-fixture.pid"
LOG_FILE="$PROFILE/task-organization-fixture.log"

case "$ACTION" in
  build)
    cd "$ROOT"
    cargo build --locked -p zeron-ui --example task-organization-fixture --features appshots-fixture
    /usr/bin/codesign --force --sign - "$EXECUTABLE" >/dev/null
    print -- "$EXECUTABLE"
    ;;
  run)
    [[ -x "$EXECUTABLE" ]] || fail "build first with: $0 '$PROFILE' build"
    if [[ -f "$PID_FILE" ]]; then
      OLD_PID="$(<"$PID_FILE")"
      if [[ "$OLD_PID" == <-> ]] && /bin/kill -0 "$OLD_PID" 2>/dev/null; then
        fail "fixture already running as pid $OLD_PID"
      fi
    fi
    /bin/mkdir -p "$PROFILE/provider-state" "$PROFILE/synthetic-codex" "$PROFILE/synthetic-claude" "$PROFILE/tmp"
    LUNA_DISABLE_SOUND=1 LUNA_DISABLE_NOTIFICATIONS=1 \
      "$EXECUTABLE" "$PROFILE" >"$LOG_FILE" 2>&1 &
    FIXTURE_PID=$!
    print -- "$FIXTURE_PID" >"$PID_FILE"
    print -- "pid=$FIXTURE_PID root=$PROFILE log=$LOG_FILE"
    ;;
  stop)
    [[ -f "$PID_FILE" ]] || { print -- "not running"; exit 0; }
    FIXTURE_PID="$(<"$PID_FILE")"
    [[ "$FIXTURE_PID" == <-> ]] || fail "invalid pid file"
    EXECUTED="$(/bin/ps -p "$FIXTURE_PID" -o comm= 2>/dev/null || true)"
    [[ "$EXECUTED" == *task-organization-fixture* ]] || fail "pid $FIXTURE_PID is not the L12 fixture"
    /bin/kill "$FIXTURE_PID"
    /bin/rm -f "$PID_FILE"
    print -- "stopped pid=$FIXTURE_PID"
    ;;
  status)
    if [[ -f "$PID_FILE" ]]; then
      FIXTURE_PID="$(<"$PID_FILE")"
      if [[ "$FIXTURE_PID" == <-> ]] && /bin/kill -0 "$FIXTURE_PID" 2>/dev/null; then
        print -- "running pid=$FIXTURE_PID root=$PROFILE"
        exit 0
      fi
    fi
    print -- "not running"
    ;;
  *) fail "unknown action: $ACTION" ;;
esac
