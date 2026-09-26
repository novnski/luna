# Workspace association repair: lifecycle authority implementation

Base: committed main `3a173e84a0d6f775499ef19e833e31709cf1ac74`.
Lifecycle branch: `codex/workspace-repair-authority` in `c906/luna`.
Status: failure-first preparation; **no builds, providers or runtime changes yet**.
Storage migration **15** is reserved to this owner. Workspace owns the identity
allocation/proof/association event and repair intent. Shared interfaces must be
agreed before runtime implementation; the old audit ref and artifacts are retained.

## Failure matrix, written before runtime code

| Boundary case | Required result |
| --- | --- |
| Atomic successful proof-only repair | CAS expected task R and workspace W, persist mapping/snapshots, Applied receipt, R+1 and W=R+1 in one transaction; binding ID/generation and historical receipts remain unchanged. |
| Wrong expected R or W | Refuse without mapping, W or execution-authority changes; no provider call. |
| Same command/payload replay, including restart | Return original receipt and apply no second repair, revision increment or grant. Different payload under the same ID remains a conflict. |
| SQLite failure during repair | No partially advanced R/W, mapping, receipt or grant. Reopen reads the prior state; a deliberate retry remains possible. |
| Commit succeeded but publication did not | Reopen obtains the complete new state; replay returns the original receipt without reapplying it or rolling back another task. |
| Old queue/command row returns after repair | Retain its original grant W. Matching row/hash and unchanged binding generation do not allow dispatch under the new task W. Missing legacy stamps mean 0, never the current W. |
| Edit, move, claim, acceptance-unknown or expiry projection | Preserve the original W while replacing a row hash; do not refresh stale authority implicitly. The hash/W CAS and document projection commit together. |
| Explicit new consent versus old receipt | An old duplicate receipt never grants new authority. A newly reviewed Send/Steer command with current R/W can explicitly adopt the exact visible row; stale expected R or old W refuses. |
| Repair races a queue drain or already accepted Run | Either execution wins and repair reports blocked without changing W, or repair wins and the dispatch's original W fails the final post-transition comparison. No stale request starts afterward. |
| Steering and warm reuse | Original W must pass before mailbox delivery. Active/pending turns and all idle warm handles block this first repair version; no implicit retirement, new session, or binding generation. |
| Scheduled/held recovery at repair admission | Block repair without cancelling/releasing the chain, changing its acceptance or decrementing its budget. Include Scheduled, Running, CheckingDelivery, Held, RecoveryNeeded and WaitingForSignIn. |
| Old recovery lineage restored after repair | Its original W remains in private frozen-request storage/CAS. Neither a timer nor an explicit retry silently replaces W. Reject/hold before provider dispatch, retaining original receipt and unknown-delivery semantics. |
| Lock order and final state | Task-command lane precedes drain and execution gates. Never hold the task transition gate and then await drain. Quiescence recheck and R/W commit must exclude fresh executable admission; SQLite/projection locks contain no provider await. |
| Nonquiescent or unrelated sibling | A blocked task changes nothing. Another task's repair/admission may proceed, and an unrelated identity-log append is preserved. |
| Fresh post-repair work | New explicit admission captures current W and executes exactly once. Unrelated rename/queue edits advancing R do not invalidate otherwise current grants because W changes only on repair. |
| Legacy compatibility before first repair | Existing unstamped grants/recovery have W=0 and remain usable while task W=0, subject to all existing owner/hash/binding/replay fences. |
| Full-profile restore/rollback | No claim of anti-replay when the high-water state and authority are rolled back together. Restore/rebuild must retain v2/W or fence unresolved legacy task state; existing restore behavior/tests must remain intact. |

## Agreed narrow contract

- Keep existing `CommandReceipt` and `TaskCommandSnapshot` wire records unchanged.
  Add a server-local `WorkspaceRepairFence` with fields `expected_task_revision`
  and `expected_workspace_repair_revision` and a store W
  getter. W is not a caller-supplied run option or provider parameter.
- Schema 15 owns current W and original admission/grant/recovery stamps. Original
  receipt JSON remains historical; private columns/records carry new evidence.
- A dedicated store completion operation extends the existing command/snapshot
  transaction with final R/W CAS and W advancement to the actual new R. Identity
  event preparation may use checked expected R+1 because commit verifies that R.
- A lifecycle quiescence guard checks queue rows, pending/claimed/executing
  commands, retryable recovery and any live handle while following existing
  gate order. The first implementation blocks instead of retiring warm runtimes.
- Original W travels internally from admission/frozen recovery through edits,
  claims and dispatch. The final check runs after task transition acquisition,
  before native resume, warm reuse, explicit steering or fresh provider startup.

Exact agreed owner boundary:

```text
DocsStore::workspace_repair_revision(task_id) -> Result<u64, StoreError>
WorkspaceHost::commit_workspace_identity_repair(
    command_id, task_id, &WorkspaceRepairFence,
    binding_id, binding_generation, identity_snapshot
) -> Result<CommandReceipt, TaskProjectionError>
DocHost::prepare_workspace_identity_repair(task_id, repair_command_id: Option<&str>)
    -> async Result<WorkspaceRepairGuard, EngineError>
```

Preview passes no exempt command; apply exempts only its own pending repair
receipt, checked against exact task and intent. The dedicated storage commit
accepts a pending repair receipt only. Normal duplicate command replay returns
its original terminal receipt in the existing RPC admission layer, before
staging any identity projection. An unexpected terminal invocation of the store
callback fails closed instead of publishing uncommitted in-memory identity.
Workspace owns the RPC route/intent and identity service; this branch owns the
WorkspaceHost wrapper and schema 15. Initial local-only repair is restricted to
verified tasks with neither current nor historical provider bindings.

The failure-first artifact will use actual engine/RPC/SQLite/CRDT boundaries and
synthetic provider homes only. Proposed-interface compile failures must be kept
separate from runtime assertion evidence. No result will be reported as a
baseline defect until execution reaches the intended behavioral assertion.

`crates/engine/tests/workspace_repair_authority.rs` contains nine prewritten
integration cases using the agreed new interfaces and synthetic provider homes.
It records JSON via `LUNA_WORKSPACE_AUTHORITY_REPORT_DIR` when later run. The
isolated transaction cases use an opaque proof snapshot; actual v2 identity
evidence and API semantics remain in the Workspace owner's suite. These sources
have not been compiled or run. New interface/schema absence is not a runtime
baseline defect. Subsequent implementation and focused verification must retain
separate logs for interface/fixture errors and reached behavioral assertions.
