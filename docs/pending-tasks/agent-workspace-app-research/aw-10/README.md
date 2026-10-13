# AW-10: host capacity and recovery controls

Baseline: `f2e2c848` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the rebase triage, the bounded contract,
the failure cases (written before the code), the criteria mapping and the
evidence plan. Sources: [research README](../README.md),
[opportunities #10](../luna-opportunities.md#ranked-opportunities) and
[workspace scenario 7](../luna-opportunities.md#workspace-acceptance-scenarios),
[settings gap matrix](../settings-analysis.md#workflow-and-machine-operation)
("Concurrency/admission controls", "Retry maximum wait and scheduled
recovery control"), [product findings, BB 3–4](../product-findings.md).

## Rebase: every corresponding row against current main

| Research row | Current main (owner) | Verdict |
| --- | --- | --- |
| Opportunity 10 "Admission queue" / settings "Concurrency/admission controls" | `RunTable` (`sessions_runs.rs`, CORE-03) is the one owner of "may a run start", but it only refuses after shutdown. No bound on concurrently active turns exists anywhere; every send, queued follow-up, automation occurrence, Board launch and automatic retry starts immediately | **Gap** (this ticket): one host turn line in `RunTable` |
| Board admission (`board_admission`, `BOARD_HOST_CAPACITY` = 8 and project/provider caps) | Board-scoped: a Ready ticket stays in Ready with a `HostCapacity`/`ProjectCapacity` reason on its card; it bounds Board executions only | **Shipped, unchanged.** A Board launch that passes it is an ordinary send and enters the host line like any other; Board's own reason and caps are not duplicated |
| "Waiting reasons" | Per-task queue hold reasons (`QueueHoldReason`), project setup wait (`wait_for_setup`), attachment-bytes wait (`drain_commands`), automation overlap skip (`claim_automation`: "Previous occurrence is active or delivery remains unknown") | Shipped for their own causes; **reused** as the pattern (the attachment gate is the exact precedent: defer *before* the processed mark). No reason exists for "the host is full" because nothing waits for it |
| "Retry schedule/cancel" / settings "show/reset/cancel the current recovery policy" | L10 recovery (`recovery.rs`, `sessions.rs`): durable chain, bounded four-attempt/six-hour policy, `retryRecovery` (Send now), `cancelRecovery`, restart reload, unknown acceptance → `CheckingDelivery` with no timer; transcript trailer shows countdown, Send now and Cancel | **Shipped** in the transcript; owner unchanged. Gap: a scheduled retry is invisible outside its own conversation (Activity's projection drops `Scheduled`), and a retry would start regardless of capacity |
| "Unknown delivery is never silently resent" | Queue rows in `Claimed`/`DeliveryUnknown` never re-drain; recovery `Unknown` never schedules; `recovery::unknown_delivery_is_visible_and_never_replayed`, `cancellation_and_chain_identity_survive_restart_without_replay` | **Shipped**; preserved. The new line only admits work that is provably unsent |
| Shared background-work rows with scoped Stop | Activity → Running terminals (`activity_center.rs`), project command Stop | Shipped; the capacity section sits beside it in the same Activity view |
| Settings row for a capacity limit | Settings is owned by the settings/registry work (AW-01/AW-09 area) | **Out of scope** here: the limit is an engine-owned host value with its control at the point of use (Activity); a Settings/search row is a follow-up |
| Configurable retry budgets (attempts, maximum wait) | Fixed policy (`MAX_AUTO_RETRIES`, `MAX_AUTO_WAIT_MS`) | **Out of scope**: the research says to make the current policy visible before making it configurable |
| Remote/paired capacity readback | Paired clients have no capacity scope | **Out of scope**: both new rows are `localOnly` |

Real native behaviour on the dev profile: starting several tasks at once
starts every provider immediately; nothing in Activity says how many turns
the host is running; a scheduled retry is visible only inside its own
conversation.

## Contract (required)

**One line, one owner.** `RunTable` gains the host turn capacity
(`TurnCapacity`): a limit (`None` = no limit, the default, so existing
behaviour is unchanged until a limit is chosen; otherwise 1–32), the starts
admitted but not yet running, and an ordered line of waiting starts.

- **Active turns** are this engine's tasks whose status is Working,
  AwaitingApproval or AwaitingInput (a turn parked on a question still holds
  its provider), plus admitted starts whose turn has not registered yet.
- **A new turn takes a slot before anything is written or sent:**
  - a Run command for an idle task (direct send, native review, goal,
    automation occurrence, Board launch, side chat) in `drain_commands`,
    *before* the processed mark and the transcript write (the same place the
    attachment-bytes gate defers);
  - the next queued follow-up of an idle task in `drain_queue`, *before* the
    row is claimed;
  - an automatic retry in `execute_automatic_recovery`, *before* the chain
    moves Scheduled → Running.
- **While the host is full** the start stays exactly as it was (command
  pending and unprocessed, queue row Held and unclaimed, retry Scheduled)
  and joins the line in issue order. Its owner re-checks every second (queue
  rows also on every status change); a waiter not re-checked for 5 s has
  gone (cancelled, expired, removed) and leaves the line.
- **Never held:** a message to a task whose turn is already active (steer,
  queue into the live turn); a delegated child chat (its parent waits for
  it, so holding it could deadlock a full host); explicit overrides chosen
  by the person (queue row *Send now*/*Steer now*, recovery *Send now*).
  These are counted, so Activity can show "5 of 4 active".
- **Lowering the limit** never stops or interrupts running turns; new
  starts wait until the count is below the limit.

**Readback and control** (`crates/proto/src/host_capacity.rs`,
`crates/engine/src/rpc/host_capacity.rs`): two `local`, `localOnly` rows.

- `GetHostCapacity` (query) → `HostCapacity { deviceId, limit, active:
  [{taskId, status}], starting, waiting: [{taskId, kind: send |
  queuedMessage | automaticRetry, sourceId, position, sinceMs}], readAtMs }`.
  `sourceId` is the waiting command id, queue row id or recovery chain id.
- `SetHostCapacity { limit }` (command) persists `{"turnLimit": n}` in the
  profile's `host-capacity.json` (atomic replace) and returns the snapshot.
  Out-of-range values are refused.

**Cancel without pretending.** New engine-private task intent
`cancelCapacityWait { commandId }`: it claims the drain's executing slot for
that exact command, and only if the command is a pending, unprocessed Run
this host lists as waiting, marks it processed and resolves it `Cancelled`
(receipt Rejected: "Cancelled while waiting for a free turn on this host;
nothing was sent."). Anything else is refused: "no longer waiting for a
free turn; if it started, use Stop". Queued rows keep their own Remove /
Send now; retries keep `cancelRecovery` / `retryRecovery`.

**Truthful readers.** Automations and Board each turn a Pending receipt
with no live turn into Unknown after a 5 s grace. Both now treat a task
whose send is waiting in the line, or was admitted from it less than 5 s
ago, as still pending (`SessionsEngine::turn_start_pending`), so a held
send is never reported as an unconfirmed delivery while a start that hangs
after admission still becomes Unknown under their unchanged rule.

**Activity** (`crates/ui/src/shell/activity_capacity.rs`, owned by the
Activity view): a *Host capacity* section that reads `GetHostCapacity` with
the Activity poll:

- "3 of 4 turns active · 2 waiting" or "2 turns active · no limit";
- limit chips *Off 1 2 4 8* (and the current value when another was set
  through the API) that call `SetHostCapacity`;
- one row per waiting start: task, what is waiting ("New message",
  "Queued message", "Automatic retry"), its place in line and how long it has
  waited; *Cancel* for a send (`cancelCapacityWait`), *Open* for a queued
  row;
- one row per scheduled retry of this device (from the session's
  `TaskRecovery`): "Retry in 40s · attempt 2/4" or, when held for capacity,
  its place in line; *Send now* (`retryRecovery`) and *Cancel*
  (`cancelRecovery`), the same intents as the transcript trailer;
- an older engine (unknown method) hides the section; replies from a
  replaced engine are dropped (generation fence).

## Failure contract (written before the code)

| ID | Failure | Defence | Owner test |
| --- | --- | --- | --- |
| C1 | A full host starts another new turn (send, queued row, retry) | Every new-turn path takes a slot from the one line before provider I/O | E2E `full_host_holds_new_turns_with_a_reason_and_cancel_withdraws_them_unsent`, `queued_follow_up_waits_unclaimed_while_the_host_is_full`, `automatic_retry_waits_for_a_slot_and_cancel_survives_restart` |
| C2 | Two starts race past the bound | Admission, the starting count and the line change under one lock; a start counts until its turn registers | by construction; E2E (two waiters admitted one at a time, in order) |
| C3 | A held send is written, claimed or marked processed, so a restart makes it "delivery unknown" | Gate before the processed mark and transcript write; queue rows stay Held; retries stay Scheduled | E2E `held_sends_survive_restart_and_are_delivered_exactly_once` |
| C4 | Waiting shows only a spinner | `GetHostCapacity` lists kind, place, age and the count against the limit; Activity renders them | E2E readback; UI `activity_capacity_renders_counts_reasons_and_controls` |
| C5 | Cancel claims a send stopped although it started, or reports success while a provider runs | Cancel claims only an unstarted, unprocessed, listed Run under the drain's executing claim; otherwise it is refused and says to use Stop | E2E (second cancel refused; cancel after the slot freed refused) |
| C6 | A cancelled held send runs later (slot frees, restart) | Marked processed and resolved Cancelled before the claim is released | E2E (slot frees, restart: never dispatched) |
| C7 | Unknown delivery is replayed when a slot frees | Only provably unsent work enters the line: non-Held rows return before admission; `CheckingDelivery` chains have no timer | existing `recovery::unknown_delivery_is_visible_and_never_replayed`, `message_queue` suite (unchanged) |
| C8 | A delegated child waits behind its own parent and deadlocks a full host | Child chats are admitted (and counted) | by construction; review |
| C9 | A message to an already-running task waits | A task whose turn is active is admitted without a new slot | by construction |
| C10 | Lowering the limit stops running work or says it stopped | Lowering affects only new starts; the snapshot shows active above the limit | E2E (limit lowered below active) |
| C11 | Automations or Board mark a held send Unknown after their 5 s grace | Both readers check `turn_start_pending` | review (one condition each); E2E follow-up in `automations_e2e` |
| C12 | A stale waiter blocks the line forever | Owners re-check every second; a waiter not re-checked for 5 s leaves | E2E (cancelled waiter no longer ahead of the next) |
| C13 | Restart loses or changes the limit | Persisted per profile, read at assembly | E2E restart test |
| C14 | A paired/remote client reads or changes capacity | `localOnly` catalog rows | contract test |
| C15 | The limit blocks an explicit Send now | Explicit overrides are admitted and counted | by construction; documented |
| U1 | A late reply from a replaced engine overwrites the section | Engine-generation fence on every reply | UI test |
| U2 | An engine without the method shows an error | Unknown method hides the section | UI test |

## Required vs optional follow-ups

Required here: the line, the two RPC rows, `cancelCapacityWait`, the
automation/Board grace exemption, the Activity section, the tests above and
this document.

Optional, separate tickets (not started): a Settings/search row for the
limit (settings registry owner); a composer/transcript hint for a held send
(composer owner; today the composer keeps its sending state and Activity
carries the reason); configurable retry budgets; per-provider or per-project
limits; paired-client capacity scope; a durable line order across restarts
(today the first start re-checked after a restart may take a free slot);
an `automations_e2e` scenario for C11.

## Criteria mapping

- **rebase:** the triage table above. Shipped owners are reused, not
  rebuilt: the run table (CORE-03) owns the line; recovery chains, Send now
  and Cancel stay L10's; queue rows keep their own hold/claim states; Board
  keeps its own admission; Activity keeps its rows and terminals.
- **scope:** the contract, the failure table and the follow-up split above,
  written before implementation. With no limit (the default) nothing waits,
  so existing behaviour is unchanged until a limit is chosen.
- **verification:** `crates/engine/tests/host_capacity.rs` (real
  task-command, queue, recovery and session paths on a disposable profile,
  synthetic provider released by the test, restart on the same profile) and
  the Activity owner's rendered tests in
  `crates/ui/src/shell/activity_capacity.rs` (held memory RPC, real AppState
  attachment, painted selectors and the exact commands the controls send),
  plus the native QA below. Unchanged owners stay covered by their suites:
  `recovery`, `message_queue`, `queue_preflight`, `automations_e2e`,
  `board_lifecycle_e2e`.
- **handoff:** the orchestrator runs the commands, QA and delivery below.

## Files

Owned: `crates/proto/src/host_capacity.rs`,
`crates/engine/src/rpc/host_capacity.rs`,
`crates/engine/tests/host_capacity.rs`,
`crates/ui/src/shell/activity_capacity.rs` (section and its tests), this
file. Capacity code in the run-table owner `crates/engine/src/sessions_runs.rs`
(`TurnCapacity`, appended).

Shared, additive hunks:
- `crates/proto/src/lib.rs`: module and re-export.
- `crates/rpc/src/lib.rs`: two constants and `ALL`.
- `crates/rpc/src/operations.rs`, `crates/rpc/contract/v1/operations.json`:
  two rows after `MutateAttention`.
- `crates/engine/src/lib.rs`: `set_host_capacity_path` at assembly.
- `crates/engine/src/sessions.rs`: re-export; capacity accessors after
  `set_recovery_settings_path`; `arm_recovery` gains `arm_recovery_after`;
  the retry gate and `hold_retry_for_capacity` in
  `execute_automatic_recovery`; `publish_committed_recovery` leaves the line
  for a non-Scheduled chain. Completion tracking (SW-13) is untouched.
- `crates/engine/src/doc_host.rs`: `HELD_SEND_CANCELLED`; the send gate in
  `drain_commands` (after the attachment gate); the queue gate in
  `drain_queue` (before the claim); `admit_turn_start`,
  `arm_capacity_wait`, `cancel_held_send` after `awaiting_attachments`.
- `crates/engine/src/rpc.rs`: `CancelCapacityWait` intent (variant, name,
  binding-fence exemption, handler); submodule and route.
- `crates/engine/src/rpc/automations.rs`,
  `crates/engine/src/board/lifecycle/drive.rs`: one condition each in the
  5 s grace.
- `crates/ui/src/shell/activity_center.rs`: section module, two
  `ActivityUi` fields, refresh on the Activity poll, `render_host_capacity`.
- `crates/ui/src/shell.rs`: four lines in the sidebar list.
- `.github/workflows/ui-tests.yml`: one engine test step.

## Commands (orchestrator)

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-ui
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract   # must leave operations.json unchanged (2 rows added by hand)
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed
EVIDENCE="$PWD/target/qa/aw10/$(date -u +%Y%m%dT%H%M%SZ)"
LUNA_AW10_EVIDENCE_DIR="$EVIDENCE" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test host_capacity -- --nocapture
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test recovery --test message_queue --test queue_preflight \
  --test automations_e2e --test board_lifecycle_e2e --test restart_resume
LUNA_EPOCH_QA_ARTIFACTS="$EVIDENCE/ui" \
  scripts/cargo.sh test --locked -p luna-ui --lib shell::activity_center::capacity::tests -- --nocapture --test-threads=1
scripts/cargo.sh clippy --locked -p luna-proto -p luna-rpc -p luna-engine -p luna-ui --all-targets
scripts/run-macos-dev.sh --refresh
(cd "$EVIDENCE" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256 > SHA256SUMS)
```

The engine suite writes `full-host-line.json`, `restart-held-sends.json`,
`retry-held-for-capacity.json` and `queued-row-held.json`; the UI tests write
`aw10-activity-capacity.json` and `aw10-activity-capacity-engines.json`. The
retry test waits for the real overload back-off (5–6.25 s) and every
"stays held" assertion waits 2.5 s (several one-second re-checks), so the
engine suite takes roughly 30–40 s.

## Native QA (LUNA Dev)

Use two disposable tasks in a scratch project on a real provider; each step
is a screenshot of the Activity view (bell in the sidebar) into
`$EVIDENCE/native/`.

1. Open Activity: *Host capacity* reads "N turns active · no limit", the
   *Off* chip is selected, and nothing else changed.
2. Choose **1**. In task A send a turn that runs for a while (for example
   "run `sleep 90` in the terminal, then reply done"). Activity reads
   "1 of 1 turns active".
3. In task B send "hello". It does not start: B's transcript shows no new
   message, and Activity lists *B · New message · waiting for a free turn ·
   1st in line · Ns* with **Cancel**.
4. Press **Cancel**: the row disappears, B's send ends as not sent with
   "Cancelled while waiting for a free turn on this host; nothing was sent."
   (record what the composer shows: draft kept or restored), and B's
   transcript still has no new message. Send it again so it waits.
5. Stop A (or let it finish): within about a second B starts. Activity
   reads "1 of 1 turns active", nothing waiting.
6. Queued row: choose **2**, start long turns in A and B, queue a follow-up
   in A, choose **1**, and stop A's turn. The follow-up stays in A's queue
   (not "sending") and Activity lists *Queued message · waiting…* with
   **Open**, which opens A. Stop B: A's follow-up starts once.
7. Restart: with B held behind A (limit 1), quit LUNA Dev normally and
   reopen it. Activity still shows limit 1. Open B: it starts once A's slot
   is free (A's interrupted turn is not resent), and B never shows a
   delivery-unknown state. Record the persisted limit:
   `find target/luna-macos-dev/data -name host-capacity.json -exec cat {} \;`.
8. Choose **Off** at the end; Activity reads "… no limit".

## Limitations

- The limit applies per profile engine on this device (`host-capacity.json`
  in the profile store); two profiles running at once each have their own.
- The line order survives a restart only as issue order among starts that
  re-check; after a restart a held send rejoins the line when its task's
  document opens (viewing the task, or any change to it).
- While a send is held its composer keeps its sending state; the reason is
  in Activity (composer hint is a follow-up).
- Explicit Send now (queue row or retry) and delegated child chats are never
  held; Activity can therefore show more active turns than the limit.
- Automations and Board keep their restart rule: a send that was pending
  when the engine stopped is reported unconfirmed after the restart even if
  it then waits and is delivered (pre-existing, also true of attachment
  waits).
- Session commands keep their order: a later command of the same task (for
  example a second send) waits behind its held send, as with the existing
  attachment-bytes wait. Cancel in Activity withdraws the held send; a
  composer-side Cancel is part of the composer follow-up.
