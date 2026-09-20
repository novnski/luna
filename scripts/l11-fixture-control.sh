#!/usr/bin/env bash
# Deterministic compaction control for the isolated L11 native QA fixture.

set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <fixture-root> running|completed|unknown|auto-running|auto-completed|auto-unknown|auto-done" >&2
  exit 2
fi

PROFILE="$1"
OUTCOME="$2"
CONTROL="$PROFILE/l11-compact-control.json"
ACTION=""

case "$OUTCOME" in
  running|completed|unknown) ;;
  auto-running)
    CONTROL="$PROFILE/l11-fixture-control.json"
    ACTION="autoContextRunning"
    ;;
  auto-completed)
    CONTROL="$PROFILE/l11-fixture-control.json"
    ACTION="autoContextCompleted"
    ;;
  auto-unknown)
    CONTROL="$PROFILE/l11-fixture-control.json"
    ACTION="autoContextUnknown"
    ;;
  auto-done)
    CONTROL="$PROFILE/l11-fixture-control.json"
    ACTION="done"
    ;;
  *)
    echo "unknown outcome: $OUTCOME" >&2
    exit 2
    ;;
esac

TMP="$CONTROL.tmp"

mkdir -p "$PROFILE"
SEQUENCE=1
if [[ -f "$CONTROL" ]]; then
  PREVIOUS="$(sed -n 's/.*"sequence":[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$CONTROL" | head -1)"
  if [[ -n "$PREVIOUS" ]]; then
    SEQUENCE=$((PREVIOUS + 1))
  fi
fi

if [[ -n "$ACTION" ]]; then
  printf '{"sequence":%s,"action":"%s"}\n' "$SEQUENCE" "$ACTION" >"$TMP"
else
  printf '{"sequence":%s,"outcome":"%s"}\n' "$SEQUENCE" "$OUTCOME" >"$TMP"
fi
mv "$TMP" "$CONTROL"
echo "fixture context outcome $OUTCOME sequence $SEQUENCE written to $CONTROL"
