#!/usr/bin/env bash
# Disposable filesystem/subprocess/RPC coverage for the workspace data audit.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
mkdir -p target/qa
report="${LUNA_AUDIT_REPORT_DIR:-$(mktemp -d "$ROOT/target/qa/workspace-data.XXXXXX")}"
mkdir -p "$report"
export CARGO_INCREMENTAL=0
git rev-parse HEAD > "$report/revision.txt"
git diff > "$report/working-tree.patch"
printf 'step\texit_code\tseconds\n' > "$report/results.tsv"
failed=0
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
    printf 'Build slot unavailable; coordinate before retrying.\n' >&2
    exit 75
  fi
}
run_step format scripts/cargo.sh fmt --all -- --check
run_step whitespace git diff --check
run_step compile scripts/cargo.sh check --locked -p luna-engine
run_step regressions scripts/cargo.sh test --locked -p luna-engine --test workspace_data_safety -- --nocapture
run_step connected scripts/cargo.sh test --locked -p luna-engine --test workspace_files --test m5_repos_diffs_terminals --test m5c_accounts_uploads_titles --test backup_restore --test backup_control --test l21_terminals --test previews --test queued_attachments --no-fail-fast
run_step checkpoints scripts/cargo.sh test --locked -p luna-engine --lib git_checkpoints::
run_step uploads scripts/cargo.sh test --locked -p luna-engine --lib uploads::
run_step source-control scripts/cargo.sh test --locked -p luna-engine --lib git_actions::
run_step clippy scripts/cargo.sh clippy --locked -p luna-engine --lib --test workspace_data_safety --no-deps
printf 'exit=%s\n' "$failed" > "$report/result.txt"
echo "Results: $report/results.tsv"
exit "$failed"
