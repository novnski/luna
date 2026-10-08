# Board movement controls execution

**Status:** explicit user requirements, 2026-10-08; not implemented. Moving a ticket from Backlog to Ready starts/resumes work. Moving it back uses the same Stop behavior as the chat. Automatic review uses temporary side chats with successful close/archive cleanup.

## 1. Columns express intent; badges show the observed phase

| Column | User intent | Possible observed phase |
| --- | --- | --- |
| Backlog | Hold this work; do not keep executing it | Not started, Stop requested, Stopping, Stopped, Needs attention |
| Ready | This ticket is enabled to run | Queued, Starting, Working, Checking, Reviewing, Fixing, Needs attention |
| Done | Current work and required review passed | Complete; optionally reviewer cleanup pending |

Ready is the execution-enabled lane, not merely a prepared draft waiting for another Start click. Cards stay there through implementation/review instead of immediately disappearing into a different column. This makes Ready → Backlog consistently available as the user's Stop gesture. Phase badges distinguish queued work from an actually running agent. This supersedes both earlier five-column and To do/Working/Done proposals.

Show short column help: **Backlog — held or stopped** and **Ready — runs automatically**. Dragging to Ready is the user's execution instruction and needs no extra confirmation when launch settings/target are already resolvable. Offer the same Move to Ready / Move to Backlog actions by keyboard/menu and through agent tools; all call one engine operation. Reordering within a column does not start, resend, stop or review anything.

## 2. First Start, Stop, and Continue

| Transition | Required operation |
| --- | --- |
| New ticket → Backlog | Persist title/brief/settings only; no working chat |
| Backlog → Ready, never dispatched | Resolve settings and destination, create/link the working chat once, send the first execution instruction once |
| Ready → Backlog while queued | Cancel the unsent launch/delivery and prevent it from running later |
| Ready → Backlog while active | Invoke the same shared Stop route as the working chat, including exact owned review work; preserve conversation and files |
| Backlog → Ready after work started | Reuse the working chat and send a continuation based on its checkpoint/current brief; do not replay the original prompt |
| Ready → Ready | No execution change; ordering only |
| Done → Backlog | Explicitly reopen/hold; preserve historical evidence and do not start yet |
| Current workflow passes while Ready | Mark Done only if the desired intent and execution/review generation still match; then clean up reviewers |

An independent first start from Board creates a main/sidebar chat. A first start requested from a main chat creates a side chat there. A side-origin request may create a fresh sibling under its existing main, following the confirmed creation contract. An existing worker is reused without changing its placement just because its card moved. Moving the card is not moving/reparenting its chat.

Stop means stop, not erase. Keep the chat ID, provider binding where resumable, history, draft, checkout, partial changes, brief snapshots and review results. It neither archives the primary chat nor resets the repository. A blocked/unknown Stop must remain visible; never claim the agent stopped merely because the card now appears in Backlog.

## 3. Instruction selection

The user's phrase “system prompt” describes an app-generated task instruction. Do not inject untrusted issue text as a privileged system message. Dispatch through the existing user/task command path with trusted workflow metadata separate from the quoted brief.

Illustrative **first-start instruction**:

> Work on ticket LUN-42 using brief revision 3 and its acceptance criteria. Preserve unrelated changes. Implement, run the required checks, and use the configured reviewer. Address blocking findings within the workflow limits. Report completion with evidence for the current result.

Illustrative **resume instruction**:

> Continue ticket LUN-42 from the work already started in this chat. The previous run was stopped by the user. Inspect the current files, partial results and latest brief before proceeding. Preserve completed work; do not restart from scratch. Resolve outstanding review findings and complete the remaining checks/review.

Include the last confirmed phase, known stop reason, brief changes since the last dispatch, remaining acceptance criteria, reviewer state and relevant artifacts. Do not assume interrupted tools rolled back or that a previous review still covers edited files. If the brief changed while stopped, present the new version/delta in the continuation rather than overwriting old messages.

Track **initial instruction delivery**, not just chat creation. If a chat was allocated but no initial message reached its provider, send the first instruction when resuming admission. If delivery is uncertain, reconcile the original command/message ID first. A converted existing chat uses a continuation of its existing work even if this is its first board-driven dispatch.

If the original provider session is unavailable, recover under the same visible LUNA chat using the existing explicit binding/context-recovery contract, with a bounded handoff summary and visible recovery status. Do not silently create another sidebar chat. If current checks/review already prove completion for the unchanged result, reconciliation can complete the ticket without another paid run.

## 4. Race handling and persistence

Store desired state separately from observed runtime state, with monotonic intent revision, execution generation, active task/run IDs, unique transition request ID, first-message delivery state, latest brief/launch snapshot, stop receipt and review-run ownership. Names are provisional; these responsibilities are required.

- Persist the move intent and idempotent command relationship before launching. Two windows repeating the same move must not create two chats or two prompts.
- If Ready is requested while an old run is Stopping, show **Waiting for stop** and wait for verified quiescence before sending Continue. Never overlap writers.
- Coalesce rapid Backlog/Ready changes to the latest desired intent while preserving the stop barrier. A delayed callback for an older revision cannot restart work or move the card to Done.
- Tie Stop to the exact admitted execution generation. A late Stop acknowledgement cannot be applied to a newer run.
- A late successful review after a move to Backlog remains historical evidence; it cannot override the user's latest hold intent.
- Offline/unavailable owner state yields a pending/unknown operation and reconciliation, not an assumed stop or a second launch on another host.
- Queueing respects project/host/provider capacity, cost/time limits, dependencies and checkout ownership. Show the reason while the ticket remains Ready.
- On restart, restore intent and query receipts/runtime ownership before acting. A saved Ready value alone is insufficient to blindly resend a start prompt.

## 5. Shared Stop and review ownership

Board movement and the chat Stop button must share the same lifecycle command and control scope. Introduce no board-only process killing. If reviewer integration expands the ticket's owned-work set, both controls must observe the same set.

For a ticket side chat, the reviewer is a sibling in the main parent's UI. Its recorded workflow owner is still the ticket, so Stop must include that exact reviewer without stopping unrelated siblings or the main parent's other work. Closing the reviewer tab is not Stop. Stop acknowledgement is not proof its process/owned work has terminated.

Use the existing ownership/snapshot/receipt patterns in [task_work.rs](../../../crates/proto/src/task_work.rs), [Stop handling](../../../crates/engine/src/rpc/task_work.rs), and [session Stop](../../../crates/engine/src/sessions_work.rs). This is an integration requirement, not a claim the new board/sibling flow already exists.

## 6. Acceptance before release

1. First drag to Ready produces one linked chat and one initial prompt; phase advances from queued/starting to observed running.
2. Drag back invokes the same Stop command/scope as the chat button, retains partial work and never marks unknown termination as stopped.
3. Drag again reuses the exact chat and sends Continue; verify first-prompt delivery count stays one and context reflects current partial work.
4. Rapid Ready → Backlog → Ready, multi-window duplicate requests, queued cancellation and restart recovery never produce overlapping/duplicate runs.
5. Side-ticket Stop cancels its reviewer sibling while preserving other main/side chats; stale review/Stop responses cannot affect the new generation.
6. After current ticket/review success, close/archive only its terminal reviewers with independent readback; preserve result history and primary chat.
7. Keyboard/menu/agent transitions produce the same behavior as drag. Reordering within Ready causes no provider traffic.

Retain exact move/intent revisions, command/message IDs, actual first-start/continuation text hashes, stop/launch/result receipts, process observations, side-chat relationships and native UI recordings. The saved mock demonstrates these interactions locally; real engine/provider E2E remains required.
