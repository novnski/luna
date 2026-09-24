#!/bin/sh
# Executed by system ssh as `sh -s -- MODE VERSION SHA PORT IPC KEY OWNER`.
set -eu
mode=$1 version=$2 sha=$3 remote_port=$4 ipc_port=$5 key=$6 owner=$7
machine=$8 environment=$9 scope=${10}
base="$HOME/.luna/runtime/versions/$version"
binary="$base/luna-headless"
stage="$base/.incoming-$owner"
state="$HOME/.luna/ssh-launch/$key"
acquire_lock() {
  lock=$1
  count=0
  while ! mkdir "$lock" 2>/dev/null; do
    count=$((count + 1))
    holder=$(cat "$lock/pid" 2>/dev/null || true)
    if [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; then
      rm -f "$lock/pid"
      rmdir "$lock" 2>/dev/null || true
      continue
    fi
    if [ -z "$holder" ] && [ "$count" -ge 5 ]; then
      rmdir "$lock" 2>/dev/null || true
    fi
    [ "$count" -lt 30 ] || { printf 'remote LUNA lock timed out\n' >&2; exit 1; }
    sleep 1
  done
  printf '%s\n' "$$" > "$lock/pid"
}
release_lock() {
  rm -f "$lock/pid"
  rmdir "$lock" 2>/dev/null || true
}
installed() {
  [ -x "$binary" ] && [ "$(cat "$base/.sha256" 2>/dev/null)" = "$sha" ] &&
    [ "$("$binary" --version 2>/dev/null)" = "luna-headless $version" ] &&
    [ "$(file_sha "$binary")" = "$sha" ]
}
file_sha() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}
owned_pid_running() {
  pid=$(cat "$state/pid" 2>/dev/null || true)
  saved_port=$(cat "$state/port" 2>/dev/null || true)
  saved_stamp=$(cat "$state/stamp" 2>/dev/null || true)
  case "$pid" in *[!0-9]*|'') return 1 ;; esac
  [ "$saved_port" = "$remote_port" ] && [ -n "$saved_stamp" ] || return 1
  command_line=$(ps -p "$pid" -o command= 2>/dev/null || true)
  observed_stamp=$(ps -p "$pid" -o lstart= 2>/dev/null || true)
  [ "$observed_stamp" = "$saved_stamp" ] &&
    [ "${command_line#*"$binary"}" != "$command_line" ] &&
    [ "${command_line#*--remote-port "$remote_port"}" != "$command_line" ]
}
case "$mode" in
  platform)
    printf '%s %s\n' "$(uname -s)" "$(uname -m)"
    ;;
  inspect)
    if installed; then printf 'installed\n';
    elif [ -e "$binary" ] || [ -e "$base/.sha256" ]; then
      printf 'version or checksum mismatch in %s\n' "$base" >&2; exit 1
    else printf 'missing\n'; fi
    ;;
  install)
    mkdir -p "$base"
    acquire_lock "$base/.install.lock"
    trap 'rm -f "$stage"; release_lock' EXIT
    if installed; then rm -f "$stage"; exit 0; fi
    if [ -e "$binary" ] || [ -e "$base/.sha256" ]; then
      printf 'version or checksum mismatch in %s\n' "$base" >&2; exit 1
    fi
    [ -f "$stage" ] || { printf 'staged artifact missing\n' >&2; exit 1; }
    actual=$(file_sha "$stage")
    [ "$actual" = "$sha" ] || { printf 'staged artifact checksum mismatch\n' >&2; exit 1; }
    chmod 700 "$stage"
    [ "$("$stage" --version)" = "luna-headless $version" ] || {
      printf 'staged artifact version mismatch\n' >&2; exit 1;
    }
    mv "$stage" "$binary"
    printf '%s\n' "$sha" > "$base/.sha256.next"
    mv "$base/.sha256.next" "$base/.sha256"
    ;;
  discard)
    rm -f "$stage"
    ;;
  start)
    mkdir -p "$state"
    acquire_lock "$state/.launch.lock"
    trap 'release_lock' EXIT
    installed || { printf 'headless artifact is not installed\n' >&2; exit 1; }
    if [ -f "$state/pid" ]; then
      saved_owner=$(cat "$state/owner" 2>/dev/null || true)
      if owned_pid_running; then
        if [ "$saved_owner" = "$owner" ]; then kind=owned; else kind=reused; fi
        printf 'port=%s\nkind=%s\n' "$remote_port" "$kind"
        exit 0
      fi
      stale_pid=$(cat "$state/pid" 2>/dev/null || true)
      case "$stale_pid" in *[!0-9]*|'') stale_pid=0 ;; esac
      if kill -0 "$stale_pid" 2>/dev/null; then
        printf 'recorded remote PID is live but its identity changed; refusing to replace it\n' >&2
        exit 1
      fi
      rm -f "$state/pid" "$state/port" "$state/owner" "$state/stamp"
    fi
    nohup "$binary" --remote-port "$remote_port" --ipc-port "$ipc_port" \
      </dev/null >>"$state/server.log" 2>&1 &
    pid=$!
    count=0
    stamp=
    while [ -z "$stamp" ] && [ "$count" -lt 20 ]; do
      stamp=$(ps -p "$pid" -o lstart= 2>/dev/null || true)
      count=$((count + 1))
      [ -n "$stamp" ] || sleep 0.1
    done
    [ -n "$stamp" ] || { printf 'new remote process exited before identity capture\n' >&2; exit 1; }
    printf '%s\n' "$pid" >"$state/pid.next"
    printf '%s\n' "$remote_port" >"$state/port.next"
    printf '%s\n' "$owner" >"$state/owner.next"
    printf '%s\n' "$stamp" >"$state/stamp.next"
    mv "$state/pid.next" "$state/pid"
    mv "$state/port.next" "$state/port"
    mv "$state/owner.next" "$state/owner"
    mv "$state/stamp.next" "$state/stamp"
    printf 'port=%s\nkind=owned\n' "$remote_port"
    ;;
  status)
    owned_pid_running || { printf 'remote process identity or liveness changed\n' >&2; exit 1; }
    if [ "$(cat "$state/owner")" = "$owner" ]; then
      printf 'running-owned\n'
    else
      printf 'running-reused\n'
    fi
    ;;
  pair)
    installed || { printf 'headless artifact is not installed\n' >&2; exit 1; }
    "$binary" pair --ipc-port "$ipc_port" --label "ssh-$key" \
      --scope environment-read --scope tasks-read \
      --expect-machine "$machine" --expect-environment "$environment" \
      --expect-workspace-scope "$scope"
    ;;
  stop)
    [ -d "$state" ] || { printf 'not-owned\n'; exit 0; }
    acquire_lock "$state/.launch.lock"
    trap 'release_lock' EXIT
    [ -f "$state/owner" ] || { printf 'not-owned\n'; exit 0; }
    [ "$(cat "$state/owner")" = "$owner" ] || {
      printf 'remote process is owned by another launch\n' >&2; exit 1;
    }
    pid=$(cat "$state/pid" 2>/dev/null || true)
    case "$pid" in *[!0-9]*|'') printf 'invalid owned PID\n' >&2; exit 1 ;; esac
    if kill -0 "$pid" 2>/dev/null; then
      owned_pid_running || { printf 'owned PID identity changed\n' >&2; exit 1; }
      kill "$pid"
      count=0
      while kill -0 "$pid" 2>/dev/null && [ "$count" -lt 20 ]; do
        count=$((count + 1)); sleep 0.1
      done
      kill -0 "$pid" 2>/dev/null && {
        printf 'owned process did not stop; ownership retained\n' >&2; exit 1;
      }
    fi
    rm -f "$state/pid" "$state/port" "$state/owner" "$state/stamp"
    printf 'stopped\n'
    ;;
  *) printf 'invalid mode\n' >&2; exit 2 ;;
esac
