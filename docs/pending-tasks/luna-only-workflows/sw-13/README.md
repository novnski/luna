# SW-13: local recurrence and host lifecycle (E16)

Baseline: main `8758745a`, 13 October 2026. Live status belongs to
`pending_tasks.py`. The failure contract below was written before the runner
and the scheduler fix. The owners are unchanged: the host scheduler and its
RPC/store (`crates/engine/src/rpc/automations.rs`,
`crates/sync/src/store/automations.rs`), the Automations UI
(`crates/ui/src/automations.rs`), `automations_e2e` and the native runners.
There is no second scheduler and no hosted/offline execution (SW-16).

## Scheduler policy under test

These are the existing, documented policies
([slice 5](../../../design/harness-evolution/slice-5.md),
[project workflow](../../../design/automations-workflow/contract.md)). The
runner checks observed behaviour against them; it does not redefine them.

| ID | Policy |
| --- | --- |
| P1 | Each occurrence has a deterministic id from automation, revision and slot. One durable claim commits with the next due time. Its command and message ids derive from that occurrence. |
| P2 | One active occurrence per automation. Claimed, Dispatching, Accepted and Unknown block the next slot, which is recorded **Skipped** ("Previous occurrence is active or delivery remains unknown"). |
| P3 | Missed slots (host stopped or asleep) coalesce on the first sweep after start: the latest due slot runs **once**, with `coalesced` counting the folded slots, if it is within `maxLatenessMs`. Otherwise it is **Skipped** ("Missed occurrence exceeded its approved lateness tolerance"). There is no catch-up burst. |
| P4 | A target with an active turn, queued or unresolved work is **Skipped**. Scheduled work never jumps a person's queue. |
| P5 | Pause and Delete stop new delivery. They do not stop an admitted run; that run ends in its conversation. Delete keeps a tombstone and history and leaves Activity. |
| P6 | Save creates a new revision. Later occurrences use it. An occurrence of the old revision that has not crossed task admission is not sent. |
| P7 | After restart, an earlier boot's claimed or admitted occurrence without proven completion is **Unknown**. Unknown is never resent. It blocks later slots until **Resolve**, which records Resolved and also never resends. |
| P8 | Graceful quit interrupts live turns. An interrupted scheduled turn is Unknown. |
| P9 | Proven provider completion is **Completed** (the turn finished; it says nothing about the work's success). A turn that ends without proven success is Unknown. A command rejected before the provider accepted it, such as a disconnected provider account, is **Failed** with its reason. |
| P10 | Failed, Unknown and Skipped raise Needs attention (`automationFailed`). Completed is a resolved `automationCompleted`. `importantUpdates` suppresses success notifications; `never` suppresses all of them. Muting never hides the Activity row. |
| P11 | Only a running owning host claims work. A saved schedule, RRULE or next-due time is not execution evidence. |

## Failure contract (written first)

| ID | Behaviour | Credible failure | Oracle / owner |
| --- | --- | --- | --- |
| F1 | Two real occurrences for each selected destination: the current chat and a project follow-up (fresh chat per run), on Codex and Claude, in Full and Auto | Fewer than two runs. A run counted from its ledger row alone. An existing-chat run lands in a new chat, or a project run reuses one. | Runner: run rows, the transcript (exact message id once, then the reply), receipts and the sentinel file |
| F2 | Provider, model, mode and tools as saved | Another provider or model runs. Full asks a question. Auto becomes a human prompt. The mode silently degrades. | Chat config readback, sampled `execution.running`, no unresolved `input` part, exec tool parts |
| F3 | Notification policy | Success notifies under `importantUpdates`, or a failure is muted under `allRuns`/`importantUpdates` | `GetAttention`: `suppressNotification` per policy |
| F4 | Receipts | Completed without an applied Run receipt correlated to the message; a project run without an applied create receipt | `GetTaskCommandReceipt` |
| F5 | No duplicate writes | One occurrence delivered twice: the same message twice, a replay after restart, an update/delete race | Disposable sentinel lines (with epoch seconds) counted by the runner against run windows and successful exec calls |
| F6 | Update | The old prompt is delivered after the update commit; the new revision is not used; the update stalls later work | Run `automationRevision`, per-marker sentinel lines |
| F7 | Delete while admitted | Delete kills or duplicates the admitted run; a new occurrence runs after delete; the deleted definition keeps an Activity item | The admitted run ends; no later run rows; sentinel; no `automation:<host>:<id>:` item in `GetAttention` (see G4) |
| F8 | Pause | A new occurrence is claimed after the pause commit | No run claimed after the commit, except one already admitted |
| F9 | Restart across slots, within tolerance | A catch-up burst, no run, or a silent miss | One run, `coalesced` ≥ 1, claimed after relaunch, sentinel +1 |
| F10 | Restart across a slot, beyond tolerance | A late run despite the tolerance, or a dropped slot with no row | One Skipped row with the tolerance reason; the next slot runs |
| F11 | Restart with a run in flight | A replay after restart, Completed without proof, or a permanent block with no Resolve path | Unknown, never resent; Resolve; then only new slots run |
| F12 | Tool failure | History claims the tool succeeded; the delivered run is marked undelivered; an effect is recorded | Run Completed (turn finished), the failing exec part, the fixture call log, no sentinel line |
| F13 | Auth-style failure | Completed or silent; resent after the account reconnects; the failure is muted | Failed (or Unknown) with reason, Needs attention, message once, sentinel 0, later slots are new occurrences |
| F14 | A saved schedule is not execution | A saved, paused or never-run schedule counted as a run | The runner counts only run rows with transcript and sentinel proof; no rows exist while the host is down |
| F15 | Refusal at the task-command fence | Pause, edit, delete or a busy target found under the task gate after Dispatching is recorded **Unknown**, which blocks later work until a manual Resolve | `automations_e2e` (see G1) |
| F16 | An occurrence routed to a parked warm runtime | The provider turn completes but the run stays Accepted, so every later slot is Skipped as busy; or a later turn's Done settles an earlier occurrence | `automations_e2e` (see G2); live runner phases `lifecycle`/`restart` |
| F17 | A Run still starting after the 5 s acceptance grace | A Run queued in, or being started by, the session-doc executor with no turn yet is recorded Unknown ("ended without confirming provider acceptance"), raising a false Needs attention | `automations_e2e` (see G3); live runner phase `runs` |

## Demonstrated gap and fix

**G1 (F15).** `dispatch_automation` marks an occurrence Dispatching, then
waits for the task-command gate. Its fence re-checks the revision, enabled
state, target idleness and checkout under that gate. When the fence refused,
`execute_task_command_with_fences` returned before `begin_task_command`, so
no receipt and no task effect existed. The sweep nevertheless recorded the
occurrence as **Unknown** ("Delivery can't be proven either way"). That made a
pause, edit or delete at admission look like uncertain delivery, raised
Needs attention, and blocked every later slot until a manual Resolve (P2). The
existing `held_user_queue_and_pause_at_admission_prevent_scheduled_delivery`
already drives this path (it holds the real task gate, pauses, then releases
it) but asserted only that the provider was not called. Traced through the
baseline code, the row it leaves is `unknown`, with no command receipt.

Fix: both dispatch-error paths (timed and PR-watch) use one classification.
Before admission it stays Skipped. After admission, a missing receipt for the
exact Run command proves the fence refused it, so the occurrence is **Skipped**
with "Not sent: …". This also holds when a fresh conversation's create
command was applied first; the note says so. Any begun Run command, or an
unconfirmed create, stays Unknown for reconciliation. Restart reconciliation
(P7) is unchanged.

Regression: the same test now asserts the refused occurrence is `skipped`
with no command receipt, then resumes the automation and requires the next
occurrence to be delivered exactly once. On the pre-fix code it fails on the
state (`unknown`), and the resumed slot would be Skipped as blocked.

**G2 (F16), found live.** In run `sw13-live-20261013-224947` the first
`codex-full-existing` occurrence after its update stayed Accepted for over
7 minutes. Its turn had completed, the reply and the single sentinel line
existed, and every later slot was Skipped as busy. The revision only
coincided with it. Codex and Claude runtimes park after a completed turn. A
later Run whose configuration still matches is routed into the parked
runtime's mailbox (`AllowFollowUpTurn`). That starts a new provider turn
inside the same run task, confirmed by a `Steered` boundary that applies the
message's receipt. The Done handler settled automation outcomes only for the
run task's original message (`resume_state.user_message_id`), which changes
only for goal-attachment runs. So the routed occurrence never completed, and
the earlier occurrence could be re-settled by a later turn. The two earlier
runs in that chat completed, so each must have started a fresh runtime
process. Routing requires an identical runtime configuration, including the
compiled runtime contract. The second dispatch's contract differs from the
first (a resumed rather than a fresh session) and forces a replacement; the
third is the first routed one. That is inferred from the code, not observed.

Fix (`crates/engine/src/sessions.rs`, the run event loop): a confirmed,
non-historical boundary that resumes a parked runtime for an
`AllowFollowUpTurn` message records that message as the turn's owner. Done
settles the owner, falling back to the original message, then clears it. A
steer into an already active turn keeps the original owner. Recovery and
transcript identity are unchanged.

Regression: `automations_e2e::revised_existing_chat_occurrences_complete_on_a_parked_runtime`.
A steerable peer parks after each turn and confirms routed messages with
`Steered`. The first occurrence starts it. A revised prompt's next occurrence,
and the slot after it, must each reach Completed without a Skipped slot. The
peer must record them as routed follow-ups, not fresh runs. On the pre-fix
code the revised occurrence stays Accepted and the wait times out.

**G3 (F17), found live.** Rerun `sw13-live-20261014-010916` failed on the
first `codex-full-existing` occurrence, in a chat with no prior turn. It was
claimed at 22:10:36.534Z. Its Run command was queued at 22:10:42.891Z (the
user entry carries the queue time). It was recorded Unknown 20 ms later, at
22:10:42.911Z. The provider then accepted and completed the turn. Every
provider event came after the Unknown, so G2's turn-owner change could not
cause it. The reconcile sweep calls a Pending Run receipt "ended without
confirming provider acceptance" after 5 s, counted from `accepted_at_ms`, which
is the scheduler's claim tick, unless a turn is live (or, on main, the host
line still counts the start as just admitted). This dispatch spent 6.4 s
before queueing. The very next sweep saw a 6.4 s-old receipt and no turn, and
the executor had held the command for 20 ms. The same window exists whenever
the executor's preparation (project setup wait, source capture, account or
startup gates) outlasts the grace.

Fix (`crates/engine/src/rpc/automations.rs`): while the session-doc executor
still owns the exact Run command, the receipt stays Pending and the
occurrence stays Dispatching, however long startup takes. The doc entry is
Pending until `execute` has registered the turn or refused it. After that,
the 5 s grace applies unchanged. A turn that dies before acceptance is still
Unknown, and restart reconciliation is unchanged.

Regression: `automations_e2e::a_scheduled_run_still_starting_after_the_grace_is_not_unknown`.
A real account change holds provider startup (`prepare_provider_account_change`,
the lock every dispatch takes) for 6 s after the Run was queued. The
occurrence must still be Dispatching with no Needs attention, then complete
once the change ends. On the pre-fix code that reconcile records Unknown.

**G4 (F7), found live.** In `sw13-live-20261014-014727` two resolved
`automationCompleted` items, both created before the delete, stayed in Recent
after it. Tombstones are not projected, and the reconcile only resolves
unresolved rows, so a resolved row was never revisited. Fix
(`crates/sync/src/store/automations.rs`): the delete transaction dismisses that
definition's items (resolving any unresolved one first, with its transition),
and every attention reconcile re-applies this, so a projection that raced the
delete cannot leave a row. Rows are dismissed, not removed. Pause, run history,
the admitted run and task-level items are unchanged. Regression:
`automations_e2e::a_deleted_automation_leaves_activity_while_its_admitted_run_ends`
(a paused definition's item is the control). On the pre-fix code the deleted
definition's completion is still listed.

## Criteria mapping

| Criterion | Evidence |
| --- | --- |
| runs | Runner phase `runs` (F1–F5): 8 automations = {Codex, Claude} × {Full, Auto} × {current chat, project follow-up}, each recurring every `I` minutes, two completed occurrences each |
| lifecycle | Phase `lifecycle` (F6–F8, F14) and phase `restart` (F9–F11, F14). G1 regression in `automations_e2e`. |
| failure | Phase `failure` (F12, F13, F3) |
| handoff | Orchestrator: QA build, runner, Rust tests, hashes, delivery |

## Runner

[`scripts/qa-automations-lifecycle.mjs`](../../../../scripts/qa-automations-lifecycle.mjs)
drives an isolated, signed QA app over owner IPC (WebSocket JSON-RPC). It
refuses to run without all four `LUNA_DEV_*` overrides, the signed plist
identity, `LUNA_QA_MCP_ISOLATION=1`, one exact listener process and the
`LocalOwnerRootClaim` for that data directory. It never touches the primary
LUNA Dev, never answers an approval, and contacts no service except the
providers running the scheduled turns.

**Fixture.** A disposable Git project in the evidence directory. Each
occurrence runs one shell command that appends `<marker> <epoch>` to
`project/sentinels/<case>.log`, after a short foreground `python3` `time.sleep` (Claude Code blocks a chained shell `sleep`) so admitted work is
observable. The runner counts these lines itself and maps each line to exactly
one completed run's time window and one successful exec call. The tool-failure
fixture `project/fixture/tool-fail.sh` logs its call and exits 7.

**Phases** (`--phases runs,lifecycle,restart,failure`; `lifecycle` needs `runs`):

1. `runs`: one source chat per provider × mode, the project follow-up using it
   as origin. Staggered anchors 75 s after setup, interval `I`
   (`LUNA_QA_SW13_INTERVAL_MINUTES`, default 2). Waits for two completed
   occurrences per case and reads back runs, receipts, transcripts, chat
   configs, `execution.running` samples, runtime contracts and Activity.
2. `lifecycle`: pauses six cases; saves a new prompt (marker V2) on
   `codex-full-existing` and waits for its next occurrence under the new
   revision; deletes `claude-code-auto-project` while an occurrence is
   admitted and checks that the run ends and nothing follows; checks no
   paused case claims after its pause commit.
3. `restart` (needs `I` ≥ 2): three new automations. `restart-coalesce`
   (Codex Full, project, 10-minute tolerance), `restart-skip` (Claude Full,
   current chat, 20-second tolerance) and `restart-inflight` (Claude Full, its
   own chat, a 45-second foreground wait before the write; Full so no reviewer can end
   the turn early). After their first slots, the runner takes the shared
   `luna-app-operation.lock` and gracefully quits the exact QA process while
   `restart-inflight` is accepted. It keeps the app down across at least two
   slots of the other two and relaunches 45 s past a `restart-skip` slot, with
   the plist environment. The expected outcome is computed from the observed
   first claim with the sweep's own fold and tolerance rule (P3), then
   checked: one coalesced run within tolerance, one tolerance skip followed by
   a normal run, and no catch-up burst. For P7/P8 the interrupted run must be
   Unknown, never resent and never written; later slots are held; Resolve
   records Resolved; only a later slot runs.
4. `failure`: a one-shot tool failure per provider (`allRuns`), then an
   auth-style failure per provider. The provider's account is disconnected
   **in the QA app only** (`ForgetAgentAccount` changes LUNA's selection in
   the QA data directory; provider credentials are untouched). The next slot
   must fail with Needs attention. The account is then reconnected
   (`ActivateAgentAccount`) and the following slot must be a new occurrence.
   A `finally` block always reconnects.

At the end every automation the runner created is paused or deleted, and
accounts are restored.

**Evidence** (default `target/qa/luna-only-workflows/sw13-<UTC>/`, or
`LUNA_QA_SW13_EVIDENCE_DIR` inside `target/qa/`): `manifest.json`,
`events.jsonl` (every mutation attempt and receipt, quit and launch),
per-phase readbacks, the sentinel and fixture logs, `result.json` (each check
with pass/fail) and `sha256.json` over every file. The runner prints a JSON
progress line at least every 30 seconds and on every phase change. Every
wait has an explicit bound. The run deadline is
`LUNA_QA_SW13_DEADLINE_MINUTES` (default 75). Exit 0 means every hard check
passed; 1 means a check failed (see `result.json` and `failure-*.json`); 2
means a refused precondition.

## Commands

Quit the primary LUNA Dev gracefully first (one GUI app at a time). From the
repository root:

```bash
export LUNA_DEV_APP_NAME='LUNA QA Recurrence'
export LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.recurrence'
export LUNA_DEV_IPC_PORT=59861
export LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/sw13-recurrence"
python3 scripts/build-budget.py --status
scripts/run-macos-dev.sh --refresh
codesign --verify --deep --strict "target/luna-macos-dev/$LUNA_DEV_APP_NAME.app"
plutil -p "target/luna-macos-dev/$LUNA_DEV_APP_NAME.app/Contents/Info.plist"
lsof -nP -iTCP:"$LUNA_DEV_IPC_PORT" -sTCP:LISTEN
node scripts/qa-automations-lifecycle.mjs 2>&1 | tee target/qa/sw13-runner.log
```

Optional: `LUNA_QA_CODEX_MODEL`, `LUNA_QA_CLAUDE_MODEL`,
`LUNA_QA_CLAUDE_AUTO_MODEL` (default prefers Opus 5.5 for Claude Auto, as in
SW-04), `--phases restart,failure` to repeat the independent phases,
`LUNA_QA_SW13_EVIDENCE_DIR`. A failed run pauses what it created and
reconnects accounts while the app is up; `result.json` says if the app was
left down.

Expected wall time with `I` = 2: setup ≈ 1 min, `runs` ≈ 6 min, `lifecycle`
≈ 5 min, `restart` ≈ 10 min (about 4 min down), `failure` ≈ 6 min. Total
≈ 28–35 min, bounded by the 75-minute deadline.

Rust owner:

```bash
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test automations_e2e
```

## Native QA (UI readback)

With the QA app still running after the runner, open Automations for the QA
project and confirm, in light and dark: the history rows show Completed,
Skipped with "Missed occurrence exceeded its approved lateness tolerance",
"N missed slots folded in" for `restart-coalesce`, Unknown then Resolved for
`restart-inflight`, Failed with the account reason for `auth-*`, and
Deleted/Paused definitions under Show deleted. Scheduled user messages show
"Sent by scheduled task". Activity lists the attention items without
prompt or provider-error text.

## Limitations

- Machine sleep is modelled as a graceful quit and relaunch across due slots.
  Real system sleep (suspended timers, wall-clock jump without a process
  restart) is not exercised: the runner never sleeps the Mac. The scheduler
  derives due slots from wall-clock time on every two-second sweep, so the
  policy is the same, but no evidence covers the suspended-process case.
- The auth-style failure is a LUNA-side account disconnect. Real provider
  token expiry (a 401 mid-turn) is not simulated; by P9 it would end the turn
  Unknown.
- A tool failure is a completed provider turn. LUNA records delivery and turn
  completion, not whether the work succeeded; the failure is visible in the
  transcript, not in the run state.
- Auto's provider reviewer may decline a sentinel write. The runner then
  requires no sentinel line for that run and records the decline; it does not
  count it as a LUNA failure.
- Activity readback proves the notification policy at the projection. Desktop
  banner delivery is a native observation.

## Results

Pending the orchestrator's QA build and runner execution.
