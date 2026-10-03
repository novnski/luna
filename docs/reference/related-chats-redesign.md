# Persistent side chats and related conversations

## Scope and evidence

Iulian requested this redesign on 2026-10-03: side chats are user-created
forks, children are fresh parent-agent-created conversations, both appear in
one Related chats panel, and the left sidebar counts both. Only the user ends
a side chat; navigating, sleeping, disconnecting and restarting do not end it.
Codex owns engine/provider logic and final QA. Claude owns the native UI.

Source baseline: LUNA `1f21760c`; BB `2573ad5e85835dc1c90aa33795e6dba52ef5aa92`, inspected without changes.
The findings below are static source evidence, not measured runtime timings.

* `crates/engine/src/side_chats.rs` had a two-hour lifetime, a 30-second view
  lease, a four-active-chat limit, and a 45-second preparation reservation.
  Startup converted active chats into read-only history; shutdown and view
  teardown unloaded them. This made ordinary navigation and restart destructive
  to a conversation's usability.
* Its two-second sweeper read and serialized every live transcript and upserted
  it into a second SQL history table. Reads did the same projection under one
  global async lock, including provider stop/send work. Unrelated side chats
  could block one another and idle conversations incurred repeated work.
* `crates/harness/src/codex/native.rs::fork_thread_staged` called a helper that
  spawned/initialized/shut down a new app-server for each operation. Fork
  readback used `observe_thread`, which additionally searched both archived and
  unarchived thread lists. Two metadata observations could therefore scan the
  provider's whole session inventory just to create one fork.
* `crates/ui/src/shell/side_chats.rs` layered heartbeat, parked leases, a
  45-second cancellation, and teardown on top of the ordinary composer. The
  separate child/side menus exposed these implementation differences.
* BB `plugins/side-chat/server.ts` creates a hidden ordinary fork with
  `sourceThreadId`, `lifecycleOwnerThreadId`, and optional agent-only quote seed.
  `apps/server/src/services/threads/thread-fork.ts` reuses ordinary thread
  creation and environment/provider configuration. Its UI deduplicates in-flight
  creation by source/anchor and embeds the normal thread view. BB only archives
  unused, unqueued forks after 24 hours; LUNA will not copy that age policy.
* BB distinguishes child `parentThreadId` from fork `sourceThreadId` and
  lifecycle ownership. LUNA already has these distinctions and role-scoped
  agent tools; combining UI must preserve those backend boundaries.

## Implementation contract

1. Keep durable side metadata, identity, native binding, document, request IDs,
   and local-only isolation. `active` means writable regardless of open views;
   `ended` means explicitly ended by the user. Never resume a provider merely
   to list or read a chat. Never replay a send whose outcome is unknown.
2. Remove lifetime, active-count and view-lease admission. Keep the legacy wire
   fields for compatibility (`expiresAtMs: 0`, `lease: null`). Advertise
   `side-chat-persistence-v1`. `UnloadSideChat` becomes explicit End side chat;
   `StopSideChat` only interrupts the current turn. Historical legacy automatic
   expiry/recovery states can reopen when their durable chat still exists.
3. Use per-chat operation serialization, ordinary document subscriptions and
   bounded list summaries. A loaded document contributes its count, timestamp,
   and at most sixteen short user-message parts; a cold document stays cold.
   Project history when required for saved/history reads
   and explicit end, not periodically for every chat. Shutdown saves state and
   interrupts active work without ending conversations. A restart never replays
   interrupted turns or unknown provider creation.
4. Native Codex fork creation reuses a bounded connection for source read,
   mutation and title assignment, then uses a fresh connection for independent
   persisted readback. Verify exact provider ID, history and title; do not scan
   unrelated archived/unarchived threads.
5. Child and side creation retain separate authority and context. Agent tools
   may create children from fresh history and report to their parent. Side
   creation is user-controlled and retains the parent context with an explicit
   side-chat runtime identity. Related chats is a shared presentation, not a
   merged ownership or authority model.
   The former user `CreateChildThread` RPC now rejects; the parent-agent
   `create_thread` tool retains its live-turn fence and approval. Existing
   in-process fixtures seed data through a trusted Rust host helper, which is
   not exposed over RPC. Old creation-dialog grant records remain readable.
6. Claude implements one menu/list, distinct child/fork icons and labels,
   normal project/harness/title/time rows, pin/settle/archive, no expiry UI,
   persistent composers and deduplicated combined left-sidebar counts.

## Failure cases recorded before implementation

* Old wall-clock deadlines, over four simultaneous chats, missing heartbeat,
  view close/back, engine disconnect and normal restart must not end a chat.
* Explicit End must stop only that chat, preserve history, reject further
  sends, remain ended after restart, and be retryable after a storage error.
* Interrupted/unknown creation must never fork twice. Identical request IDs
  return the original outcome; changed payloads conflict. A failure before
  provider mutation must not claim an unknown successful fork.
* Restart must retain native provider/account binding and accept a new user
  turn without replaying the last one. List/read must not start providers or
  hydrate every saved transcript. Preparing/failed/deleted rows stay distinct.
* Slow work in one side chat must not serialize unrelated side operations.
  History projection must still remove rewound tails and support pagination.
* Creation must prove exact fork history/title with bounded provider processes;
  mismatched history, missing persisted target, preflight failure and timeout
  retain the correct acknowledged provider ID and never publish a false success.
* Parent deletion retires owned side history. Parent stop and side stop remain
  independent. Permission/account ceilings and local-only sync isolation hold.
* A parent tool cannot create or message a side fork. Side chats cannot acquire
  child-creation tools. Children stay fresh and retain parent reporting.
* Unified UI covers loading, empty, error/retry, simultaneous child/side rows,
  more than 50 sides, duplicate WatchChats/list entries, archive/settle/pin,
  new side creation, back/close/reopen, drafts, stop/end and restart.

## Verification record

Evidence is under `target/qa/related-chats-redesign/`. The subprocess scripts
write summary JSON and hashes; native fixtures write screenshots, readback JSON
and a pass marker. A success exit without the expected pass artifact is not
accepted. The first visual run caught an empty-title summary bug; screenshot
review also caught an ended-chat rendering bug despite intact backend history.
Those failed artifacts are retained separately from corrected runs.

Verified on 2026-10-03:

* 25 engine integration checks: `side_chats`, `pane_chat`, `pane_chat_mcp`,
  and `claude_child_chats`. This covers provider/account/permission boundaries,
  ordinary queue/steer/stop, fresh child creation through Codex and Claude MCP,
  parent independence, persistence, paging, unknown sends and local-only sync.
* Two native Codex protocol integration checks prove exactly two app-server
  processes, no `thread/list`, independent persisted readback and correct
  acknowledged-session provenance after mismatch or timeout.
* `scripts/qa-related-chats.mjs`: 11 actual headless-process scenarios, including
  six simultaneously active chats, 32 seconds without a heartbeat, two engine
  restarts, continue-without-replay, explicit End, and 53 paginated sides.
  The Mock-provider run recorded creation p50 5.65 ms and p95 8.70 ms.
* `scripts/qa-related-chats-live.mjs`: real Codex (`gpt-6.1-sol`) and real Claude
  (`claude-sonnet-4-6`) each remembered a unique parent code word in the fork,
  retained the exact provider session ID across an engine-process restart,
  continued with a new user turn, and rejected a send only after explicit End.
  Creation was 310 ms for Codex and 871 ms for Claude in these small test
  conversations. These are observed samples, not large-history benchmarks or
  a claim about every provider/account/environment.
* Existing UI library suite: 1,195 passing checks, rerun after the final pane
  ownership and history-rendering fixes. Native fixture coverage
  includes the combined list, pin/settle/archive, sidebar count, full composer,
  saved draft, in-flight send across Back, more than 30 seconds idle, pane
  close/reopen and engine reattachment. All four final fixtures passed:
  `related-chats-fixture`, `child-chats-pane-fixture`, `side-chat-pane-fixture`,
  and `composer-parity-fixture` (see `final-ui/summary.json`). Screenshot review
  confirmed both kinds together with count 4, plus saved messages immediately
  after End and after reopening. Deferred pane releases now carry owner tokens
  so an old view cannot clear a newer view or its saved history.
* Parent-tool subprocess E2E: the manual child RPC rejects, the native MCP
  endpoint authenticates the caller, and a pending child approval stays parked
  beyond 40 seconds until Stop retires it without creating a child.
* Signed native app verification used a separate `LUNA QA Related Chats`
  bundle/profile on port 59793. Computer control created and sent a side chat,
  gracefully quit/restarted the app, reopened it writable, sent a second turn,
  and explicitly ended it while all four messages remained visible. The
  independent RPC readback is in `native-ui/`; the app's exact process, IPC
  ownership, signature and executable hash were recorded. The reviewed QA
  bundle was unregistered and removed; its data and evidence were preserved.
* Backend Clippy and workspace formatting checks passed. Primary **LUNA Dev**
  was refreshed (3-second cached build), and its exact bundle process, port
  59777 and deep/strict code signature were verified. No cache cleanup or
  unrelated pending-task edits were included.

`reviewed-evidence.json` hashes the reviewed screenshots, readbacks and logs.
The two real-provider runs and the native UI flow are separate evidence: the
provider runs prove preserved context/session identity, while the native UI
flow proves actual controls and rendering across application restart.

Repeatable subprocess commands (from this checkout):

```sh
LUNA_RELATED_QA_EVIDENCE="$PWD/target/qa/related-chats-redesign" node scripts/qa-related-chats.mjs
LUNA_RELATED_QA_EVIDENCE="$PWD/target/qa/related-chats-redesign" node scripts/qa-related-chats-live.mjs
LUNA_RELATED_QA_PROVIDER=claude-code LUNA_RELATED_QA_EVIDENCE="$PWD/target/qa/related-chats-redesign" node scripts/qa-related-chats-live.mjs
```

The live script uses the existing provider login through LUNA's normal adapter;
`LUNA_RELATED_QA_MODEL` and `LUNA_RELATED_QA_BINARY` override its defaults. It
creates isolated engine data and a QA project, never uses a user's existing
conversation, and records the tested executable hash.

## Compatibility and remaining boundaries

Legacy automatic expired/unloaded/recovered chats reopen when their durable
workspace row still exists. A new explicit `ended` state is never revived.
Imported histories explicitly marked read-only remain read-only: importing a
profile does not grant authority to resume another profile's provider session.
An interrupted model turn is not replayed on startup; a new user message
continues the same conversation. Old clients retain their legacy UI behavior
until upgraded; the new client uses the advertised persistence capability.

Native forks still require an exact eligible history boundary. Active parent
turns, unresolved approvals, pending commands or missing provider history can
prevent that operation. The UI may offer an explicit recent-context snapshot;
that fallback is bounded and is labeled separately from a native fork. Neither
path changes the parent's transcript or permits the parent to create side chats.
