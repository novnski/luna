# SW-10: scoped helper stop and readback

Baseline: `3c959fb3`. Live status and acceptance belong to the shared queue.
This file is the contract and evidence plan; it records no test result.

## Demonstrated gap

Current main already owns helper creation, read, messaging and waiting
(`create_thread`, `read_thread`, `send_message_to_thread`, `wait_threads`),
workspace views (`manage_workspace`), the Board lifecycle, and a user-side
owned-work stop. The user-side stop is `GetTaskWork`/`PreviewStopTaskWork`/
`StopTaskWork` in `crates/engine/src/rpc/task_work.rs`, shown by
`crates/ui/src/task_work.rs`.

An agent could not stop a helper it had launched:

- `CrossTaskTools::catalog` always omitted `interrupt_thread` with the reason
  "no verified cross-task command adapter is attached".
- The only agent-side stop was the legacy `CrossTaskTools::control` path. It
  needs a `CrossTaskControlGrant`, and no provider path mints one. Its
  `Interrupt` intent stops the whole target task with no launch-ownership
  check, so it stays withdrawn.

SW-10 attaches `interrupt_thread` as a scoped stop. It reuses the user-side
owned-work projection, durable receipts and dispatch. It adds no RPC method,
receipt type, store or permission grant. `archive_thread`, `open_thread`,
`fork_thread` and `move_thread` stay omitted.

## Contract against the existing control API

**Input.** `{taskId, idempotencyKey}`, with unknown fields denied. Nothing in
the input names an owner, run, work item, scope or provider ID.

**Attachment.** The tool needs verified LUNA delivery, the automatic
`threads.read` scope and exactly one local project. This is the same gate as
`create_thread` and `send_message_to_thread`. Codex sees `readOnlyHint: false`.
Claude gets it in `--allowedTools`.

**Authority chain.** Each step must pass before the next one runs:

1. **Adapter fence** (`luna_mcp::dispatch`). Checks the exact root thread and
   active turn, that the tool is attached, and that the call ID is claimed only
   once per turn.
2. **Engine claim and stop evidence** (`invoke_luna_thread_stop` and
   `prepare_luna_stop_evidence` in `sessions_work.rs`). This reuses
   `wait_and_claim_luna_provider_invocation` and the authority shape now
   shared with `prepare_luna_control_evidence` (`control_evidence_authority`).
   The caller must be the run's task and hold only `threads.read`. It must
   have one project and be on the local host. The target must differ from the
   caller and be in the same project on this host. One `control_target_snapshot`
   read checks scope and supplies the target's binding generation.

   The stop deliberately skips the send path's
   `revalidate_control_target_snapshot`. That check needs two consecutive
   full snapshots to be equal: task revision, sequence, binding and active
   turn. It fences a send's commit to the target revision. For a stop, any
   command on a busy helper between the two reads refused the call with "the
   target task changed since the captured snapshot".

   A stop is fenced on identity instead:
   - **Binding at admission.** The planned run must still be on the binding
     generation read at admission, or the stop is refused with no record.
   - **Run between plan and dispatch.** The existing run, message and binding
     fence still applies.
3. **Backend scope** (`EngineRpc::stop_owned_thread`). Runs
   `local_control_evidence_target` again, then reads the owned-work projection
   `work_rows(caller)`. The target needs a `childChat` row. The engine-recorded
   `WorkOrigin.owner_task_id` must equal the caller. The helper's current run
   must still be its initial delegated message. Anything else is refused and
   nothing is recorded. This includes a sibling, the parent, an unrelated chat,
   an ancestry-only legacy child, and a helper later reused independently.
4. **Live source.** The claimed provider call must still be live immediately
   before the stop intent is recorded (`claimed_call_is_live`). Otherwise the
   result is `Cancelled` and nothing is recorded.
5. **Durable dispatch.** `record_and_dispatch` is now shared with
   `StopTaskWork`. It records the intent as `unknown` before any side effect.
   Then `stop_work_turn` re-checks run ID, message ID and binding generation
   under the transition gate. Its interrupt holds recovery. When the host
   sees the launching turn end, it also records `luna-work-ended:<workId>`
   (run and message ID) beside the receipt.

**Exact task/run/host.** The task is `taskId`. The run is the `Control::Turn`
fence. The host is the owning-host check, and the result's `hostDeviceId`
reports it.

**Authorization and ceilings.** The trust root is the user's own task: the
engine minted `WorkOrigin` from the caller's live run when the helper was
created. A model argument, report, link or ancestry never grants stop
authority.

A stop only ends activity. It changes no grant, mode or ceiling. LUNA's user
modes are Full access, which never asks, and Auto, where the harness decides.
Like the other in-app coordination in
[agent workspace controls](../../../design/agent-workspace-controls/README.md),
`interrupt_thread` adds no LUNA approval card in either mode.

**Idempotency.** Keys are `luna-thread-stop:` + digest(caller, key). This is a
separate key space from panel requests. The stored request hash is
digest(caller, target).

- An exact retry returns the first receipt with a fresh readback, and nothing
  is dispatched again.
- The same key used for another target is a conflict, and nothing happens.
- A new attempt needs a new key.

**Result.** The result holds the existing `TaskWorkStopReceipt` (exactly one
result), a fresh `target` work row, up to 8 `remaining` owned unfinished rows
(siblings, descendants, pending result deliveries), `remainingCount` and
`hostDeviceId`. `outcome` is derived on every answer and never stored. A
dispatched stop wins over how the turn's last row reads, because a killed
turn often ends as failed:

| Receipt | Readback state | `outcome` |
| --- | --- | --- |
| rejected | completed / failed / stopped | `alreadyEnded` (ended before the stop could be dispatched) |
| rejected | unknown, or no row | `unknown` |
| rejected | otherwise | `notStoppable` (no live delegated turn yet) |
| stopped | any | `stopped` (the host saw the launching turn end) |
| requested / unknown | completed / failed / stopped | `stopped` (terminal after dispatch) |
| requested | otherwise | `stopRequested` (admitted, not confirmed) |
| unknown | otherwise | `unknown` (never retried) |

**Readback after a stop.** The work row comes from the transcript. If the
turn was interrupted before it streamed a reply, the transcript has no
terminal row. The row then reads `stopped` from the host's ended-turn record
for that exact delegation and message, never `unknown` needing attention.
This applies to the panel, agent replays and restarts.

**Presentation.** In the transcript, the tool chip reads "Threads · Stop a
thread" and its status reads "Stopping a thread". A group summary counts
"sent N stop requests", never "stopped".

## Failure contract (written before code)

| # | Credible failure | Required behaviour | Owner / proof |
| --- | --- | --- | --- |
| F1 | A helper stops its sibling, or the parent stops an unrelated or other-project chat | Refused with no receipt; the target keeps running | New E2E: sibling, unrelated and foreign cases |
| F2 | Model input smuggles authority (`ownerTaskId`, work IDs, scopes) | Input error before any claim | New E2E: smuggle case |
| F3 | The stop hits later independent work or a changed run | Refused (`independent`), or Rejected "changed before stop" | Existing `task_work` stale/independent case; the shared dispatch is unchanged |
| F4 | A stale, replayed, cancelled or sub-thread provider call admits a stop | Adapter or engine fence refuses; the source is re-checked before the durable record | Existing endpoint fence test (generic per tool name); `source_live` check |
| F5 | A retry dispatches a second interrupt, or a key is reused for another target | Same receipt; conflict | New E2E: replay and key reuse |
| F6 | An accepted stop is reported as stopped, or a confirmed stop as finished on its own | `stopRequested` until terminal evidence; a dispatched stop with a terminal row is `stopped` | Derivation above; existing `task_work` native peer covers Requested |
| F7 | A completion that won the race reports a stop | `alreadyEnded` with a `completed` readback | New E2E (deterministic completed-first case) |
| F7b | A turn stopped before it streamed a reply reads `unknown` and keeps needing attention | Ended-turn record: `stopped`, no attention, not blocking | New E2E: tool readback and `GetTaskWork` row |
| F8 | An unknown or offline outcome looks settled or is retried | `unknown`; replay returns the stored receipt | Shared crash rule (intent recorded before dispatch) |
| F9 | One stop ends siblings, descendants or the parent, or changes the draft or layout | Only the exact `childChat` turn; the others are listed in `remaining`; draft and workbench are byte-equal | New E2E |
| F10 | A late result loses or swaps its origin, or a stopped helper is reported as finished | Each `child-outcome:<child>:…` notice is in arrival order with its own kind; the stopped helper never reads as finished | New E2E: h3 then h2 `turn-completed`; h1 has only `interrupted` notices, if any |
| F11 | The tool is advertised where it cannot run | Omitted with a reason (delivery, scope, projectless, backend) | `catalog_omits_unverified_delivery_and_controls`; catalog set test |
| F12 | Output is unbounded or leaks native IDs | Titles and the remaining list are bounded, 16 KiB total; work rows carry no native IDs | `stop_thread` validation |
| F13 | Storage is unavailable and the stop still dispatches | Nothing is dispatched before the durable intent | Shared `record_and_dispatch` |
| F14 | The target is on another host | Owning-host refusal | `work_task` / `local_control_evidence_target` |
| F15 | A busy helper's ordinary progress refuses its stop as a stale snapshot | Only binding, run and ownership identity fence the stop | New E2E: three concurrent read-state command streams advance h1's revision through the FIFO task gate for the whole call; the stop still returns `stopped` |

## Acceptance criteria mapping

- **contract**: this file, plus the attachment and authority code listed below.
- **delivery**: `interrupt_thread` with `stopped`, `stopRequested`,
  `alreadyEnded`, `notStoppable` and `unknown`. Unrelated work, siblings and
  the parent keep running.
- **verification**: the E12 engine E2E below, plus native E12/E13 on Codex
  and Claude (orchestrator).
- **handoff**: orchestrator commands, exit codes and hashes, recorded here
  after the runs.

Code: `crates/engine/src/cross_task_tools.rs` (types, catalog, guide,
`stop_thread`), `crates/engine/src/sessions_work.rs` (claim and evidence),
`crates/engine/src/rpc/task_work.rs` (shared dispatch, owned stop and readback),
plus shared hunks in `rpc.rs`, `sessions.rs` and `proto/src/view.rs`.

## Commands for the orchestrator

```sh
scripts/cargo.sh check --locked -p luna-proto -p luna-engine --tests
sw10=target/qa/luna-only-workflows/sw10-$(date +%Y%m%d)
mkdir -p "$sw10"
CARGO_INCREMENTAL=0 SW10_EVIDENCE_DIR="$PWD/$sw10/e2e" scripts/cargo.sh test --locked \
  -p luna-engine --test cross_task_tools -- --nocapture 2>&1 | tee "$sw10/cross_task_tools.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test task_work --test claude_luna_tools --test agent_workspace_controls 2>&1 | tee "$sw10/owners.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --lib cross_task_tools 2>&1 | tee "$sw10/lib.log"
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-proto --lib 2>&1 | tee "$sw10/proto.log"
(cd "$sw10" && shasum -a 256 e2e/*.json *.log > SHA256SUMS)
```

The new test is `interrupt_thread_stops_only_the_exact_owned_helper` in
`cross_task_tools`. It writes `e2e/interrupt-thread.json`. That file holds the
receipts, the replay, the alreadyEnded result, the notice order, and the
draft and workbench before and after.

The outcome sweep (`sweep_child_thread`, every 2 s) reports a child only from
its assistant transcript entries. An empty Codex turn writes no entry: with no
writer and no parts, `finish_segment` returns without writing. So helpers h2
and h3 use `scenario:mcp-hold:reply`, which streams one reply before holding,
as a working CLI does. h1 uses a plain hold and is stopped before any reply.
That keeps the ended-turn readback path under test.

## Native E12/E13 (orchestrator, read-only against production)

**E12.** Use the QA identity from the QA guide and a disposable project
holding one Git checkout.

1. In a parent chat, ask for three helpers: one Codex, one Claude, and one in
   `newWorktree`. Give each a long bounded task.
2. Let two finish out of order.
3. While the third runs, edit the parent draft and switch tabs. Then ask the
   parent to stop that helper.
4. Capture the `interrupt_thread` result (`outcome`, `receipt`, `target`,
   `remaining`) and the Activity work panel (Stopped row; siblings still
   listed).
5. Check that the parent draft and selected tab are unchanged.
6. Check the transcript chip "Stop a thread" and the late `child-outcome`
   notices.
7. Ask a helper to stop its sibling: expect a refusal, with the sibling
   still running.
8. Repeat on a Claude parent.

**E13.** Use a disposable repo with a local bare remote.

1. Run the implementation and review helpers.
2. Stop the reviewer mid-run with `interrupt_thread` and restart the review
   with a new brief.
3. Commit and push through the approved delivery skill.
4. Verify both refs and that the unrelated fixture work survives.

## Limitations

- **Later follow-ups.** `interrupt_thread` stops only the delegated (initial)
  turn. A later follow-up sent with `send_message_to_thread` belongs to the
  helper itself under the slice-3 ownership model. It is refused here; the user
  stops it from that chat.
- **Grandchildren.** A helper cannot stop grandchildren created during its own
  delegated turn. They stay owned by the root.
- **Not started.** A helper whose startup is not yet observed returns
  `notStoppable`. Retry with a new key once it runs.
- **Parent notice for a stop with no reply.** A helper stopped before it
  wrote any reply has no transcript turn for the outcome sweep. So the parent
  gets no `interrupted` notice for it. The `interrupt_thread` result and the
  work panel carry its terminal state. A helper that had streamed text is
  reported as `interrupted`, as before.
- **Requested and Unknown.** Neither receipt is reproducible end to end
  without a test seam (the engine forces an interrupted turn to settle within
  3 s). The shared dispatch's Requested, Unknown and Rejected receipts stay
  owned by the existing `task_work` native-peer test.
- **Cancelled parent call.** If the parent's call is cancelled after the
  intent is recorded, the receipt may stay `unknown`. A retry with the same key
  returns it with a true readback.
- **Provider subagents.** Transient provider-native subagents are not LUNA
  threads, and this tool does not address them. The panel's native-child stop
  is unchanged.
