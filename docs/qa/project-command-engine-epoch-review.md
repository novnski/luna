# ProjectCommandPanel engine-replacement review

Source-only review of immutable `3e62cf03`. No runtime patch, build or native
interaction was performed for this review. The interleaving below is established
by source; a held-RPC GPUI integration reproduction remains required before a fix.

## The parent does not recreate the child

- `crates/ui/src/shell.rs:2888` and `:3992` return retained right/drawer terminal
  entities. Assignments to those fields occur only at initialization/first mount.
- `crates/ui/src/shell.rs:2503` handles engine-generation changes for usage and
  history, but does not drop either terminal entity.
- `crates/ui/src/terminal/panel.rs:387` creates ProjectCommandPanel once and stores
  it in the terminal's `commands` field. Its output subscription is retained too.
- `crates/ui/src/terminal/panel.rs:577` clears tabs/loading/desired-terminal state
  on an engine-generation change; it neither replaces `commands` nor cancels its
  request task or poller.
- `crates/ui/src/state.rs:2031` attaches a replacement handle and increments
  engine_generation at2040. It does not clear selected_chat in that method.
  A replacement can therefore retain the selected task ID, particularly when
  reconnecting the same workspace or replacing its backend instance.

## Concrete stale paths

1. Mount engineA with selected chatC; expand Commands. `load` captures handleA
   and sends ProjectCommandStatus (`project_commands.rs:288-303`).
2. Attach engineB while chatC remains selected. The parent clears its terminal
   tabs. The child observer (`:244-256`) compares only selected_chat, so it keeps
   the old snapshot, request and busy state.
3. Let A's response arrive. The load completion (`:304-318`) compares chatC only,
   then writes A's snapshot/error into the B-backed panel. Until a later poll,
   the visible command list, cwd and receipts describe the wrong engine.
4. The execute path (`:321-343`) has the same guard. If A returns terminalTA, it
   emits Output(chatC,TA). The parent listener (`terminal/panel.rs:390-405`) checks
   only whether chatC exists in current state, then selects chatC and requests
   TA through the current engineB. Thus an old run result can initiate a terminal
   focus/load against a different connection. Reusing the task ID is sufficient;
   terminal ID reuse is not required to demonstrate the stale side effect.
5. While an old request stays pending, busy remains true. The one-second poll
   (`project_commands.rs:258-273`) skips loads while busy, so the new connection
   cannot refresh Commands until that old request finishes. No timeout is added
   by the child itself.

An already accepted old-engine command may legitimately continue in engineA.
Dropping a UI request must not be presented as canceling that command. The defect
is stale UI state and a terminal event crossing the replacement boundary.

## Failure-first integration/native cases

Before changing runtime behavior, use two independently controllable RPC handles
and the same selected task ID in the actual GPUI entity lifecycle. Retain a JSON
trace of generation/device, request admission/completion, rendered snapshot cwd,
busy state and emitted Output events.

1. Hold A's status response; replace withB; release A success. A's snapshot must
   never become visible and B must be independently loadable. Repeat with an
   A error so an old failure cannot replace B's state.
2. Hold A's Run/Retry response; replace withB; release a success containing TA.
   No A-origin Output event may reach B's parent listener or issue a B terminal
   lookup. Inspect both the emitted event and the receiving terminal path.
3. ReplaceA withB while A never responds. The child must clear busy/cached data,
   cancel its UI request and allow a B status load. Do not infer cancellation of
   an accepted engineA command from request-task cancellation.
4. Keep the same engine device ID but replace its generation; then replace with
   a different device retaining chatC. Both are invalidation boundaries. Also
   exercise a completion ready at the same time as the state notification, so
   a guard must compare current AppState, not rely solely on observer ordering.
5. Existing ordinary navigation and same-engine status/execute/output behavior
   continue to work. Dropping the entity still cancels its one-second poller.

Native acceptance should show no old cwd/status/output after connection change;
synthetic held-RPC integration is needed for deterministic completion ordering.
No assertion should require terminating an already accepted old-engine run.

## Minimal proposed correction (not implemented)

Track the engine generation and engine device identity alongside selected chat.
On a scope change, cancel the child request and clear its transient/cache state;
reload if expanded. Capture that scope for each request and compare it to current
AppState immediately before applying a response or emitting an output event.
Carry the origin epoch/device in Output, and reject stale origin at the parent
listener before selecting a task or loading a terminal. Keep the existing
one-second timer and entity-owned cancellation. No new execution path or backend
command cancellation is required.

## Executable reproduction prepared (uncompiled)

`crates/ui/src/project_commands_engine_epoch_tests.rs` contains six held-RPC GPUI
integration cases: two delayed-status cases, same-device/different-device delayed
execute cases, stuck-busy replacement, and unchanged same-engine operation/drop.
The fixture drives real AppState.attach_engine through a test-only proxy, obtains
the actual retained child from TerminalPanel, invokes its production load/execute
methods, and observes the existing parent listener issuing LIST_TERMINALS. No
completion callback or scope predicate is mirrored. Replacement keeps the task
ID while supplying a controlled current-owner projection. B's held status response
provides a deterministic completion boundary for the execute cases.

Shared generic helper is independently available as
`3bc2fbf51e4d31cddd1b30077e69623da0ca7e4e` for PR/Defaults workers without importing
Commands runtime code. All helper/proxy/getter additions are cfg(test). Runtime
behavior is still `3e62cf03`; no epoch fence has been added.

After the coordinator grants the single warm-cache baseline slot, run:

```sh
LUNA_EPOCH_QA_ARTIFACTS="$PWD/target/qa/commands-engine-epoch-baseline" \
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-ui --lib \
commands_engine_epoch -- --nocapture --test-threads=1
```

The six JSON traces record engine generation/device, retained entity ID, snapshot,
busy/error state, emitted Output events and request/reply/cancellation envelopes.
The same-engine case must pass; the stale cases are expected to fail before any
production correction. This expectation is not a runtime observation yet.

## First Commands baseline and positive-control disposal correction

Defaults ran the six cases at combined `97804ba0`:0/6 passed. All five stale cases
reached their specific assertions and wrote traces. The same-engine load/execute/
Output assertions passed; its final disposal assertion observed3 status calls
instead of2. Original log/traces remain in the Defaults worktree at
`target/qa/project-defaults/commands-epoch-baseline-1/`.

Source review identifies a fixture ordering error at the disposal boundary:
TestAppContext.run_until_parked only drains its dispatcher, while App's effect
flush calls release_dropped_entities. Dropping Scene outside an App update leaves
the queued parent entity value owning its Commands child until a subsequent
update. Advancing the child's timer first can therefore cause one more request
before parent disposal. Context.spawn itself holds only a weak entity, and the
parent event subscription also uses weak handles; this evidence does not establish
a production ownership leak.

The source-only fixture correction drops Scene within cx.update, then checks
actual parent/child weak handles are released before advancing the same2-second
clock and retaining the original no-extra-poll assertion. A second JSON captures
both release checks and before/after request counts. No runtime fence, helper
change or stale-case assertion change is included. The corrected control still
requires its own authentic rerun before declaring the baseline valid.

## Control verified; minimal Commands correction prepared

The corrected positive control passed1/1 on combined `86111689` in Defaults'
`target/qa/project-defaults/epoch-fixed-2/commands-control.log`. Its drop trace
records parentReleased=true, commandsReleased=true and status calls2→2. The
normal same-engine load/execute/Output assertions also passed. These results were
read back before preparing the runtime correction; the earlier five genuine
stale-response failures remain preserved separately.

The source correction captures engine generation, engine device and selected
chat as a Commands scope. State observation invalidates old cache/error/busy
state and cancels the UI waiter on scope change, then loads the new scope if the
panel is expanded. Load/execute callbacks compare their captured scope against
both the retained panel scope and current AppState before changing UI state.
Output events carry origin scope; the parent checks it before selecting a chat
or requesting terminal inventory. Manual Output also checks its captured scope.
The one-second poll cadence and entity lifetime are unchanged.

Cancellation here belongs to the UI waiter. No Stop RPC or backend command
cancellation was added; accepted command execution remains owned by its original
engine. No visual redesign, helper change or assertion change accompanies this
fix. The unused AnyElement import was removed with the same source correction.
The six Commands regression cases still need an authorized fixed verification
run; source formatting and diff checks alone do not prove they pass.
