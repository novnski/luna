# Workspace identity boundary: source and evidence checkpoint

Source: `a54a865116b5c41e2edb4883b2e7e2f7a0370dd6`. This is a read-only
investigation of runtime and native evidence. No runtime/schema edit, Cargo run,
provider read, or QA app build was performed for this checkpoint.

## Historical bound tasks

The retained native readback at
`/Users/iulian/workspace/code/personal/luna/target/qa/parallel-audit-2026-09-26/retired-sidebar/qa/sidebar/native/origin-handoff/workspace-fence-readback.json`
contains **two** bound projectless tasks sharing the v1
`projectless:84c4c6b0-dd22-477e-95a1-0a7744b9d615` workspace row:

The preserved readback and failed receipt are byte-identical to the earlier
`894c` files (SHA-256 `d62ba2d5989215f305a5fd5fa4fdca191ac9396245480ce7a264368db0e53ea8`
and `4d59d92dea39674337888abf5e54f40540eea955003e229b1661a9dc099362a1`).

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
2. **Same-chat project grouping.** Keep the positive MoveProject control within
   current containment rules: a nested source project moved to its containing
   project while cwd stays fixed. Assert chat `spaceId` changes, original
   `source_space_id`, workspace ID, binding ID/generation and W stay fixed, and
   a valid continuation uses the same root. This path is already exercised at
   `workspace_identity_repair.rs:452-570`. With materialized A in an **external**
   linked worktree, create a project for that worktree and attempt MoveProject.
   Require a terminal refusal and unchanged chat, identity, binding and earlier
   receipts. This is a negative X03 control: current validation requires cwd
   containment in the source project (`crates/engine/src/rpc.rs:3484-3503`)
   and compatible checkout identities (`rpc.rs:3514-3571`).
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

The coordinator confirmed the external linked-worktree MoveProject refusal as
the current X03 boundary and granted one serialized build slot for the focused
offline E2E after failure-first source is ready. Bound `refreshReadOnly`
remains a later design decision. No runtime/schema change is requested.

## Focused E2E baseline

At test-source commit `119c35530b9a500295f88a417ac273058b8409cc` (SHA-256
`470aeeb8a083bcf811f0fd258721ad7c6b2be5597705eda1b418971cd7c46950`),
the exact focused command above exited 0 in 108 seconds: **17 passed, 0 failed**.
The retained artifact is
`/Users/iulian/.codex/worktrees/00a8/luna/target/qa/workspace-identity-boundary-20260927.T0hJqH/`.
Its 72-file SHA-256 manifest includes the later format and Clippy checks and
verified without mismatches (manifest SHA-256
`daa8ed43d2ab5409ce4b1bb5834875cc0d11b333b7e235070647235872d2a9a8`).
`summary.json`, `test.log` and per-case JSON record the following readback:

- One actual linked Git worktree was created. A's chat cwd, identity root and
  first binding agree; its sibling remains at the repository root. Restart and
  duplicate command replay preserved the original receipt and worktree inventory,
  with one Mock start total.
- External linked-worktree MoveProject returned terminal `rejected` /
  `invalidDestination` under the current X03 boundary. Chat, identity and the
  original receipt remained unchanged. The containing-project positive control
  also passed.
- SetCwd on the bound chat **applied** a different cwd while retaining the
  original execution identity. Explicit resume and the next Run were then
  rejected, with no second Mock start. Command-time refusal or proof-based
  relocation is a separate design decision; this baseline authorizes neither.

The configured format check exited 0 in 5 seconds. Focused `luna-engine`
Clippy for this test exited 0 in 51 seconds; it reported existing library
warnings and no warning in the changed test. Both exact commands, results and
logs are retained beside the E2E artifacts.

The suite used disposable Git paths, EngineCore/memory RPC and an in-process
Mock harness with native metadata disabled. It made no real provider call or
native app build. Aggregate build accounting moved from 123.56 to 127.92 GiB,
then 128.56 GiB after Clippy, below the 170 GiB operating ceiling.
