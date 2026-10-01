#!/usr/bin/env bash
# Coordinator-owned deterministic native adapter + durable queue regressions.
# Synthetic protocol evidence is not real-provider/model/UI proof.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
if [[ "${LUNA_STEERING_COORDINATOR:-0}" != 1 ]]; then
  printf 'Run only in the coordinator validation slot with LUNA_STEERING_COORDINATOR=1.\n' >&2
  exit 64
fi
if [[ $# != 1 || "$1" != /* ]]; then
  printf 'Usage: qa-steering-status.sh /absolute/evidence-directory\n' >&2
  exit 64
fi
mkdir -p "$1"
report="$(mktemp -d "$1/deterministic.XXXXXX")"
chmod 700 "$report"
export LUNA_STEERING_EVIDENCE_DIR="$report/adapter-events"
export CARGO_INCREMENTAL=0
git rev-parse HEAD > "$report/revision.txt"
git status --short > "$report/working-tree.txt"
printf 'step\texit_code\tseconds\n' > "$report/results.tsv"
printf 'Artifacts: %s\n' "$report"
finish() {
  local code=$?
  trap - EXIT
  if agent-python - "$report" "$code" <<'PY'
import hashlib, json, sys
from pathlib import Path
root = Path(sys.argv[1])
files = {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
         for p in sorted(root.rglob('*')) if p.is_file() and p.name != 'manifest.json'}
(root / 'manifest.json').write_text(json.dumps({'exit_code': int(sys.argv[2]),
    'evidence_kind': 'deterministic native protocol adapter and engine/document regressions',
    'real_provider_model_ui_verified': False, 'sha256': files}, indent=2) + '\n')
PY
  then
    :
  else
    printf 'Evidence manifest could not be written.
' >&2
    if [[ "$code" == 0 ]]; then code=1; fi
  fi
  printf 'Final exit=%s; manifest: %s/manifest.json\n' "$code" "$report"
  exit "$code"
}
trap finish EXIT
run_step() {
  local name="$1" start=$SECONDS code=0
  shift
  printf '%q ' "$@" > "$report/$name.command"
  printf '\n' >> "$report/$name.command"
  if MBX_STATS_REPORT="$report/$name-cache.json" "$@" > "$report/$name.log" 2>&1; then code=0; else code=$?; fi
  printf '%s\t%s\t%s\n' "$name" "$code" "$((SECONDS - start))" >> "$report/results.tsv"
  printf '%s: exit=%s (%ss); log: %s/%s.log\n' "$name" "$code" "$((SECONDS - start))" "$report" "$name"
  return "$code"
}
run_step whitespace git diff --check
run_step adapter scripts/cargo.sh test --locked -p luna-harness --test codex steer -- --nocapture
run_step external-interruption scripts/cargo.sh test --locked -p luna-harness --test codex native_terminal_interruption_is_truthful_without_local_stop -- --nocapture
run_step terminal-race scripts/cargo.sh test --locked -p luna-harness --test codex native_terminal_status_wins_the_local_interrupt_race -- --nocapture
run_step durable-queue scripts/cargo.sh test --locked -p luna-engine --test message_queue steer -- --nocapture
run_step legacy-boundaries scripts/cargo.sh test --locked -p luna-engine --test account_lifecycle_acceptance --test self_continued_quiesce --test turn_quiesce --test restart_resume
run_step compile scripts/cargo.sh check --locked -p luna-harness -p luna-engine -p luna-ui
run_step clippy scripts/cargo.sh clippy --locked -p luna-harness -p luna-engine --all-targets
