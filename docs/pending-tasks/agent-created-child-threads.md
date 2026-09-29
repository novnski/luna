# Add agent created child threads to LUNA

**Status:** Pending implementation. Research completed on 2026-09-30.

An agent should be able to create a fresh LUNA thread when the user asks it to delegate a task. The new thread should have its own conversation, execution identity, transcript, and normal chat controls, while appearing underneath the thread that created it. Results and requests for attention should return to the parent.

BB provides a useful reference: its child is an ordinary persistent thread with a parent relationship. Parenting, conversation inheritance, workspace selection, and lifecycle ownership are separate concepts. LUNA already has much of the required infrastructure. The recommended feature connects that infrastructure through one engine-owned creation operation, a verified agent tool, a sidebar hierarchy, and durable outcome delivery.

This document preserves the source investigation and proposed implementation for a later feature task. The recommendations below have not been implemented or tested.

## Evidence and scope

The investigation inspected these local source revisions:

| Repository | Inspected commit | Local checkout |
| --- | --- | --- |
| BB | `f1172452bb0ca60c042ce884b3f8b98112fe377d` | `/Users/iulian/workspace/code/personal/bb` |
| LUNA | `e71d90caad0798ca6dcec28da4b352d8f8c39c0c` | `/Users/iulian/workspace/code/personal/luna` |

BB source links below are pinned to the inspected commit. LUNA file links identify the implementation locations to revisit before starting work. Later changes may alter either implementation.

This was a read-only source review. Live BB creation and desktop behavior were not exercised because `bb` was unavailable in the research shell. UI behavior described as existing comes from code inspection, not a screenshot or a live acceptance run. Existing test cases were inspected as additional descriptions of intended behavior; they were not run during the investigation.

## How BB creates a child thread

The agent-visible path is:

```text
Parent agent runs bb thread spawn --parent-self with a task prompt
  -> CLI resolves the current BB thread ID
  -> SDK posts to the ordinary thread creation API
  -> Server validates and persists the new thread and parent relationship
  -> First prompt enters the normal dispatch pipeline
  -> Runtime starts a fresh provider conversation
  -> Sidebar renders the child beneath its parent
  -> Child outcomes are delivered into the parent conversation
```

### Runtime identity and agent access

BB supplies `BB_THREAD_ID`, `BB_PROJECT_ID`, and `BB_ENVIRONMENT_ID` in the provider runtime's shell environment. The CLI's `--parent-self` flag resolves the current thread ID from that environment. An explicit `--parent-thread` flag is also available. The CLI rejects combining both flags and rejects `--parent-self` when the current thread ID is unavailable.

Parenting is opt-in: ordinary spawn without a parent creates a root thread. The CLI forwards the task prompt, execution choices, environment selection, and parent ID to `sdk.threads.spawn`.

Sources: [runtime identity environment](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/packages/agent-runtime/src/thread-shell-environment.ts#L17), [CLI parent resolution](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/cli/src/commands/thread/spawn.ts#L101), [CLI spawn request](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/cli/src/commands/thread/spawn.ts#L579).

### Ordinary creation API and durable startup

The SDK posts to `/api/v1/threads`. The server delegates to `createThreadFromRequest`, the ordinary thread creation service. There is no separate disposable child-agent database.

Creation inserts a thread with status `pending`, records startup context, and attempts the first dispatch. Admission and environment provisioning happen through that dispatch pipeline. A queued first message leaves the pending thread and its startup information available for a later drain or server restart.

The response therefore represents a persistent thread, whose execution may still be pending. Creation should not be interpreted as proof that the provider has already accepted the first turn.

Sources: [SDK spawn API](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/packages/sdk/src/areas/threads.ts#L1325), [server creation route](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/routes/threads/base.ts#L348), [pending creation and first dispatch](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/thread-create.ts#L367).

### Parent relationship and fresh context

BB stores three separate relationships:

| Relationship | Meaning |
| --- | --- |
| `parentThreadId` | Hierarchy and parent reporting |
| `sourceThreadId` with `originKind` | Derivation from another conversation, such as a fork |
| `lifecycleOwnerThreadId` | Separate lifecycle dependency |

Ordinary child creation follows the provider `thread/start` path. Fork creation follows `thread/fork` and supplies the source provider thread ID. The creation service copies source history only for the fork case with a resolved history boundary.

A fresh child receives its explicit task prompt and normal runtime/project instructions. It does not automatically inherit the parent's conversation transcript. The parent can deliberately include a brief in the prompt, and a child can later read a referenced thread through available tools; those are separate context transfers.

Sources: [relationship storage](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/packages/db/src/schema.ts#L608), [parent versus source resolution](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/thread-create.ts#L549), [fresh start versus provider fork](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/packages/agent-runtime/src/runtime.ts#L1579).

### Workspace and execution policy

Conversation isolation and filesystem isolation are independent. For a same-project child using the project-default environment, BB selects a new Git worktree on the parent's machine. Explicit environment selection can instead reuse an existing environment. Personal-workspace defaults can reuse the personal parent's environment.

Provider and model resolution uses project execution defaults and explicit selections. The model is not automatically copied merely because a parent ID is present. Permission resolution considers the parent and its last execution settings, then applies provider and host limits.

Source: [environment selection](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/thread-default-policy.ts#L300), [execution policy](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/thread-execution-plan.ts#L268).

### Hierarchy and lifecycle policy

BB validates that a parent exists and is live. It rejects self-parenting, cycles, and excessive depth, including when reparenting a subtree. The inspected implementation permits four total hierarchy levels and permits a parent from another project.

Visibility is independent: a child inherits its parent's visibility unless overridden. Hidden children still report to their parents, but remain outside ordinary sidebar discovery.

Parent links are also distinct from destructive lifecycle behavior. BB's individual archive helper releases remaining unarchived children from that parent. Its explicit archive-all path archives descendants, including hidden children. Lifecycle-owner relationships have their own rules. These distinctions should be preserved when deciding LUNA's archive/delete behavior.

Sources: [parent validation](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/thread-parent.ts#L6), [visibility defaults](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/thread-create.ts#L484), [archive and release](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/thread-ownership.ts#L210), [archive-all behavior cases](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/test/public/public-thread-parenting.test.ts#L497).

## How BB presents child threads

The main sidebar implementation is in the `thread-list` plugin. It builds a tree from ordinary thread rows using a map of children grouped by `parentThreadId`. Each node carries its depth, descendants, and aggregate child activity. Missing parents become roots, and defensive visited/ancestor sets prevent malformed relationships from breaking traversal.

The row implementation provides:

- Indentation and a disclosure control for children.
- Aggregated descendant activity on collapsed parents, including running work, pending input, unread results, and errors.
- Persisted collapse state.
- Ancestor expansion when revealing a child.
- Parent navigation in the child view.
- Normal thread controls and an independent transcript for each child.

The activity rollup is essential. A collapsed parent should still make a blocked or failed child visible to the user.

BB's tree also participates in project, environment, section, and pinned organization. That is more involved than inserting child rows into a flat recent-chat list. Filtering, sorting, pagination, and keyboard navigation need to consume the same visible tree model.

Sources: [tree construction](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/plugins/thread-list/app/model/project-thread-groups.ts#L247), [collapsed-child indicators](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/plugins/thread-list/app/rows/ThreadRow.tsx#L390), [disclosure control](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/plugins/thread-list/app/rows/ThreadRow.tsx#L634), [saved preferences](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/plugins/thread-list/app/preferences/atoms.ts#L6), [parent navigation](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/app/src/views/thread-detail/ThreadDetailView.tsx#L1922).

## How BB reports results to the parent

BB sends generated messages for child completion, failure, interruption, and requests for attention. Nearby turn outcomes are batched with a two-second delay. Completion excerpts are bounded to 4,000 characters and link to the child for further inspection. The code also distinguishes a completed turn from a still-running background workflow.

Parent delivery checks that the parent still exists and is unarchived. It uses the normal delivery path where possible, and a durable queued message when delivery is blocked by an interaction, an unavailable host, or another relevant runtime condition.

There is an implementation risk to improve in LUNA: the pre-delivery batches use an in-memory map and best-effort delivery. A process restart before flushing could lose a pending notification. This is a static inference from the inspected code, not a reproduced BB incident.

Sources: [batch timing and excerpt bounds](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/child-thread-notifications.ts#L78), [batch construction](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/child-thread-notifications.ts#L431), [parent delivery and durable queue](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/apps/server/src/services/threads/parent-system-messages.ts#L425).

## What LUNA already has

| Foundation | Existing implementation | Missing connection |
| --- | --- | --- |
| Stable task identity | `LunaTaskIdentity`, provider execution bindings, and `TaskLineage` | Validated child creation and hierarchy policy |
| Separate lineage concepts | `parent_task_id`, `source_task_id`, `lifecycle_owner_task_id` | Projection into normal chat rows and UI |
| Durable chat creation | `TaskCommandIntent::CreateChat` and transactional identity/registry updates | Creation coupled to the initial prompt and startup state |
| Cross-task coordination | List, read, approved send, and wait handlers | Verified create adapter and attached tool |
| Runtime parent/source context | `RuntimeContext` populated from task lineage | Populate lineage for agent-created children |
| Live native sidebar | `WatchChats`, `AppState`, and GPUI row rendering | Shared hierarchical visible-row model |
| Project execution defaults | Desktop new-chat picker resolution | Reusable policy for engine-created threads |

Relevant source locations:

- [TaskLineage](../../crates/proto/src/task_identity.rs): the three lineage fields already exist.
- [Task identity service](../../crates/engine/src/task_identity.rs): the authoritative identity log is stored in SQLite and replayed on restart; `register_new_task_with_command` stages identity with the command transaction.
- [Engine RPC](../../crates/engine/src/rpc.rs): `CreateChat` persists an ordinary chat with no provider resume session; the lower-level cross-task create control currently uses default lineage.
- [Cross-task tools](../../crates/engine/src/cross_task_tools.rs): `create_thread` is explicitly omitted because no verified cross-task command adapter is attached. Existing control grants are engine-owned and are not model arguments.
- [Session manager](../../crates/engine/src/sessions.rs): runtime context already reads parent/source IDs from task identity; durable dispatch and completion observations can be reused.
- [Runtime contract](../../crates/engine/src/runtime_contract.rs): verified dynamic tools attach through Codex; Claude MCP transport is explicitly unverified in this adapter. Existing Codex sessions without registered dynamic tools require a new provider session to attach them.
- [Chat wire model](../../crates/proto/src/entities.rs) and [registry document](../../crates/doc/src/workspace.rs): ordinary `Chat` rows do not currently expose lineage.
- [UI state](../../crates/ui/src/state.rs): `WatchChats` subscribes to ordinary chat rows.
- [Sidebar rendering](../../crates/ui/src/shell/spaces.rs) and [shell rows](../../crates/ui/src/shell.rs): the current visible order is built from flat chat lists.
- [Composer](../../crates/ui/src/composer.rs) and [pickers](../../crates/ui/src/pickers.rs): manual creation resolves execution/checkout choices and submits creation and execution commands. The child operation should reuse the policy without copying this UI orchestration.

LUNA also has provider-owned subagent transcript documents such as `{chatId}--sub--{spawnCallId}`. Those views should remain distinct from ordinary app-owned child threads. A child thread needs a normal chat identity and independent controls, rather than only a nested transcript document. See [Codex subagent research](../research/codex-subagents.md).

## Recommended backend design

### One engine owned creation operation

Add a dedicated create-child-thread operation shared by the agent tool and a manual New child thread action. The request should contain a task prompt, optional title and execution overrides, checkout selection, and an idempotency key. These are proposed fields, not a current API.

For an agent call, derive the parent from the engine-resolved active run. Do not accept caller authority from model arguments, a transcript, or another task's content. Preserve the existing user-authorization and permission boundaries; do not attach the tool by setting an unconditional permission flag.

The operation should:

1. Validate the parent, project, host, permission ceiling, and hierarchy depth.
2. Allocate a stable LUNA child task ID.
3. Persist child identity, parent lineage, the ordinary chat row, and a durable first-turn request through the existing command/store transaction boundary.
4. Dispatch a fresh provider conversation through the normal execution pipeline.
5. Return the child ID, command receipt, and actual startup state.

Retries with the same request identity must resolve to the same child. Persist startup intent before contacting the provider, so restart recovery does not guess whether to create a second child. Distinguish durable acceptance from provider acceptance and preserve an unknown-delivery outcome when provider acknowledgment is lost.

A startup failure should leave a visible recoverable child with an accurate error. A UI-only create-then-send sequence is insufficient for agent creation because partial failure can otherwise leave an empty child or duplicate its initial turn.

### Fresh context and explicit workspace choice

Create a new provider binding without the parent's resume ID. Do not fork or copy parent history by default. Send only the explicit task prompt plus normal runtime/project instructions. Do not automatically generate a parent-history summary; an intentional brief supplied by the parent is enough.

Use the same project-default execution policy as manual new chats, with explicit overrides. Move or extract that resolution into reusable policy so backend-created threads have parity with desktop-created threads. Resolve provider/account/model settings and permission ceilings explicitly.

Keep checkout selection separate. A fresh conversation may use the parent checkout. Independent coding may select a new worktree. Show that choice in the child UI. A new worktree starts from its selected Git reference; it does not automatically contain the parent's uncommitted working-tree edits.

### Bounded first version

Recommended initial scope:

- Same project and execution host as the parent.
- Visible ordinary children, with fresh context.
- A bounded hierarchy; BB's four-level cap is a reasonable starting point.
- Explicit checkout choices using existing worktree materialization.
- Engine-validated creation authorization scoped to the active parent run.
- No automatic escalation of provider or host permissions.

Keep the engine and UI provider-neutral. Prove the Codex tool path first, then verify additional transports before advertising their creation capability. A Claude or other-provider adapter needs an actual verified delivery path; instructions alone do not make a tool callable.

Cross-project parenting, hidden workers, arbitrary reparenting, and broader automation can be later increments. The stored model should retain the separation of parent, source, and lifecycle owner from the start.

## Recommended frontend design

Project engine-owned lineage into the existing synced chat registry and `WatchChats` stream. The authoritative task identity and the chat read model should be updated consistently. Do not require a separate identity RPC for every sidebar row or create a second editable hierarchy store in the UI. Older chats should default to no parent without bulk rewriting history.

Build one shared tree/visible-row model for rendering, keyboard navigation, filtering, and pagination. Sort roots/families and siblings deliberately. Child activity should not move a child into an unrelated top-level position.

The initial native UI should include:

- Indented child rows with saved disclosure state.
- Aggregated working, unread, error, and attention indicators when a parent is collapsed.
- Ordinary transcript, composer, stop/retry, rename, and navigation behavior for each child.
- A parent breadcrumb in the child view.
- A linked creation entry and outcome entries in the parent transcript.
- Ancestor reveal when opening a child from search or a transcript link.
- Reachable rows when a parent is missing, archived, or outside the current filtered/page subset.

Example presentation:

```text
Project LUNA
  v Add child threads                 2 children working
      Trace the creation backend      Working
      Design the sidebar hierarchy    Needs input
```

Define pinning, search, settled groups, and archive behavior before modifying row rendering. A selected child must remain reachable, and a family must not disappear because flat-list pagination loaded its children without its parent.

Arbitrary reparenting or detach actions need an explicit identity event and consistent registry projection. Updating only the UI parent field would conflict with the identity service's existing lineage consistency checks.

## Durable outcome reporting and lifecycle behavior

Persist a child-outcome outbox before delivering results to the parent. Deduplicate by child task ID, turn ID, and outcome. Batching should affect presentation only, so a restart does not discard undelivered outcomes.

Use the normal parent queue and engine-owned notification provenance. Child output is untrusted content and must not confer authority to create or control more tasks. A child-completion notice should identify a completed turn, not claim that the task can never run again. Busy parents, blocked parents, failures, interruptions, and outstanding background work need accurate states.

Creation should record whether results are to be reported and the authorized continuation behavior. A result can be persisted and displayed even when the parent should not automatically start another model turn. Do not silently reopen an archived or deleted parent to deliver a result.

Recommended lifecycle defaults keep children independently usable. Archiving a parent should leave surviving children reachable and retain historical lineage. A separate archive-subtree or delete-subtree action should use an explicit reviewed scope. Parenting alone should not imply automatic destruction or worktree removal.

## Implementation stages

1. **Creation and durable startup:** define the request/receipt, hierarchy validation, identity projection, idempotency, execution defaults, and durable first-turn state.
2. **Agent tool:** attach `create_thread` through the verified runtime adapter, derive parent authority from the active run, and expose accurate capability diagnostics. Verify registration on a fresh provider session.
3. **Native hierarchy:** add the chat lineage projection, shared sidebar tree, disclosure/activity rollup, parent navigation, and linked transcript entries.
4. **Reporting and lifecycle:** add the durable outcome outbox, deduplicated parent delivery, restart recovery, and scoped lifecycle behavior.

Expected areas include `crates/proto`, `crates/doc`, engine task identity/RPC/cross-task tools/sessions, and UI state/sidebar/transcript code. Follow the repository's plan-before-broad-edits rule when implementation starts. This document is not authorization to implement the feature now.

## Failure cases and E2E acceptance

Record failure expectations before implementation. Prefer end-to-end coverage through the real engine and native UI. Use a bounded real-provider acceptance run to prove fresh session identity and callable agent creation; deterministic fixtures can cover restart and failure timing but cannot prove live provider transport.

| Scenario | Required observation |
| --- | --- |
| User asks the parent agent to create a child | One ordinary child is persisted, starts with the supplied prompt, and appears under the correct parent |
| Fresh context | A distinct provider session ID is created and the initial transcript contains no inherited parent history |
| Deliberately supplied brief | Explicit prompt context is retained without silently copying additional parent messages |
| Duplicate tool delivery or request retry | The same child ID and first-turn receipt are returned; no duplicate child or first turn |
| Restart after creation but before dispatch | Durable startup intent is recovered with the original child identity |
| Provider acknowledgment lost | The result is reported as unknown where appropriate; no blind duplicate provider submission |
| Provider/model unavailable or worktree setup fails | The child remains visible with an accurate recoverable error |
| Unauthorized or out-of-scope request | No child or external execution is created; authority cannot be widened through tool arguments |
| Invalid parent, cycle, or excessive depth | Creation/reparenting rejects the relationship before publishing it |
| Parent collapsed while child runs or blocks | The parent row exposes aggregate running/attention/error state |
| App restart and chat-watch reconnect | Parent placement, disclosure state, child transcript, and controls remain consistent |
| Child completes while parent is busy | One outcome is durably queued and delivered without losing parent work |
| Restart during outcome batching | The persisted result remains deliverable and does not duplicate |
| Interrupted or failed child | The outcome is distinguished from successful completion |
| Search, pinning, filtering, and pagination | The child is reachable and opening it reveals the appropriate hierarchy |
| Parent archive/delete | Surviving children remain reachable; explicit subtree operations affect only their reviewed scope |
| Existing provider session lacks tools | Capability diagnostics remain accurate; a fresh verified session gets the tool |
| Additional provider transport | A harmless real creation through that transport proves the capability before it is advertised |

Retain a repeatable evidence artifact containing:

- Source commit, exact commands, provider/runtime versions, and exit statuses.
- Stable LUNA parent/child IDs and the corresponding distinct provider session IDs.
- Creation and first-turn command receipts, startup readback, and persisted lineage.
- Screenshots showing nesting, collapsed activity, parent navigation, and a normal child composer.
- Outcome/outbox/queue records before and after restart, including deduplication readback.
- Fixture configuration and a rerun script that isolates its own data and artifacts.

Run the relevant compile, formatting, and E2E checks through the repository's configured tools. Follow [the QA infrastructure guide](../reference/qa-infrastructure.md), the one-LUNA-GUI rule, build-budget admission, and scoped cleanup rules in [AGENTS.md](../../AGENTS.md). Preserve and hash the QA evidence. After implementation checks pass, refresh the primary LUNA Dev app and verify its exact process, IPC listener, and strict signature before handoff.
