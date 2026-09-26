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
