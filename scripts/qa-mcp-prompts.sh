#!/bin/bash
# Coordinator-owned validation; never invoke from a delegated source worker.
# No worktrees, extra clones, native apps or model/API calls are created here.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
evidence=${1:?usage: scripts/qa-mcp-prompts.sh ABSOLUTE_EVIDENCE_DIRECTORY}
case "$evidence" in /*) ;; *) printf 'Supply an absolute evidence directory\n' >&2; exit 2 ;; esac
mkdir -p "$evidence/logs" "$evidence/results"
export LUNA_MCP_E2E_ARTIFACT_DIR="$evidence/results"
run_check() {
    local label=$1
    shift
    local log="$evidence/logs/$label.log"
    if "$@" >"$log" 2>&1; then
        printf '%s: passed (%s)\n' "$label" "$log"
    else
        local result=$?
        printf '%s: failed, exit %s (%s)\n' "$label" "$result" "$log" >&2
        tail -n 35 "$log" >&2
        exit "$result"
    fi
}
run_check budget python3 scripts/build-budget.py --status
run_check format scripts/cargo.sh fmt --all -- --check
run_check compile scripts/cargo.sh check --locked -p luna-proto -p luna-harness -p luna-engine -p luna-ui
run_check adapter scripts/cargo.sh test --locked -p luna-harness --test codex generic_mcp_elicitation_roundtrips_typed_fields_and_actions -- --nocapture
run_check engine scripts/cargo.sh test --locked -p luna-engine --test e2e mcp_form_url_validation_concurrency_and_secret_boundaries_e2e -- --nocapture
run_check remote-authorization scripts/cargo.sh test --locked -p luna-engine --test remote_auth mcp_authorization_urls_are_owner_only_even_with_remote_read_scopes -- --nocapture
run_check computer-use scripts/cargo.sh test --locked -p luna-harness --test codex computer_use -- --nocapture
run_check clippy scripts/cargo.sh clippy --locked -p luna-proto -p luna-harness -p luna-engine -p luna-ui --all-targets -- -D warnings
if command -v agent-python >/dev/null 2>&1; then qa_python=agent-python; else qa_python=python3; fi
"$qa_python" - "$evidence" <<'PY'
import hashlib
import json
import subprocess
import sys
from pathlib import Path
root = Path(sys.argv[1])
required = ['adapter.json', 'engine-form-url.json', 'engine-stop-restart.json', 'remote-authorization.json']
for name in required:
    assert (root / 'results' / name).is_file(), f'Missing proof: {name}'
files = {}
for path in sorted(root.rglob('*')):
    if path.is_file() and path.name != 'manifest.json':
        files[str(path.relative_to(root))] = {'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
manifest = {'synthetic': True, 'nativeUiAndRealProviderVerified': False,
            'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(), 'files': files}
(root / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(f'Hashed {len(files)} evidence files: {root / "manifest.json"}')
PY
