#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
out="${LUNA_DEFAULTS_QA_OUTPUT:-$PWD/target/qa/project-defaults/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$out"
git rev-parse HEAD > "$out/source.txt"
git status --short >> "$out/source.txt"
export CARGO_INCREMENTAL=0
export LUNA_PROJECT_DEFAULTS_EVIDENCE="$out/persistence.json"
python3 scripts/build-budget.py --status > "$out/budget-before.json"
run() {
  local name="$1"; shift
  local status=0
  printf 'CARGO_INCREMENTAL=0 ' >> "$out/commands.txt"
  printf '%q ' "$@" >> "$out/commands.txt"
  printf '\n' >> "$out/commands.txt"
  "$@" > "$out/$name.log" 2>&1 || status=$?
  printf '%s exit=%s log=%s\n' "$name" "$status" "$out/$name.log"
  printf '%s\t%s\n' "$name" "$status" >> "$out/exits.tsv"
  return "$status"
}
run engine scripts/cargo.sh test --locked -p luna-engine --test project_defaults
run picker scripts/cargo.sh test --locked -p luna-ui --lib project_defaults_tests -- --test-threads=1
run format scripts/cargo.sh fmt --all -- --check
run compile scripts/cargo.sh check --locked -p luna-ui -p luna-engine
run lint scripts/cargo.sh clippy --locked -p luna-ui -p luna-engine --no-deps
python3 scripts/build-budget.py --status > "$out/budget-after.json"
printf 'passed\n' > "$out/result.txt"
