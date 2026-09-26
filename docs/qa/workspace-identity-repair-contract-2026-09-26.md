# Workspace identity repair: API and authorization contract

Source-only extension to `ea229301`; this contract module is unlinked and has not
been compiled or run. A separate one-run Cargo grant reproduced only the original
three allocation cases at `050dd9d7`; see the parent design's baseline evidence.
No provider reads, runtime changes or native actions have run. The branch remains
based on committed main
`def1b8a8`; supported caller behavior was also inspected at current main
`570d951b72c950eceb9493e4a07fe7908039edbc`, without changing Sidebar-owned files.
The lifecycle audit owner then reviewed this contract read-only against main
`3a173e84a0d6f775499ef19e833e31709cf1ac74`. The refinements below incorporate that
review; they do not authorize or implement runtime changes.

## Required invariant and current implementation trace

Let `R` be a task's optimistic revision, `W` its last committed workspace-repair
authorization revision, and `G` the provider binding generation. A successful
association-only repair has one atomic outcome:

```text
(old mapping, R, W, binding ID, G)
    -> (verified v2 mapping, R + 1, R + 1, same binding ID, same G)
```

The new repair receipt records that transition. Original creation/provider/user
receipts and v1 event payloads remain unchanged. Replaying the same command ID and
payload returns its original terminal receipt with no second event or revision
increment. A conflicting payload under that ID remains a command-ID conflict.

Current source at `def1b8a8` establishes why a plain identity-log write is unsafe:

- `rpc.rs:2624` takes the task-command lane; `:2676` admits the command through
  `DocHost::begin_task_command`. `store.rs:2726` compares expectedTaskRevision.
  The safe-local-pending retry also checks the original receipt revision at
  `rpc.rs:2692`. Advancing only binding metadata would evade these comparisons.
- `task_identity.rs:2154` independently saves identity snapshots and does **not**
  advance task revision. Repair must not use that standalone commit path.
- The transactional path is `TaskIdentityService::commit_events_with_task_command`
  -> `WorkspaceHost::commit_task_command_projection_with_changes` (`:1032`)
  -> `DocsStore::complete_task_command_with_snapshot_changes` (`:2861`). It commits
  identity/registry snapshots and an Applied receipt with `applied=true` together;
  `commit_task_command_tx` (`store.rs:3878`) advances R by one, even when provider
  binding fields and registry display fields are unchanged.

**R alone does not fence already admitted executable rows.**
`LocalExecutableChange` (`store.rs:294`) carries row/hash authority, not a task or
workspace revision. `drain_commands` (`doc_host.rs:6300`) and `drain_queue` (`:5206`)
check that exact hash. Legacy queue delivery begins with no expected revision
(`:4781`). Therefore a mapping repair needs both the task revision and a durable
execution boundary; claiming the revision increment alone solves queued work
would be incorrect.

The lifecycle review confirms that durable W is necessary in addition to R for
non-destructive preservation of old history/grants. It must cover every executable
and recovery lineage, not just queued text or the repair endpoint.

1. Apply only while the target task is quiescent: no active operation, pending
   command/claim, queued row or nonterminal recovery. This includes Scheduled,
   Held, RecoveryNeeded, WaitingForSignIn and CheckingDelivery, not only Running.
   Command drains have their own executing set/processed ledger; an empty queue
   is insufficient. Pending work blocks **that task's repair** without silently
   cancelling, releasing, copying or re-authorizing it.
2. Persist W in the same SQLite transaction as R, mapping and receipt. CAS expected
   R/W/evidence; set W to the actual newly committed R. Capture original W with
   local grant insertion, edit, move, claim and unknown/delivery transitions in
   their existing hash-CAS transactions. Edits and claims preserve the captured W;
   they must not reseal an old matching-hash row with current W. Legacy missing
   stamps may mean zero only before the task's first repair; after repair they
   hold/refuse. Normal renames/queue edits do not advance W.
3. A request that passed an earlier check can wait behind repair. Compare its
   **original** captured W after the Sessions transition gate, before a warm
   mailbox send, fresh startup, or native steer. RecoveryRecord must persist that
   original W and check it both at claim and at post-gate dispatch; unchanged
   binding ID/G is not sufficient proof. Never substitute current W at restart.
4. Respect the existing lane -> drain -> transition direction, or use fail-fast
   quiescence if a drain is busy. Do not take transition then await drain_lock:
   a drainer can already hold drain_lock while awaiting transition, creating a
   cycle. Keep projection/SQLite locks synchronous and short. Admission, repair
   and the final dispatch decision must share this serialization boundary.
5. First repair version requires no retained idle provider runtime, even when
   active_turn_id is None. A warm handle's current key lacks workspace/W. Block
   with an actionable unload requirement unless an independently authorized
   lifecycle action has gracefully retired it and observed positive EOF. Do not
   force-kill, silently retire it, or reuse unstamped warm execution authority.

A genuinely new, reviewed explicit Send/Steer command may re-admit held content
with a fresh receipt/current W. Duplicate old receipts, edits, claims, import or
recovery must not exploit that explicit adoption path. Historical applied receipts
stay immutable and alone cannot create a fresh executable grant.

W fences old-row reappearance only while its high-water mark survives. A full
restore that rolls back W together with grants is **not** anti-replay protection.
Restore/rebuild contracts must preserve v2/W or leave restored legacy execution
unresolved until separately verified; no backup-internal patch is proposed here.

### Reviewed current-source anchors (`3a173e84`)

| Surface | Review evidence |
| --- | --- |
| Queue/admission/dispatch | `rpc.rs:2667` task lane; `doc_host.rs:5234/5252/5384` drain/hash/claim; `sessions.rs:2762-2764` transition waits and `:3029` warm send. |
| Grant renewal | `store.rs:312-317/1474-1535` hash-only authority; `doc_host.rs:3459-3486` edits/inserts; `:3820-3860/4992-5030` explicit adoption. |
| Recovery and idle runtime | `recovery.rs:22-36/346-350` unstamped record/CAS; `sessions.rs:4125-4190` recovery handling; `:1124-1149` idle live handles; `:540-545` binding key lacks workspace/W. |
| Atomic repair | `task_identity.rs:2176-2206`, `workspace_host.rs:1032-1096`, `store.rs:3116-3153/3925-3947`: identity append, snapshots, receipt and applied R advance; extend the same transaction with W/evidence CAS. |

## Proposed API

Implementation phase is now authorized on the clean composition with committed
main `3a173e84`; builds, native/provider activity remain gated. The agreed private
interfaces use `WorkspaceRepairFence { expected_task_revision,
expected_workspace_repair_revision }`, store `workspace_repair_revision(task_id)`,
`WorkspaceHost::commit_workspace_identity_repair(command_id, task_id, &fence,
binding_id, binding_generation, identity_snapshot)` and
`DocHost::prepare_workspace_identity_repair(task_id, repair_command_id)`. Preview
passes None for the latter; apply exempts only its exact admitted pending repair
command, verified against task and intent. The dedicated store commit requires
that pending receipt; terminal duplicates return through existing command admission
before any identity projection is prepared. The lifecycle owner owns these APIs
and the single schema-15 migration; Workspace owns identity/RPC repair glue.

`PreviewWorkspaceIdentityRepair { taskId, providerProof: "localOnly" }` is read-only
and returns one task's result:

```text
taskId, status: ready | blocked | unchanged, previewId?, evidenceDigest?,
expectedTaskRevision, expectedBindingGeneration?, targetWorkspace?, blockers[]
```

`localOnly` is the default and must never call a provider. Unbound tasks can be
ready if original registration, current chat, source scope/environment and root
identity agree. Bound tasks lacking fresh verified provenance are blocked. A later,
explicitly requested `refreshReadOnly` mode may inspect an eligible provider; it
must not create, resume, fork or prompt. No provider reads are authorized now.
Do not accept caller-supplied provider observations as repair proof.

Apply uses the existing durable task-command surface:

```text
ExecuteTaskCommand {
  commandId, taskId, expectedTaskRevision, expectedBindingGeneration?,
  intent: { kind: "repairWorkspaceIdentity", previewId, evidenceDigest }
}
```

The opaque, expiring preview binds task-specific historical events/registration,
current task revision/root/scope, exact binding provenance, normalized target and
the proof. Recheck all facts under the commit boundary. Rejected/missing evidence
produces a terminal refusal, no association event, no R/W change and no provider
activity. A consumed preview cannot apply a second repair under a new command ID.

Preview/apply are per task. An unresolved bound sibling is neither a veto on an
unbound task nor an excuse to rewrite the sibling. Unrelated log events may occur
between preview and apply; compare the relevant task's evidence digest, not a
global log-head token. The shared log transaction appends onto its latest state.
Previously unresolved legacy task/binding assignments retain explicit fences;
upgrading the envelope for a different task must not relabel them verified.

## Binding provenance and genuine changes

Proof must agree on the exact provider ID, provider instance, profile/home/store
identity and native task ID for the specific binding being repaired. Historical
BindingCreated/BindingMaterialized/BindingReplaced chains remain authoritative;
a recent observation for one binding is not evidence for every retired generation.
Changing the store/native ID or moving the genuine root is an explicit rebind or
relocation operation, not an association repair. Missing, contradictory, ephemeral,
foreign-active or unsupported provider metadata stays fenced. No startup auto-resume.

The version-2 envelope stays at the existing snapshot key, and old events remain
the unchanged prefix. A deterministic association event carries the evidence and
old/new mapping, R/W transition and repair command ID. Old code must refuse v2;
there is no writable stale v1 shadow. Crash/replay acceptance requires a fault
injection between transaction preparation/commit and projection publication, with
exactly one repair after reopen and no cross-task rollback.

## Supported relative-cwd caller audit

At main `570d951b`:

| Caller | Observed contract and compatibility consequence |
| --- | --- |
| New desktop projectless composer, `composer.rs:8797-8804`, `:8858-8884` | Sends deviceId without cwd; runs default `~`. Resolve it on the owning environment. No engine-process cwd is needed. |
| Desktop current/reused checkout, `composer.rs:8816-8846` | Uses space path or picker worktree path. Reused-worktree cwd is explicit. Keep source-space identity separate from checkout instance. |
| Cross-task Create, `rpc.rs:6885-6892` | Sends source project, no cwd; derive root from that project on its owning device. |
| Sidebar snapshot child, `rpc/side_chats.rs:123-127` | Copies parent cwd. Existing legacy relative/null parent data can reach creation; refuse with an absolute/owner-home instruction unless verified parent identity supplies an unambiguous root. Do not alter Sidebar code in this phase. |
| History fork/recovery UI, `shell.rs:5310`, `:5432`; engine `rpc.rs:3535`, `:4858`, `:5084` | Uses stored chat cwd with `.` fallback for absent legacy data. This is a real compatibility surface, not evidence that process cwd is the intended workspace. Require verified parent root or explicit folder selection before managed creation/rebind. |
| Native sidebar QA drivers, `qa-sidebar.py:175-178`, `:205-210`, `qa-sidebar-agents.py` | Current main resolves the synthetic provider workspace to an absolute path. No relative-path dependency. |
| Repository registration, `repos.rs:2222-2228` | AddRepo explicitly absolutizes relative input before returning a repo path. Its established separate contract should not be changed by this repair. |
| Direct CreateChat/SetCwd RPC clients and provider import metadata | String inputs are currently permissive. Tightening new managed registration is observable API behavior; return an actionable error for empty/relative cwd rather than silently using process cwd. Preserve existing legacy records for proof-based repair. |

No shipping CLI task-creation caller using relative cwd was found in the inspected
`apps/luna`/`apps/luna-headless` sources. This is a bounded source search, not a claim
about external clients or unmerged feature work. No breaking change is implemented.
The proposed safe default requires an absolute directory, owner-local `~`, or a
verified source project/parent root; no data is discarded. No new user decision is
needed for this proposal, but its compatibility behavior still needs code review.

## Additional failure-first contracts

The allocation suite remains three concrete current-RPC regressions. Proposed API
contract cases are prepared beneath `tests/support/workspace_identity_repair_contract.rs`
but are deliberately **not linked into the baseline Cargo target**:

- unbound repair advances R once, returns the same receipt on duplicate, survives
  restart, preserves v1 event prefix/creation receipts, and rejects an old revision;
- a stale preview after an actual cwd edit cannot alter the mapping;
- blocked bound metadata in B leaves a verified unbound A independently repairable;
- foreign store/native provenance and genuinely moved roots remain blocked;
- a pending queued row blocks that task's correction without deleting or sending it.

They seed a **disposable offline v1 fixture**, use EngineCore + memory RPC and
disabled native metadata, and emit JSON before contract assertions. No credentials
or provider calls are needed. The RPC strings intentionally describe a proposed
interface; they do not add a runtime endpoint. If wired against the current
baseline, they would fail at the absent API, which must be classified separately
from the three original aliasing failures. They have not been compiled or run.
The original three-case target's authentic baseline remains separate and contains
no proposed migration API tests.

After interface review, these contracts are now wired only into the separate
`workspace_identity_repair` target. The original three-case target remains frozen.
This new target also contains failure-first canonical-root/source-scope, relative
refusal, owner-home and UUID first-worktree materialization cases; an additional
SQLite-trigger case checks failure after revision preparation cannot publish an
identity snapshot. Test source was written before runtime edits and is not yet
compiled/run. Child tests use disposable HOME/worktree roots and an in-process
Mock harness only; they never discover an installed provider or credentials.

After API review, explicitly wire the contract module in a separate source change
before any runtime implementation. Its prospective filtered command is
`CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test
workspace_identity_roots repair_contract -- --nocapture`; this does not currently
select runnable tests. Set
`LUNA_WORKSPACE_IDENTITY_REPORT_DIR` to a retained unique target/qa directory and
capture stdout/stderr plus the original exit code, as in the parent design.

Required lifecycle-boundary fixtures before runtime implementation:

- Pause queue/Run/native-steer and scheduled recovery after their early checks but
  before the transition gate; race repair, then release dispatch and require the
  original W to hold/refuse with zero provider starts/sends. Include an occupied
  drain-lock case to prove the lock order does not deadlock.
- Reimport an old locally matching-hash row after repair/restart; edit/move/claim
  must retain old W and cannot execute it. A new reviewed Send/Steer can explicitly
  re-admit; duplicate old receipt and implicit recovery cannot.
- Exercise every nonterminal recovery state and an idle warm handle. Refusal must
  preserve queued/recovery data and history; no forced termination or auto-start.
- Inject failure before commit and after atomic R/W/snapshot commit but before
  projection publication; reopen and prove one complete repair, immutable earlier
  receipts, consistent execution authority, and independent task append.
- Retain proven-bound ID/G with injected read-only metadata; prove no default
  provider reads, old-reader refusal, and restoration of v2/W (or explicit restored
  legacy fencing). Never claim a full rollback of both stamp and grants is fenced.

These require reviewed fault-injection seams; no new runtime seam is introduced
by this source-only preparation. The five unlinked API contracts are not claimed
to cover these lifecycle races.

## Proposed ownership for implementation review

Workspace owns root/environment/source-scope allocation, versioned association
events/proof, per-task preview/apply and identity fixtures. The original lifecycle
owner should own DocHost admission/drains, Sessions post-gate/steer/idle guards and
RecoveryRecord stamping. One coordinated sync/store migration and atomic-CAS patch
must serve both, with only the necessary RPC/WorkspaceHost plumbing. This is a
proposed split for the coordinator, not a parallel implementation grant.

Do not alter `project_command_*` execution policy, Sidebar behavior, provider
adapters or backup internals. Their owners review explicit compatibility/restore
contracts separately. Foreign CRDT display integrity is a different concern.
