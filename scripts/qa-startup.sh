#!/usr/bin/env bash
# Repeat the production startup flow in two fresh processes per persisted case.
# Build first with:
# scripts/cargo.sh build --locked -p luna-ui --example startup-fixture --features appshots-fixture
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
binary="$ROOT/target/debug/examples/startup-fixture"
[[ -x "$binary" ]] || { echo "Build startup-fixture first" >&2; exit 1; }
mkdir -p "$ROOT/target/qa"
evidence="$(mktemp -d "$ROOT/target/qa/startup.XXXXXX")"
echo "Startup acceptance evidence: $evidence"
checks=0
for case in settled archived mixed-finished auto-settled empty active mixed-active manual-new manual-archived; do
  mkdir "$evidence/$case"
  for label in first-launch restart; do
    if "$binary" "$evidence/$case" "$case" "$label" >"$evidence/$case/$label.log" 2>&1 \
      && [[ -f "$evidence/$case/$label.pass" ]]; then
      checks=$((checks + 1))
      echo "PASS $case $label"
    else
      result=$?
      echo "FAIL $case $label (exit $result); $evidence/$case/$label.log" >&2
      tail -n 8 "$evidence/$case/$label.log" >&2
      exit "$result"
    fi
  done
done
find "$evidence" -type f \( -name '*.png' -o -name '*.json' -o -name '*.pass' \) \
  -exec shasum -a 256 {} + >"$evidence/SHA256SUMS"
printf '{"result":"pass","checks":%s,"processes":%s}\n' "$checks" "$checks" >"$evidence/result.json"
echo "PASS $checks startup/restart checks; screenshots, readbacks and SHA256SUMS: $evidence"
