# SW-01: effective per-chat integration inventory

Baseline: `2d456009`. Live ownership belongs to the shared queue.

## Bounded delivery

1. Extend the existing inventory with task/project/host/provider/account/mode
   provenance and a generation. Resolve discovery against that exact chat;
   reject mismatches and scope changes during discovery.
2. Consume Claude's native init observation, reuse the actual normalized MCP
   call/result stream for bounded redacted evidence, and use Codex's existing
   scoped native skill catalog. Retire the unused duplicate Codex observation
   parser and its tests. No new settings database or permission grant.
3. Update existing settings/picker scope and stale/remediation text. Keep
   native selection anchors and drafts intact. Verify existing owner E2Es plus
   real CLI nonce calls and native UI in the selected modes.

## Failure contract and test authoring gate

| Behavior / owner | Credible failure | Strongest proof |
| --- | --- | --- |
| Exact discovery ownership / inventory RPC | Default account or a different task/project supplies the inventory | Existing `claude_mcp_inventory` and native input owners, with independently scoped fixtures and mismatch rejection |
| Effective attachment/call evidence / real adapter + engine | A connected/configured server or another chat's call is marked verified | Native init and actual call/result events; independently chosen service nonce/counter, no supplied admission receipts |
| Freshness / same RPC and session lifecycle | Old proof stays current after mode/account/config change or restart | Fresh/resumed/config-transition RPC readbacks and actual allowed/denied operations |
| UI / current inventory and picker | A late response follows focus; revalidation discards a draft/anchor | Existing integration picker owner plus native scope/readback capture |

No post-implementation unit tests, duplicate renderer/catalog, test-only
production hooks, raw request/result retention, or invented provider capability.

## Delivered behavior

The existing Skills / MCP panel follows the focused chat's provider, exact
account, project and owning host. Its header includes requested/effective
access, discovery age and session freshness. A successful model-visible MCP
call earns a verified status; a denial/failure remains visible separately.
Configured servers without current call evidence remain unverified.

Evidence is bounded in memory and keeps tool name, success and timestamp only.
Mode/account/configuration changes invalidate its source fingerprint. Restart
requires a fresh native observation. Discovery rechecks the chat configuration
and source before returning, and the UI rejects responses from an old focus,
engine or chat configuration. This changes no execution permission or auth
storage and introduces no Keychain flow.

Claude init skill names are consumed by the production adapter. Names alone
cannot prove exact scoped content or executable hooks. Codex uses its existing
scoped native skill catalog; the unused duplicate observation parser and its
three helper-only tests were retired. `claude_mcp_inventory` remains the single
owner for inventory projection, redaction, ownership and stale-mode behavior.

## Verification and retained evidence

See [verification.json](verification.json) for commands, outcomes, native mode
matrix, artifact hashes and limits. Raw transcripts, screenshots and build logs
are retained in `target/qa/luna-only-workflows/sw01-20261013/`.

Real Codex 0.160.0 and Claude 2.1.293 sessions ran LUNA MCP calls under Ask and
Full. Each returned its independently assigned task ID without that ID being
supplied in the prompt. All four also attempted one cross-project read and
received the expected scope denial. Fresh/resumed sessions, engine restart and
an Ask-to-Full safe-boundary transition were read back through the inventory.
Native UI captures show the exact account/mode and verified row, with the
unsent draft and selected model preserved after ownership rejection.

The native recipe is retained as `native_inventory.py` in that evidence
directory. Disposable Git fixtures and the QA bundle were removed after their
hashes were retained; QA data and the build cache remain. To repeat without
overwriting the original proof, copy the helper into a new evidence directory
and recreate the isolated app (quit the primary app gracefully first):

```sh
sw01_repeat=target/qa/luna-only-workflows/sw01-repeat
mkdir -p "$sw01_repeat"
cp target/qa/luna-only-workflows/sw01-20261013/native_inventory.py "$sw01_repeat/"
CARGO_INCREMENTAL=0 LUNA_DEV_APP_NAME='LUNA QA Inventory' \
  LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.inventory LUNA_DEV_IPC_PORT=59844 \
  LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/sw01" scripts/run-macos-dev.sh --refresh
agent-python "$sw01_repeat/native_inventory.py" seed
agent-python "$sw01_repeat/native_inventory.py" deny-seed --index 0
agent-python "$sw01_repeat/native_inventory.py" start --index 0
agent-python "$sw01_repeat/native_inventory.py" snapshot --index 0
```

Wait for the real turn to finish before reading its final snapshot. Repeat
`start`/`snapshot` for indices 1–3, then `deny`/`snapshot` for all four.
`mode-change --index 2` invalidates Ask evidence; resume and snapshot it again
to verify Full. Apply the same signed-app checks and scoped cleanup policy.
Do not run against primary user data. The canonical automated owner can be
repeated with the exact test command in the verification report.

Claude Auto is secondary and was not certified live here. Skill content/hook
delivery belongs to SW-03. The live call matrix certifies LUNA MCP, not every
configured external connector. Remote-host inventory remains unsupported by
the existing local-only RPC policy.
