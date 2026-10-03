# LUNA backend and UI audit — 2026-10-03

Status: investigation, fixes and verification complete. Baseline `5b2df11e`;
backend owner Codex, frontend owner
Claude Code (Opus 5.5, Extra), both working on `main`. Pre-existing changes in
`docs/pending-tasks` are excluded from this audit's commits.

## Confirmed findings

| Finding | User impact | Evidence / disposition |
| --- | --- | --- |
| Idle RPC subscription survives receiver drop | Closed views keep server watchers and document resources alive until another event occurs | Socket acceptance test failed before fix; passes after cancellation monitor |
| Full RPC stream queue blocks the shared connection indefinitely | A stopped consumer can freeze unrelated controls and replies | Socket flood/echo test failed before fix; passes with bounded drain grace and stream cancellation |
| Requests can register after inbound EOF | A half-open connection accepts a request that nobody can answer | Transport EOF test failed before fix; passes with synchronized closed state |
| Canceled unary wait retains a pending reply slot | Timeout/cancellation retains bookkeeping until a reply or disconnect | Scoped request guard removes the slot; server mutations continue across local wait cancellation |
| Six project-command methods missing from shipped RPC inventory | Policy inventory regression fails; known local-only methods are reported as unknown | Existing completeness test failed; inventory corrected without expanding remote scopes |
| Document eviction depends on further activity | A burst of recently opened chats remains resident after the user goes idle | Real-timer test failed with 20 retained documents; passes with periodic cleanup to the 12-document warm budget |
| Queue-only readers were absent from eviction pins | A live queue view can be orphaned from further updates | Residency acceptance now protects queue and transcript receivers and active writers |
| Eviction removes a document before verifying its final save | SQLite failure can discard the only unsaved copy; concurrent cold open can precede final flush | Trigger-injected SQLite failure reproduced loss; checked save precedes removal, cold opens are serialized, recent touches are rechecked |
| Canceling the server connection leaves spawned request tasks alive | Quiet watcher tasks can retain resources after relay retirement | Connection-cancellation acceptance failed before scoped request cleanup; passes afterward |
| Lagged network hints terminate the credential supervisor | Existing peer connections may survive sign-out after a network-event burst | Deterministic 32-event burst reproduced the failure; supervisor now treats lag as missed duplicate hints |
| A ready cross-chat result can win over an already-canceled request | Canceled operations can continue reading backend state or return a result instead of cancellation | Existing cancellation regression exposed nondeterministic selection; cancellation now has priority in both backend calls and waits |
| Recovery compares resolved turn settings with raw task defaults | “Send now” can accept a retry, then cancel it even though the user changed nothing (native case: inherited reasoning `null` versus resolved `medium`) | Native receipt/session readback and a failing production integration case reproduce it. Migration 17 adds an immutable task-configuration snapshot to the existing local execution-lineage row; retries compare with that snapshot. Existing rows retain the conservative legacy comparison. |

The transport closes a stream only after its bounded 256-item queue cannot drain
for one second. Consumers see the existing stream-end signal; transcript watchers
resubscribe from authoritative state. The UI consumer review also corrected
native-task retry handling and unsupported preview-discovery handling.

## Coverage and verification

Local artifacts: `target/qa/backend-review-2026-10-03/`.

- `rpc-before.log`: 3 failing acceptance cases and 2 passing controls.
- `rpc-after.log`: all 5 transport acceptance cases pass.
- `rpc-regressions.log`: identified the pre-existing RPC inventory failure.
- `server-cancel-before.log`: reproduced orphaned child request tasks.
- `lag-signout-before.log`: reproduced sign-out failing after network-event lag.
- `rpc-runtime-verified.log`: 37 passed, 1 explicitly ignored live relay case.
  The full UI run caught a compatibility error in the initial cleanup patch:
  GPUI can poll RPC futures outside a Tokio context. The client now retains its
  owning runtime for monitors and cancellation sends. A separate socket test
  reproduces that foreign-executor call path and passes after the correction.
- `residency-before.log`: idle cache regression failed before periodic cleanup.
- `eviction-save-before.log`: final snapshot fault reproduced unsaved-copy loss.
- `documents-final.log`: 2 document, 2 self-continuation and 4 turn-quiesce checks pass.
- `engine-unit-verified.log`: all 435 existing engine regression tests pass,
  3 intentionally ignored. Queue watches keep normal documents resident while
  completed document migrations can replace retired reader-only handles.
- `engine-final-e2e.log`: 90 checks pass after the final backend changes,
  including document residency, SQLite failure recovery, queues/attachments,
  cross-chat tools, remote document authority, publication, transcript salvage,
  workspace identity and native agent ownership.
- The final native recovery exercise found an additional defaults-comparison
  bug after that run. `recovery-defaults-before.log` reproduces the false
  `Superseded` state; `recovery-backup-verified.log` now passes all 26 recovery
  cases and 12 workspace-authority cases. The new cases cover inherited
  defaults, restart, preserved same-session continuation, and a later edit
  back to inherited defaults that must still block recovery.
- `backup-schema17-verified.log`: all 25 backup/restore checks pass. Migration
  17's execution-configuration snapshot survives restore, incomplete schema
  exports are refused, future versions remain refused, and old v9 archives
  still migrate and restore.
- `backend-clippy.log`: engine, harness and RPC Clippy checks complete
  successfully across all targets. Existing style/dead-code warnings remain.
- `final-lib-regressions.log`: after migration 17 and the final UI fixes,
  435 engine, 75 sync/store and 1,194 UI tests pass (3 engine cases are explicitly
  ignored). `final-clippy.log` passes engine/UI/RPC/sync across all targets, with
  warnings. An existing sync test helper's never-executed loop was simplified
  without changing its assertion; its 22 affected checks pass in
  `sync-helper-verified.log`.
- `headless-sync.log` + `project-store-verified.log`: headless startup,
  pairing, authenticated-socket admission and shutdown pass; sync/store and
  registry regressions pass. The project-command migration check now verifies
  its migration remains applied instead of failing whenever a later migration
  is added.
- `preview-transports.log`: local preview proxy and transport tests pass.
  The WebRTC churn test timed out waiting for a connection, before its resource
  assertions. macOS displayed an incoming-connections firewall prompt for
  `leak-9c5db5d3bb7de2e4`; no firewall exception was granted and the process
  exited. That WebRTC/memory case remains **unverified**, not a measured leak.
- `providers.log` + `claude-verified.log`: 353 provider checks pass. The nested
  agent fixture deliberately contains a real repeated instruction; its old
  expectation omitted that event. The updated test preserves it, consistent
  with the existing engine acceptance case.
- `engine-surfaces.log` + `lifecycle-fixtures-verified.log`: all 130 selected
  checks pass across files, project defaults/actions, identity, profiles,
  accounts, usage, skills, backups, terminals, recovery, inputs and publication.
- Four local-first fixtures now seed encrypted credential references rather
  than obsolete plaintext sessions. The unmanaged test executable correctly
  skips release polling. The shutdown counter excludes another running app's
  preview-discovery `HEAD /`, which cannot originate from the tested runtime
  because preview discovery excludes its own process.
- Identity fixtures now reopen archived tasks explicitly, compare source-space
  identity separately from canonical workspace identity, and remove an existing
  workspace before testing recovery from its disappearance.
- Live installed versions: Codex `0.159.0`, Claude Code `2.1.288`, OpenCode
  `2.0.14`. `live-codex.log` and `live-claude.log` each pass a real single turn.
  `live-codex-steer.log` proves a shell tool result, one accepted steer boundary,
  the corrected response, and successful completion with GPT-6 Luna.
- `engine-lifecycle.log`: drafts, end-to-end dispatch, idle archive, message queue,
  model lifecycle, pane chat, restart/resume, history, side chats and task commands
  passed. The legacy turn-quiesce fixture used explicit steering on an idle
  runtime, contrary to the current active-turn requirement. It now asserts that
  rejection and dispatches a warm follow-up before checking missing completion.

Source review covers document persistence/eviction, RPC multiplexing and remote
policy, model/composer caches, provider stream control, shutdown, run journals,
uploads, terminal history and project-command execution. Native checks also
crossed the real UI → RPC → durable receipt → recovery dispatch boundary.
Passing fixtures do not establish every live-provider or cross-platform behavior.

## Reproduction

Use the repository Cargo wrapper with `--locked`. On this checkout, bounded checks
use `LUNA_BUILD_RESERVE_GIB=5`, `10` or `15` and `CARGO_INCREMENTAL=0`, as permitted by the
build-cache guide; the aggregate guard and growth limits remain enabled.

```sh
scripts/cargo.sh test --locked -p luna-rpc --test subscription_lifecycle
scripts/cargo.sh test --locked -p luna-engine --test document_residency
```

The document residency acceptance uses real time and disposable SQLite storage.
It deliberately waits beyond the 30-second attach grace without new opens/writes.

## Frontend handoff

Claude session: “LUNA UI audit and fixes”, opened through the native app in the
Luna project on `main`, confirmed Opus 5.5 / Extra. Work is restricted to
`crates/ui` and UI assets. Its incremental handoff is
`/tmp/luna-2026-10-03-ui-audit-handoff.md`. Codex owns combined verification, app
refresh and publication.

The frontend findings and scope are in [the UI report](ui-audit-2026-10-03.md).
After the user's clarification, Codex performed every native interaction and
readback; Claude supplied UI source corrections and test instructions only.
The native evidence is under `target/qa/ui-native-final-20261003/`: persisted
toggle comparisons, pixel bounds for focus layout, modal draft/engine snapshots,
and recovery command receipts. All QA data is isolated from the primary profile.

The final “Send now” case records exactly one new retry receipt, transitions
`working/running` → `idle/completed` at attempt 1, and proves a same-session
continuation with no original attachments. Closing the last fixture window
saved a fresh draft and closed its process and IPC listener. Recorded backend
assertions can be replayed with:

```sh
node target/qa/ui-native-final-20261003/verify-native-results.mjs
```

## Practical limits

- Native GUI validation ran on macOS. Windows/Linux behavior was not exercised.
- WebRTC churn/resource assertions remain unverified after the connection
  timeout/firewall prompt described above; no exception was granted.
- Unsupported-host preview discovery and the other confirmation-dialog variants
  were reviewed in source; the native dialog exercise used Archive.
- Real-provider checks are the specific Codex/Claude runs above. Fixture coverage
  is not a claim that every provider configuration or production endpoint was tested.

## Build hygiene and handoff

The audit grew the main dev cache past the 70 GiB review threshold and blocked
build admission. No other LUNA build was active and no process had the dev cache
open. A verbose Cargo dry run enumerated only this checkout's `target/debug`;
the scoped dev-profile clean removed it. Release outputs, app/data directories,
QA evidence and the other checkout were preserved. Logs:
`dev-cache-open-files.log`, `dev-clean-preview-verbose.log`, and `dev-clean.log`.
The two completed QA bundles were unregistered and moved to Trash after their
processes exited and their evidence/binary hashes were retained.

The primary LUNA Dev refresh succeeded in **198 seconds**. The exact bundle
process was verified as PID **36839**, its listener is **127.0.0.1:59777**, and
`codesign --verify --deep --strict` passed. The primary data directory is
`target/luna-macos-dev/data`. Evidence: `primary-final-refresh.log`,
`primary-handoff.json`, and `primary-final-binary.sha256`. After rebuilding,
`target/debug` is **6.7 GiB**, `target/release` is **3.2 GiB**, and aggregate
guard accounting is approximately **91.2 GiB**, below the 170 GiB ceiling.
