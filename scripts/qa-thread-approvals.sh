#!/bin/bash
# Reproducible offline thread-policy/approval E2E evidence. No live accounts.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
if [[ $# != 1 || "$1" != /* ]]; then
  echo 'usage: scripts/qa-thread-approvals.sh /absolute/fresh/evidence/directory' >&2
  exit 2
fi
evidence="$1"
if [[ -e "$evidence" ]]; then
  echo 'Choose a fresh evidence directory; previous receipts are preserved.' >&2
  exit 2
fi
mkdir -p "$evidence"
git rev-parse HEAD > "$evidence/revision.txt"
git diff HEAD --stat > "$evidence/working-diff.txt"
printf '%s\n' 'Offline production engine/adapter/UI paths with deterministic peers; no model or OS-sandbox claim.' > "$evidence/provenance.txt"
export CARGO_INCREMENTAL=0
export LUNA_PERMISSION_MODES_RECEIPT="$evidence/mode-wire.jsonl"
fixture_log="$repo/crates/engine/.cc09-fake.log"
fixture_log_existed=false
[[ ! -e "$fixture_log" ]] || fixture_log_existed=true
finalize() {
  local code=$?
  set +e
  if [[ "$fixture_log_existed" == false && -f "$fixture_log" ]]; then
    mv "$fixture_log" "$evidence/fixture-native.log"
  fi
  printf 'exit=%s\n' "$code" > "$evidence/result.txt"
  (cd "$evidence" && shopt -s nullglob && shasum -a 256 ./*.txt ./*.log ./*.jsonl > SHA256SUMS)
  return "$code"
}
trap finalize EXIT
run() {
  local label="$1" code=0
  shift
  "$@" > "$evidence/$label.log" 2>&1 || code=$?
  printf '%s exit=%s log=%s\n' "$label" "$code" "$evidence/$label.log"
  return "$code"
}
run mode-wire scripts/cargo.sh test --locked -p luna-harness --test permission_modes -- --test-threads=1
run approval-detail scripts/qa-permission-detail.sh "$evidence/detail"
run claude-adapter scripts/cargo.sh test --locked -p luna-harness --test claude_permissions -- --test-threads=1
run claude-engine scripts/cargo.sh test --locked -p luna-engine --test claude_permissions -- --test-threads=1
run child-lifecycle scripts/cargo.sh test --locked -p luna-engine --test claude_child_chats -- --test-threads=1
run permission-transitions scripts/cargo.sh test --locked -p luna-engine --test e2e permission -- --test-threads=1
run side-ceilings scripts/cargo.sh test --locked -p luna-engine --test side_chats side_chat_permission_ceiling -- --test-threads=1
run policy-regressions scripts/cargo.sh test --locked -p luna-engine --lib permissions::tests -- --test-threads=1
run picker scripts/cargo.sh test --locked -p luna-ui --lib permission_ -- --test-threads=1
