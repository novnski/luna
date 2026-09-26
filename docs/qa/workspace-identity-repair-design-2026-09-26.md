# Workspace identity collision: failure-first repair design

Status: the original three allocation regressions reproduced the defect against
unchanged runtime source. Reviewed runtime source now implements v2 allocation,
local-only unbound repair and atomic first-run materialization; this composed
runtime and the separate 15-case repair target are **not compiled or executed**.
The final per-task registry witness requires Lifecycle's assigned WorkspaceHost
wrapper follow-up before any atomicity claim or source checkpoint. Builds,
native actions and provider reads remain gated.
Original baseline: `def1b8a8021f3373919d0e6580677ee9f7e686eb`. Current source composition
includes committed main `3a173e84` and lifecycle source `7c783f89`.
Branch: `codex/workspace-identity-repair`. The earlier audit branch remains at
`04074e6b1ce93989b5c3ade3588e0883cf75b606`.

## Failure cases recorded before implementation

| Case | Required behavior / failure to prevent |
| --- | --- |
| Two local projectless UUID tasks with roots A and B | Registering B must not replace A's resolved workspace path or identity. |
| Restart after both registrations | Persisted replay must resolve A and B independently; creation receipts and task IDs stay identical. |
| One project with two existing Git worktrees | Both tasks retain their own checkout root and the same source-space association. Neither rewrites the other's workspace metadata. |
| First managed Run materializes a new worktree | The UUID task must acquire that root-instance before/with first binding, preserving a sibling task at the project root. This is authorized first-run initialization, not permission to auto-repair legacy/bound mismatches. |
| Same root, aliases and home shorthand | Canonical aliases and owner-local `~` resolve consistently; identity never depends on the RPC client's working directory. |
| MoveProject changes UI grouping but not cwd | Preserve origin source-space, root instance, binding ID/generation and W. A supported move to a containing local project is not a relocation; legacy proof must preserve the current grouping. |
| Historical import preview has a missing cwd | Return readable EnvironmentMissing/non-adoptable metadata with a display-only unresolved reference, never a canonical v2 claim or persisted identity. A valid-root control and existing stored history remain unchanged; permission, file and dangling-alias failures stay distinct. |
| Genuine root/provider-store/native-ID change | Existing resume/fork fences reject mismatches before provider execution. |
| Polluted v1 workspace row | Repair must use task-specific historical evidence, never the last shared row as proof for every task. |
| Missing/conflicting evidence, missing root, foreign active provider | Keep an explicit unresolved/fenced state; do not relabel it `current` or start a provider. |
| Migration interrupted or replayed | Publish the repair atomically and once; no partial task/binding remap, duplicated receipt, lost event or invisible generation change. |
| Optimistic authorization survives a mapping correction | Repair must advance taskRevision exactly once and invalidate pre-repair execution authority, even with unchanged provider binding ID/generation. |
| Unresolved sibling blocks independent work | A blocked bound task must not prevent an independently verified unbound task from being repaired. |
| Tombstones, imports, forks and binding history | Preserve IDs, origin/provenance, lineage, ownership, receipts and retired generations. Imports stay read-only unless explicitly adopted. |
| Old reader / rollback / restored backup | Older code must refuse an upgraded identity log instead of silently reopening v1 as writable state. Restore/rebuild must preserve v2 repairs. |

The supplied native failure remains terminal:
`side-chat-create:b1587031-42b7-4254-b577-cf72de6153a2`, status `failed`, reason
`task workspace changed; explicit resume refused`. Its receipt/history live under
`/Users/iulian/.codex/worktrees/894c/luna/target/qa/sidebar/native/origin-handoff/`.
No replay, deletion, or mutation of that receipt is part of this proposal.

## Evidence and current source contract

At sidebar `c34cc0f5`, `task_identity.rs:2254` allocates
`projectless:{owner}` while accepting `cwd` into `current_path`. Registration
emits `WorkspaceObserved` then `TaskRegistered`; projection lines 120-122 replace
the workspace by ID, and lines 328-330 resolve all referring tasks through that
single row. Main has the same allocator at lines 2245/2249. Sequential creation
alone explains the native mismatch; concurrency is unnecessary.

The CreateChat RPC accepts explicit cwd with or without a space. Ordinary desktop
projectless creation defaults to `~`; it does not expose an arbitrary-directory
picker. The supported reused-worktree UI sends a project's spaceId with an
existing worktree's cwd. That allocator likewise uses only `space.id`, despite
different `current_path`/`worktree_id` values. The new tests cover both contracts.

The existing `tests/projectless.rs` uses the human ID `chat-projectless`.
Managed registration intentionally bypasses non-UUID IDs, so those tests cannot
establish isolation of the durable identity registry.

## Concrete allocation design

1. Resolve the root on the owning environment before registration: explicit cwd,
   otherwise source-space path, otherwise owner-local home. Expand `~` on that
   environment; canonicalize an existing directory. Preserve the requested path
   separately if needed for display, but never key identity by raw spelling.
2. Allocate `workspace:v2:<digest>` using a domain-separated encoding of
   `(environment_id, source_scope, canonical_root)`. `source_scope` is either
   `projectless` or the source-space ID: the current DTO has a single source-space
   association, which must not be overwritten by another project at the same root.
   A project's repository and worktree metadata remain separate fields. Distinct
   roots never reuse the same mutable workspace record; identical roots in the
   same scope may share one instance. Git branches are not part of the key.
3. Registration must reject an attempted observation that changes an existing v2
   key's defining root/environment/scope. Metadata updates may refresh verified
   non-defining observations. Explicit relocation is a separate reviewed action,
   never an implicit rewrite performed by resume/fork.
4. Keep `validate_explicit_resume`, provider-store/native-ID checks and reconciliation
   root comparison. Route their lookups through the repaired workspace projection;
   do not set a shared path from the caller merely to satisfy a fence.

## Versioned legacy repair

Use a version-2 envelope at the **existing identity snapshot key**, retaining the
original event prefix unchanged and appending an explicit identity-association
repair event. A separate v2 key alone is unsafe: old code could reopen its familiar
v1 key and resume writing stale state. Current old readers reject a different
schema or unknown event; verify that refusal with a fixture before release.

Migration first produces a read-only plan, with per-task/per-binding evidence and
ambiguities. For a repairable binding, all of the following must agree:

- the workspace observation effective at that task's original registration or
  explicitly approved later materialization/transition, reconstructed by ordered
  replay (not the final overwritten workspace map);
- the current authoritative chat root, local environment and source-space scope;
- a matching provider observation for that exact provider/native task and bound
  store/profile, whose root agrees after normalization. Applying a bound repair
  requires a fresh read-only observation when that provider supports it. Cached
  observations support the proposal only; unsupported or unavailable refresh keeps
  the binding fenced. A provider read is not authorized in this source-only phase.

For an unbound task, registration plus current chat agreement can repair only its
workspace association: this must not create a binding or claim provider agreement.
Missing roots, mixed-device facts, real relocations, conflicting evidence or
foreign active sessions stay unresolved with an actionable reason. Do not infer
agreement from a valid path supplied to resume. A legacy alias/moved-path case
whose original root identity cannot be established also stays unresolved, even
if its spellings normalize to the same directory today.

The proposed new event records a deterministic migration ID, evidence digest,
expected original assignments and their replacements. It explicitly corrects
task/binding **workspace associations** in the v2 projection while preserving all
original event payloads, task IDs, provider/native IDs, binding IDs/generations,
provenance, lineage, origin and receipts. No provider state changed, so this is not
an invisible new execution generation. Historical bindings are assessed separately;
never point every retired generation at the current task root.

Apply the approved plan through the existing task-command transaction under the
task lane, device write lease and identity lock. Recheck task-specific history and
evidence, atomically advance taskRevision, persist the execution-authorization
boundary and corrected identity snapshot, then publish the projection. Do not
invalidate an independent task's plan merely because the global log gained an
unrelated event. Idempotent replay recognizes the migration ID. Old receipt
payloads/snapshots are historical facts and are not rewritten. The failed sidebar
request remains failed; a later user action uses a new request ID after resolution.

The coordinator accepted explicit, proof-based association-only correction with
unchanged provider binding ID/generation, provided taskRevision and execution
authorization advance. The concrete API, queued-work invariant, caller audit and
additional failure-first contracts are in
[the migration contract](workspace-identity-repair-contract-2026-09-26.md).
Relative-path refusal remains a proposed compatibility change, not a runtime patch.

## Failure-first regression source and focused command

`crates/engine/tests/workspace_identity_roots.rs` uses actual EngineCore, local
DocsStore, memory RPC, UUID command/task IDs and disposable roots. Native Codex
metadata is disabled, the registry is empty, and all provider homes are synthetic
with Claude keychain fallback disabled. It queues no run and reads no credentials.
The reused-worktree case prepares actual local Git worktrees when execution is
authorized. The separately granted baseline executed only these three cases.

Three allocation tests record RPC identities, expected paths, chat rows and
creation receipts to JSON before their expected failing assertions. The restart
case shuts down/reopens the same profile and verifies saved receipts as well.
Additional migration contract tests are source-only and use proposed RPC strings;
they are distinguished from the allocation repros in the migration contract.

After a separate build grant, run from this checkout with a unique report directory:

```sh
report="$PWD/target/qa/workspace-identity-failure-first-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$report"
git rev-parse HEAD > "$report/revision.txt"
code=0
LUNA_WORKSPACE_IDENTITY_REPORT_DIR="$report" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test workspace_identity_roots \
  -- --nocapture > "$report/test.log" 2>&1 || code=$?
printf 'exit=%s\n' "$code" > "$report/result.txt"
exit "$code"
```

## Authentic baseline result

At clean checkpoint `050dd9d770cc42087866f72c28065e8eafd16667`, the original three-test
source was byte-identical to `ea229301` (blob
`b45d29b0c46006f76dbebd934b0e903064016c95`), runtime directories were unchanged, and
the proposed migration module was not linked into the target. The exact focused
command above ran once with `CARGO_INCREMENTAL=0` and the default 25 GiB growth
reservation. Compilation succeeded; **0 passed / 3 expected assertion failures**,
53 seconds including compilation and 0.26 seconds of test runtime. No retries,
guard rejection, compiler error or fixture failure occurred.

Each case failed at `workspace_identity_roots.rs:189`: A's observed root was B's
directory. Individual evidence confirms the shared workspace ID in registration,
restart replay, and the supported reused-worktree case. Restart receipt/task checks
and worktree source-space checks completed before the expected root assertion.

Retained artifact directory:
`target/qa/workspace-identity-baseline.HSp2GK/`. It contains `test.log`, `result.txt`,
`command.txt`, `revision.txt`, the exact test source, `cache.json`, budget admission
evidence and `summary.json`. The three `project*.json` files were emitted before
assertions. Their recorded fixture roots were confirmed removed afterward.

Cargo was explicitly released immediately after this run. No native app, provider
or credential calls were made. No runtime code was changed. Exit 75 on any future
grant still requires coordination, never a guard bypass.

Before implementation, extend failure-first coverage to poisoned-v1 proof/refusal,
repair replay/crash atomicity, downgrade refusal, same-root/home normalization and
the fake-provider resume/fork negative controls. Scope is the identity service,
versioned identity contract and its E2E fixtures; shared RPC integration only if
required. Sidebar, Commands, Defaults and PR modules remain with their owners.
