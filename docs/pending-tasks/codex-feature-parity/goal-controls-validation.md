# Goal controls and continuation validation

Implementation branch: `codex/parity-goal-controls`. Runtime validation belongs to the coordinator; the feature worker ran formatting and static checks only.

## Contract and behavior

The inspected installed binary is `/opt/homebrew/Caskroom/codex/0.159.0/bin/codex`. Its matching source tag `rust-v0.159.0` provides stable `thread/goal/set`, `thread/goal/get`, `thread/goal/clear`, `thread/goal/updated` and `thread/goal/cleared`. `ThreadGoalStatus` uses camelCase, including `usageLimited` and `budgetLimited`. Set parameters distinguish omitted token budget (preserve), null (remove), and positive integer (set). Goal snapshots report provider token/time usage and creation/update timestamps.

LUNA exposes an explicit goal editor and status row below the composer on existing Codex chats. Controls work after the task has an accepted native thread binding. Imported provider-owned tasks permit readback; mutation requires adoption. Creating a new task's native thread solely from a goal is outside this patch: send its first ordinary message before setting the goal. Ordinary messages never imply a goal.

A live control targets the registered `(CODEX_HOME, threadId)` app-server connection. Cold controls use the account-bound metadata client without loading/resuming the native thread, and report that continuation waits for the next message. The provider owns all goal scheduling. LUNA never invokes `turn/start` to continue a goal and never completes a goal because a turn ended. User controls can pause, resume, mark achieved, update objective/budget, clear and refresh. The model's installed native goal tools retain their own creation/pause/blocked/completion rules.

Readback is durable task-local provider state transported in `contextControl.goal`. Its `checkedAtMs` and observed subscription readiness are visible in the UI; it is a cached observation until another notification or refresh. A stopped warm subscription marks readiness false without inventing a new provider timestamp. Context clearing/provider switching resets the projection with the accepted binding boundary. A crash can leave the previous observed readiness cached until refresh, so the UI labels it as the last observation rather than current runtime proof.

`ProviderTurnStarted` bypasses the parked-output delay for an authoritative Codex turn. It rearms the accepted binding/turn fence, opens a new segment and starts the existing Git checkpoint path for autonomous turns. Child traffic and late starts for completed turns cannot reopen the parent. Native goal notifications are consumed before the parked gate and do not set the task Working.

Goal commands reuse the task-command owner, revision and binding fences. Goal mutations are not safe local projection retries. A timed out/cancelled/disconnected control remains delivery unknown and is never replayed against a cold connection. A correlated native rejection is failed, and a later explicit refresh can resolve state. Readback and an Applied receipt commit atomically while preserving unrelated provider-control fields.

## Failure scenarios

The pre-implementation list is retained outside the clone at:

`/Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/evidence/goal-controls/failure-scenarios.md`

## Coordinator verification

Run from the coordinator checkout after applying the branch. Use the existing target/cache and build guard. Preserve normal project profile and flags; do not start parallel Cargo jobs.

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-ui
scripts/cargo.sh test --locked -p luna-harness --test codex
scripts/cargo.sh test --locked -p luna-engine --test task_commands
scripts/cargo.sh clippy --locked -p luna-ui -- -D warnings
```

Focused existing suites verify adapter lifecycle and task-command ownership/receipt behavior. No unit tests were added after implementation. The meaningful feature proof is the live end-to-end scenario below plus native UI readback.

Refresh primary LUNA Dev from the coordinator checkout and verify its exact process, IPC listener and strict signature per AGENTS.md. Then run the script against that already-running app with an available explicit model:

```sh
LUNA_GOAL_QA_MODEL=gpt-6.1-sol LUNA_GOAL_QA_REASONING=high node scripts/qa-goals-live.mjs run /Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/evidence/goal-controls/live-01
```

The script never builds, launches/quits an app, makes a worktree, or removes data. `LUNA_GOAL_QA_APP`, `LUNA_GOAL_QA_PORT`, and optional `LUNA_GOAL_QA_ACCOUNT` select an already-running exact QA identity or named account. It verifies process/port/signature before calls. Use a fresh evidence directory on every run. It retains command inputs/receipts, native binding, full continuation timelines, goal usage and checkpoint readback. A model refusal or early completion is a failed live scenario, not a fixture pass.

Expected automatic checks: no inferred goal from bootstrap; paused creation; objective update preserves status/budget; explicit budget removal; active goal continues for two native turns while warm; separate transcript segments and history checkpoints; no synthetic user messages; provider usage and final goal completion; explicit pause and completion controls.

For reopened history, save/quit the chosen app gracefully, reopen that same data/identity through the normal coordinator workflow, then run:

```sh
node scripts/qa-goals-live.mjs snapshot /Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/evidence/goal-controls/live-01
```

## Native UI and boundary scenarios

Retain labeled screenshots/accessibility readback and their hashes next to the live artifacts.

1. Open the script-created Goal QA chat. Verify both STEP markers, Complete, token/time accounting, and the last-readback timestamp. Open Edit goal: check objective and exact budget. Enter zero, negative and non-numeric budget values; verify a visible error and no new native mutation receipt. Cancel leaves the main message draft unchanged.
2. Pause, change objective/budget, then save. Verify status remains Paused and no next goal turn starts. Blank budget removes the cap. Resume explicitly, and verify native turns appear immediately even when the preceding Done was less than one second earlier. Mark complete only after its objective is achieved; Clear goal produces a native no-goal readback.
3. Navigate away with an editor open, then back. Saving must target its original chat only. Observe a failed/unknown receipt: error must remain visible and cached data must still be labeled as the last observation. Refresh after reconnect; do not repeat a mutation with a new ID to hide the unknown outcome.
4. Exercise an inherited active goal by adopting/resuming a native thread with a pre-existing goal. Verify matching native creation timestamp/objective, no second goal creation and actual provider continuation. Read-only imported tasks may Refresh but mutation remains rejected until adoption.
5. During a longer native continuation, queue an ordinary marker message through the usual queue control. Keep the goal incomplete until that marker is incorporated. Verify one user entry/one confirmed queue receipt, continued native binding, provider usage, and checkpoints for every turn. Queue promotion may steer the live goal turn or become the next normal turn; it must not start a competing goal scheduler.
6. On a disposable fixture only, gracefully stop its runtime during a continuation and exercise the existing recovery UI. Verify goal readback is retained, status is not automatically complete, accepted native control receipts are not replayed, and resume uses the same provider binding. An interrupted/budget/usage-limited goal must show its actual native status. Refresh a cold goal: subscription readiness is false until the next message loads a runtime.
7. Clear context or switch provider when idle. Verify the old binding's goal vanishes from the visible current projection while historical transcript/checkpoints remain. Run goal controls under two named accounts and verify each account/home's native thread identity; same string thread IDs must not collide in the live connection registry.

After review, preserve/hash evidence, stop only the exact disposable QA runtime gracefully, and let the coordinator perform approved fixture/checkout cleanup. Preserve the primary app, user data and shared caches.
