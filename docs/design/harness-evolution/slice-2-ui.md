# Slice 2 UI: portable context transfer and Send findings back

Status: **implemented in UI against `crates/proto/src/context_transfer.rs`**
(capability `context-transfer-v1`), including PortableFork via `newTarget`. Host matrix: [slice-2.md](slice-2.md). UI owner: the established Claude conversation
(`crates/ui` only). Root owns engine/proto/storage, builds, E2E and delivery.

## Failure scenarios (written before any implementation)

Provider switch with context:

1. The UI builds or edits the transferred text itself (today's
   `recent_visible_handoff` in `pickers.rs`), so what is shown differs from
   what the host sends, or a remote host receives a locally built payload.
2. "Continue with context" is preselected, or Enter/the primary button picks
   a mode the user didn't choose; "Start fresh" silently carries context.
3. The preview omits that history was left out, or presents an unknown model
   capacity as a known budget.
4. The conversation advances (a turn finishes, another window sends, a queue
   drains) while the preview is open and the stale preview is sent; or
   refreshing clears the user's draft, picked provider/model or queue.
5. A commit whose acceptance is unknown is shown as delivered or failed,
   offers Retry, or is resent automatically (duplicate injection).
6. Returning to an earlier provider resends history it already received,
   or the UI claims "only new messages" without the host's cursor proof.
7. Hidden reasoning, tool output, approval payloads, attachments, secrets or
   provider-native ids appear in the preview, receipt or readback.
8. A preview or commit result from an earlier engine connection/profile, or
   for another chat, installs into the current dialog.

Send findings back (fork → its source/parent):

9. The action is offered for a chat with no source, for Main itself, for a
   New thread (no captured context), or reads like a Git merge; files,
   branches or the index change.
10. It sends to the wrong destination (the workbench Main, the focused tab or
    a same-titled chat) instead of the fork's lineage source.
11. The parent advances while the dialog is open and the old preview is sent;
    the parent's draft or queue is overwritten; a turn starts in the parent
    without the user asking.
12. A double click, retry or reopen sends the same findings twice; a second
    send repeats findings the parent already received without saying so.
13. Deleting the source hides or breaks the surviving fork, its tooltip or
    an already captured transfer receipt; or the action stays enabled for a
    deleted destination.
14. Long excerpts, many omitted items or a long reason overflow the dialog or
    become translucent; keyboard focus escapes the dialog or Escape reaches
    stop handling.

Receipts and history:

15. A transfer receipt is missing from the destination's transcript, names
    the wrong source/kind, or survives as "Planned" after the host settled it.
16. Omitted items can't be found (no list, no jump), or a jump targets a
    deleted chat without saying it is unavailable.
17. The workbench, Related chats or Execution card disagree about a
    transfer's state.

## Proposed surfaces (reuse, no new pane)

1. **Provider switch panel** (`pickers.rs` render, "Switch to {provider}?",
   ~line 5767). Replace the "Include up to 8 recent visible messages" toggle
   with two explicit, unselected choices: **Continue with context** and
   **Start fresh**. Choosing Continue requests the host preview and shows:
   included/omitted counts, bytes against the host budget and its source
   (model capacity or default; unknown stated as unknown), a scrollable list
   of included items (role, short excerpt), omitted reasons, and an
   expandable "Exactly what will be sent" text. The confirm button names the
   choice ("Switch and continue" / "Switch fresh") and is disabled while the
   preview loads, errors or is stale. Stale: "The conversation changed.
   Refresh the preview." Older hosts (no capability): the current toggle path,
   unchanged and labelled as the older behavior; nothing is fabricated.
2. **Send findings to {source}…** for a fork, in three existing menus: the
   workbench tab menu (`workbench/render.rs` next to "Go to source chat"),
   the chat context menu (`shell.rs` next to "Create side chat from here…"),
   and the Related chats row menu. It opens a `popover::dialog_card` dialog
   (same pattern as the history fork dialog): fork → destination titles,
   "Conversation context only. Files and Git changes are not merged.", the
   host preview of the fork's new findings since its fork point or its last
   send-back, budget/omitted lines, the parent's state (running → "delivered
   with the parent's next turn" if the host says so), and **Send findings** /
   Cancel. No auto-start in the parent.
3. **Receipt** in the destination transcript at the boundary: a compact
   divider such as "Switched to Claude · continued with 6 of 14 visible
   messages" or "Findings from {fork} · 4 messages", with a **Details**
   popover: kind, source title (or "a deleted chat"), state
   (Planned / Delivered / Unknown / Failed), included and omitted items, the
   host's reason. Unknown shows "Delivery unknown. It will not be resent."
   with **Check status** (re-reads the receipt; never re-injects).
4. **Omitted history**: in Details, omitted items with their reason and
   **Show in source** (scrolls/opens the source transcript at that message;
   "unavailable" when the source was deleted). Agent-side retrieval stays a
   host capability; the UI only says whether it is offered.
5. **Execution card** (slice 1) gains one "Last context transfer" line from
   the same projection, so the three surfaces agree.
6. "Start new task with context…" (`shell.rs` `start_new_task_with_context`,
   also client-built today) can move to the same host preview later; listed
   here so it is not left as a second client-built path.

## Existing hook points

- `Pickers::send_provider_switch` / `confirm_provider_switch` and the
  `provider_switch_target` render block (`pickers.rs`); `context_error`.
- `Shell::mutate_task_reviewed` with `TaskCommandSnapshot` + host review
  payload + `HistoryEngineOrigin` (engine generation): the stale-preview and
  earlier-connection guard pattern, already used by history fork/recovery.
- `task_commands::execute` and command receipts (`Pending/Applied/Rejected/
  Conflict/Superseded/Failed`); `GET_TASK_COMMAND_RECEIPT` for Check status.
- `AppState::context_control` (`ContextControlState.last_boundary`,
  `pending_handoff`, `last_handoff`), per-pane `context_control`.
- `state.fork_source_rows` / `side_chat_rows` / `tab_origin` for the fork's
  lineage source; `tab_help` for deleted-source wording.
- Workbench tab menu, chat context menu, Related chats rows; the history
  fork dialog as the dialog template; `popover::tooltip_surface` cards.

## Contract needs (for root's proto)

1. A read-only **preview** request: kind (`providerSwitch` | `forkFindings`,
   later `newTask`), source task, destination task (and target provider/model
   for a switch). Response: `previewId`, `payloadHash`, the source and
   destination `TaskCommandSnapshot` basis, items (message id, role, bounded
   excerpt, bytes, included or omitted + reason), included bytes, budget and
   budget source (`modelCapacity` | `default` | `unknown`), whether this is a
   delta and from which earlier transfer, the exact bounded text to be sent,
   and an optional host note. Visible user/assistant text only.
2. **Commit** through task commands, carrying `previewId` + `payloadHash`
   (+ basis): `switchProvider { config, transfer: {…} | null }` and
   `sendFindings { sourceTaskId, destinationTaskId, previewId, payloadHash }`.
   The host refuses a changed basis as `Conflict` (UI: refresh), and the
   command id makes a double click idempotent.
3. A **transfer projection** on both tasks (WatchChats row or
   `ContextControlState`), bounded: transfer id, kind, source/destination ids
   with a captured source title, state (`planned` | `delivered` | `unknown` |
   `failed` | `consumed`), included/omitted counts and bytes, created and
   settled times, host reason. It must survive source deletion.
4. **Omitted items** read: by transfer id (paged), each with message id,
   role, excerpt, reason and whether its source is still available.
5. A **capability** string for transfer preview so older hosts keep the
   current path, and the fork's eligibility (has a lineage source, source
   alive, destination provider) from the host rather than UI guesses.
6. Delivery semantics for findings: how they appear in the parent (an
   attributed context entry, not a user prompt) and whether they wait for
   the parent's next turn; whether a running parent queues them.

## Fixture plan (after the proto)

Extend `chat-workbench-fixture` (forks, workbench menus, deleted sources)
and `composer-parity-fixture` (provider switch panel): explicit choice with
no default, Enter safety, preview counts/omitted/budget text, stale basis →
refresh with draft/provider/queue preserved, earlier-connection result
refused, unknown → no retry + Check status, receipt divider and Details,
omitted jump (alive and deleted source), Send findings from a Side tab only,
destination = lineage source, parent advanced → refresh, double send once,
deleted source → fork and receipt intact, action unavailable; long text in
opaque cards; no native ids or hidden content in any readback.

## Later slices kept in view

Shared background work and stop scope (status in the workbench strip and
Execution card, Stop current turn vs task-owned work), durable app schedules
and PR watches (read-only ledger first, Activity/attention), measured
history/pane performance, and owning-composer routing (the workbench's
keyboard-owner model from the contained-UI milestone is the base).

## Implemented (UI)

- `crates/ui/src/context_transfer.rs`: `TransferFlow` (request, connection
  generation, host preview, apply state, attempts), preview/apply/Check status
  helpers, receipt wording, and one preview renderer (counts, omissions with
  reasons, budget with its source, notes, exact text on request; bounded
  scroll heights). Nothing is built or edited locally.
- `task_commands::apply_context_transfer`: `applyContextTransfer` on
  `preview.target.taskId`, fenced by the reviewed target revision and binding
  generation, with a caller-kept command id. A lost answer is Unknown; Check
  status re-reads the receipt (`read_receipt`) and never resends.
- Provider switch (`Pickers::render_transfer_switch`): on a supporting host,
  Continue with context / Start fresh with no default; primary label and
  enabled state from `switch_primary_state` (button and QA); Enter does
  nothing in the panel and Escape cancels it; a preview for another chat,
  target provider or connection is stale and never applied; Conflict keeps
  the draft and asks for a refresh. Start fresh is the existing
  `switchProvider` with no handoff. Older hosts keep the old toggle.
- Send findings (`shell/findings.rs`): offered (chat menu, workbench tab
  menu) only for a chat whose lineage source exists on a supporting host;
  the dialog previews ForkMerge into that source, sizes the budget with the
  source's draft, applies only a current preview, and never starts a turn.
  Escape closes it; it owns the keyboard while open.
- Long omitted messages: Details shows each read segment in full (bounded
  scroll), labels a truncated one "Excerpt. More of this message exists.",
  and offers Read more, which reads that one message from exactly the
  returned `nextTextOffset` (fenced by task, transfer, connection, item and
  cursor) and appends only a segment that starts there. A resumed partial
  item in a preview reads "(continued)".
- Action boundaries: every apply, Check status and Details answer installs
  only on the engine connection generation it was sent on and for the same
  dialog target / bound chat and transfer; a late answer from an earlier
  connection or profile settles, opens or notifies nothing. Refresh, a new
  dialog and a new switch target refuse while an apply is in flight or
  unknown, so its command id is kept and no second apply is enabled. Read
  more appends only a segment with `startByte == offset`, `endByte == offset
  + its UTF-8 bytes` and, when more remains, an advancing `nextTextOffset`;
  anything else adds nothing and claims nothing.
- Receipts: the context tray (model picker) lists the newest two with
  Details (kind, direction, status, counts, reason, omitted items read from
  the source page by page or "Unavailable", Check status for unknown); the
  Execution card shows "Last context transfer".

## Start new task with context (PortableFork, option b)

On a host with transfers, "Start new task with context…" (chat menu; its own
row when a native fork is also offered) opens the shared transfer dialog in
new-task mode: it reserves a fresh UUID as `targetTaskId` for the dialog's
lifetime (refreshes and Check status keep it), shows the source's provider,
model and reasoning, and previews `portableFork` with `newTarget {config:
source config}`. Preview and Cancel create nothing. "Create task" applies the
reviewed preview on that id (the host creates the ordinary task and its
captured context atomically); on success the UI opens exactly that task. A
lost answer stays unknown with the same id; Check status only. The source and
its draft are untouched. Older hosts keep the existing client-staged draft.

## Fixture

`composer-parity-fixture` failure cases 26-35, steps t01-t07 after e08:
findings offered only fork → living source; preview writes nothing; apply
stores a Planned receipt with no source turn and an untouched draft; the
source advancing makes the preview stale (draft kept), refresh shows only new
findings; a lost answer reads unknown and Check status settles it with no
second send; an earlier connection's preview is stale and unsent; the switch
has no default choice, its primary waits, a target change drops the preview,
Start fresh has no preview; Continue applies a handoff with a receipt and no
turn; Details read without error and the Execution card names the transfer;
deleting the fork keeps the source's receipts, naming it deleted. t03b
(failure case 36): new task with context previews into a reserved id kept
across refreshes, Cancel creates nothing, a lost answer is recovered by Check
status into exactly one ordinary task (no child, no native session or turn)
on the source's config with a Planned copy, opened, source draft untouched.
t06b (failure case 37): an oversized findings transfer leaves a long
multibyte fork message out; Details reads it in a bounded first segment
labelled an excerpt, and Read more appends that same message from exactly
the returned nextTextOffset.
Failure cases 38-39: t03c applies a new task with the fork's context and
changes the engine connection before the answer (no settle, no navigation,
Refresh refused); t05 loses a handoff answer, Refresh and re-targeting are
refused with the command id kept, and a Check status answer after a
connection change settles nothing. The host refuses a second preview from a
source with a planned transfer, so steps first let the destination send.
