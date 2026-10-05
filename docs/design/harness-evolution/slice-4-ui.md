# Slice 4 UI: provider-unavailable queue hold

Host contract: [slice-4.md](slice-4.md). The engine persists
`executionUnavailable` on the exact claimed row (with a Rejected command
receipt) when a known pre-provider check fails before any provider I/O. The
UI's only production change is root's label in `crates/ui/src/queue.rs`:
`QueueHoldReason::ExecutionUnavailable` reads "Provider unavailable — review
and send". Everything else is the existing queue panel and resolver.

## Behavior the UI relies on (failure cases, before the fixture)

1. The row reads exactly "Provider unavailable — review and send", never a
   generic or steer-specific hold label.
2. Its id and revision stay exactly as the host wrote them; the UI never
   rewrites, re-enqueues or duplicates it.
3. It appears only in its own chat's composer (Main's or a pane's), never in
   another visible composer.
4. A reconnect or engine restart resends nothing: automatic drain leaves the
   hold alone and the UI issues no command on its own.
5. The only way forward is that composer's explicit Send now (the existing
   resolver: Queue/held rows offer Send now). The host rechecks current
   configuration; it either sends once or holds the same row again.
6. Send now from that composer never touches Main's (or another pane's)
   draft, queue or transcript, and never sends twice.

## Fixture

`chat-workbench-fixture` step w20l (failure case 61): seeds one held row for
Child A through the real document host, reads it back from Child A's
composer through the production resolver (`Composer::fixture_queue_rows`:
id, revision, hold label, primary action, tooltip), checks it is absent from
Main's composer, reconnects the engine and confirms the exact row is still
held with no new user message, then runs Child A's own Send now
(`Composer::fixture_queue_primary`, the button's resolver and fences). The
row is seeded as locally authored (this engine's own device), so no remote
review gate applies. Proof comes from the durable command ledger: both chats'
task-command sequences are recorded first; the step requires a NEW, settled
`sendQueuedNow` receipt for Child A whose correlation is the row id, Applied,
with exactly one send and the row gone. Pending, a refusal, an unchanged held
row or a timeout fails, and the last receipts, rows and texts are saved to
`w20l-timeout.json`. The fixture's Mock peer is a wrapper that changes only
w20l's exact prompt: that run reports `SessionStarted` (a unique native
session in the request's own cwd) before its reply, so acceptance can be
durably correlated; every other fixture send keeps the original script. Main must have no new send receipt
and an unchanged draft, queue and transcript. Both hooks live in `crates/ui/src/queue_steer_e2e.rs`
(appshots-fixture only); `queue.rs` is unchanged by this UI work.
