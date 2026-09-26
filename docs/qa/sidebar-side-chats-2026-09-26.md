# Sidebar agents and persistent side chats

## Final handoff status

The isolated Sidebar implementation passed 42 engine/storage integration cases,
the existing RPC cancellation regression, compile/format/Clippy checks, and the
native acceptance described at the end of this report. The signed **LUNA QA
Sidebar** app is retained for inspection on IPC 59787 with its QA data and unsent
parent draft. Product source is `e4788f6e`; the packaged checkout was report-only
commit `c34cc0f5`. Subsequent changes are confined to the two QA drivers and this
report. No primary LUNA Dev refresh, push, or main-branch change was performed.

The native run also exposed shared projectless-workspace relocation between
the two original QA output directories. The identity guard refused the stale
parent's fork. That failure is preserved and reported separately; it was not
replayed or bypassed. New QA runs now share a stable provider working directory.
The broader workspace behavior is not fixed by this sidebar change.

Earlier pending statements below describe intermediate checkpoints. The final
native section and `target/qa/sidebar/native/acceptance.json` are the handoff
record, including the limits of what was actually observed.

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

## Verification after reviewed runtime integration

Runtime `9bb805ac` merged without conflicts as
`538c1cfc9b15a64cf7865e3d16485c02571bb687`. No source changed during these checks:

| Check | Result | Log under `target/qa/sidebar/` |
| --- | --- | --- |
| Side lifecycle, permissions and export boundary | 12/12 | `post-merge-side-chats.log` |
| Native fork receipt/dedupe and explicit snapshot alternative | 2/2 | `post-merge-native-fork.log` |
| Synced peer visibility and missing-metadata restart | 1/1 | `post-merge-peer-restart.log` |
| Project command store and v13→v14 side migration cases | 2/2 + 3/3 | `post-merge-migrations.log` |
| Complete backup/restore target | 22/22 | `post-merge-backup-restore.log` |
| Existing immediate checked-subscription cancellation regression | 1/1 | `post-merge-subscription-cancel.log` |
| Engine/UI compile | Exit 0, 30.84s | `post-merge-ui-engine-check.log` |
| Engine/UI library Clippy, no dependencies | Exit 0, 32.92s | `post-merge-clippy.log` |
| Workspace formatting check | Exit 0 | `post-merge-format.log` |

The 42 integration passes and separate existing RPC regression are recorded in
`post-merge-verification.json` with exact commands, source SHA and log hashes.
Compiler/lint warnings remain recorded; this is not a warning-free claim.
Native UI, accessibility, app lifecycle and real-provider acceptance still need
the separately granted isolated app build.

Coordinator review additionally identified agent cards with no native transcript
reference being presented as clickable buttons. Preserve this verification
checkpoint before correcting those cards, then rerun affected UI compile/lint
and include available and missing-reference cards in native screenshot/AX QA.

## Native acceptance preparation after that checkpoint

Missing-reference cards now use a non-interactive Group role and display
"Transcript unavailable" with a matching accessible explanation. Only cards
with an actual reference receive button role, tabindex, focus/hover/pointer
styling and activation handlers. Geometry, activity/status and LUNA tokens stay
the same. Four new side-panel collapsible-if warnings were also addressed.
The affected `luna-ui` check passed in 10.98s (`agent-card-ui-check.log`);
post-correction Clippy and native render/AX checks remain pending.

The earlier immediate-cancellation regression proves releasing a server stream,
not that repeated subscribe/cancel is harmless for provider reconciliation.
`scripts/qa-sidebar.py` therefore now uses one multiplexed reader and one
persistent WatchDocMessages subscription per active task. It folds the actual
reset/upsert/remove/UTF-8 byte-count append protocol, aborts visibly on desync,
preserves immutable prior snapshots, and cancels the exact watch on exit.
Creation/send IDs are saved before admission; partial attempts are never replayed.

The failure-first loopback acceptance in `scripts/qa-sidebar-protocol.py` passed
with `SIDEBAR_PERSISTENT_PROTOCOL_PASS` and zero provider calls. Its result is
`target/qa/sidebar/persistent-driver-protocol.json`: interleaved RPC/stream
frames, 20 reads through one subscription, UTF-8 folding, prior-snapshot
immutability, explicit desync, cancellation and prompt disconnect failure.

`crates/engine/examples/seed_sidebar_qa.rs` reuses the existing synthetic engine
fixture pattern to seed available/missing-reference cards into only the reserved
Sidebar QA root before its first app launch. It refuses an existing profile or
occupied 59787 port, uses fake provider homes with native metadata disabled, and
labels the task/cards as synthetic. It is source-only and not yet compiled/run.
The actual signed QA app will render these cards for screenshot/AX checks.

Separately, `scripts/qa-sidebar-agents.py prepare` creates a disposable real QA
parent without starting provider work. After selecting it in the Agents UI,
`run` requests exactly one native gpt-6-luna child, with only a bounded
`/bin/sleep 20` and exact result marker. It requires actual native spawn metadata,
observed running status, a real child transcript reference and the child result;
unsupported capability is a failure to report, never a simulated agent. The
driver retains observations and never retries an attempted run. Both real-agent
and native-fork drivers remain unexecuted pending the separate native grant.

## Final source checks before native grant

The bounded final compile block ran on clean
`e4788f6e422eb6657a742a3fc8f7476fa5574f04`, without source repairs:

- `CARGO_INCREMENTAL=0 scripts/cargo.sh clippy --locked -p luna-ui --lib --no-deps`
  passed in 13.41s. The four new side-panel style warnings are gone; 253 existing
  UI warnings remain. Log: `final-ui-clippy-e4788f6e.log`.
- `CARGO_INCREMENTAL=0 scripts/cargo.sh check --locked -p luna-engine --example seed_sidebar_qa`
  passed in 22.91s. Log: `seed-fixture-check-e4788f6e.log`.

`target/qa/sidebar/final-source-checks.json` records exact commands, source and
log hashes. Both builds used the default 25 GiB growth reservation; neither
returned guard exit 75. The slot was released immediately after the second
check. No packaging, seed execution, or real-provider turn occurred in this
grant. Native app/process/signature, fixture and real-provider screenshot/AX,
restart, origin navigation and parent-draft handoff acceptance remain pending.

## Final isolated native acceptance

The seed fixture ran once against the empty reserved QA profile. The granted
refresh used `CARGO_INCREMENTAL=0`, `scripts/run-macos-dev.sh --refresh`, and all
four isolation overrides: `LUNA_DEV_APP_NAME=LUNA QA Sidebar`,
`LUNA_DEV_BUNDLE_ID=com.novnski.luna.qa.sidebar`,
`LUNA_DEV_IPC_PORT=59787`, and the absolute worktree-local
`LUNA_DEV_DATA_DIR=target/luna-qa-data/sidebar` path. Packaging took **121 seconds**.
The Cargo slot was released immediately afterward; native inspection and the
final QA-only edits did not rebuild the app.

`target/qa/sidebar/native/build-identity.json` records the original process,
source, bundle, owner-root claim, device and strict code-sign verification.
A graceful app-menu Quit and relaunch preserved the same binary:
SHA-256 `d2a59f4d860b714451e4ccee2ecdd1fb4bf89560fa8422a14a9920549ec0428c`.
`post-restart-identity.json` records PID **15166**, exact bundle executable,
listening IPC 59787, the same root/device, `codesign --verify --deep --strict`
exit 0 and zero working/approval/input provider sessions. The retained app is
in Light appearance with the parent draft unsent.

All following paths are under `target/qa/sidebar/native/`:

| Acceptance | Evidence and observed result |
| --- | --- |
| Synthetic available/missing agent cards | `agents-fixture-dark.png`, `agents-fixture-light.png`, `agents-fixture-ax.txt`: available card is a named Button; missing-reference card is a non-interactive Group with visible transcript-unavailable reason. Both themes were visually inspected. |
| Agent card opens actual referenced transcript | `fixture-child-open.png` shows the explicitly synthetic fixture result; `agents-native-child-result.png` separately shows the real child's exact result. |
| One actual native agent | `agents/result.json`, `agents/native-observations.json`, `agents/parent-complete.json`, `agents/child-result.json`: exactly one native spawn, running then done, real `subagentRef`, and exact child marker. One persistent parent watch and one child watch. |
| Actual running and completed presentation | `agents-native-running-ax.txt` records the named Working button; `agents-native-completed-light.png` shows Completed. The screenshot was captured after completion and is not presented as a running image. |
| Native fork retains parent context | `side-fork/result.json`: `SIDEBAR_NATIVE_CONTEXT_HISTORY_PASS`, 31,371 ms, exact marker recalled, parent transcript unchanged. The applied fork receipt and provider child identity are retained beside it. |
| Saved history and restart | `side-fork/history-readback.json`, `restart-history-readback.json`, `restart-history-driver.log`: unloaded side transcript retained and read-only after graceful restart; no side provider runtime registered. |
| Read-only admission after unload | `side-fork/negative-after-unload.json`: SendSideChat denied with “This side chat is read-only or expired”; saved history unchanged and runtime absent. |
| Actual Codex side-turn policy | `side-fork/side-native-wire-policy.json`: the exact forked child's post-fork turn context records `sandbox_policy.type=read-only`, `approval_policy=on-request`, model `gpt-6-luna`. This is policy-record evidence; no filesystem-write-denial experiment was performed. |
| Message anchor and writable side | `side-writable-anchor.png`, `origin-handoff/anchored-native-fork-receipt.json`: selected parent message, frozen quote, native fork, writable deadline, one real side reply, and existing parent draft preserved. |
| Explicit result handoff | `side-readonly-origin-draft.png`, `origin-handoff/result.json`: after unloading and reopening saved history, one click inserts the result into the existing main draft; submissions remain zero, queue items empty, parent transcript exactly unchanged, side runtime absent. |
| Reveal origin | `side-origin-before-expanded.png` and `side-origin-after-reveal.png`: the expanded side pane hides the parent; clicking Show original message collapses the pane and reveals the exact anchored parent message plus unchanged unsent draft. Both images were inspected. |

The real-agent parent is `e12ac2f7-2a79-46fe-bba9-6b5f1e4fd807`; its child result
is `LUNA_NATIVE_CHILD_becfa73c5b`. Its separate anchored side chat is
`1e2c40c3-114f-447a-a1f3-9ccfb67660bf`, with origin message
`83428f5c-9572-47f3-9de6-d73b5affb6f0` and result
`SIDE_UI_HANDOFF_1e2c40c3`. The parent draft retains
“Existing parent draft — keep this.” followed by that result. Copy formatting
adds two terminal newlines, preserved exactly in the evidence.

Two initial saved-evidence verifier assertions were corrected without repeating
the UI action: one incorrectly demanded no terminal formatting newlines; the
other treated WatchQueue's `{items: []}` envelope as a bare list. The actual
draft, zero submissions, empty queue and byte-equivalent parent transcript all
pass the corrected verifier. Run this read-only check to reproduce the result:

```sh
PYTHONDONTWRITEBYTECODE=1 agent-python target/qa/sidebar/native/origin-handoff/verify.py
```

It prints `SIDEBAR_NATIVE_ORIGIN_DRAFT_HANDOFF_PASS`. The origin images establish
reveal from an expanded pane; this short transcript does not establish scrolling
through a long off-screen history. The native anchor exercise used a whole
message, not a partial text selection. Engine integration covers bounded
history, permission ceilings and lifecycle edge cases separately.

### Preserved native failure and QA-driver correction

The first anchored attempt against the older context/history parent failed with
“task workspace changed; explicit resume refused”. Receipt
`side-chat-create:b1587031-42b7-4254-b577-cf72de6153a2` is retained as
`origin-handoff/failed-native-anchor-receipt.json`, with the error screenshot
`origin-native-fork-failed.png` and workspace readback. It failed before native
fork admission and did not alter the parent draft.

The two drivers originally created projectless tasks with their respective
evidence directories as cwd (`native/side-fork` and `native/agents`). Projectless
tasks share the device's workspace identity, so the second creation moved its
current path and fenced the first parent's explicit fork. Remaining UI checks
used a fresh request on the already-current real-agent parent, as authorized;
the rejected receipt was never replayed and the guard was not weakened.

For future runs, both drivers use `target/qa/sidebar/provider-workspace` and save
that cwd into the run record. Historical records keep their original cwd; an
attempted agent run still refuses replay. This changes QA setup only, leaving
the broader projectless identity behavior to its separate audit. Python syntax
checks and the existing persistent-protocol loopback pass after this correction
(`final-driver-syntax.json`, `final-driver-protocol.json` and its log), with zero
provider calls. The new cwd setup was not used to replay native acceptance.

### Native capture and handoff limits

The Executor native bridge found the correct app but its worker lacked
Accessibility permission; preparation refused without changing permissions.
CUA supplied actual native accessibility/action readbacks. macOS Stage Manager
initially supplied a small background thumbnail, so the user-approved exact QA
`open -a` command and full-size app window were used with the screenshot skill.
`current-window.png` is that old thumbnail and is excluded from acceptance.
Capture failures are retained; no failed or unreceived screenshot is treated as
visual evidence. The unused bridge session was released without closing the app.

Artifact JSON stores hashes of the accepted images and readbacks. The QA profile,
app and evidence remain available for coordinator review; no automatic parent
send occurred. Final disk inspection: `target/debug` **11 GiB**, no
`target/release` directory, and about **390 GiB** available. No extra target
directory or disposable build fixture remains to clean, and the shared cache
was preserved.
