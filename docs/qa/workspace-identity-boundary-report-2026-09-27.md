# Workspace identity boundary: source and evidence checkpoint

Source: `a54a865116b5c41e2edb4883b2e7e2f7a0370dd6`. This is a read-only
investigation of runtime and native evidence. No runtime/schema edit, Cargo run,
provider read, or QA app build was performed for this checkpoint.

## Historical bound tasks

The retained native readback at
`/Users/iulian/.codex/worktrees/894c/luna/target/qa/sidebar/native/origin-handoff/workspace-fence-readback.json`
contains **two** bound projectless tasks sharing the v1
`projectless:84c4c6b0-dd22-477e-95a1-0a7744b9d615` workspace row:

| Task | Native readback | Supported conclusion |
| --- | --- | --- |
| `30eba898-e6d4-42e3-ad8a-3374134c99d7` | Chat and provider cwd are `…/side-fork`; projected workspace cwd is `…/agents`; reconciliation is `externalChange`. | This polluted task is fenced. The native `side-chat-create:b1587031-42b7-4254-b577-cf72de6153a2` receipt is terminal `failed`, with `task workspace changed; explicit resume refused` (`failed-native-anchor-receipt.json`). Its receipt must stay failed. |
| `e12ac2f7-2a79-46fe-bba9-6b5f1e4fd807` | Chat, provider, and projected cwd are `…/agents`; reconciliation was `current` in this old readback. | It is **bound and ineligible for local-only repair**, but this readback does not establish an execution fence or fresh provider ownership today. Do not infer that it is polluted merely because it shares the v1 row. |

The v1 collision is mechanically possible because `WorkspaceObserved` replaces
the projection row by workspace ID, while registration captures a task-specific
observation (`crates/engine/src/task_identity.rs:135-174`). The current v2
allocator includes owner, source space and canonical root
(`crates/engine/src/task_identity/workspace_repair.rs:201-215,631-685`), but it
does not retroactively repair either historical binding. The local-only proof
explicitly blocks a current **or historical** binding and legacy provider-session
metadata (`workspace_repair.rs:776-825`); projection apply and commit likewise
require an unbound task (`workspace_repair.rs:312-331,1051-1064`). An independent
verified unbound sibling may still be repaired (`workspace_repair.rs:756-889`).

Current execution compares the requested root, projected root, chat cwd and
owning device, and refuses a mismatch (`workspace_repair.rs:712-753`). Explicit
resume also checks provider profile/home and native task ID
(`task_identity.rs:1004-1057`). Native reconciliation can mark provider cwd,
profile/home or foreign activity as external/stale/foreign
(`task_identity.rs:1218-1348`). These are fences, not proof that an old binding
is safe to rewrite. The native artifact establishes the observed failure above;
it is historical evidence, not a fresh provider read for this checkpoint.

## Proof required before a bound repair can be proposed

For **each binding generation**, reconstruct the effective root at registration
or an explicitly approved materialization from ordered identity events, then
compare the current authoritative chat/device/source scope and the binding's
provider ID, instance, profile/home/store and native task ID. The final shared
v1 workspace row or a caller supplied cwd is not task-specific proof. Require a
fresh, read-only observation from the exact bound provider store/native task:
its canonical cwd must agree, its status must be quiescent and non-ephemeral,
and it must not be foreign active. Cached reconciliation can support a preview,
not an apply. Missing, contradictory or unsupported native metadata remains
unresolved. Retired bindings need their own evidence; no current observation
may relabel all generations. See `docs/qa/workspace-identity-repair-design-2026-09-26.md:94-121`
and `docs/qa/workspace-identity-repair-contract-2026-09-26.md:170-187`.

No `refreshReadOnly` repair mode exists in this source: the RPC preview takes
`providerProof: "localOnly"` and does not call a provider
(`crates/engine/src/rpc.rs:8230-8255`; contract `:121-149`). A bound repair needs
a separately reviewed read-only provider adapter and per-generation proof
contract before runtime changes. A successful association-only correction must
atomically advance task revision **R** and execution watermark **W** while
retaining binding ID/generation and immutable earlier receipts. Quiescence must
include pending queue, recovery, executing drain and idle warm runtime; old
grants must remain stamped with their original W through dispatch. The exact
lock, replay and restoration boundaries are recorded in the contract at
`:15-95,249-265`. The current apply path passes no binding ID/generation
(`workspace_repair.rs:1087-1095`) and cannot be widened by merely removing its
bound-task check.

## Failure-first E2E plan for the next serialized build grant

Before changing runtime, write the expected outcomes and failure artifacts for
these EngineCore + memory-RPC scenarios. Use UUID task/command IDs, a disposable
local Git repository with a committed base, **actual** `git worktree`
materialization, and an in-process Mock harness that records starts/cwds. Keep
native metadata and credentials disabled. The existing first-Run scenario
already executes a real worktree directive and checks A's first binding and B's
unchanged root (`crates/engine/tests/workspace_identity_repair.rs:946-1028`);
extend it rather than replacing it with an isolated helper test.

1. **First Run and replay.** Capture A and sibling B's chat/identity/snapshot
   before Run. Send A's worktree Run; record the accepted/terminal receipts,
   actual `git worktree list --porcelain`, provider start cwd and both identities.
   Assert one registered checkout, A's chat/identity/binding all at that root,
   B still at the repo root, and unchanged source-space origin. Shutdown/reopen
   the same profile; assert the same identities and receipt, then replay the same
   command ID and verify no second worktree, binding or provider start. Inject a
   failure before atomic publication and require a recoverable or fenced state
   with no mixed chat/identity root. A failed gate must retain evidence and never
   dispatch to the repo root by fallback. Source path:
   `crates/engine/src/doc_host.rs:6955-7067`,
   `crates/engine/src/task_identity/workspace_repair.rs:451-629`.
2. **Same-chat project grouping.** With that materialized A, create a project
   for the exact worktree root and issue MoveProject. Expected identity contract:
   chat `spaceId` changes, cwd stays at the worktree, original
   `source_space_id`, workspace ID, binding ID/generation and W stay fixed; a
   fresh valid continuation uses that same root. A nested-project control is
   already exercised at `workspace_identity_repair.rs:452-570`. The external
   linked-worktree variant is **not established**: current MoveProject validation
   demands cwd containment in the source project (`crates/engine/src/rpc.rs:3484-3503`)
   and compares checkout identities (`rpc.rs:3514-3571`). Record its terminal
   receipt before deciding whether to extend this supported move contract.
3. **Same-chat cwd change and negative controls.** On a bound A, issue SetCwd
   to a distinct existing root; record its receipt and the chat/identity readback.
   Require subsequent resume/fork/Run to refuse before provider start while the
   roots disagree, with unchanged binding/old receipts. A stale first-Run
   materialization must fail if chat project or cwd changes before commit.
   Retargeting the selected project's path to an unrelated folder must never
   supply repair proof; a registered linked-worktree control with unchanged
   repository should remain eligible. SetCwd currently mutates chat metadata
   (`rpc.rs:5711-5767`), while execution's identity fence is separate
   (`workspace_repair.rs:712-753`); decide whether command-time refusal is part
   of this wave after the baseline readback.
4. **Historical bound proof gate.** Seed a disposable v1 collision and one
   independent unbound task. The polluted binding, a matching-looking second
   binding, a retired generation, missing/foreign/unsupported provider metadata,
   and nonquiescent states must all return blocked under `localOnly` with zero
   provider reads, starts, log mutation or W advance. The unbound task may
   repair independently. After a separately approved `refreshReadOnly` design,
   use an injected read-only observation to prove exact store/native/root matching
   and test R/W atomicity, crash/replay and old-grant refusal; no live provider
   activity is part of this first gate.

Retain a unique `target/qa/workspace-identity-boundary-<timestamp>/` directory
containing `revision.txt`, exact command and exit code, `test.log`, per-case JSON
of before/after chat, identity, task snapshot, receipts, worktree inventory and
Mock start count/cwd, plus a `manifest.json` with SHA-256 of each artifact.
The test writes JSON **before** its assertions so a failure is inspectable.
Run only after the coordinator grants the sole build slot, using
`CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test
workspace_identity_repair -- --nocapture` with the report-dir environment
variable. Stop and report exit 75. No QA app is needed for this offline E2E;
any later native pass needs its own four-part QA app identity and separate grant.

## Review requested

Confirm the next scope: (a) the first-Run/restart/same-chat E2E contract and
external linked-worktree MoveProject expectation, and (b) whether a bound
`refreshReadOnly` design should follow only after those results. No build or
runtime/schema change is requested yet.
