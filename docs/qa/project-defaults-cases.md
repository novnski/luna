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

## Retained page across engine replacement (failure-first, 26 September)

These cases were specified before adding any production epoch fence. Use the
actual ProjectsPage save/model methods, retained GPUI entities, the in-memory
RPC transport and a held engine-A reply. Keep project ID identical throughout;
exercise both a new generation on the same device and a different device.

For each identity variant, verify four delayed completions independently:

1. Engine A acknowledges a project-default save after B is attached. The
   engine-B project row, selected B draft and current page state must remain
   unchanged. A's acknowledgement must not mark B's settings as saved.
2. Engine A fails that save after B is attached. Its error must not appear as
   an error for engine B or change B's draft/project row.
3. Engine A returns its model list after B is attached with the same project
   and provider. That list must not populate B's picker or replace B's current
   catalog state; current project/model/permission choices must remain intact.
4. Engine A fails model discovery after B is attached. Its error must not
   replace B's current catalog state or alter the selected draft.

Retiring A must also release its stale busy/loading indicators so B remains
usable; cancellation is an acceptable settled outcome for the old request.

Hold only the intended request; the fixture must prove it reached engine A
before replacement. Observe B's state before releasing A's reply, then observe
completion through the actual RPC transport. Do not directly invoke a fabricated
callback or weaken assertions to match current behavior. The same-device case
must really increment AppState's engine generation; the cross-device case must
change the EngineHandle device identity as well as the owning project row.
The shared held-RPC helper is owned by Commands and must be agreed before use.

Preserve the immutable unfenced production baseline `3e62cf03`, existing ten
passing cases and the original E0433 log. First run the new reproduction without
production changes under the coordinator's explicit build grant. Only a genuine
failing baseline authorizes the scoped runtime fence in this workflow. Retain
baseline/fixed logs and exact source SHAs separately. No native app is involved.
