#!/bin/bash
# Coordinator-owned, no-worktree E2E runner. Synthetic proof is explicitly
# separated from live provider/native UI verification in the handoff.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
if [[ $# != 1 || "$1" != /* ]]; then
  echo 'usage: scripts/qa-permission-detail.sh /absolute/evidence/directory' >&2
  exit 2
fi
evidence="$1"
mkdir -p "$evidence"
if [[ -e "$evidence/result.txt" || -e "$evidence/native-wire.jsonl" ]]; then
  echo 'Evidence already exists; choose a fresh directory to preserve prior receipts.' >&2
  exit 2
fi
git rev-parse HEAD > "$evidence/revision.txt"
printf '%s\n' 'Synthetic actual-adapter/engine/doc proof; live provider and GUI verification are separate.' > "$evidence/provenance.txt"
export LUNA_PERMISSION_RECEIPT="$evidence/native-wire.jsonl"
finalize() {
  local code=$?
  if [[ ! -f "$evidence/result.txt" ]]; then printf 'FAILED exit=%s\n' "$code" > "$evidence/result.txt"; fi
  (
    cd "$evidence"
    for file in revision.txt provenance.txt native-wire.jsonl ./*.log result.txt; do
      if [[ -f "$file" ]]; then shasum -a 256 "$file"; fi
    done > SHA256SUMS
  )
  return "$code"
}
trap finalize EXIT
run_logged() {
  local label="$1"
  shift
  local code=0
  "$@" > "$evidence/$label.log" 2>&1 || code=$?
  printf '%s exit=%s log=%s\n' "$label" "$code" "$evidence/$label.log"
  if [[ $code != 0 ]]; then
    printf 'failed check: %s\n' "$label" > "$evidence/result.txt"
    return "$code"
  fi
}
run_logged adapter scripts/cargo.sh test --locked -p luna-harness --test permission_detail -- --nocapture --test-threads=1
run_logged approval-regressions scripts/cargo.sh test --locked -p luna-harness --test codex approval -- --test-threads=1
run_logged permission-regressions scripts/cargo.sh test --locked -p luna-harness --test codex provider_permission_event -- --test-threads=1
run_logged engine-doc scripts/cargo.sh test --locked -p luna-engine --test e2e structured_approval_preserves_details_and_rejects_unadvertised_choice -- --nocapture --test-threads=1
run_logged legacy-engine scripts/cargo.sh test --locked -p luna-engine --test e2e typed_live_approval -- --test-threads=1
run_logged engine-expiry scripts/cargo.sh test --locked -p luna-engine --lib approval -- --test-threads=1
printf '%s\n' 'PASS: synthetic adapter wire choices, engine/doc boundaries and focused legacy approvals.' > "$evidence/result.txt"
printf 'Evidence: %s\n' "$evidence"
