#!/usr/bin/env bash
# Coordinator-only focused validation. Uses this checkout's existing guarded target.
set -uo pipefail
repo_root=$(cd "$(dirname "$0")/.." && pwd)
qa_output=${1:-$(mktemp -d /tmp/luna-native-inputs-qa.XXXXXX)}
mkdir -p "$qa_output"
qa_output=$(cd "$qa_output" && pwd)
export LUNA_NATIVE_INPUTS_EVIDENCE="$qa_output"
cd "$repo_root" || exit 1
run_check() {
    local name=$1
    shift
    "$@" > "$qa_output/$name.log" 2>&1
    local code=$?
    printf '%s\n' "$code" > "$qa_output/$name.exit-code"
    printf '%s: exit %s; log %s\n' "$name" "$code" "$qa_output/$name.log"
    return "$code"
}
run_check adapter scripts/cargo.sh test --locked -p luna-harness --test native_inputs
status=$?
if [ "$status" -eq 0 ]; then
    run_check engine scripts/cargo.sh test --locked -p luna-engine --test native_inputs
    status=$?
fi
if command -v shasum >/dev/null 2>&1; then
    (cd "$qa_output" && find . -type f ! -name SHA256SUMS -print0 | xargs -0 shasum -a 256) > "$qa_output/SHA256SUMS"
else
    (cd "$qa_output" && find . -type f ! -name SHA256SUMS -print0 | xargs -0 sha256sum) > "$qa_output/SHA256SUMS"
fi
printf 'Evidence: %s (exit %s)\n' "$qa_output" "$status"
exit "$status"
