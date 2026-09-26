# Same-chat Commands context: separate failure-first reproduction

Parent runtime checkpoint a679a411 and its accepted engine/busy results remain
unchanged. Initial candidate81bea5bb is preserved; this revision corrects only
its test design. No project/cwd scope extension or new build is implemented.

Six GPUI cases use the validated retained-panel/held-RPC/real-mouse machinery:

1. Paint project-A Run, then apply the projection of a supported local move
   from /repo/sub to /repo, both containing cwd /repo/sub in the same checkout.
   Engine/device/chat stay fixed; both projects use action ID build but different
   command text. The stale UI action must not dispatch into the new context.
2. Hold A's status across that move. Its old project/actions must not replace
   current data; the current context must remain readable.
3. Paint Run before same-chat worktree cwd/checkout materialization, then deliver
   the real painted callback with the new live task context. Reject stale UI
   carryover without claiming the backend's new authoritative cwd is incorrect.
4. Hold pre-materialization status; release after the projection. Old cwd/status
   must not overwrite current context.
5. Fresh-context controls for both transitions read current data and dispatch a
   freshly painted action exactly once.
6. Title/project-label-only metadata changes preserve current status and Run.

Live engine generation, engine device, task ID and owning device are recorded
and asserted individually as stable. Project move changes project ID while cwd
and checkout stay stable; materialization changes cwd/checkout while project
stays stable. No equality of the UI implementation's whole scope representation
is required. Stale reply/current-snapshot and no-old-dispatch assertions remain.
UI paths are real temporary directories and controlled registry projections;
the backend companions establish the product-path preconditions separately.

## Separate UUID backend companions

Every task, task command and Run message gets a real generated UUID. CreateChat
uses memory RPC EXECUTE_TASK_COMMAND; no non-UUID shortcut is permitted. Engines
use local profiles, synthetic account homes and no native provider metadata.

`uuid_project_move_uses_current_action_and_preserves_task_cwd` independently
exercises supported overlapping-root MoveProject and current-target commands.
It captures managed identity before/after and checks normal Move R advancement,
stable canonical task cwd/device/ID, changed current project, destination command
selection for the same action ID, and existing old-receipt Retry/Stop rejection.
Workspace's own target owns the deeper origin workspace/source-space/binding/W0
invariants; this companion records their full diagnostics without conflating a
Workspace identity repair failure with a Commands UI-scope failure.

`uuid_materialization_dependency_uses_real_run_path` is separately dependent on
the pending Workspace/Lifecycle atomic pending-Run metadata bridge. It creates
UUID target+sibling tasks, verifies first-run target has no current/historical
binding, then sends actual memory-RPC EXECUTE_TASK_COMMAND intent session→run
with WorktreeSpec and fresh expectedTaskRevision. It does NOT use direct
DocHost.queue_command, SetCwd or model_options/W tricks. A recording Mock harness
captures the actual dispatched cwd. Expectations are strict: Applied receipt,
mock cwd = Chat.cwd = managed workspace currentPath, binding.workspaceId matches
task workspaceId, sibling remains at the original root, and a fresh Commands Run
uses the current materialized cwd. Root/identity data and submission/final receipts
are saved to uuid-materialization-preassert.json BEFORE assertions. A parent child-
process wrapper captures stdout/stderr and exit status even on failure, with its
own temporary LUNA_WORKTREES_DIR. Physical root creation alone is not success.

Workspace contract was coordinated with owner01a0db2e-8f58-7372-946e-5bc6e4f7288e.
Its prewritten49e8938ef408e34446371c22d38ce0618ff552e1 target uses this exact UUID/RPC
shape. Runtime repair was source-only and had no verified immutable SHA at this
preparation. WAIT for the coordinator's composed Workspace+Lifecycle checkpoint
before running the materialization companion; do not weaken resume validation or
change the ID/path to make it pass. The coordinator owns that dependency decision.

## Review/run boundaries

These eight cases are source-prepared, not compiled or executed. Review and a
build slot must precede any run. Original81bea5bb files remain in artifact copies.
No active source or existing accepted candidate was changed while preparing this
revision in an isolated index.

Independent Move control (when granted):

```sh
LUNA_COMMAND_QA_ARTIFACTS="$PWD/target/qa/commands-same-chat-baseline" \
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test project_commands_context uuid_project_move_uses_current_action_and_preserves_task_cwd \
  -- --exact --nocapture --test-threads=1
```

Materialization dependency case (only on reviewed composed runtime and grant):

```sh
LUNA_COMMAND_QA_ARTIFACTS="$PWD/target/qa/commands-same-chat-baseline" \
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test project_commands_context uuid_materialization_dependency_uses_real_run_path \
  -- --exact --nocapture --test-threads=1
```

UI baseline:

```sh
LUNA_EPOCH_QA_ARTIFACTS="$PWD/target/qa/commands-same-chat-baseline" \
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib \
  commands_same_chat -- --nocapture --test-threads=1
```

Fresh/metadata controls must pass before interpreting stale-case failures. No
expected failure is a measured result; correct current-target backend execution
must never be labeled a Commands materialization defect.
