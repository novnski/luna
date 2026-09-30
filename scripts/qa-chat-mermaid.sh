#!/usr/bin/env bash
# Coordinator-owned compiled/native E2E. Never builds or creates worktrees.
# Build: scripts/cargo.sh build --locked -p luna-ui --example chat-mermaid-fixture --features appshots-fixture
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence="${1:?Usage: scripts/qa-chat-mermaid.sh ABSOLUTE_EVIDENCE_DIRECTORY [BINARY]}"
binary="${2:-$ROOT/target/debug/examples/chat-mermaid-fixture}"
[[ "$evidence" = /* ]] || { echo 'Evidence directory must be absolute' >&2; exit 2; }
[[ -x "$binary" ]] || { echo "Build chat-mermaid-fixture first: $binary" >&2; exit 2; }
mkdir -p "$evidence"
git -C "$ROOT" rev-parse HEAD >"$evidence/source-sha.txt"
for mode in fixture reopen; do
  rm -f "$evidence/$mode.pass"
  if "$binary" "$evidence" "$mode" >"$evidence/$mode.log" 2>&1; then
    [[ -f "$evidence/$mode.pass" ]] || { echo "Missing pass receipt: $evidence/$mode.pass" >&2; exit 1; }
  else
    result=$?
    echo "FAIL $mode (exit $result): $evidence/$mode.log" >&2
    tail -n 12 "$evidence/$mode.log" >&2
    exit "$result"
  fi
done
find "$evidence" -type f ! -name SHA256SUMS -exec shasum -a 256 {} + >"$evidence/SHA256SUMS"
echo "PASS: native Mermaid transcript fixture and separate-process reopening; PNG/JSON/hashes: $evidence"
