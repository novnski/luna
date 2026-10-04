# Persistent fork threads

Requested on 2026-10-04 after the chat-workbench milestone (`0800dfc6`).
A child starts a fresh conversation; a side thread starts from a fork or
frozen context snapshot. After creation both use ordinary persistent thread
management. Fork origin is provenance, not a lifetime or visibility rule.

## Implemented behavior

1. Retain the existing workspace chat IDs, documents, TaskIdentity bindings
   and side metadata/context index. Reconcile locally materialized legacy
   ended/expired/unloaded/closing rows without invoking fork creation again.
   Unmaterialized creation failures remain explicit failures.
2. Remove side-only expiry, lease, End, unload/reload and read-only behavior.
   Normal close/reopen affects the view. Normal archive/delete are explicit
   thread operations. Source deletion never cascades into surviving forks.
3. Publish project identity and fork-origin metadata together. Support normal
   sidebar placement, workbench Main, marks, nested forks and child creation.
   Preserve existing local marks while moving them to ordinary thread fields.
4. Route commands, configuration, queueing, execution, recovery and runtime
   capabilities through the ordinary managed-task contracts. Initial fork
   settings remain unchanged; later explicit configuration changes obey normal
   command fences and binding-generation rules. Source configuration must not
   override a fork's own configuration after creation.
5. Preserve the no-replay contract for legacy direct side sends. A missing
   imported managed binding is a normal actionable binding-recovery error,
   never permission to silently create a replacement provider conversation.
   Keep history visible and preserve its original task ID and provenance.
6. Verify migration, lifecycle, nested creation, source independence, provider
   identity and import/recovery boundaries through E2E scenarios, then refresh
   the signed developer app and deliver the verified changes to `origin/main`.

SQL side tables remain as context/provenance and cold-history adapters for
compatibility with backups and old clients. They no longer define a separate
user-facing thread lifecycle. Provider-native creation timeouts remain ordinary
operation timeouts; they are not expiration policies.

Codex owns engine, shared contracts, migration, E2E and delivery. Claude Code
owns UI and native workbench fixture changes in the same checkout. The user
requested one read-only subagent audit; it supplied source references and made
no edits. Preserve unrelated `docs/pending-tasks/` work.

## Failure cases recorded before implementation

- Migration creates a second fork, changes a task/provider ID or binding
  generation, loses an inherited context mapping, history, draft or receipt,
  or replays an interrupted send. Repeating startup must be idempotent.
- Locally materialized ended/expired/unloaded/closing/closeFailed records stay
  blocked by old lifecycle guards, or list/open/restart starts a provider.
- An incomplete reservation or imported prior-session record is mistaken for
  a verified managed binding and silently starts a replacement conversation.
- Closing/reopening a view stops a live turn, loses its draft, requires a
  lease, displays a countdown, or turns a fork into read-only history.
- Source archive/delete ends or deletes a fork, or later sends require the
  missing source config. A surviving fork becomes undiscoverable.
- Deleting an open source leaves a layout watcher stale until its next
  mutation, so opening the surviving fork loses that action to an internally
  generated revision conflict. Deletion must publish the pruned layout first.
- Fork-from-fork or child-from-fork is categorically refused, while allowing
  it bypasses ordinary project, permission, provider-readiness, depth or cycle
  checks. Approved cross-task operations must use the normal scope guards.
- Ordinary model/account/project changes are accepted then silently overridden
  by frozen side metadata, or bypass native binding/account validation.
- Ordinary queued sends cannot recover because their task has fork lineage;
  enabling recovery accidentally replays legacy direct side-send receipts.
- Startup reconciliation, usage/history indexing, attention or runtime tool
  capabilities still omit otherwise ordinary managed forks.
- Archive/pin/settle/placement state is lost when old local marks migrate, or
  an archived fork becomes visible/runnable without explicit unarchive.
- A fork cannot be selected as workbench Main, its source icon/link disappears,
  or nested descendants disappear from the Related reopen library.
- Codex or Claude native forks resume a different provider conversation after
  close/restart/source deletion. Snapshot forks duplicate their frozen context
  or create a provider before first Send.
- The first legacy direct send after ClearContext or SwitchProvider resumes an
  empty or old session instead of the explicit fresh binding generation. A
  native fork's registry session and managed binding disagree without a clear
  error; no provider may run against that disagreement.
- Managed restore and local import are conflated. Missing account/home or
  missing managed identity must preserve history and fail clearly without
  creating, forking or binding a different conversation automatically.
- An imported fork has indexed messages but no document snapshot. Opening the
  ordinary view creates an empty document, hides or deletes that sole history,
  or rewrites unverified imported stream status during startup reconciliation.
- A nested native fork starts a provider mutation before discovering that a
  completed display segment has no exact checkpoint (for example, a segment
  split by steering). Validate inherited-history mapping prerequisites before
  mutation; report the normal history precondition and offer snapshot creation.

## Evidence

New milestone output belongs under `target/qa/persistent-fork-threads/`.
The completed workbench/native evidence in
`target/qa/super-chat-workbench/native-workbench-final/` must stay immutable.
The existing isolated QA profile contains a real ended Codex fork with its
recorded binding and reply, making it a useful migration/resume control.

The workbench keeps its rounded outer frame when either sidebar opens.
An inset content viewport clips overflowing groups inside that frame; edge
fades indicate more content. Selected tabs use a rounded fill, with a soft
accent tint on the focused group's selection and a neutral fill elsewhere.
There is no tab underline or full pane accent border. Tabs and the compact source/status caption
share one uninterrupted header: horizontal rules beneath both rows are
removed, while the seam between split chats remains. Saved splits, minimum
pane sizes, left anchoring
and the Balance action retain their existing behavior.

See [verification](verification.md) for repeatable checks, evidence and limits.
