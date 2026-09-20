#!/usr/bin/env bash
# Deterministic control surface for the isolated L10 native QA fixture.

set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <fixture-root> overload|subscription|done|tool [reset-at-ms]" >&2
  exit 2
fi

PROFILE="$1"
COMMAND="$2"
CONTROL="$PROFILE/l10-fixture-control.json"
TMP="$CONTROL.tmp"

case "$COMMAND" in
  overload) ACTION="failOverload" ;;
  subscription)
    ACTION="failSubscription"
    if [[ ! "${3:-}" =~ ^[0-9]+$ ]]; then
      echo "subscription requires an absolute reset-at-ms integer" >&2
      exit 2
    fi
    ;;
  done) ACTION="done" ;;
  tool) ACTION="tool" ;;
  *)
    echo "unknown command: $COMMAND" >&2
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

if [[ "$COMMAND" == "subscription" ]]; then
  printf '{"sequence":%s,"action":"%s","resetAtMs":%s}\n' \
    "$SEQUENCE" "$ACTION" "$3" >"$TMP"
else
  printf '{"sequence":%s,"action":"%s"}\n' \
    "$SEQUENCE" "$ACTION" >"$TMP"
fi
mv "$TMP" "$CONTROL"
echo "fixture command $ACTION sequence $SEQUENCE written to $CONTROL"
