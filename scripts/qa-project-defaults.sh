#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
out="${LUNA_DEFAULTS_QA_OUTPUT:-$PWD/target/qa/project-defaults}"
mkdir -p "$out"
export CARGO_INCREMENTAL=0
export LUNA_PROJECT_DEFAULTS_EVIDENCE="$out/persistence.json"
python3 scripts/build-budget.py --status > "$out/budget-before.json"
run() {
  local name="$1"; shift
  local status=0
  "$@" > "$out/$name.log" 2>&1 || status=$?
  printf '%s exit=%s log=%s\n' "$name" "$status" "$out/$name.log"
  return "$status"
}
run engine scripts/cargo.sh test --locked -p luna-engine --test project_defaults
run picker scripts/cargo.sh test --locked -p luna-ui --lib project_defaults_tests
run format scripts/cargo.sh fmt --all -- --check
run lint scripts/cargo.sh clippy --locked -p luna-ui -p luna-engine --no-deps
python3 scripts/build-budget.py --status > "$out/budget-after.json"
printf 'passed\n' > "$out/result.txt"
