# Persistent child chats: validation handoff

Implementation is prepared for central coordinator validation. Formatting and static script syntax were checked in the feature checkout. Compilation, executable E2E, real-provider acceptance, and native screenshots have not been run by the feature worker.

The failure expectations below were recorded outside the checkout before source implementation at `luna-feature-checkouts/2026-09-30-codex-parity/evidence/child-chats/failure-scenarios.md`. No new unit tests were added. Existing full `Chat` fixtures were mechanically updated for the additive optional wire field.

## Contract and behavior

- `CreateChildThread { parentTaskId, request }` is the authenticated manual RPC. Request fields: `prompt`, `idempotencyKey`, optional `title`, `model`, `reasoning`, `cwd`, `reportResults` (default true), and `continueParent` (default false).
- Codex `create_thread` carries the same request without a parent argument. The engine resolves the active provider call, binding, run, project and host; the private one-use local approval shows the exact prompt, checkout and reporting behavior. Other providers remain omitted from the agent creation catalog. A resumed Codex session that lacks the registered tool retains the existing restart-required diagnostic.
- Parent plus key yields a deterministic UUID and `child-create:{UUID}` receipt. Different content with that identity conflicts. Identity, ordinary chat, parent lineage and initial startup intent commit in the existing SQLite task-command transaction.
- A weak background maintenance worker feeds `child-initial:{UUID}` through the normal queue with `child-start:{UUID}`. Existing receipts and queue claims own provider acceptance and unknown delivery. The creation receipt reports durable intent/queue state and does not claim provider acceptance.
- New provider conversations have no source/fork lineage, resume ID or copied history. The engine and desktop share project/global provider-model-permission selection; the engine reads the device's `composer-defaults.json`. Only explicit prompt context is copied. Provider/account changes during child creation are deferred: this version requires the selected provider to match the parent and retains that provider account.
- Checkout is always an existing directory, defaulting to the parent checkout; this version creates no worktrees. A parent's non-Full filesystem authority prevents an alternate cwd outside its workspace. Child run policy retains the creation permission/sandbox ceiling and clamps against the current parent ceiling when available. A missing historical parent does not raise permissions.
- Outcomes live in the child row's durable outbox. Stable child/turn/outcome IDs deduplicate recording and parent notices. Transcript entries flush before delivery is marked. Optional continuation uses the ordinary held parent queue with engine notification provenance. It does not mint task-control authority. Archived/deleted parents hold undelivered results and are not reopened.
- Sidebar rendering and jump navigation share the tree. Pagination adds available ancestors and descendants in the same filter/host/pin partition. Separately pinned children are roots when their parent is unpinned. Missing or archived parents promote surviving children to roots. Disclosure is device-local and saved. Opening a child reveals its ancestors; collapsed families roll up working, attention, error and unread status. Child transcripts have parent navigation and ordinary independent controls.
- “New child chat…” in the chat context menu uses the shared operation and supplies only the entered brief. It defaults to display-only reporting.

## Failure expectations

1. Denied/expired approval, forged provider call, changed source binding/run, missing scope: no child or startup.
2. Retry with the same parent/key: one child and one startup message; changed payload conflicts.
3. Crash before creation commit: no partial identity/chat/startup. Crash afterward: startup is recovered with the same IDs.
4. Unknown provider acknowledgment: queue/recovery remains held; no new-ID resend.
5. Provider/model unavailable: the child stays visible and its ordinary queue/recovery path exposes the error.
6. Missing/archived immediate parent, wrong host/project, cycle or fifth hierarchy level: rejection before publication.
7. Parent read-only/permission ceiling and existing-checkout limits remain enforced on later child dispatch.
8. Outcome crash/reconnect: durable outbox remains deliverable and parent entries/queue messages stay deduplicated.
9. Completed turn, failure, interruption and attention have distinct outcomes; completion does not claim permanent task completion.
10. Archived/deleted parent: surviving child remains usable and reachable; results do not reopen the parent.
11. Filter, pin, page, search, direct link and restart: coherent visible order, saved disclosure, ancestor reveal and independent child controls.
12. Claude/OpenCode agent transports remain omitted until actual creation delivery is verified.

## Coordinator commands

Run from the coordinator's primary checkout after importing this committed patch. All builds must use its normal guard/cache and `--locked`.

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-engine -p luna-ui
scripts/cargo.sh clippy --locked -p luna-engine -p luna-ui --all-targets -- -D warnings
scripts/cargo.sh build --locked -p luna
LUNA_CHILD_QA_EVIDENCE=/Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/evidence/child-chats node scripts/qa-child-chats.mjs
```

`qa-child-chats.mjs` launches only its own isolated headless engine, uses a disposable ordinary fixture directory and the synthetic Mock adapter, stops its own process gracefully, and retains source/binary hashes, creation/startup receipts, lineage, outbox/restart readback, transcript frames, engine log, result JSON and `SHA256SUMS`. Its evidence is actual engine/document/queue proof with a synthetic provider. It does **not** prove the real Codex tool approval or native sidebar.

The coordinator should additionally run the relevant existing cross-task/runtime, command, restart/queue and project-default resolver checks. Review any changed fixture expectations as behavior changes, not as reasons to weaken assertions. Suggested existing commands:

```sh
scripts/cargo.sh test --locked -p luna-engine --lib cross_task_tools::
scripts/cargo.sh test --locked -p luna-engine --lib runtime_contract::
scripts/cargo.sh test --locked -p luna-engine --test restart_resume
scripts/cargo.sh test --locked -p luna-engine --test session_publication
scripts/cargo.sh test --locked -p luna-ui --lib project_defaults -- --test-threads=1
```

## Real Codex and native acceptance

Use one exact isolated QA app identity, all four QA overrides, its matching IPC port and preserved evidence directory. Follow the one-GUI, strict codesign, build-budget and graceful-quit rules in the QA infrastructure guide. Do not run worktree-creating harnesses.

1. Make a fresh parent Codex session in an ordinary fixture project. Save `GetRuntimeContract`, task identity and provider session ID. Confirm `create_thread` is registered; a resumed old session must report its omission/restart requirement truthfully.
2. Ask the parent to call `create_thread` once with a bounded harmless explicit brief, one fixed key, `reportResults: true`, `continueParent: false`. Save the exact approval question. Deny one call and verify no child. Allow a subsequent call and save the receipt, child row, identity and startup/queue readbacks.
3. Confirm distinct parent/child provider session IDs and an initial child transcript containing only the supplied brief plus normal runtime instructions. Read back child effective permissions and the runtime parent ID. Repeat the same request/key and verify no additional child or initial user entry; change the prompt with the same key and expect conflict.
4. Capture screenshots of nested rows, collapse while child works, input/failure rollup, ordinary child composer and parent navigation. Pin/unpin a child, open it from search/deep link, and check the jump shortcut order matches visible rows.
5. Let the child complete while the parent is busy. Verify one durable outcome and one parent system entry. With display-only reporting, verify the parent starts no extra turn. Separately opt into continuation and verify one normal held queue row, no synthetic permission grant.
6. Restart the exact QA engine/app at startup and outcome boundaries; retain before/after snapshots and command receipts. Unknown provider delivery must remain held with the same ID rather than automatically re-submitted. Exercise a provider/model failure and a real denied/expired approval.
7. Archive/delete the parent and verify the child remains independently usable and visible as a root; undelivered outcomes remain durable. Do not use subtree cleanup implicitly.
8. Hash evidence, then let the coordinator refresh and verify the primary LUNA Dev process, IPC port and strict signature for final handoff.

## Known bounded limitations

Creation is same project/host and same provider account. Cross-provider defaults are rejected accurately. Existing checkout selection is explicit; project “new worktree” defaults are not materialized by this operation. Outbox delivery uses a two-second sweep rather than loss-prone in-memory batching. The synced child row is bounded below the registry's 16 KiB per-op cap (a rejected op would permanently wedge that device's registry sync): briefs are limited to 8,000 characters and a 12 KiB serialized row, summaries to 2,000 characters, delivered summaries are cleared, and only the 16 most recent delivered outcomes are kept behind an outcome floor. An outcome that would still exceed the budget while its parent is unavailable is deferred by the sweep (no command is issued) until pending results deliver; the engine-side commit refuses an oversize row as a last resort. Native and real-provider behavior remain unverified until the coordinator's acceptance run.
