#!/usr/bin/env bash
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
artifact_dir="$root/target/qa/managed-skills"
mkdir -p "$artifact_dir"
log="$artifact_dir/e2e.log"
artifact="$artifact_dir/result.txt"
started_at="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

cd "$root"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test skill_library_e2e >"$log" 2>&1
status=$?

cat >"$artifact" <<EOF
suite=skill_library_e2e
command=CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test skill_library_e2e
started_at_utc=$started_at
exit_code=$status
log=$log
source=crates/engine/tests/skill_library_e2e.rs
fixtures=synthetic temporary provider and project roots
EOF

printf 'Managed skill E2E: exit %s; artifact %s; log %s\n' "$status" "$artifact" "$log"
exit "$status"
