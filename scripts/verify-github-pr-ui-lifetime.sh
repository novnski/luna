#!/bin/bash
# Actual retained Changes + memory RPC cases. Requires coordinator build slot.
set -u
cd "$(dirname "$0")/.."
artifact="${LUNA_EPOCH_QA_ARTIFACTS:-$PWD/target/qa/github-pr-ui-lifetime.$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$artifact"
export LUNA_EPOCH_QA_ARTIFACTS="$artifact"
export CARGO_INCREMENTAL=0
scripts/cargo.sh test --locked -p luna-ui --lib pr_ui_lifetime_ -- --test-threads=1 --nocapture >"$artifact/test.log" 2>&1
result=$?
printf 'PR UI lifetime exit: %s\nArtifacts: %s\n' "$result" "$artifact"
exit "$result"
