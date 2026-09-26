#!/bin/bash
set -u
cd "$(dirname "$0")/.."
artifact="${LUNA_PR_ARTIFACT_DIR:-$PWD/target/qa/github-pr.$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$artifact"
export LUNA_PR_ARTIFACT_DIR="$artifact"
export CARGO_INCREMENTAL=0
scripts/cargo.sh test --locked -p luna-engine --test github_draft_pr -- --nocapture >"$artifact/integration.log" 2>&1
result=$?
if [ "$result" -eq 0 ]; then
  scripts/cargo.sh test --locked -p luna-engine --test backup_restore github_pr_attempt_journal -- --nocapture >"$artifact/backup.log" 2>&1
  result=$?
fi
printf 'Integration exit: %s\nArtifacts: %s\n' "$result" "$artifact"
exit "$result"
