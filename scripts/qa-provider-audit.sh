#!/usr/bin/env bash
# Repeatable provider/account regressions; run only with the coordinator's slot.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
mkdir -p target/qa
report="${LUNA_AUDIT_REPORT_DIR:-$(mktemp -d "$repo_root/target/qa/provider-audit.XXXXXX")}"
mkdir -p "$report"
report="$(cd "$report" && pwd)"
export CARGO_INCREMENTAL=0
git rev-parse HEAD > "$report/revision.txt"
git status --short > "$report/working-tree.txt"
printf 'step\texit_code\tseconds\n' > "$report/results.tsv"
printf 'Artifacts: %s\n' "$report"

run_step() {
  local name="$1" start=$SECONDS code=0
  shift
  printf '%q ' "$@" > "$report/$name.command"
  printf '\n' >> "$report/$name.command"
  if MBX_STATS_REPORT="$report/$name-cache.json" "$@" > "$report/$name.log" 2>&1; then
    code=0
  else
    code=$?
  fi
  printf '%s\t%s\t%s\n' "$name" "$code" "$((SECONDS - start))" >> "$report/results.tsv"
  printf '%s: exit=%s (%ss)\n' "$name" "$code" "$((SECONDS - start))"
  if [[ "$code" != 0 ]]; then
    printf 'Stopped. Inspect %s/%s.log\n' "$report" "$name"
    return "$code"
  fi
}

run_step format scripts/cargo.sh fmt --all -- --check
run_step whitespace git diff --check
run_step lifecycle scripts/cargo.sh test --locked -p luna-harness --test provider_lifecycle_audit
run_step account-selection scripts/cargo.sh test --locked -p luna-engine --test usage_account_selection
run_step providers scripts/cargo.sh test --locked -p luna-harness
run_step accounts scripts/cargo.sh test --locked -p luna-engine --lib agent_accounts::
run_step engine-integration scripts/cargo.sh test --locked -p luna-engine --test account_lifecycle_acceptance --test model_cache_lifecycle --test provider_scope_e2e --test m5c_accounts_uploads_titles
run_step compile scripts/cargo.sh check --locked -p luna-harness -p luna-engine
run_step clippy scripts/cargo.sh clippy --locked -p luna-harness -p luna-engine --all-targets
printf 'All checks passed. Results: %s/results.tsv\n' "$report"
