#!/usr/bin/env bash
# Repeatable backend audit checks. --live-codex uses authenticated GPT-6 Luna,
# consumes quota and creates two native QA sessions with harmless prompts.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
case "${1:-}" in
  "") live=0 ;;
  --live-codex) live=1 ;;
  *) echo "Usage: $0 [--live-codex]" >&2; exit 2 ;;
esac
mkdir -p target/qa
report="${LUNA_AUDIT_REPORT_DIR:-$(mktemp -d "$ROOT/target/qa/backend-audit.XXXXXX")}"
mkdir -p "$report"
report="$(cd "$report" && pwd)"
export CARGO_INCREMENTAL=0
failed=0
git rev-parse HEAD > "$report/revision.txt"
git status --short > "$report/working-tree.txt"
printf 'step\texit_code\tseconds\n' > "$report/results.tsv"
echo "Audit artifacts: $report"

run_step() {
  local name="$1" start=$SECONDS code=0
  shift
  printf '%q ' "$@" > "$report/$name.command"
  printf '\n' >> "$report/$name.command"
  if MBX_STATS_REPORT="$report/$name-cache.json" "$@" > "$report/$name.log" 2>&1; then
    code=0
  else
    code=$?
    failed=1
    tail -25 "$report/$name.log"
  fi
  printf '%s\t%s\t%s\n' "$name" "$code" "$((SECONDS - start))" >> "$report/results.tsv"
  printf '%s: exit=%s (%ss)\n' "$name" "$code" "$((SECONDS - start))"
}

run_step format scripts/cargo.sh fmt --all -- --check
run_step whitespace git diff --check
run_step compile scripts/cargo.sh check --locked -p luna-engine -p luna-harness -p luna-doc
run_step doc scripts/cargo.sh test --locked -p luna-doc --lib
run_step providers scripts/cargo.sh test --locked -p luna-harness --lib --test codex --test claude --test opencode --no-fail-fast
run_step engine scripts/cargo.sh test --locked -p luna-engine --lib --test e2e --test usage_history --test model_cache_lifecycle --test message_queue --test drafts --test task_commands --test restart_resume --test task_identity --test session_history --test backup_control --test backup_restore --test local_profiles --test session_publication --test provider_scope_e2e --test remote_doc_authority --no-fail-fast
if [[ "$live" == 1 ]]; then
  run_step live-codex scripts/cargo.sh test --locked -p luna-harness --test codex live_real_app_server_ -- --ignored --nocapture --test-threads=1
fi
printf 'exit=%s\n' "$failed" > "$report/result.txt"
echo "Results: $report/results.tsv"
exit "$failed"
