# Project execution defaults acceptance (written before implementation)

Scope: desktop new-task entry points that feed the shared Pickers/composer path.
Existing task configs and engine Run receipts remain authoritative. A provider
and optional model form one override: clearing it inherits the global provider
and its remembered model. A provider override with no model uses that provider's
catalog default, never a remembered model belonging to another provider.
Checkout supports Current checkout (including existing worktree reuse) and New
worktree using the existing checkout flow. Absent checkout inherits Current
checkout, the existing global behavior.

Failure cases, all required before handoff:

1. Old Space JSON and registry data omit executionDefaults: read as inherit,
   preserve git/name/path data, and create a new draft using ComposerDefaults.
2. Save A = Codex/model A, Full, New worktree; B = Claude/model B, Full, Local.
   Switch A -> B -> A, recreate picker/state from persisted data, and assert
   resolved model/provider/permission/checkout match the project each time.
3. Explicitly clear overrides to `{}`; restart; global fallback is restored.
   Project settings never rewrite composer-defaults.json.
4. Existing task config remains byte-for-byte equal after project edits and
   after restart; selecting that task shows its own config.
5. A project watch update must not reset deliberate selections in an existing
   new-task draft; an actual project/device transition must drop stale picks,
   including provider account, traits, permissions and branch.
6. Reject missing project, wrong owner, unknown fields/enums, blank/oversized
   model ids and invalid provider choices without changing any stored row.
7. An unavailable explicit project provider/model stays visible and blocks
   creation with actionable text. Do not silently select the first available
   model/provider. Selecting a supported explicit draft choice clears the block.
8. Unsupported permissions use the existing provider/machine guard, including
   Full confirmation; a worktree preference on a non-Git folder blocks until
   the user selects Current checkout. No implicit new worktree for old tasks.
9. Native settings: select project, change each control, show persisted readback,
   clear to inherit, restart the isolated QA app, inspect the new-task chips.
   Save screenshots and identity/signature/IPC evidence.

Repeatable runner: scripts/qa-project-defaults.sh. It requires the coordinator's
build slot and writes logs/artifacts under target/qa/project-defaults. Tests use
real registry persistence, RPC validation and the public GPUI picker path; they
never invoke paid providers. Native QA supplements the integration assertions.
