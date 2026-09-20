#!/usr/bin/env bash
# Deterministic compaction control for the isolated L11 native QA fixture.

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <fixture-root> running|completed|unknown" >&2
  exit 2
fi

PROFILE="$1"
OUTCOME="$2"
CONTROL="$PROFILE/l11-compact-control.json"
TMP="$CONTROL.tmp"

case "$OUTCOME" in
  running|completed|unknown) ;;
  *)
    echo "unknown outcome: $OUTCOME" >&2
    exit 2
    ;;
esac

mkdir -p "$PROFILE"
SEQUENCE=1
if [[ -f "$CONTROL" ]]; then
  PREVIOUS="$(sed -n 's/.*"sequence":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONTROL" | head -1)"
  if [[ -n "$PREVIOUS" ]]; then
    SEQUENCE=$((PREVIOUS + 1))
  fi
fi

printf '{"sequence":%s,"outcome":"%s"}\n' "$SEQUENCE" "$OUTCOME" >"$TMP"
mv "$TMP" "$CONTROL"
echo "fixture compaction outcome $OUTCOME sequence $SEQUENCE written to $CONTROL"
