#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
artifact="${1:-$PWD/target/qa/project-commands.$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$artifact"
export LUNA_COMMAND_QA_ARTIFACTS="$artifact"
export CARGO_INCREMENTAL=0
run_phase() {
    local label="$1"
    shift
    set +e
    scripts/cargo.sh "$@" >"$artifact/$label.log" 2>&1
    local result=$?
    set -e
    printf '%s exit=%s artifact=%s\n' "$label" "$result" "$artifact"
    if [ "$result" -ne 0 ]; then exit "$result"; fi
}
run_phase store test --locked -p luna-sync --test project_command_store -- --nocapture --test-threads=1
run_phase engine test --locked -p luna-engine --test project_commands --test worktree_on_run --test backup_restore -- --nocapture --test-threads=1
