#!/bin/bash
set -u
cd "$(dirname "$0")/.."
artifact="${LUNA_PR_ARTIFACT_DIR:-$PWD/target/qa/github-pr.$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$artifact"
export LUNA_PR_ARTIFACT_DIR="$artifact"
export CARGO_INCREMENTAL=0
scripts/cargo.sh test --locked -p luna-engine --test github_draft_pr -- --nocapture >"$artifact/integration.log" 2>&1
result=$?
printf 'Integration exit: %s\nArtifacts: %s\n' "$result" "$artifact"
exit "$result"
