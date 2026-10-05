# Shared background work verification

Evidence lives under `target/qa/harness-evolution/slice-3/`. The failure cases
were specified in [slice-3.md](slice-3.md),
[slice-3-ui.md](slice-3-ui.md), and
[sidebar-manual-placement.md](sidebar-manual-placement.md) before their changes.

## Engine and protocol evidence

Four RPC/SQLite/protocol-peer E2Es cover nested app-child ownership, a later
independent message in the same warm child, nested native children after an
intermediate completion, shell deduplication, scoped stopping, stale previews,
service survival and stop-receipt persistence failures.
The final run passed together with the two existing native-coordination engine
E2Es. Seven Claude coordination protocol E2Es passed; one existing native-only
case remains ignored. Queue, recovery, task-command, restart and Claude child
regressions passed: 109 cases, plus one existing ignored restart case. The
existing Claude child scenarios retain their result-publication and parent-wake
checks.

The retained failures established two projection bugs before repair: a live
nested child and shell vanished after their intermediate child completed, and
an old shell observation looked live when the native session ID was reused.
Traversal now follows only engine-stamped child documents, with explicit
bounds; live status requires an observation from the current engine run.

Stop intent is saved before provider I/O. Injected SQLite failures prove zero
dispatches when that write fails and a durable Unknown outcome when a later
receipt write fails. Repeating the request, including after restart, never
replays the stop. A reused request ID with a different payload conflicts.
The final native-control fence also checks the application message identity
under the same transition lock used by dispatch.

## Signed native acceptance

`scripts/qa-task-work-live.mjs` verifies the exact bundle, PID, signature, IPC
port and data path before sending anything. The accepted run is
`native/run-OthFfz`; it used the ordinary local provider accounts and fresh
disposable tasks. No credentials were copied.

- Codex (`gpt-6.1-sol`): one native child appeared as owned, blocking work and
  reached Completed. Individual stopping was correctly unavailable in this
  adapter.
- Claude: one background Agent appeared as owned work. The exact stop returned
  Requested; a later native observation independently showed Stopped.
- Repeating that stop returned the identical durable receipt. A separate
  checkout service remained Running and did not block completion. Its own
  command control stopped it at the end.
- A root background Bash job appeared once despite both launch-tool and native
  task observations, then reached Completed.
- Each submitted message appeared once in its transcript. The work projection
  did not expose the provider session ID. The fixture checkout stayed clean.
- A graceful restart changed the app PID from 9658 to 19215. Terminal evidence
  remained intact, no cold item offered a stop, and the same stop request still
  returned its saved Requested receipt without provider I/O.

The first attempt, `native/run-ZJhU5A`, is retained as a failed QA setup:
`gpt-6-luna` ran the sleep in its parent instead of creating the requested
native child. The provider transcript and native record confirmed that action;
LUNA conservatively showed the unresolved shell observation as Unknown. A new
independent task using the other advertised Codex model exercised the intended
native-child scenario. The original attempt was not replayed.

The QA build took 94 seconds. After review, the exact app quit gracefully and
its port closed. Its signed executable hash is recorded in
`native/bundle-verification.json`. Only that disposable bundle was unregistered
and removed. Both disposable Git fixtures were checked for open files and
changes, archived and hashed before removal. QA data and the shared build cache
are preserved; `cleanup/scoped-cleanup.json` records the scope.

## UI and static checks

The production Shell sidebar-placement fixture passed with real pointer input,
saved-settings reload and 15 captures. All 1,185 existing UI library checks
passed. Clippy passed for every changed crate and all its targets, including
fixture examples. The final engine/UI pass includes the last native-stop fence.
Rust formatting, JavaScript syntax and whitespace checks are part of delivery.

The first workbench run found an old fixture assumption that new forks enter
the sidebar automatically. The corrected fixture explicitly places each fork
before checking sidebar-dependent behavior and proves the profile accepts
writes after the no-profile scenario. Its initial failure evidence is retained
under `workbench/`.

The second run passed the new Work panel checks and exposed a fixture cleanup
error at the later theme check: the intentional stale-Open notice remained over
a background sample. The fixture now clicks its Dismiss control and confirms
the notice is gone before measuring contrast. The thresholds and production
theme code are unchanged; the diagnostic capture remains under `workbench-2/`.

The final complete workbench run passed in `workbench-final/`, with 73 PNG
captures and `readback.json`. It includes focused-composer ownership, exact
selection and preview, requested-versus-stopped receipts, unknown responses
without replay, stale preview refusal, 50/100/120-row paging, pane-specific Open
and service-command routing, reconnect refusal, and all existing frame/theme
checks. The Work popover and receipt captures were visually reviewed.

## Reproduce

Run the focused engine and protocol checks through `scripts/cargo.sh --locked`
using `luna-engine` tests `task_work` and `claude_native_tasks`, and
`luna-harness` test `claude_coordination`. Set `LUNA_WORK_EVIDENCE` to an
absolute disposable evidence directory for the engine artifact files. Build
the `luna-ui` examples `chat-placement-fixture` and `chat-workbench-fixture`
with `appshots-fixture`; each takes an absolute evidence directory.

For real-provider acceptance, first quit any running LUNA GUI gracefully.
Use the [QA identity guide](../../reference/qa-infrastructure.md) with app name
`LUNA QA Work`, bundle ID `com.novnski.luna.qa.work`, port `59812` and data path
`$PWD/target/qa/harness-evolution/slice-3/native/data`. Refresh that signed bundle,
then run `node scripts/qa-task-work-live.mjs run`. After a graceful restart,
`verify-restart` reads the saved receipt. `inspect` and `finish-readback` never
send another provider message. Preserve failed attempts; do not replay an
uncertain send.

The primary LUNA Dev refresh took 4 seconds. Its exact executable (PID 47820)
owned port 59777 and passed strict deep signature verification. The evidence
manifest covers 327 files, including 189 captures from successful and retained
failed runs; the 107-file slice-1 and 258-file slice-2 manifests still verify.
