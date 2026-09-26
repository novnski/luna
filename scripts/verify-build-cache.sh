#!/usr/bin/env bash
# End-to-end check against a real LUNA crate. Retains evidence, removes builds.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
for tool in cargo jq cmp; do
  command -v "$tool" >/dev/null || { echo "Required tool unavailable: $tool" >&2; exit 127; }
done

mkdir -p "$ROOT/target/qa"
report_dir="$(mktemp -d "$ROOT/target/qa/build-cache.XXXXXX")"
scratch_dir="$(mktemp -d "$ROOT/target/.build-cache-e2e.XXXXXX")"
cleanup() {
  result_code=$?
  trap - EXIT
  rm -rf -- "$scratch_dir"
  printf 'exit=%s\n' "$result_code" > "$report_dir/result.txt"
  echo "Build-cache verification exit=$result_code; artifacts: $report_dir"
  exit "$result_code"
}
trap cleanup EXIT

target_identity="$(ls -di "$ROOT/target")"
missing_mbx="$scratch_dir/mbx-not-installed"

# Exercise the actual wrapper and Cargo CLI, with no mock compiler or cache.
LUNA_MBX_BIN="$missing_mbx" LUNA_BUILD_CACHE=auto \
  scripts/cargo.sh check --help > "$report_dir/missing-auto.log" 2>&1
missing_status=0
LUNA_MBX_BIN="$missing_mbx" LUNA_BUILD_CACHE=1 \
  scripts/cargo.sh check --help > "$report_dir/missing-required.log" 2>&1 || missing_status=$?
[[ "$missing_status" == 127 ]]
LUNA_MBX_BIN="$missing_mbx" LUNA_BUILD_CACHE=0 \
  scripts/cargo.sh +stable check --help > "$report_dir/disabled.log" 2>&1
LUNA_MBX_BIN="$missing_mbx" LUNA_BUILD_CACHE=1 \
  scripts/cargo.sh clean --help > "$report_dir/clean-help.log" 2>&1

cached_failure=0
LUNA_BUILD_CACHE=1 scripts/cargo.sh check --locked -p luna-mbx-deliberately-missing \
  --target-dir "$scratch_dir/failure" > "$report_dir/cached-failure.log" 2>&1 || cached_failure=$?
plain_failure=0
LUNA_BUILD_CACHE=0 scripts/cargo.sh check --locked -p luna-mbx-deliberately-missing \
  --target-dir "$scratch_dir/failure" > "$report_dir/plain-failure.log" 2>&1 || plain_failure=$?
[[ "$cached_failure" != 0 && "$cached_failure" == "$plain_failure" ]]

for build_name in first restored; do
  echo "Checking luna-proto in fresh $build_name target; log: $report_dir/$build_name.log"
  CARGO_INCREMENTAL=0 LUNA_BUILD_CACHE=1 MBX_STATS_REPORT="$report_dir/$build_name.json" \
    scripts/cargo.sh +stable check --locked -p luna-proto --target-dir "$scratch_dir/$build_name" \
    > "$report_dir/$build_name.log" 2>&1
done

# A second up-to-date build cannot satisfy these assertions.
jq -e '.hits > 0 and .compiler_invocations_avoided > 0 and .restored_output_files > 0
  and .divergences == 0' "$report_dir/restored.json" > /dev/null
shopt -s nullglob
# itoa has no build-script OUT_DIR. serde deliberately recompiles with each
# original OUT_DIR under our source-path policy, so its metadata can differ.
itoa_outputs=("$scratch_dir/restored/debug/deps/"libitoa-*.rmeta)
[[ "${#itoa_outputs[@]}" -gt 0 ]]
for restored_file in "${itoa_outputs[@]}"; do
  cmp "$scratch_dir/first/debug/deps/${restored_file##*/}" "$restored_file"
done

# Confirm Cargo can consume/write the restored target without the wrapper.
CARGO_INCREMENTAL=0 LUNA_BUILD_CACHE=0 scripts/cargo.sh check --locked -p luna-proto \
  --target-dir "$scratch_dir/restored" > "$report_dir/plain-after-restore.log" 2>&1
[[ "$(ls -di "$ROOT/target")" == "$target_identity" ]]

jq -n --slurpfile first "$report_dir/first.json" --slurpfile restored "$report_dir/restored.json" \
  --argjson cached_failure "$cached_failure" --argjson plain_failure "$plain_failure" \
  '{first: ($first[0] | {hits, misses, unconsulted, session_duration_ns}),
    restored: ($restored[0] | {hits, misses, unconsulted, session_duration_ns,
      compiler_invocations_avoided, restored_output_files, restored_output_bytes}),
    missing_required_exit: 127, cached_failure_exit: $cached_failure,
    plain_failure_exit: $plain_failure, target_preserved: true,
    itoa_metadata_identical: true, plain_cargo_after_restore: true}' \
  > "$report_dir/summary.json"
cat "$report_dir/summary.json"
