# T3 Code Orchestrator V2: source and behavior audit

Research date: 2026-10-04, Europe/Bucharest. Local source: /Users/iulian/workspace/code/personal/t3code, pulled with git pull --ff-only origin main to **4ee6bfd50ef4a089440d5c3662db2298da9cc50e**. Installed application exercised: **T3 Code Nightly 0.0.46-nightly.20261004.2648**. The installed bundle was not rebuilt from this source revision.

## What V2 actually changes

V2 is an application-owned execution coordinator across provider runtimes. It does not replace Codex's or Claude's internal agent loop. The app owns durable conversations, requested runs, attempts, provider bindings, queues, context transfers, requests for user input, delegated work, checkpoints, and completion delivery. Adapters translate each provider's native behavior into that model.

The main rewrite is commit **de343914273eceb852a1d1d739cd1d38df7796ee**, “introduce new orchestrator.” Its overall diff spans 1,907 files; that includes much more than the coordinator itself. The previous harness audit examined **8792f95762092ea33e74d41a07e8818befd84970**. There are **203 commits** between that baseline and this snapshot.

**Event sourcing is not new to T3.** V1 already had an orchestration engine, decider, projections, provider-command reactors, and checkpoints. What deserves attention is V2's more explicit execution identities, transactional side-effect outbox, adapter capabilities, context-transfer machinery, and server-owned controls. The old audit specifically found no reachable native Codex fork and a client-driven follow-up queue; both changed.

The same-day product/UI research used **a21702d0e58ad20263dc965d84472bb6d7eb38c8**, which already contained V2. There are **74 subsequent commits**. “New since the September harness audit” and “new since today's UI snapshot” are different claims.

## Execution model

~~~mermaid
flowchart LR
  U["Web / Desktop / Mobile / Agent MCP"] --> C["Typed command + command ID"]
  C --> O["Per-thread orchestration decision"]
  O --> T["SQLite transaction"]
  T --> E["Events + projections + receipt"]
  T --> X["Durable effect outbox"]
  X --> W["Effect worker"]
  W --> A["Provider adapter / checkpoint / filesystem"]
  A --> N["Normalized events"]
  N --> O
  E --> UI["Bounded client subscriptions"]
~~~

The key records are distinct:

| Record | Meaning | Why it matters |
| --- | --- | --- |
| AppThread | The conversation the user recognizes | Survives provider changes |
| Run | One requested unit of work, including queued or preparing work | Stable identity above native turns |
| RunAttempt | An initial execution, steering restart, retry, or provider recovery | A restarted provider turn need not become a new user request |
| ProviderSession | The live runtime connection/process | Can disappear without erasing the app thread |
| ProviderThread / ProviderTurn | Native conversation and turn identity | Supports resuming an old provider after a switch |
| ExecutionNode | Root turn, message, tool, approval, subagent, hook, etc. | Parent/root relationships and explicit lifecycle; countsForRun distinguishes contributing work |
| Subagent | Native or app-owned delegated work | Records backing child thread, origin, model, result and completion delivery |
| ContextTransfer / ContextHandoff | Intent and delivery of conversation context | Fork, provider switch and merge-back can be inspected and recovered |

Source: [contracts: capabilities and identities][contracts], [run and attempt records][runs], [provider thread records][provider-threads].

### Acceptance and effects

[EventSink.commitCommand][sink] reserves a command receipt, appends events, folds persisted projections, enqueues effects, and stores the resulting sequence within the transaction. Publication happens after commit. Repeating an already accepted command ID returns its previous result.

That receipt proves **accepted durable intent**, not that a provider accepted a prompt or that a filesystem operation finished. Provider execution cannot be made exactly-once merely by putting its intent in SQLite.

The [outbox][outbox] uses leases, attempt counts and pending/running states. Effects for one thread run in order; a failed rollback waiting to retry blocks a later provider start. Title generation uses a separate lane so slow metadata work does not block execution. The worker checks cancellation after claiming work. Recovery retires effects tied to a lost process instead of blindly replaying them.

V2 also guards late writes against run/attempt ownership. For example, EventSink's conditional provider-thread write checks the run, attempt and native run ordinal before accepting a late update. This is worth borrowing as an explicit invariant, while preserving Luna's existing unknown-delivery protection.

### Provider completion versus application completion

The provider may finish before checkpoint capture, diff refresh, title work or a background child finishes. [RunFinalizationService][finalization] is separate from provider turn handling. Duration should reflect actual provider work, and a failed checkpoint should not pretend the model is still generating.

This distinction also appears in task status: a completed child turn with live nested work is not yet a completed delegated task. T3 exposes working, waiting_for_children and result_available.

## Delegation: what happened in the supplied screenshot

The Claude parent queried the app's live provider/model catalog, then created an **app-owned Codex child**. This is separate from a provider's native subagent tool.

[delegate_task][mcp-tools] creates a child with only the supplied task prompt; it does not implicitly copy the parent's entire conversation. Target provider instance, model and model options are resolved from the same catalog as the composer. Model options inherit only when the relevant provider/model selection remains compatible. Runtime and interaction modes have explicit inheritance rules.

The [service][mcp-service] verifies that the invoking MCP provider session owns an active parent run. Stable client request IDs become scoped command IDs. This is the part that prevents a retry of the same delegated round from creating another child.

Two modes have different delivery semantics:

- **async:** returns the task identity and later wakes the parent through a persisted notification.
- **wait:** waits for a result within a timeout. Timeout does not cancel the child. After timing out, delivery is upgraded so the eventual result can still wake the parent.

Task result delivery has pending, claimed, acknowledged, delivered and disposed states, plus a cohort/generation mechanism. Reading a terminal result with task_status acknowledges automatic delivery. Consequently, task_status is explicitly marked as not read-only even though it looks like a status query. A thread-level wait only reports status and does not acknowledge the delegated result.

Completed task results stay tied to the original delegated run. Sending a later message to the backing child thread does not reopen that task. T3's tool guidance therefore requires a new delegate_task call and new request ID for a new review round. Cancelling an already-terminal task disposes delivery without stopping unrelated later turns in the backing child conversation.

**Observed:** I opened the existing “Random number from Sol” card. It led to a persistent child conversation with the original random-number prompt, result 73, GPT-6.1-Sol / Low, a parent link and Lineage. This run did not create another delegated child. The user's existing completed example, not a new delegation test, establishes this observation.

## The wider app-control tool surface

V2 exposes more than delegate_task. The source registers these tool families through shared services:

| Family | Representative tools | Purpose |
| --- | --- | --- |
| Discovery/delegation | orchestrator_capabilities, delegate_task, task_status, task_cancel | Discover real model options and control one app-owned delegated round |
| Conversations | create_threads, t3_thread_launch, t3_thread_list/read/send/wait/interrupt/update | Persistent top-level work and bounded cross-thread coordination |
| Configuration/history | t3_thread_configuration/configure/fork/merge_back/transfers/search | Inspect effective settings, continue history and trace context movement |
| Queue | t3_queue_list/read/edit/cancel/reorder/promote_to_steer | Agent and UI control the same server-owned queue |
| User requests | t3_pending_request_list/read/respond | Discover and resolve persisted approvals/questions through the proper capability |
| Scheduling | schedule_task, list_scheduled_tasks, update_scheduled_task, delete_scheduled_task, run_scheduled_task_now | Persistent recurring work with an inspectable owner and next run |
| PRs | link_pull_request, unlink_pull_request, list_thread_pull_requests, watch_pull_request, unwatch_pull_request | Track changes and wake only on meaningful PR events |
| Workspace/environment | t3_project_*, t3_worktree_*, t3_environment_* | Project launch, worktree handoff/status and execution-environment controls |

The actual registrations are in [orchestrator tools][mcp-tools] and the adjacent thread, project, worktree, environment and pullRequests toolkits. [Thread access policy](https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/mcp/threadAccess.ts#L21) derives authority from the bound credential, verifies live caller ownership for mutations and resolves project scope before accessing a caller-supplied thread ID. The tool catalog is not permission to act outside the user's request.

Luna should extend its existing engine-owned registry selectively: diagnostics, context transfers and queue inspection are better first additions than exposing every administrative action.

## Provider switching and portable context

The [transition planner][transition] chooses among reuse, model switch within a session, restart-and-resume, fresh conversation with handoff, and rejection. Continuation compatibility includes provider driver and continuation/account identity. A missing or disabled target is rejected; an adapter must classify a model-option transition rather than letting the core guess.

A different provider receives a **budgeted selection of saved history**. Despite schema names such as full_thread_summary, this is not necessarily an LLM-written summary. The current implementation selects intact attributed messages and selected activity. It prioritizes the latest user request and assistant response, the original request, then other recent items.

[Budget policy][budget]:

- Default imported-history allowance: 16,000; configurable and clamped to 1,024–64,000.
- Separate 64,000-byte imported-history ceiling.
- Conservative accounting charges a UTF-8 byte as a token, including framing.
- Leaves at least 16,000 tokens or a quarter of the model window for other context/work.
- Uses known model capacity and provider usage; unknown capacity assumes 128,000.
- Reserves estimates of 8,192 tokens per current image and 4,096 per other attachment reference.
- Never truncates the current user request to make history fit.
- Omits oversized historical items whole and includes retrieval references and cursors.

It does not replay hidden reasoning, native tool-call state or old binary attachments. Historical text is labeled as historical context, not a new instruction. A lookup tool can retrieve omitted text. The [delivery layer][handoff-delivery] tracks target native identity, delivered item IDs and omitted coverage to avoid repeatedly injecting the same history.

**Verified live:** Codex → Claude preserved an earlier marker. Claude → Codex preserved a second marker introduced only during the Claude segment. Read-only database verification showed two provider bindings for one app thread. The return trip used delta_since_target_last_seen with injected delivery; the first switch used full_thread_summary with inline delivery. This supports actual continuity, not merely a changed model label.

## Fork, merge-back and rollback are different operations

A fork creates a persistent app thread immediately. It retains a source point, lineage and a context-transfer record. [Fork admission][fork] allows provider-finished completed, failed, interrupted and cancelled runs; it rejects in-progress and rolled-back runs. Providers that support native fork use it; portable history transfer is the fallback for other cases.

**Verified live:** the Codex fork had a distinct native thread ID and a forkedFrom link to the parent's provider thread/turn. It recalled both original markers. A third marker added only in the fork became available in the parent after **Merge back**.

**Merge back transfers conversation context. It is not a Git merge.** In this test, the parent's next response recalled the fork-only marker, and the persisted transfer was consumed with fork_delta_summary / injected delivery. No repository files or branches were changed by the probe.

Rollback is separate again. A filesystem checkpoint cannot restore another chat's concurrent changes safely. [CheckpointRestoreSafety][restore] checks canonical paths, other live/archived thread ownership, provider sessions and overlapping checkout roots. The guard runs at admission and again before rollback. A provider without conversation rollback support must not proceed with a combined rewind-and-files operation.

Luna already has native forks, rewind controls and isolated-worktree checkpoint protection. The particularly useful addition here is inspected, budgeted fork-context merge-back.

## Queue, steering and Stop

The queue now belongs to the server, not an open tab. Run records retain queue order and queueHeld state. UI and MCP operations can list, edit, reorder, remove and promote a queued message to steering. [QueuedRunsControl][queue-ui] exposes those operations.

Dispatch modes distinguish immediate start, queue after active work, native active steering, interrupt/restart steering and deferred workspace preparation. A provider's inability to steer natively does not justify falsely advertising native steering: the orchestration layer can deliberately use an interrupt/restart path instead.

**Verified live:** I started a harmless sleep command, queued a follow-up, edited its sentinel before dispatch, and observed WAIT_DONE followed by QUEUE_EDITED_OK. The original QUEUE_FIRST was not emitted. This verifies the live edit-and-drain path; it does not prove restart recovery.

Stop's scope is broader than “interrupt the currently selected provider.” The current [interrupt planner][stop] visits older provider bindings with pending background work, including a dev server left behind before a switch. It also prevents pending provider-start effects from resurrecting a stopped run and can hold queued work. [BackgroundWorkStop.integration.test][stop-test] covers the earlier-provider case.

This area has recent follow-up fixes: **d65cd33a03** addresses background work after provider switching; **5108c978b1** repairs delegated delivery, held queues and stops across restarts. These are useful regression scenarios, not evidence that every V2 edge case is already mature. Those destructive/restart scenarios were source-inspected, not exercised against the user's live app.

## Durable questions, capabilities and provider differences

The capability contract covers session reuse, thread fork/history/rollback, turn steering, streaming, tools, approvals, planning, native subagents, context transfer, checkpoints and identity strength. It explicitly distinguishes native enforcement from enforcement only at the client boundary. Old persisted capability records decode conservatively.

Codex supports app-server capabilities; Claude uses its native SDK/runtime; OpenCode 1 and 2 have distinct adapters; Pi and ACP registry integrations broaden provider choice. Cursor V2 uses the official SDK. Capabilities can be negotiated with the running session, so a static declaration is not proof that every installed version supports a feature. For example, Cursor's adapter advertises no native active steering or conversation rollback; Pi/ACP declare the weaker client-boundary enforcement.

Codex asynchronous questions are notifications answered through a new message, not pending RPC calls. V2 stores a runtime request with responseCapability.type=message. The response service validates and records the answer transactionally, then routes it through ordinary queue/steer/start behavior. Blocking approvals/questions retain their live callback semantics. See [provider constraints][provider-doc].

For Luna, this argues for one inspectable capability snapshot and transition decision, not hundreds of provider checks scattered through UI code.

## Scheduler, usage limits, attention and PR watches

The [scheduler][schedules] stores recurring tasks, can bind runs to the current thread or launch fresh top-level threads, and uses the same thread-launch/dispatch services as manual work. It rereads a task before execution so a pause, deletion or changed prompt cannot be ignored by a stale poll. Missed fixed-time slots are rescheduled rather than fired arbitrarily late. The server must be running; “no active chat turn” does not mean “the machine can be powered off.”

[Usage-limit recovery][limits] keys a recovery opportunity by thread, run and reset time. It can snooze and later post a continuation with deterministic command/message IDs. New user activity, changed run identity or a stopped thread must invalidate stale recovery.

[PR watches][pr-watch] compare new state with the last state already reported: failed/passed checks, reviews/comments and conflicts. They ignore the agent's own remarks, distinguish remarks sharing a timestamp by ID, reset state on a new head SHA, and limit consecutive comment-only wakes to ten. This is much better than repeatedly prompting an agent to poll GitHub itself.

Server-owned settlement and the experimental Working section separate ongoing work from items needing attention. Recent fixes prevent a background server from being described as a waiting agent, preserve work duration over wake turns, and keep settled threads asleep after restarts.

Schedules and PR watches were inspected in source; no ongoing jobs or GitHub writes were created during this audit.

## Other changes since the harness baseline

The complete commit inventory is saved in [evidence][commits]. This table groups the user-visible changes and the infrastructure families; it does not treat every maintenance commit as a new feature.

| Family | Current change / representative commits | Luna relevance |
| --- | --- | --- |
| Projectless work | Scratch threads and projects created from a name; 6b286ae8a2, 148e6deea0 | Already has projectless foundation; compare startup receipt and directory provenance |
| Workspace launch | Retry failed preparation; 737993303d; preserve branch/worktree intent | Valuable for explicit retryable setup failures |
| Provider maintenance | One-click updates across machines, installer ownership and readback; 0a04cc50de, fe93a5dbdd, 3b0093d716 | Luna already has provider updates; compare installer/version proof |
| Skill/command discovery | Workspace-scoped Claude commands, scalar frontmatter handling, session restart; 836098543c, b3b6ae2ceb, 921cb3c8bc | Cache invalidation and refresh semantics |
| Tool presentation | Call above muted result, output expansion fixes, skill/MCP identity; df4ae529ea, 5a96895a85, 1e7c8e0f24 | Presentation choice; preserve raw/full output access |
| Child visibility | Correct actual model/account, follow-up liveness, matching completion cards; 4f7760e6a0, daa1d0ed94, d3071275d5 | High value for trustworthy child status |
| Plans and diagrams | Plan artifacts and Mermaid rendering; 5e35272fda | Nice follow-on; not a core harness dependency |
| Usage | Cost by token type, speed and model; Fast/Ultrafast costing and preview-ID grouping; e8545b293b, 56914128c1, b05adb70b0 | Useful accounting, with pricing freshness handled separately |
| Attention/sidebar | Working section, settle/wake sweep, stable ordering; b33eda1399, 1826fb55cc, 1302ccacbd | Extend Luna's attention model, do not add competing status logic |
| PR/Git workflow | PR quick actions, linked PR routing, branch-wide diffs, staging preservation; 33b7d7b3ae, d1034d62b2, 4ee6bfd50e | Never let title/commit-message helpers mutate the user's index |
| Navigation | Machine cycling, background new-thread shortcuts, draft actions | Productivity polish after orchestration correctness |
| Mobile | Full-screen simulator, complete child detail, persistent dictation, Android widgets and grouped alerts; d60a71ef6e, 65731f986b, bf5c146be9 | Future remote/mobile product work |
| Performance | Command-palette virtualization, narrower CSS invalidation, usage-scan caching; fffe6e6c82, 39efcd8558, 71dbaf1f94 | Borrow measurement and bounded work; React CSS fixes do not port to GPUI |
| Transport/storage | Jittered reconnect, healthy-socket preservation, stream cancellation, SQLite lock waiting; 9333509c91, eac52f0087, 0fe4fa40fe | Inspect equivalent Luna reconnect and write serialization invariants |
| Browser/device | Avoid focus theft and Save dialogs, early diagnostics, preview recovery | Mostly device/browser integration polish |
| Migration/release | V2 database, client compatibility, beta mobile channels, Windows/macOS packaging | Migration lessons, not runtime feature parity |

The V1→V2 migration preserves a separate old database, but does not recreate old native sessions, tool activity, approvals, plans or checkpoint/diff history in V2. Old text is imported and a new provider conversation receives portable context. See [migration source documentation][migration]. Luna should not copy this migration strategy blindly: its CRDT documents, native history and execution bindings need their own lossless compatibility plan.

## What to borrow and what to leave

Borrow explicit run/attempt identity, capability-driven transitions, budgeted handoffs with retrieval, durable delegated-result delivery, background-work ownership, and service methods shared by UI and agent tools.

Do not transplant the entire TypeScript/Effect stack into Rust, replace Luna's synchronized document model, equate a receipt with provider success, or erase native features merely to fit a lowest-common-denominator abstraction. The coordinator itself is now a large file; a Luna adaptation should keep transition decisions small and independently reviewable.

Both repositories' root LICENSE files are MIT. If later implementation copies substantial source, retain the corresponding notices. This research copies no implementation into Luna.

[contracts]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/packages/contracts/src/orchestrationV2.ts#L312
[runs]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/packages/contracts/src/orchestrationV2.ts#L534
[provider-threads]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/packages/contracts/src/orchestrationV2.ts#L835
[sink]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/EventSink.ts#L518
[outbox]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/EffectOutbox.ts#L282
[finalization]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/RunFinalizationService.ts#L56
[mcp-tools]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/mcp/toolkits/orchestrator/tools.ts#L58
[mcp-service]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/mcp/OrchestratorMcpService.ts#L1378
[transition]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.ts#L34
[budget]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/ContextHandoffBudget.ts#L111
[handoff-delivery]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/ContextHandoffDelivery.ts#L11
[fork]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/ThreadForkService.ts#L40
[restore]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/CheckpointRestoreSafety.ts#L14
[queue-ui]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/web/src/components/chat/QueuedRunsControl.tsx#L30
[stop]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/Orchestrator.ts#L8235
[stop-test]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/BackgroundWorkStop.integration.test.ts#L44
[provider-doc]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/internals/providers.md#L103
[schedules]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/scheduledTasks/ScheduledTaskService.ts#L458
[limits]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/UsageLimitRecoveryWorker.ts#L11
[pr-watch]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/apps/server/src/orchestration-v2/pullRequestWatch.ts#L44
[migration]: https://github.com/pingdotgg/t3code/blob/4ee6bfd50ef4a089440d5c3662db2298da9cc50e/docs/user/thread-migration.md
[commits]: evidence/t3code-since-parity-20261004.txt
