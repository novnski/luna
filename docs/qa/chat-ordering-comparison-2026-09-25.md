# Chat ordering review and live GPT-6-Luna QA — 2026-09-25

Source snapshots: BB `get-bb/bb` at `9c8112e`, T3 Code at local commit
`97db94c9b`, and LUNA starting at `3e07f303`. This review followed provider
events into the persisted timeline and the disclosure controls, then exercised
LUNA Dev against the real Codex app server with GPT-6-Luna, High reasoning.

## What the three apps display

| Area | BB | T3 Code | LUNA |
| --- | --- | --- | --- |
| Ordering | Projects persisted events by `meta.seq`; user inputs during a turn are explicit sequence boundaries (`packages/thread-view/src/group-event-projection-turns.ts`, `external-user-boundaries.ts`). | Merges messages, plans, and work entries by `createdAt` (`apps/web/src/session-logic.ts:1573`). Tool lifecycle updates collapse by call id (`session-logic.ts:1074`). | The session document keeps message-list order, and each assistant entry keeps part order (`crates/doc/src/schema.rs`, `crates/ui/src/transcript.rs:1283`). A steer must split the assistant entry at acceptance. |
| Reasoning | Projects reasoning into an operation row with expandable detail (`packages/thread-view/src/reasoning-lifecycle-projection.ts`, `apps/app/src/components/thread/timeline/TimelineReasoningDetail.tsx`). | Codex reasoning deltas become runtime activity (`apps/server/src/provider/Layers/CodexAdapter.ts`); the work timeline can disclose activity detail. | A reasoning part becomes a **Thought process** chip in the adjacent tool group; live thought detail opens by default and settled detail can be reopened (`crates/ui/src/transcript.rs:1418`, `6670`). |
| Tools | Keeps typed work rows and lifecycle state. Expandability depends on kind: command and generic tool details expand; file reads and searches stay compact (`packages/client-core/src/timeline/timeline-auto-expand.ts`). | Renders command, argument, and output detail in individual work rows. Completed turns fold, old tool rows can be collapsed behind a count, and agent spawn rows stay visible (`apps/web/src/components/chat/MessagesTimeline.logic.ts`, `MessagesTimeline.tsx`). | Consecutive tools fold into an activity group; each chip can disclose invocation and available detail. Spawn chips stay visible outside ordinary folds (`crates/ui/src/transcript.rs:1337`, `6529`). |
| History | Merges paginated timeline windows using stable row IDs and source sequences (`packages/client-core/src/timeline/timeline-merge.ts`). | Rebuilds the merged timeline and retains completed-turn disclosure state (`MessagesTimeline.logic.ts`). | Replays the document message list into stable block rows and minimally splices changed rows (`crates/ui/src/transcript.rs:1`, `1770`). |

These are app design choices, not a single normative UI standard. The shared
requirement is that provider events retain their causal order, updates change
the original tool call, and a delivered user input appears at its turn boundary.

LUNA deliberately stores only tool invocation metadata, completion/failure,
and diff statistics in the shared transcript. Full stdout and sensitive tool
inputs remain in the host run journal (`crates/doc/src/parts.rs:414`,
`crates/engine/src/sessions.rs:6828`). Thus its disclosure cannot show the
full output that BB and T3 can show. This is an existing privacy/product rule,
not a transport failure; expanding shared storage would change what synced
devices receive and was outside this ordering fix.

## Defect and change

Before the fix, a native `turn/steer` response was treated only as an
unconfirmed acceptance. In the live baseline, Codex persisted the steering
`userMessage` and answered `STEER ACK`, while LUNA showed no matching user
bubble and left the queue row `deliveryUnknown`. Because the assistant entry
was not split, later tool/reasoning activity could render above the missing
user boundary. The engine also dropped a tool result if its call was in an
earlier segment.

The Codex adapter now checks the returned `turnId` against the active turn and
emits a steer boundary for both ordinary and explicit steers. The engine
records the entry that owns each tool call and resolves a late result on that
entry after a steer, without adding a second chip below the user message.

## Repeatable real-provider exercise

Use a signed LUNA Dev app with an authenticated Codex account. Create a
disposable task on a local checkout, select GPT-6-Luna and High reasoning,
then:

1. Ask it to run a read-only `sleep 25` tool command and answer `ORIGINAL ACK`.
2. Once the tool chip is running, submit a **Steer** follow-up asking for
   `STEER FIXED` after the sleep. Confirm the tool chip stays above the new
   user bubble, and the steering queue row disappears with an applied receipt.
3. While that turn is running, submit a normal **Queue** follow-up asking for
   `QUEUE FIXED`. Confirm held → claimed → empty, with this user message and
   answer after the steered answer.
4. Reopen the task. Confirm the original tool is completed, both user bubbles
   and both answers remain in that order, and reasoning still precedes its
   associated tool.

The actual before-fix task is `cb417b3c-32b4-4067-ad8b-4ff14f833b35`.
Codex `thread/read` for its provider task
`01a0d7d2-738f-7e00-9911-445472532438` contains the second user input and
the `STEER ACK` answer; LUNA's queue row
`8a869e76-1b40-437f-b1fe-7ed6572bcfa8` ended `deliveryUnknown` with no
matching transcript bubble.

The fixed LUNA Dev task is `24594d12-592d-4224-8f06-1bae768fa418` and its
Codex task is `01a0d7da-4b80-7ac1-8718-65d13481e78f`. The app's
`WatchDocMessages` reset, taken after both turns completed, reads:

```
user: read-only sleep 25
assistant: reasoning, commentary, completed sleep 25 tool
user: Steering QA … STEER FIXED
assistant: STEER FIXED
user: Queued follow-up QA … QUEUE FIXED
assistant: QUEUE FIXED
```

The steer receipt `legacy-steerQueuedNow-5dd92a9e-2f7f-42ef-a62b-1e5aefd7720f`
is `applied`; the queue is empty. Codex `thread/read` reports the same input,
tool, and answer order across one steered turn and one queued follow-up turn.
To read the durable LUNA result again from this checkout:

```sh
target/debug/examples/rpc_probe ws://127.0.0.1:59777 WatchDocMessages '{"chatId":"24594d12-592d-4224-8f06-1bae768fa418"}' --stream 1
target/debug/examples/rpc_probe ws://127.0.0.1:59777 WatchQueue '{"chatId":"24594d12-592d-4224-8f06-1bae768fa418"}' --stream 1
target/debug/examples/rpc_probe ws://127.0.0.1:59777 GetTaskCommandReceipt '{"commandId":"legacy-steerQueuedNow-5dd92a9e-2f7f-42ef-a62b-1e5aefd7720f"}'
```

The native Computer Use capture returned only a small perspective view of the
LUNA window, and its accessibility tree did not expose transcript rows. The
visual layout was therefore not pixel-verified in this run. The app IPC,
persisted document, queue receipt, and provider-native `thread/read` provided
the ordering and lifecycle readback above.
