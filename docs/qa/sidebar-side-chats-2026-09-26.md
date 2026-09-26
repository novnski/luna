# Sidebar agents and persistent side chats

## Proposal and source evidence

Reference checkouts were fetched without checking out or resetting local work.
T3 Code `2a9832b8019039f812706639c11ccc24ce4c4e05`:
`apps/web/src/components/AgentsPanel.tsx` and
`packages/client-runtime/src/state/subagentRuntime.ts`. Its agents panel preserves
spawn order, reserves identity/activity/metrics rows, reports observed statuses
and token counts, and separates native direct spawns from workflow groups.
LUNA exposes direct native subagent observations, not T3 workflow phases.

BB `9c9bae7f36a237c7e1b96de3d4c2186d13967686`:
`plugins/side-chat/server.ts`, `app.tsx`, and `PLUGIN_OVERVIEW.md`. BB creates a
hidden fork with a lifecycle owner, optional message boundary and agent-only
quoted text. Its hourly cleanup archives unused forks older than 24 hours; that
is not LUNA's requested two-hour writable lifetime.

LUNA uses the existing receipt-backed native fork path for Codex and OpenCode.
The native fork retains context; the side document starts empty and contains
only the side conversation's new exchange. Message anchors use reviewed provider
checkpoints. Claude's current adapter has no safe idle native fork, so it uses
an explicitly labelled bounded visible-text snapshot instead. The snapshot is
limited to 32 recent messages and 24,000 characters, stopping at an anchor when
one is selected. Parent/provider/account/model/cwd are frozen at creation;
permissions can only become stricter afterward. No provider session ID from the
parent is reused as the side session ID.

Metadata, immutable creation hashes, no-replay send admission and transcript
pages belong in profile `docs.sqlite3`. Migration v14 follows the commands
storage prerequisite `fdefdfbababb0efa8587d333023d0e75437b6cfd` (adopted locally
as `5d5cca95`). The source now registers v14 through DocsStore, removes ad hoc
DDL from service/import connections, and raises backup compatibility to 14.
The prerequisite's pre-WAL future-version guard is retained. Migration and
backup runtime verification of the combined implementation is still pending.
Native children remain hidden registry tasks for existing deletion/identity
ownership. Parent deletion seals them and uses normal child deletion receipts.
Local profile import copies ownership before publishing hidden child rows.
History lists contain at most 50 metadata records; transcript reads return at
most 50 entries. A current UI loads one side chat. The engine admits at most four
writable/preparing/incomplete-close side chats, including orphaned reservations.
Startup guards exclude side chats before native reconciliation, full journal
reads, retry recovery, transcript salvage/migration and activity/attention scans.
Historical reads never start a provider or renew a writable lease.

Writable lifetime is two hours from creation, never inactivity. Unload seals
the chat permanently. Restart/crash seals all previously writable chats; no
uncertain send is replayed. A short renewable view lease also seals orphaned
views after UI disconnection. One loaded view per panel; bounded metadata and
transcript pages, no history preloading. Expiry stops work, saves final output,
then releases resources. Stop/save errors remain visible and retryable.

## Failure scenarios authored before implementation

1. Parent changes after creation: the snapshot and provider boundary must stay
   frozen; no side-chat message or title can appear in the parent.
2. Missing/unconfigured/remote/archived parent: refuse creation, never infer an
   account, cwd or broader permission mode.
3. Side-chat create/send reply is lost: stable request IDs must prevent replay.
4. Double send, close/send race, expiry/send race: serialize admission and seal
   before cancellation; reject writes after the deadline.
5. Provider ignores cancellation or storage fails: do not report successful
   unload; preserve metadata and surface failure for retry.
6. UI closes, switches parents, disconnects or crashes while generating: stop
   within the lease bound and release subscriptions/runtime ownership.
7. App restarts: metadata/transcript remain, runtime count stays zero, history
   is read-only, original creation/deadline remain unchanged.
8. Many side chats/large transcripts: list metadata only; page reads bounded;
   unloading removes rendered rows and watch handles.
9. Permission/account changed: never silently route to another account or
   raise the frozen permission ceiling. General chat mutation paths cannot
   resume side-chat IDs.
10. Subagent waiting/completed/unobserved/failed states and missing token usage:
    show actual observations, stable row geometry, no invented progress.
11. Agents panel hidden: no additional child transcript subscriptions; detail
    view closes/unwatches explicitly.
12. Light/dark theme, narrow panel, keyboard: controls named and focusable;
    loading/empty/error/expired/history states remain legible.

## Repeatable verification

Author engine integration regressions before implementation; run after the
coordinator opens the build gate. Native QA identity is LUNA QA Sidebar,
`com.novnski.luna.qa.sidebar`, IPC 59787, worktree-local data. Do not refresh the
primary LUNA Dev app. Retain command logs, JSON assertions, screenshots and
real Codex gpt-6-luna disposable-session evidence in the final handoff.

## Added anchored conversation scope (coordinator/user follow-up)

BB's `app.tsx` message action supplies `sourceSeqEnd` and either selected text or
message text. Its result action queues directly into the source. LUNA will use
its existing native history boundary for the selected message, persist the
origin task/message plus quote, and show that quote in the panel. A user-message
anchor forks before that message and seeds the quoted message on the first
side-chat turn; an assistant anchor forks after its completed provider turn.
Unsupported providers use an explicitly labelled bounded snapshot through the
selected message. Selecting a result in a writable or historical side chat
inserts it into the current parent's existing draft, requiring the user's Send.

Additional failure-first coverage: missing anchor refuses creation; quote and
origin survive unload/restart; creation and history reads leave parent messages
unchanged; selected-result insertion preserves existing draft text/attachments,
checks parent/profile ownership, never submits, and never creates a runtime.
Selections from another transcript must not be misattributed to the chosen row.

## Permission failure observed in exploratory E2E

The preimplementation lifecycle assertion caught a real boundary regression:
`permissions::resolve_run_policy` derives WorkspaceWrite for legacy Ask, so a
parent carrying a stricter ReadOnly sandbox was widened. Preserve this failed
5/6 run as `target/qa/sidebar/side-chats-e2e-2.log`.

Before further correction, cover this matrix through the real engine boundary:
ReadOnly+Ask must remain ReadOnly+Ask; WorkspaceWrite+Full must not acquire Full
bypass or DangerFullAccess; explicit DangerFullAccess+Full may retain that same
boundary; tightening the parent after side-chat creation must further restrict
a later side turn. Codex has a native read-only wire contract. Claude/OpenCode
adapters do not expose an equivalent read-only sandbox contract, so that strict
legacy combination must refuse creation rather than claim enforcement. Ordinary
task resolution must remain unchanged. Repeat the native fork and real Codex
permission behavior after the correction.

## Shared-registry visibility failure case

A native/fallback side child remains a real registry task, but the side-chat
history database is profile-local. A second synced device must not display that
child as a regular writable chat merely because it lacks local metadata. Test
with two real engine instances and the existing in-process registry server.
Creation must publish an additive `sideChatParentId` field atomically with the
child row; current clients use that marker for visibility/startup exclusions.
No server row kind or merge behavior changes. Old binaries that ignore the new
field remain unsupported for newer profiles; a schema migration cannot retrofit
rules into a binary already shipped.

Lifetime failure case: a wall-clock rollback must not extend a writable view
or its orphan lease. Keep the persisted creation/expiry timestamps for history,
but bound each live lease with monotonic time as well. A restart is always
read-only, regardless of either clock. History reads never acquire that lease.

Creation interruption case: a cancelled RPC after reservation must not leave one
of four creation slots occupied forever. Reservations have a 45-second monotonic
admission deadline (the native fork call itself has a 30-second bound). A late
receipt cannot reactivate a reservation already sealed as uncertain. Shutdown
also seals pending reservations; restart never replays them.

Persistence-failure unload case: rejecting the closing-state write must still
attempt cancellation, retire the writable lease, and report incomplete unload.
It must not leave generation running merely because the metadata write failed.
After storage recovers, explicit unload/history read can finish saving the
retained transcript; a failed unload never reports success or replays a send.

Agent-detail subscription failure case: dropping the legacy unchecked RPC
receiver only cancels after another server frame. An idle agent may never emit
that frame. Use the existing checked RPC subscription with immediate drop
cancellation for subagent views, and verify that closing/switching the panel
releases its watch even when the provider is idle.

An unavailable/busy native fork does not silently lose parent context or retry.
The UI may offer an explicit **Start with recent context** alternative after a
failed native creation. It is a new request, visibly labelled as a bounded
snapshot. The immutable creation hash includes this user-selected context mode;
reusing the same request ID with a different mode must reject. Historical reads
still never call either creation path.

The export regression exercises a configured fake HTTP Edge endpoint with an
ordinary-parent control. Side documents, native subagent documents, tool blobs,
and registry previews must retain no outbound content path. It also seeds
persisted side/subagent outbox batches, restarts, and requires their bytes to
remain inspectable locally without any side-specific HTTP request. The peer
regression restarts the second engine without local side metadata and still
requires direct history/runtime access to fail closed.

Current source review also bounds a complete panel operation, including approval
and follow-up reads, to 45 seconds; a timeout is an unknown outcome, not permission
to replay. Startup pending-adoption recovery excludes side IDs before native
provider reads. Agent rendering stops collecting after the first 100 rows plus
a one-row overflow signal; no roster-specific subscription is created.

## Schema v14 source checkpoint

The prerequisite applied without conflicts. Failure-first file-backed cases in
`crates/sync/tests/side_chat_store.rs` cover upgrading an existing v13 profile
without losing project-command config, idempotent reopen with side transcript
and send records, transaction rollback after a midway DDL collision, and exact
byte preservation when version 15 is refused before WAL changes.

The existing backup future-schema fixture now uses version 15 and expects a
ceiling of 14. Its synthetic v9 fixture removes every post-v9 table before
restore, including v13 project commands and v14 side storage. Current-version
expectations are 14. Backup validation also refuses a v13/v14 database missing
the feature tables its version requires. The saved side-chat archive round trip
checks metadata, origin quote and transcript bytes through the real service.

No Cargo or native app command ran for this schema checkpoint. Required queued
checks include the `project_command_store` and `side_chat_store` integration
targets, the complete `backup_restore` target, and the sidebar lifecycle suites.
The fake-Edge failing-before evidence and correction are recorded below.

## Export and lifecycle verification before audited-runtime merge

All runs below use base `68dbc36e`, before adopting the reviewed runtime
`9bb805ac`. The Cargo slot was released after this bounded batch.

- `target/qa/sidebar/export-before-68dbc36e.log`: compilation passed in 38.66s;
  the exact fake-Edge case failed in 0.76s with "side document or blob escaped to
  Edge". The immediately preceding ordinary-parent control assertion passed.
- `target/qa/sidebar/export-after-1.log`: the narrow guard compiled in 35.24s;
  the initial outbound assertions passed, but the restart fixture failed in
  0.78s because its fabricated upload bytes were not valid Loro updates.
- `target/qa/sidebar/side-chats-e2e-3.log`: after replacing only that invalid
  setup with actual encoded document updates, all 12 cases passed (1.68s compile,
  13.41s tests, exit 0). This includes the permission ceiling matrix, expiry and
  restart, paged cold history and doc release, anchor dedupe, deletion cascade,
  failed-close history retry, and the full export/restarted-outbox boundary.

The guard prevents side and native-subagent docs from joining, seeding,
checkpointing or publishing tails to Edge, suppresses tool-blob upload/fetch and
registry previews, and preserves locally readable snapshots and saved outbox
bytes. Ordinary parent syncing remains exercised by the HTTP control. Side docs
also do not count as disconnected remote rooms in connectivity status.

`target/qa/sidebar/export-guard-tested.patch` preserves the exact tested changes
from `68dbc36e`; `export-lifecycle-verification.json` records commands, outcomes
and SHA-256 hashes. This batch does not establish native app/UI behavior or
post-merge migration/backup/native-fork/peer behavior; those remain queued.
