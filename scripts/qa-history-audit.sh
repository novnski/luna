#!/usr/bin/env bash
# Repeatable history/cache regressions. --live-readback reads provider
# transcripts and the public pricing endpoint; it never creates provider runs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
case "${1:-}" in
  "") live=0 ;;
  --live-readback) live=1 ;;
  *) echo "Usage: $0 [--live-readback]" >&2; exit 2 ;;
esac
mkdir -p target/qa
report="${LUNA_HISTORY_REPORT_DIR:-$(mktemp -d "$ROOT/target/qa/history-audit.XXXXXX")}"
mkdir -p "$report"
report="$(cd "$report" && pwd)"
export CARGO_INCREMENTAL=0
failed=0
git rev-parse HEAD > "$report/revision.txt"
git status --short > "$report/working-tree.txt"
printf 'step\texit_code\tseconds\n' > "$report/results.tsv"
echo "History audit artifacts: $report"

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
    tail -20 "$report/$name.log"
  fi
  printf '%s\t%s\t%s\n' "$name" "$code" "$((SECONDS - start))" >> "$report/results.tsv"
  printf '%s: exit=%s (%ss)\n' "$name" "$code" "$((SECONDS - start))"
  if [[ "$code" == 75 ]]; then
    printf 'exit=75\n' > "$report/result.txt"
    echo "Build slot or storage admission is unavailable; coordinate before resuming." >&2
    exit 75
  fi
}

run_step format scripts/cargo.sh fmt --all -- --check
run_step whitespace git diff --check
run_step compile scripts/cargo.sh check --locked -p luna-engine -p luna-proto
run_step history scripts/cargo.sh test --locked -p luna-engine --test usage_history --test session_history
run_step usage-existing scripts/cargo.sh test --locked -p luna-engine --lib usage::
run_step merge-existing scripts/cargo.sh test --locked -p luna-proto --lib usage::
run_step lint scripts/cargo.sh clippy --locked -p luna-engine --test usage_history
if [[ "$live" == 1 ]]; then
  run_step live-history env LUNA_USAGE_REAL_READBACK=1 scripts/cargo.sh test --locked -p luna-engine --lib usage::tests::selected_provider_history_readback -- --ignored --exact --nocapture --test-threads=1
  run_step live-prices env LUNA_USAGE_REAL_READBACK=1 scripts/cargo.sh test --locked -p luna-engine --lib usage::tests::current_litellm_price_readback -- --ignored --exact --nocapture --test-threads=1
fi
printf 'exit=%s\n' "$failed" > "$report/result.txt"
echo "Results: $report/results.tsv"
exit "$failed"
