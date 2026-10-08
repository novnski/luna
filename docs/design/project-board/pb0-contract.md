# Project board — PB-0 engineering contract

**Status:** authoritative engineering contract for PB-0 through PB-3 (including PB-1R); PB-4 stays
deferred. Written 2026-10-08 on branch `board/pb0-foundation` from `main` 424e3e48.

**Product source:** [`docs/pending-tasks/devin-project-board/`](../../pending-tasks/devin-project-board/README.md)
(README confirmed decisions, `chat-model.md`, `board-lifecycle.md`, `automatic-review.md`,
`ticket-authoring.md`, `proposal.md`). Product decisions there are final; this document only decides
mechanisms.

**Code anchors:** wire types `crates/proto/src/board/`, RPC names `crates/rpc/src/lib.rs`
(`methods::BOARD_*`, `methods::BOARD_METHODS`), storage `crates/sync/src/migrations/board.sql` and
`crates/sync/src/store/board.rs`, engine service `crates/engine/src/board/`, dispatcher
`crates/engine/src/rpc/board.rs`, UI `crates/ui/src/shell/board/`, E2E
`crates/engine/tests/board_store_e2e.rs`.

**Rule for every slice:** build against these types and signatures. A contract change is the smallest
additive change in your branch (new optional field, new enum variant, new function) listed under
"Contract changes" in your report. Never rename or remove a foundation type, field, method or table.

---

## 1. What PB-0 delivers

| Area | Delivered by the foundation | Left to slices |
| --- | --- | --- |
| Records | Every record of proposal §3 plus `BoardDependency`, as proto types and v19 tables | Optional fields their slice needs |
| Storage | One additive migration (v19) for **all** board tables; one atomic commit primitive | No new migrations (see §2.4) |
| Core service | Create (idempotent, save only), read, list, field-level update, rank, archive/unarchive, receipts, event journal, `BoardWatch` | — |
| Engine skeleton | `crates/engine/src/board/{mod,store,lifecycle,chat_identity,review,authoring,github,tools,context}.rs` with full signatures; non-store functions return `BoardError::NotImplemented` | Bodies |
| RPC | One dispatcher for all 49 `Board*` methods | Nothing (do not edit the dispatcher) |
| Backup | Schema 19 accepted; all 13 board tables required in archives | — |
| UI | Board nav item below Automations, page route, selected state, back/forward, restart restoration, command palette "Open Board", typed API, `BoardModel`, minimal page, stub files per UI slice | All real UI |
| E2E | `board_store_e2e.rs` (failure cases 1–13 of the store), manifests in `target/qa/board/store/` | Slice E2E files (§12) |

---

## 2. Storage

### 2.1 Decision: engine-owned SQLite on the project's owning host

Board records live in the profile's `docs.sqlite3` (`DocsStore`, crate `luna-sync`), migration **v19**,
next to automations, side chats and task commands. The project's owning host (`Space.device_id`, fixed at
creation) is the **only writer**; its engine is the only authority for every ticket of that project.

| Requirement | SQLite on the owning host (chosen) | Loro workspace document (rejected) |
| --- | --- | --- |
| Idempotency | `board_mutations(request_id PRIMARY KEY)` checked in the same transaction as the write; same key + same payload hash returns the stored receipt, different payload is `idempotencyConflict` | No transactions; a retried import/create on two devices produces two CRDT rows that must be reconciled after the fact |
| Multi-window races | One engine serves all windows; SQLite transactions serialize; revision CAS and field-level merge (§3.4) | LWW per field silently drops one writer; no "stale revision with current state" |
| Multi-device races | Single authority per project removes them: no two devices allocate `LUN-42` or import one issue twice | Concurrent key allocation and issue dedupe need a reconciliation rule the spec says the UI cannot provide |
| Restart | `synchronous`/WAL durability, receipts, journal, intents and launch snapshots persist with the commit | Doc snapshots are durable but carry no receipt/journal semantics |
| Sync | Not replicated in PB-0 (§2.3) | Replicated, but foreign imports of executable rows are exactly what `local_executable_authority` exists to fence |
| Backup | Included in the `docs.sqlite3` archive; v19 tables are required at validation (`backup.rs`) | Included, but restore cannot rebuild counters/receipts |
| Older clients | An older build refuses a newer schema (`StoreError::NewerSchema`) instead of dropping data; the workspace doc gains **no** board fields, so older peers see unchanged chats | Older builds would ignore unknown maps but could not enforce invariants |

### 2.2 Tables (v19)

All tables are `STRICT`; every row stores its full proto record as JSON in `record`/`receipt`, other
columns exist only for constraints, ordering and bounded lookups.

| Table | Key / constraints | Record |
| --- | --- | --- |
| `board_projects` | `project_id` PK; `UNIQUE(owner_host_id, key_prefix)` | prefix + `next_number` counter |
| `board_items` | `id` PK; `UNIQUE(project_id, number)`; order index `(owner_host_id, archived, project_id, column_order, rank, id)` | `WorkItem` |
| `board_mutations` | `request_id` PK | `BoardMutationReceipt` |
| `board_events` | `seq` AUTOINCREMENT (journal cursor); `id` UNIQUE | `WorkItemEvent` |
| `board_task_links` | `id` PK; **one active Working link per ticket** and **per chat** (partial unique indexes) | `WorkItemTaskLink` |
| `board_issue_refs` | `id` PK; **one active reference per `(project_id, dedupe_key)`** | `IssueReference` |
| `board_launches` | `id` PK; `launch_request_id` UNIQUE (no-replay key) | `LaunchSnapshot` (immutable) |
| `board_outcomes` | `id` PK | `OutcomeReference` |
| `board_intents` | `work_item_id` PK; CAS on `version` | `BoardExecutionIntent` |
| `board_review_runs` | `id` PK; CAS on `version` | `ReviewRun` |
| `board_authoring_sessions` | `id` PK; CAS on `version` | `TicketAuthoringSession` |
| `board_authoring_entries` | `(session_id, seq)` PK | authoring transcript entry JSON |
| `board_dependencies` | `(work_item_id, blocked_by_id)` PK | `BoardDependency` |

### 2.3 Multi-device scope in PB-0

Board methods are **owner-local**: every `Board*` method is inventoried in `methods::ALL` with no remote
scope (remote sessions get "method is local only"), and the dispatcher refuses a `targetDeviceId` naming
another host with `BoardError::Unavailable`. A device can manage boards of projects it hosts. Viewing a
remote host's board (a relay-forwarded read scope, or a read-only summary projection into the workspace
document) is deliberately deferred; adding it later is additive and needs a remote-scope decision.

### 2.4 Migrations

v19 already contains every table any slice needs. **No slice adds a migration.** Add optional
`#[serde(default)]` fields to proto records instead (JSON records absorb them). If a genuinely new
indexed column is unavoidable, stop and ask the orchestrator: v20 must be allocated once for the merged
tree, and `backup.rs` `CURRENT_SCHEMA` plus its required-table list and `backup_restore.rs` must move with
it.

### 2.5 The commit primitive

`DocsStore::board_commit(BoardCommit, permit) -> Result<Option<BoardMutationReceipt>, StoreError>` is the
only board write path. In one transaction it:

1. Returns the stored receipt when `request_id` exists with the same `payload_hash`
   (`BoardRequestConflict` otherwise). No effect, no event.
2. Writes the ticket per `BoardItemWrite`:
   * `Create { item, prefix_hint, placement }` — allocates `number`/`key` (§3.2) and the rank; inserts.
   * `Replace { expected_revision, item, placement }` — CAS on the stored revision
     (`BoardRevisionConflict` → engine `Busy{BOARD_CONCURRENT_CHANGE}`), optional re-rank in the record's
     (possibly new) column.
   * `Rank { work_item_id, placement, .. }` — rank only, no revision check or bump.
   * `None` — auxiliary rows only.
3. Writes every `BoardRowWrite` (links, issue refs, launches, outcomes, versioned intent/review/authoring
   rows with CAS on `expected_version`, authoring entries, dependencies). Unique violations become
   `StoreError::BoardConflict(reason)`.
4. Appends **at most one** `WorkItemEvent` (requires a ticket); fills `Created.key` and
   `Reordered.{column,rank}` from the committed row.
5. Stores the receipt (template filled with the committed key/revision/column/archived/item/event seq).
6. After commit, bumps `DocsStore::board_changes()` (wakes every `BoardWatch` in the process).

Engine helpers wrap it: `EngineRpc::board_commit(commit)` (device write admission + error mapping),
`board_insert_item(...)` (atomic create with extra rows and a chosen event — used by create, conversion
and issue import).

---

## 3. Identity, keys, revisions, idempotency

### 3.1 Stable ids

| Id | Form | Source |
| --- | --- | --- |
| Ticket | `wi-<32 hex>` = SHA-256(`"board-item"`, normalized request id) | Deterministic: a caller can reconcile a timed-out create by id or key |
| Normalized request id | `br-<40 hex>` = SHA-256(caller scope, client key) | Caller scope: `user`, `agent:<task>`, `authoring:<session>`, `engine:<component>` — two chats can reuse a key without collision; a model cannot replay another caller's request |
| Event | `ev-<uuid>` + global `seq` | Store |
| Source link | `wl-<32 hex>` from (request id, task) | Store service |
| Review run / launch / outcome / authoring session | slice-chosen, prefixed (`rr-`, `ls-`, `oc-`, `auth-`), never a chat or provider id | Slices |
| Chat | existing stable LUNA task id (never a provider session id) | Existing |

Client keys (`requestId`): `[A-Za-z0-9_.:-]{1,128}` (`board_request_id_valid`). Reuse a key **verbatim**
after an uncertain result; reconcile with `BoardGetReceipt { requestId }`.

### 3.2 Human ticket keys

`<PREFIX>-<n>`, e.g. `LUN-42`:

* **Prefix** — first three ASCII letters of the project display name, upper-cased (`TKT` when there are
  none), fixed in `board_projects` when the project's first ticket is created. Unique per host: a
  collision gets a numeric suffix (`LUN`, `LUN2`, …). Never derived from a ticket title; renaming the
  project later does not change it.
* **Number** — per-project monotonic counter, allocated inside the creating transaction. Archived
  tickets keep their number; there is no delete in PB-0, so numbers are never reused. (A backup restore
  is a rollback of the whole profile: numbers allocated after the backup point no longer exist and may be
  issued again.)
* Distinct from GitHub issue numbers; an imported ticket shows its own key plus `owner/repo#123`.
* `BoardRead` accepts an id or a key; `parse_board_key` validates `PREFIX-n`.

### 3.3 Revisions

| Counter | On | Meaning |
| --- | --- | --- |
| `WorkItem.revision` | ticket | +1 on every content, workflow, archive or link mutation; **not** on rank-only reorders |
| `WorkItem.field_revisions.*` | ticket | Item revision at which each editable field last changed |
| `WorkItem.brief_revision` | ticket | +1 only when title or description changes; launch snapshots and instructions cite it |
| `BoardExecutionIntent.version` | intent row | CAS version (+1 per write) |
| `BoardExecutionIntent.intent_revision` | intent | +1 per accepted move; callbacks with an older revision are ignored |
| `BoardExecutionIntent.execution_generation` | intent | +1 per admitted execution (first start, continuation, retry); Stop/review/Done tie to one generation |
| `ReviewRun.version`, `TicketAuthoringSession.version` | rows | CAS versions |

### 3.4 Field-level updates

`BoardUpdate` changes only the fields present. It conflicts (`staleRevision`, carrying the current
`WorkItemView`) only when a changed field's `field_revisions` entry is newer than `expectedRevision`; an
edit of an unrelated field from an old revision merges. An edit that changes nothing stores a receipt
without an event or revision bump. A concurrent commit between read and write retries the merge (3
attempts). Column, archive, rank and links are never changed by `BoardUpdate`. Launch snapshots are
separate immutable rows and are never touched by an edit.

### 3.5 Receipts

Every mutation returns `BoardMutationReceipt` (or a slice receipt that embeds one): normalized request
id, payload hash, operation, ticket id/key, resulting revision/column/archived, the committed record, the
event seq (`None` for a no-op), actor, commit time, linked task id when applicable, and a navigation
target. A retry returns the identical receipt.

---

## 4. Records (proposal §3)

All in `crates/proto/src/board/`. "Owner" is the slice that writes it after PB-0.

| Record | Type | Table | Owner | Invariants |
| --- | --- | --- | --- | --- |
| WorkItem | `WorkItem` (`item.rs`) | `board_items` | A1 store; A2 column/`last_report`; A3 title sync | Saving creates no chat; `origin` is provenance, never a parent; distinct from `TaskWorkItem` |
| WorkItemTaskLink | `WorkItemTaskLink` (`records.rs`) | `board_task_links` | A3 (Working/Source/Reference/Attempt), A5 (Reviewer) | One active Working per ticket and per chat; `original_container` frozen at first creation/conversion |
| IssueReference | `IssueReference` | `board_issue_refs` | A7 | One active ref per `(project, dedupe_key)`; remote state separate from workflow |
| LaunchSnapshot | `LaunchSnapshot` | `board_launches` | A2 (A5 for repair requests) | Immutable; requested vs resolved settings; exact instruction text + hash; persisted before sending |
| OutcomeReference | `OutcomeReference` | `board_outcomes` | A5 (A7 writeback) | References, not copies; tied to a generation and basis |
| WorkItemEvent | `WorkItemEvent` (`events.rs`) | `board_events` | every mutation | Exactly one per accepted mutation |
| TicketAuthoringSession | `TicketAuthoringSession` | `board_authoring_sessions` (+ entries) | A6 | Never a chat row; excluded from every chat surface |
| ReviewRun | `ReviewRun` | `board_review_runs` | A5 | Distinct id; placement rule; fixed basis; once-only delivery and cleanup |
| BoardExecutionIntent | `BoardExecutionIntent` | `board_intents` | A2 | Desired state persisted before effects; observed state separate |
| Dependency | `BoardDependency` | `board_dependencies` | A7 | Acyclic; never dispatches work |

Read models: `WorkItemView` (ticket + engine-derived phase, attention, working chat, queue reason, issue
summary, open blockers, last activity, permitted actions), `BoardItemDetail` (view + links, issues,
launches, outcomes, review runs, dependencies, intent, recent events — each bounded),
`BoardProjectSummary`, `ChatTicketBadge`.

---

## 5. State machines

### 5.1 Workflow column (intent) and archive

```
            create (save only)
                 │
                 ▼
  ┌──────── Backlog ◄──────────── Done
  │  move/Start ▲  │ move/Stop      ▲ │ reopen (hold; evidence kept)
  │             │  ▼                │ │
  └──────────► Ready ──────────────┘ │
       engine-only: current checks + review pass for the current generation
```

* `Backlog` = held/stopped (Stop may still be pending — see phase). `Ready` = enabled to run; stays Ready
  through queueing, work, checks, review and repairs. `Done` = engine-owned; never set by a client.
* Within-column reorder: rank only (§3.3).
* `archived` is orthogonal: hides the card in any column, keeps links/history/rank, never stops work or
  touches an external issue; unarchive restores it in place.

| Transition | Effect (A2) |
| --- | --- |
| new → Backlog | persist only; no chat |
| Backlog → Ready, never dispatched | resolve settings/destination, create the worker once (A3), freeze `LaunchSnapshot(FirstStart)`, send once |
| Backlog → Ready, dispatched before | same worker, `Continuation` instruction from its checkpoint; never replay the brief |
| Ready → Backlog, queued/unsent | cancel the unsent launch (`CancelledUnsent`) |
| Ready → Backlog, active | shared Stop (task_work) for the exact generation, including owned reviewers |
| Ready → Ready | rank only |
| Done → Backlog | hold; evidence historical |
| Ready, current result passes | Done only if intent and generation still match; then reviewer cleanup |

### 5.2 Observed phase (`BoardPhase`, engine evidence only)

`NotStarted · Queued · Starting · Working · Checking · Reviewing · Fixing · Idle · WaitingForStop ·
StopRequested · Stopping · Stopped · Failed · Complete · CleanupPending · Unknown`

Typical by column — Backlog: NotStarted, StopRequested, Stopping, Stopped, Unknown. Ready: Queued,
WaitingForStop, Starting, Working, Checking, Reviewing, Fixing, Idle, Failed, Unknown. Done: Complete,
CleanupPending. `Unknown` whenever the owner/runtime cannot be observed; it is never inferred as stopped
or complete. Until A2 lands, `store.rs` uses honest fallbacks: Done → Complete, never dispatched →
NotStarted, otherwise the intent's cached phase or Unknown.

### 5.3 Needs attention overlay

`BoardAttentionReason { kind, detail, attention_id?, task_id? }`, kinds: Question, Approval,
ProviderFailure, AuthRequired, HostUnavailable, CompatibilityBlocked, SettingsUnresolved,
ModelUnavailable, ReviewBlocked (repair cap reached), ReviewUnavailable, StopUnknown, DeliveryUnknown,
DependencyBlocked, CleanupFailed, AgentBlocked, Other. Derived (A2/A5) from existing attention items,
runtime, task-work and review state; `attention_id` routes to the existing engine attention UI.
Acknowledging is not resolving.

### 5.4 Intent revision, execution generation, stop barrier (A2)

`BoardExecutionIntent` holds desired column, `intent_revision`, `execution_generation`,
`last_request_id`, working task, active command/message/turn, `FirstPromptDelivery`
(`NotSent → Pending → Delivered | Unknown`, or `NotApplicable` for converted chats), launch snapshot,
`BoardStopBarrier { execution_generation, request_id, state: Requested|Stopping|Stopped|Unknown|Rejected,
task_ids, stop_receipt_ids }`, queue reason, review/repair counters, cached observed phase and
`needs_reconcile`.

Rules: persist intent before any effect; two windows repeating a move share one request outcome;
Ready during Stopping → `WaitingForStop`, Continue only after verified quiescence; rapid toggles coalesce
to the latest intent behind the barrier; a late Stop acknowledgement or review result for an older
generation changes nothing; restart reads intents (`board_intents_needing_reconcile`) and queries
receipts/runtime before acting; offline owner → Unknown/reconciling, never a launch elsewhere.

### 5.5 Review runs (A5)

`Queued → Starting → Running ⇄ Waiting → Completed | Failed | Cancelled | Stale`, plus `OutcomeUnknown`
(recover by the original launch key). Round 0 = initial review; at most `BOARD_MAX_REPAIR_ROUNDS = 2`
repairs (≤ 3 passes), counters persisted on the intent so Stop/Continue/restart cannot reset them.
Cleanup `NotStarted → Pending → Archived → Closed | Failed` with independent readback; Done may show
`CleanupPending`.

### 5.6 Authoring sessions (A6)

`Active → Saved → Ended`; `Active → Draft` (closed unfinished: resumable Board draft, runtime stopped);
`Draft → Active` (resume); `Active|Draft → Discarded` (draft removed; minimal exclusion metadata kept).

---

## 6. Canonical chat roles (A3)

Roles come from durable lineage, never from tabs, layout, titles or model claims:

* **Main** — no side-chat row (`WorkspaceHost::side_chat_parent`) and no `ChildThread` metadata.
* **Side** — a side-chat row or `ChildThread` (today's side chats and child chats, unified).
  `main_parent_id` is the top-level main ancestor: the side parent, the child's `parent_task_id`, or —
  for legacy nested records — walk parents to the first Main (`legacy_nested = true`).
* `display_container_id` = itself (main) or `main_parent_id` (side).
* **Creator** (`ChatProvenance::CreatedBy`) and **source** (`ForkedFrom`, `HandedOffFrom`) are
  provenance, distinct from the parent. A side-created sibling records the side creator; its parent is
  the main.
* **Purpose** (`ChatPurpose`): `Ordinary`, `TicketWorker { work_item_id, key }`, `Reviewer { … }` —
  from active `board_task_links`.
* **Ticket workers**: `original_container` frozen on the Working link (`MainChat` or `SideOf{main}`),
  `parent_locked = true`, `permitted.reparent = false` always; an unavailable parent sets
  `parent_unavailable` and navigation opens the chat directly.
* **Temporary authoring sessions are not chats**: they have no `CanonicalChatIdentity`; `BoardCaller::
  Authoring` is their only identity.
* **Permitted actions**: main → side fresh/with context, main with handoff, convert, start; side → fresh
  sibling only, new main with handoff, convert, start; never forks/context from a side chat; never
  reparenting. Enforced in `board_validate_creation` for every creation path (UI menus, message Fork,
  `create_thread`, side chats, tools, RPC).

Existing `RuntimeChatRole { Main, Side, Child }` maps to `BoardChatRole { Main, Side }` (Side and Child →
Side).

---

## 7. Operations

### 7.1 Conventions

* RPC method names follow the house PascalCase convention; the `board.*` family is the `Board*` constants
  (`methods::BOARD_METHODS`). Agent tool names are `luna_proto::BOARD_TOOL_NAMES`.
* Requests are `deny_unknown_fields` and never carry the caller. UI requests that act "in a chat" name it
  (`callerTaskId`/`taskId`); tool calls bind it from the run and reject a mismatching value.
* Every mutation takes `requestId`. Errors travel as `RpcError::Failed("board-error:" + JSON of
  BoardError)` (`BoardError::to_rpc_message` / `from_rpc_message`); tools return the same JSON.
* Engine entry points are `impl EngineRpc` methods taking `caller: &BoardCaller` and returning
  `BoardResult<T>`; the dispatcher binds `BoardCaller::User`.

### 7.2 Authority (caller-bound)

| Caller | Bound from | Rules |
| --- | --- | --- |
| `User { device_id }` | IPC owner (dispatcher) | Any project hosted here |
| `Agent { task_id, project_ids, granted_scopes, explicit_user_request }` | `BoardCaller::from_authority(&CrossTaskAuthority)` | Only its own project (`visible_project_ids`); reads need scope `board.read`, writes `board.write` (A4 grants both to ordinary main/side runs). Routine writes inside that project need no extra approval (no `explicit_user_request` gate); reading never grants authority over other projects; a created ticket's origin is always the caller chat; A2 limits `board_set_status` on a ticket to its working chat (or a user-requested manager) and rejects stale generations |
| `Authoring { session_id, project_id }` | A6 session runtime | Only its project; create (origin `Authoring`), read/list, execution options, checkpoint; start only to `NewMain` on an explicit request; never a chat role |
| `Engine { component }` | internal | Lifecycle/review/import automation |

Reviewer attribution (`BoardActor::Reviewer`) is resolved by A5 from the recorded review run, never from
arguments. No extra approval prompt for routine board operations the user requested (existing permission
modes apply to launched work).

### 7.3 Operation table

| Operation | RPC | Tool | Request → Response | Slice |
| --- | --- | --- | --- | --- |
| List | `BoardList` | `board_list` | `BoardListRequest` → `BoardListPage` | A1 ✔ |
| Read | `BoardRead` | `board_read` | `BoardReadRequest` → `BoardItemDetail` | A1 ✔ |
| Create (save only) | `BoardCreate` | `board_create` | `BoardCreateRequest` → `BoardMutationReceipt` | A1 ✔ |
| Update | `BoardUpdate` | `board_update` | `BoardUpdateRequest` → `BoardMutationReceipt` | A1 ✔ |
| Rank | `BoardRank` | (UI) | `BoardRankRequest` → `BoardMutationReceipt` | A1 ✔ |
| Archive/unarchive | `BoardArchive` | (A4 exposes it as a `board_control` action) | `BoardArchiveRequest` → `BoardMutationReceipt` | A1 ✔ |
| Journal | `BoardEvents` | — | `BoardEventsRequest` → `BoardEventPage` | A1 ✔ |
| Watch | `BoardWatch` (stream) | — | `BoardWatchRequest` → `BoardWatchFrame`* | A1 ✔ |
| Reconcile | `BoardGetReceipt` | — | `BoardReceiptRequest` → `Option<BoardMutationReceipt>` | A1 ✔ |
| Move | `BoardMove` | `board_move` | `BoardMoveRequest` → `BoardMoveReceipt` | A2 |
| Start | `BoardStart` | `board_start` | `BoardStartRequest` → `BoardStartReceipt` | A2 |
| Batch start | `BoardStartBatch` | `board_start` (list form) | `BoardStartBatchRequest` → `BoardStartBatchReceipt` | A2 |
| Control | `BoardControl` | `board_control` | `BoardControlRequest` → `BoardControlReceipt` | A2 |
| Status report | `BoardSetStatus` | `board_set_status` | `BoardSetStatusRequest` → `BoardMutationReceipt` | A2 |
| Send update | `BoardSendUpdate` | `board_send_update` | `BoardSendUpdateRequest` → `BoardSendUpdateReceipt` | A2 |
| Execution options | `BoardExecutionOptions` | `board_execution_options` | `BoardExecutionOptionsRequest` → `BoardExecutionOptions` | A2 |
| Settings preview | `BoardResolveSettings` | — | `BoardResolveSettingsRequest` → `BoardResolvedSettings` | A2 |
| Chat identity | `BoardChatIdentity` | (context) | `BoardChatIdentityRequest` → `CanonicalChatIdentity` | A3 |
| Badges | `BoardChatBadges` | — | `BoardChatBadgesRequest` → `BoardChatBadges` | A3 |
| Side chat | `BoardCreateSideChat` | `create_side_chat` | `BoardCreateSideChatRequest` → `BoardChatCreated` | A3 |
| Main chat / handoff | `BoardCreateMainChat` | `create_main_chat` | `BoardCreateMainChatRequest` → `BoardChatCreated` | A3 |
| Convert chat | `BoardConvertChat` | `board_convert_chat` | `BoardConvertChatRequest` → `BoardConvertReceipt` | A3 |
| Link chat | `BoardLinkChat` | `board_link_thread` | `BoardLinkChatRequest` → `BoardMutationReceipt` | A3 |
| Go to chat | `BoardNavigation` | (in receipts) | `BoardNavigationRequest` → `BoardNavigationTarget` | A3 |
| Review start/status/cancel | `BoardReviewStart` / `…Status` / `…Cancel` | `review_start` / `review_status` / `review_cancel` | → `ReviewRunReceipt` / `BoardReviewStatus` | A5 |
| Review submit | `BoardReviewSubmit` | `review_submit` | `BoardReviewSubmitRequest` → `ReviewRunReceipt` | A5 |
| Evidence | `BoardOutcomes` | — | `BoardOutcomesRequest` → `BoardOutcomes` | A5 |
| Authoring | `BoardAuthoringStart` / `Send` / `Read` / `Watch` (stream) / `Checkpoint` / `Interrupt` / `Close` / `Discard` / `List` | `board_draft_checkpoint` (+ board tools under `Authoring`) | `BoardAuthoring*Request` → `TicketAuthoringSession` / `BoardAuthoringPage` / `BoardAuthoringList` | A6 |
| Issue sources/search/detail/comments | `BoardIssueSources` / `Search` / `Detail` / `Comments` | `board_issue_search` | → `BoardIssueSources` / `IssueSearchPage` / `IssueDetail` / `IssueCommentPage` | A7 |
| Import / link / unlink | `BoardIssueImport` / `Link` / `Unlink` | `board_issue_import` | → `BoardIssueImportReceipt` / `BoardMutationReceipt` | A7 |
| Dependencies | `BoardSetDependency` | `board_set_dependency` | `BoardSetDependencyRequest` → `BoardMutationReceipt` | A7 |
| Writeback (PB-3) | `BoardIssueWritebackPreview` / `BoardIssueWriteback` | — (UI, explicit) | → `IssueWritebackPreview` / `IssueWritebackReceipt` | A7 |
| Tool diagnostics | `BoardToolCatalog` | — | `{taskId}` → JSON | A4 |

### 7.4 Errors

`BoardError` (`#[serde(tag = "code")]`): `notFound`, `staleRevision{current}`, `idempotencyConflict`,
`invalidInput{field,reason}`, `projectNotFound`, `wrongHost{ownerHostId}`, `ambiguousProject`,
`insufficientScope{required}`, `notAuthorized`, `unavailableProvider`, `unavailableModel{supported}`,
`unsupportedEffort{supported}`, `invalidRole{allowed}`, `invalidDestination{canonical}`,
`notConvertible`, `parentLocked{originalParentId}`, `invalidTransition{from,to}`, `busy` (incl.
`BOARD_CONCURRENT_CHANGE`; retry with the same key), `dependencyCycle{path}`,
`duplicateIssue{existingWorkItemId}`, `sourceUnavailable{retryAfterMs}`, `outcomeUnknown{requestId}`,
`unavailable`, `storage`, `notImplemented{operation}`. Never silently substitute a model, destination or
project.

### 7.5 Engine signatures per slice (all `impl EngineRpc`, `caller: &BoardCaller`)

**A1 store (`store.rs`, implemented)** — `board_create(BoardCreateRequest) -> BoardMutationReceipt`,
`board_read(BoardReadRequest) -> BoardItemDetail`, `board_list(BoardListRequest) -> BoardListPage`,
`board_update(BoardUpdateRequest) -> BoardMutationReceipt`, `board_rank(BoardRankRequest) ->
BoardMutationReceipt`, `board_archive(BoardArchiveRequest) -> BoardMutationReceipt`,
`board_get_receipt(BoardReceiptRequest) -> Option<…>`, `board_events(BoardEventsRequest) ->
BoardEventPage`, `board_watch(BoardWatchRequest) -> BoxStream<Value>`. Shared helpers:
`board_load(id_or_key)`, `board_item_for(caller, id, BoardAccess)`, `board_replay(request_id, hash)`,
`board_view(WorkItem) -> WorkItemView`, `board_default_navigation(&WorkItem)`, `board_insert_item(caller,
operation, BoardCreateRequest, origin, rows, event, task_id)`, `board_commit(BoardCommit)`,
`board_project(caller, project_id, access) -> Space`, `board_check_caller`, `board_write_permit`,
`board_hub() -> &BoardHub` (`gate(item).await`, `notify_observed(ids)`).

**A2 lifecycle (`lifecycle.rs`)** — `async board_move(BoardMoveRequest) -> BoardMoveReceipt`; `async
board_start(BoardStartRequest) -> BoardStartReceipt`; `async board_start_batch(BoardStartBatchRequest) ->
BoardStartBatchReceipt`; `async board_control(BoardControlRequest) -> BoardControlReceipt`; `async
board_set_status(BoardSetStatusRequest) -> BoardMutationReceipt`; `async
board_send_update(BoardSendUpdateRequest) -> BoardSendUpdateReceipt`; `async
board_execution_options(BoardExecutionOptionsRequest) -> BoardExecutionOptions`; `async
board_resolve_settings(BoardResolveSettingsRequest) -> BoardResolvedSettings`; `board_observe(&WorkItem,
Option<&BoardExecutionIntent>) -> BoardObservation` (called by every read model; `NotImplemented` →
fallback); `board_validate_requested_settings(&Space, &BoardExecutionProfile, &BoardReviewProfile) -> ()`
(called on create/update; `NotImplemented` → shape-only); `async board_reconcile_on_boot() ->
BoardReconcileReport`; `async board_deliver_outcome(work_item_id, execution_generation) -> ()`.

**A3 chat model (`chat_identity.rs`)** — `board_chat_identity(BoardChatIdentityRequest) ->
CanonicalChatIdentity`; `board_chat_badges(BoardChatBadgesRequest) -> BoardChatBadges`; `async
board_create_side_chat(BoardCreateSideChatRequest) -> BoardChatCreated`; `async
board_create_main_chat(BoardCreateMainChatRequest) -> BoardChatCreated`; `async
board_convert_chat(BoardConvertChatRequest) -> BoardConvertReceipt`; `async
board_link_chat(BoardLinkChatRequest) -> BoardMutationReceipt`; `board_navigation(BoardNavigationRequest)
-> BoardNavigationTarget`; `async board_sync_title(BoardTitleOrigin, &str) -> BoardTitleSyncReceipt`;
`async board_title_changed(&WorkItem, BoardTitleOrigin) -> ()` (hook from `board_update`);
`board_validate_creation(caller_task_id, BoardChatCreationKind) -> CanonicalChatIdentity` (no caller
arg); `async board_migrate_legacy_nesting() -> BoardLegacyMigrationReport`.

**A4 tools/context (`tools.rs`, `context.rs`)** — `board_tool_definitions(BoardToolAudience) ->
Vec<RuntimeToolDefinition>`; `async board_tool_call(caller, tool: &str, arguments: Value) -> Value`;
`board_tool_catalog(task_id) -> Value`; `board_runtime_context(task_id) ->
Option<BoardRuntimeContext>`; `board_authoring_runtime_context(session_id) -> BoardRuntimeContext`.

**A5 review (`review.rs`)** — `async board_review_start(BoardReviewStartRequest) -> ReviewRunReceipt`;
`board_review_status(BoardReviewStatusRequest) -> BoardReviewStatus`; `async
board_review_cancel(BoardReviewCancelRequest) -> ReviewRunReceipt`; `async
board_review_submit(BoardReviewSubmitRequest) -> ReviewRunReceipt`; `async
board_review_cleanup(review_run_id) -> ReviewCleanupReceipt`; `board_review_owned_work(work_item_id,
generation) -> Vec<String>` (for shared Stop); `async board_capture_outcomes(work_item_id, generation) ->
Vec<OutcomeReference>`; `board_outcomes(BoardOutcomesRequest) -> BoardOutcomes`.

**A6 authoring (`authoring.rs`)** — `async board_authoring_start(BoardAuthoringStartRequest)`, `async
board_authoring_send(BoardAuthoringSendRequest)`, `async board_authoring_checkpoint(…)`, `async
board_authoring_interrupt(…)`, `async board_authoring_close(…)`, `async board_authoring_discard(…)` →
`TicketAuthoringSession`; `board_authoring_read(BoardAuthoringReadRequest) -> BoardAuthoringPage`;
`board_authoring_watch(BoardAuthoringSessionRequest) -> BoxStream<Value>`;
`board_authoring_list(BoardAuthoringListRequest) -> BoardAuthoringList`. Exclusion predicate:
`DocsStore::is_board_authoring_session(id)`.

**A7 GitHub (`github.rs`)** — `async board_issue_sources(…) -> BoardIssueSources`, `async
board_issue_search(…) -> IssueSearchPage`, `async board_issue_detail(…) -> IssueDetail`, `async
board_issue_comments(…) -> IssueCommentPage`, `async board_issue_import(…) -> BoardIssueImportReceipt`,
`async board_issue_link(…)`, `async board_issue_unlink(…)`, `async board_set_dependency(…)` →
`BoardMutationReceipt`, `async board_issue_writeback_preview(…) -> IssueWritebackPreview`, `async
board_issue_writeback(…) -> IssueWritebackReceipt`.

Internal hooks take no caller: `board_observe`, `board_validate_requested_settings`,
`board_reconcile_on_boot`, `board_deliver_outcome`, `board_title_changed`, `board_validate_creation`,
`board_migrate_legacy_nesting`, `board_review_cleanup`, `board_review_owned_work`,
`board_capture_outcomes`, `board_tool_definitions`, `board_tool_catalog`, `board_runtime_context`,
`board_authoring_runtime_context`. Stub modules carry `#![allow(dead_code)]` for entry points not yet
called; remove it when wiring them.

### 7.6 Slice notes that affect the contract

* **Conversion (A3)** = `board_insert_item(caller, BoardOperation::ConvertChat, request, BoardItemOrigin::
  Converted{task}, [TaskLink(Working, original_container frozen from the chat's canonical role)],
  Some(WorkItemEventKind::Converted{task}), Some(task))` — one commit, one event, nothing sent;
  `FirstPromptDelivery::NotApplicable` when an intent is later created.
* **Import (A7)** = `board_insert_item(…, BoardOperation::IssueImport, …, BoardItemOrigin::Issue{ref},
  [IssueReference], Some(IssueLinked{ref}), None)`. Check `board_issue_ref_by_dedupe` under
  `board_hub().gate(...)` first; a racing unique violation surfaces as `Busy` — re-read the dedupe key and
  return `Existing`/`DuplicateIssue`.
* **Moves (A2)** commit the column change (`Replace` with `placement`), the intent row (CAS) and one
  `Moved` event together; launches add a `Launch` row in the same or a following commit **before** the
  send. Observation-only changes call `board_hub().notify_observed(ids)` (no journal event).
* **Title sync (A3)**: `board_update` calls `board_title_changed` after a committed title change; chat
  renames (the `Mutate renameChat` path and provider auto-titling) must route through `board_sync_title`.
* **Reviewer creation (A5)** uses A3's side-chat creation with `ChatPurpose::Reviewer`; reviewer links
  are `TaskLinkRole::Reviewer` (never Working).

---

## 8. Events, watch and activity

* One canonical `WorkItemEvent` per accepted mutation, in the same transaction; replays and no-ops add
  none (identical status reports are no-ops — A2).
* `WorkItemEventKind` (internally tagged `kind`) already lists every slice's kinds; unknown future kinds
  decode as `Unknown`. `activity` is false for rank-only events (Activity/search show only activity
  events).
* `BoardWatch { scope, afterSeq }` emits a first frame immediately (journal head), then one frame after
  each committed change or observation ping in scope: new events (bounded to 200, else `reset: true`),
  `observed` item ids, `reset` when the cursor is unknown (e.g. after a restore). Clients re-read with
  `BoardList`/`BoardRead`.
* `BoardList.asOfSeq` is read before the page so a watch from it cannot miss a change.

---

## 9. UI architecture

* `SidebarPage::Board` (key `board`) directly after Automations in `SidebarNavItem::ALL`; icon
  `icons::CLIPBOARD_LIST` (existing Solar Linear glyph; a final glyph can be chosen on an icon picker
  page). Route `Route::Page(Board)`, `NavEntry::Page` back/forward, selected state from the route,
  command palette **Open Board**, `LUNA_OPEN_ROUTE=board`.
* Restart restoration: `UiSettings.last_sidebar_page` follows the route (`Shell::sync_restored_page`);
  boot restores it and `boot_select_chat` no longer replaces a restored page with the new-thread canvas.
  Reachable with zero projects, issues or chats.
* `UiSettings.board: BoardUiPrefs` — last project scope, Board/List view, search and filters, selected
  ticket, scroll offset (A8 reads/writes; presentation only).
* Data layer (`crates/ui/src/shell/board/`): `BoardApi` (one typed async method per unary `Board*` RPC,
  plus `watch`/`authoring_watch` subscriptions; errors as `BoardCallError::{Board, Lost, Unsupported,
  Other}` — `Lost` means reconcile by request id), `BoardModel` (entity per page: scope, filters, items,
  projects, status; loads via `BoardList`, follows `BoardWatch`, re-reads on events/observations/reconnect;
  `apply_receipt` shows a committed record immediately), `BoardPage` (minimal three-column page).
  `Shell::board_page()` creates it with the sidebar's project filter, else the last Board scope, else All
  projects. QA hook: `Shell::fixture_board_page()`.

---

## 10. File ownership map

| Slice | Owns (create/modify freely) | May touch minimally (list in "Shared-file edits") |
| --- | --- | --- |
| A1 foundation | everything listed in §1; `crates/engine/tests/board_store_e2e.rs` | — |
| A2 lifecycle | `crates/engine/src/board/lifecycle.rs`; `crates/engine/tests/board_lifecycle_e2e.rs` | `rpc/task_work.rs`, `sessions_work.rs` (shared Stop scope), project defaults/model cache readers, boot hook to call `board_reconcile_on_boot` |
| A3 chat model | `crates/engine/src/board/chat_identity.rs`; `crates/engine/tests/board_chat_model_e2e.rs` | `child_threads.rs`, `side_chats.rs`, `rpc/side_chats.rs`, fork paths, `Mutate renameChat`/title paths in `rpc.rs` and `titles.rs` (guards + title sync), `workspace_host.rs` lineage readers |
| A4 tools | `crates/engine/src/board/tools.rs`, `crates/engine/src/board/context.rs`; `crates/engine/tests/board_tools_e2e.rs` (fake CLIs for Codex/Claude/OpenCode) | `cross_task_tools.rs`, `runtime_contract.rs` (proto + engine; additive `board` context field and scopes), `harness/src/codex/luna_mcp.rs`, `harness/src/claude/luna_tools.rs`, OpenCode MCP delivery |
| A5 review | `crates/engine/src/board/review.rs`; `crates/engine/tests/board_review_e2e.rs` | archive-with-close cleanup path, `diff_sync`/`git_actions` readers, `github_pr` readers for PR evidence |
| A6 authoring | `crates/engine/src/board/authoring.rs`, `crates/ui/src/shell/board/authoring_pane.rs`; `crates/engine/tests/board_authoring_e2e.rs`, `crates/ui/examples/board-authoring-fixture.rs` | chat listing/search/import exclusion points (`local_import.rs`, history search, `workspace_host.rs`), sessions engine for the scoped runtime |
| A7 GitHub | `crates/engine/src/board/github.rs`, `crates/ui/src/shell/board/github_picker.rs`; `crates/engine/tests/board_github_e2e.rs`, `crates/ui/examples/board-github-fixture.rs` | `source_control.rs` (+ an `issues` submodule) |
| A8 Board page | `crates/ui/src/shell/board/{page,columns,card,list,filters}.rs`; `crates/ui/examples/board-page-fixture.rs` | `shell.rs` page wiring, `UiSettings.board` use, icons |
| A9 Board detail | `crates/ui/src/shell/board/{detail,manual_form,settings_controls,evidence}.rs`; `crates/ui/examples/board-detail-fixture.rs` | page hooks to open the drawer/form |
| A10 chat UI | `crates/ui/src/shell/board/chat_badges.rs`; `crates/ui/examples/board-chat-ui-fixture.rs` | `shell/workbench/*`, `shell/spaces.rs`, `shell/side_chats.rs`, `shell/tabs.rs`, chat header, activity center/search entries |

Shared by everyone, additive only: `crates/proto/src/board/*` (add fields/variants in your area),
`crates/ui/src/shell/board/api.rs` and `model.rs` (add helpers; do not change existing signatures),
`crates/ui/src/icons.rs` + `crates/ui/assets/icons/` (Solar Linear). Nobody edits
`crates/engine/src/rpc/board.rs`, `board/mod.rs`, `board/store.rs`, the v19 migration or
`crates/rpc/src/lib.rs` method lists without a contract change.

---

## 11. Failure inventory → E2E coverage

Engine tests live in `crates/engine/tests/`, UI fixtures in `crates/ui/examples/`. Abbreviations: S =
`board_store_e2e.rs` (A1, done), L = `board_lifecycle_e2e.rs` (A2), C = `board_chat_model_e2e.rs` (A3),
T = `board_tools_e2e.rs` (A4), R = `board_review_e2e.rs` (A5), AU = `board_authoring_e2e.rs` +
`board-authoring-fixture` (A6), G = `board_github_e2e.rs` + `board-github-fixture` (A7), P =
`board-page-fixture` (A8), D = `board-detail-fixture` (A9), CU = `board-chat-ui-fixture` (A10).

### 11.1 Proposal §8

| Risk | Covered by |
| --- | --- |
| Double-click Start; timeout after acceptance | L |
| Crash between saving card and linking chat | S (atomic save), L (restart reconcile) |
| Two devices import the same issue | G (concurrent import → one card, unique index) |
| Edit brief while a run is active | L (snapshot immutable, explicit send), S (update never touches snapshots) |
| Issue changes or closes during preview | G |
| Branch/repository changes before Start | L |
| Private repo auth expires / account switch | G |
| Rate limit, timeout, partial pagination | G |
| PR appears in issue API results | G |
| Large body/comments/attachments | G |
| Host disconnects; stale observation | L (Unknown), P (display) |
| Provider switches / native ids change | C |
| Parent turn ends while child work continues | L |
| Agent claims success but checks/review fail | R |
| Non-code task returns a document | R |
| Drag Backlog/Ready during execution | L, P |
| Malicious issue text | G, T |
| External write succeeds, response lost | G |
| Concurrent rank/status edits | S (rank LWW, field merge), L (status) |
| Older client reads upgraded workspace | S (additive v18→v19 upgrade; older builds refuse newer schema rather than drop data) |
| Agent creates an issue then retries | T, S |
| Create-and-start partly succeeds | L, G |
| Requested model/reasoning unavailable | L, D |
| Late worker or concurrent human changes status | L |
| Source chat closes before work finishes | L |
| Compaction/handoff loses board context | T |
| Provider omits board tool delivery | T |
| Sidebar page switch changes project unexpectedly | P |

### 11.2 Board lifecycle §4

Intent persisted before launch / two windows → L; Ready while Stopping → L; rapid coalescing → L; Stop
tied to generation → L; late successful review after Backlog → R; offline owner → L; capacity/
dependency/checkout queue reasons → L (dependencies with G); restart restores intent before acting → L.

### 11.3 Automatic review §9 failures

Unknown model/effort, unsupported read-only execution, incomplete context, stale diffs, reviewer crash,
lost launch response, parent restart, repeated findings, exhausted budget/repair cap, cancelled owner,
late results, wrong-provider attribution → R (placement and cleanup UI → CU).

### 11.4 Ticket authoring §6

Temporary-session leakage into chat/Settled/Archive/search/import, duplicate creation after timeout,
closing during an active tool call, model/account loss, incompatible reasoning, deferred settings,
escalation hidden in a proposal, authoring permissions copied to execution, execution access applied to
authoring, stale project selection, provider-history re-import after restart → AU; provider tool parity →
T; manual-form settings → D.

### 11.5 Chat model §7

Save creates zero chats → S (Board/normal-chat save), T (agent save); Start from Board/parent, convert
main/side, three tickets, Go to chat after restart, rename sync, reference vs working badge, duplicate
launch/conversion, legacy migration, missing/archived parent, depth/context restriction, side → main
handoff, side → sibling, self-promotion attempts, harness role context, new main creates side chats,
legacy nested active work → C (engine) + CU (badges, navigation, menus); role context after
restart/compaction → T.

---

## 12. E2E plan (proposal §9 items 1–17)

Each run writes a manifest under `target/qa/board/<slice>/` (steps, expected/observed, SHA-256 of
snapshots, commit). No live providers, no network, no real GitHub writes in tests; live GitHub reads are
orchestrator QA.

| # | Scenario | Files |
| --- | --- | --- |
| 1 | Backlog → Ready → Stop → Continue → Done | L (engine), R (review + cleanup), P (drag) |
| 2 | Intervention and recovery | L, CU (chat/board agreement) |
| 3 | Duplicate and restart protection | S (create/receipts/restart), L (start), G (import) |
| 4 | GitHub read (recorded fixtures; live read is QA) | G |
| 5 | Ownership and freshness | G, L (target re-resolution) |
| 6 | Review and writeback (PB-3) | R (evidence), G (writeback/readback), D (evidence UI) |
| 7 | Native usability and compatibility | P (keyboard, narrow list, focus), S (older profile upgrade) |
| 8 | Normal-chat authoring and launch | T, AU, D (manual form), L (settings) |
| 9 | Agent status and shared state | L, R, T |
| 10 | Provider and continuation parity | T, L (once-only delivery) |
| 11 | Sidebar placement and scope | P (S covers scope rules in the engine) |
| 12 | Dependencies and admission limits | G (blocked-by, cycles), L (batch admission/queue) |
| 13 | Brief revisions and external status | L, S, G |
| 14 | Chat identity, fixed parent, independent handoff | C, CU |
| 15 | Temporary reviewer chats across providers | R |
| 16 | Drag admission and continuation races | L, P |
| 17 | Temporary authoring and real board tools | AU, T |

`board_store_e2e.rs` failure cases (header of the file): save-only creation (zero chats/launches, Source
link only); idempotent retries (sequential, concurrent, conflicting payload); stale vs merged field edits
and no-op edits; restart preservation; keys (monotonic, never reused, host-unique prefixes, not
title-derived); All-projects scope; archive hides but preserves; backup → restore of every board table
(at the original data-root path) without post-backup tickets; rank without execution effect; invalid
input; one canonical event per mutation through `BoardWatch`; every contract method routed with
structured errors; additive v18 → v19 upgrade.

---

## 13. Known limitations and deliberate deferrals

* Remote-device board access (§2.3) and any synced projection: deferred.
* Rank keys grow by bisection; rebalancing is deferred (keys stay correct, only longer).
* Post-filters (attention/provider/source) may return fewer than `limit` items per page; `nextCursor`
  continues.
* Restore rollback can reissue ticket numbers allocated after the backup point (§3.2).
* Backup activation refuses a root that holds files outside its allowlist (e.g.
  `profiles/local/terminals/terminals.sqlite`), which any root a live engine has used contains; the E2E
  therefore restores onto a reinstalled root at the same path. This predates the board.
* PB-4 (feature groups, more trackers, automatic dependency dispatch) is out of scope.
