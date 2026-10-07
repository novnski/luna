# Luna integration plan: borrow the contracts, preserve the foundation

This is an implementation proposal, not a completed feature. Luna source was inspected at HEAD **0800dfc6ec3ed05ef74a0e52f517eb44099508dd** plus the existing working-tree changes. Those changes include persistent-fork/workbench work. [Source fingerprints](evidence/source-index.json) identify the exact inspected file contents; an installed-app build from this working tree was not produced or certified here.

## Recommendation

Keep Luna's Rust engine, native adapters, CRDT documents, task identity and permission model. Improve the contracts between them in small steps. The strongest product additions are **portable provider context and fork-result merge-back**, **one truthful view of background work**, and **durable app-owned schedules/PR watches**.

The first engineering step should be a common capability/transition view and regression coverage. Do not start with a replacement orchestrator, a plugin marketplace, or a large provider expansion.

## What Luna already has

| Area | Current source evidence | Assessment |
| --- | --- | --- |
| App-owned task identity | [TaskLineage and ExecutionBinding][identity] distinguish task IDs, provider observations and binding generations | Existing; do not rename native IDs into app IDs |
| Durable commands | [CommandReceipt][receipts] includes payload hash, task/binding revision, correlation, status and reason | Existing; already more than a transient tool call |
| Atomic context boundaries | [commit_context_boundary][context-commit] coordinates task identity, registry, context state, recovery and receipt | Existing; extend rather than replace |
| Queue/steer | [RPC intents][rpc] cover queue, edit, remove, send-now and steer-now; DocHost owns claims/edit leases | Existing; compare crash/acceptance behavior |
| Recovery | [Recovery policy][recovery] distinguishes rejected-before-acceptance, accepted and unknown; unknown requests are not replayed | Existing and essential to preserve |
| Child chats | [Child lifecycle][children] has idempotent startup, durable outcomes and optional parent continuation | Existing; shared lifecycle and completion semantics can improve |
| Agent API | [Cross-task tools][tools] expose model discovery, create/list/read/send/wait; [runtime contract][runtime] records actual attached capabilities | Existing, narrower than T3's app-control API |
| Native background work | [Native task ledger][native-tasks] persists workflows/teammates after the launching turn; session exit settles them | Existing; not yet one app-wide work-state contract |
| Provider switches | [visible_handoff][handoff] and [context contract][context] create a fresh binding and optional selected visible history | Partial compared with T3's automatic delta-aware transfer |
| Fork/rewind | [History operations][history] and provider native-history interfaces already exist | Existing; persistence/workbench changes are in progress |
| Filesystem checkpoints | [GitCheckpoints][checkpoints] requires an isolated managed worktree, checks ownership, previews restore and rechecks state | Existing; do not add a second checkpoint system |
| Model options | [Codex model_service_tier][model-options] reads native serviceTiers and additionalSpeedTiers | Existing; test options through every execution path |
| OpenCode 2 | [OpenCode adapter][opencode] supports both 1.x and 2.x wire layouts | Existing; do not propose “add OpenCode 2” as a blank feature |
| Native questions | Harness/engine question bridge and secret-input path already exist | Existing; compare async/message-response versus live callback semantics |
| Schedules | [NativeSchedule][native-schedules] observes Claude-owned jobs; engine explicitly reports app automations unavailable | Gap: a provider-independent persistent scheduler |
| PR workflow | [GitHubPullRequests][pull-requests] supports detail/create/action flows | Gap: no T3-style durable agent wake-watch identified in the inspected paths |
| Attention | [Attention engine][attention] reconciles requests, run/recovery state and queues | Existing; best home for background and scheduler signals |
| Transcript rendering | [Transcript][transcript] virtualizes rows and uses incremental/live parsing | Existing; host/pane scaling still needs measurement |
| Drafts | [Prompt stash][stash] stores deliberate saved prompts | Existing; automatic paged prompt history is a different feature |

The cross-task create tool currently requires exact one-use local approval, has explicit permission ceilings, and says durable startup is not provider acceptance. T3's broader tool catalog is not a reason to weaken that contract.

## 1. Consolidate capability and transition decisions

**Value:** the UI, agent tools, scheduler and queue all tell the same truth about what a provider can do now.

Luna currently exposes related facts through Harness methods, permission capabilities, fork capabilities, context controls and the runtime tool contract. Build an additive, versioned **ProviderExecutionCapabilities** snapshot from those existing facts instead of inventing a separate source of truth.

Suggested groups:

- Session continuity: warm reuse, native resume, account/home compatibility, exclusive-writer requirement.
- Turn controls: native steer, interrupt/restart steer, interruption acknowledgement, completion readback.
- Context: native history injection, attributed text handoff, delta handoff, capacity/usage quality.
- History: native fork, source-point precision, conversation rewind, filesystem checkpoint operation.
- Requests: blocking callback, message-based async reply, secret-input support, expiry/cancellation behavior.
- Work: native child IDs, background shell/task visibility, stop capability and ownership scope.
- Policy: supported modes, effective enforcement, degradation reason and parent ceiling.
- Provenance: executable version, account identity key, observed time and stale/unknown state.

A small pure transition planner should return reuse, apply-on-next-turn, restart-and-resume, new-binding-with-handoff, or unavailable-with-reason. It should consume provider/account/workspace/policy/model changes and produce an inspectable receipt. Existing engine transition gates remain authoritative.

**Likely touch points:** Harness trait and adapter implementations; proto runtime/context contracts; engine context/permission resolution; existing runtime diagnostics and provider controls UI. Keep legacy wire fields readable.

**Failures to specify before implementation:** stale model catalog, account switch with the same model name, mode change during a steer, unsupported option retained from a previous model, remote executable upgrade, and a warm runtime still holding obsolete permissions.

**E2E acceptance:** for Codex, Claude and OpenCode, record requested and effective settings, transition choice, provider native identity, and subsequent observed behavior. A changed picker label alone is not a pass.

## 2. Portable context transfer and fork-result merge-back

**Value:** move from Codex to Claude, return later, and bring a fork's findings back without manually restating everything.

Current Luna handoffs are explicit selected visible messages, bounded to **8 messages / 12 KiB**. This is a deliberate conservative policy, not a broken feature. T3 demonstrates an optional richer model: provider-specific history bindings plus a cursor for what each target has already seen.

Add a context-transfer record above the provider operation:

- Stable transfer ID and idempotency key.
- Source task/binding/generation and selected source range.
- Target task/binding/native identity.
- Kind: provider handoff, portable fork or fork-result merge-back.
- Selection policy and provenance for each included item.
- Byte/token estimate, model capacity source and reserved budget.
- Included/omitted item IDs and retrieval cursor.
- Planned, delivered, unknown, failed and consumed states.
- Native acceptance/readback where the provider offers it.

Start with **visible user/assistant text only**, matching Luna's existing expectation. Selected tool outcomes can be a later explicit option. Do not silently copy hidden reasoning, approval payloads, secrets or binary attachments. Do not manufacture an LLM summary without saying so.

Offer “continue with context” and “start fresh” using an explicit preview. A return to a still-compatible native conversation can receive only unseen context. If identity or acceptance is uncertain, preserve the transfer and show the limitation instead of injecting again.

For merge-back, transfer a bounded fork delta to the parent; show exactly which task and source range supplied it. This action must be named and described as **conversation context transfer**, with Git merge remaining a separate action. Do not make source deletion invalidate a surviving fork.

**Likely touch points:** context_control.rs, rpc.rs visible_handoff/apply_context_boundary, doc_host.rs, sessions.rs handoff acceptance, history_operations.rs, cross_task_tools.rs and related-chat UI. Extend the existing durable command transaction and local journal; project only compact metadata into shared documents.

**Failures:** duplicate merge click; parent advances while preview is open; provider accepts but response is lost; source task is deleted; fork is switched to a different provider; old target context is compacted; message exceeds budget; a custom model has unknown capacity; current prompt alone exhausts capacity.

**E2E artifact:** an A→B→A marker transcript, native IDs before/after, transfer receipts with included/omitted IDs, a fork-only marker successfully recalled by the parent, and proof that no Git branch or user index changed. Repeat across a process restart in an isolated QA profile.

## 3. One background-work model and explicit completion ownership

**Value:** “Done,” “waiting for a child,” and “a server is still running” become different observable states; Stop affects the intended work.

Luna already separates provider-native tasks from ordinary transcript turns. Its child sweep reports **turn-completed**, not “the whole delegated task finished.” Preserve that honesty. The missing abstraction to investigate is a shared **TaskWorkState**, covering the current turn, app-created descendants, native subagents, workflows, background commands and pending result deliveries.

Suggested small work ledger:

- Stable work ID and origin: native child, app child, shell, workflow, monitor.
- Launching task/run/binding generation and native reference.
- Lifecycle owner, distinct from source/fork ancestry.
- Whether it blocks task completion, needs attention, or is merely a persistent service.
- State and evidence: starting/running/waiting/terminal/unknown, last observed sequence.
- Stop target and acknowledgement/terminal readback.
- Completion delivery ownership and acknowledgement.

T3's “completed turn with live nested work” behavior is a useful reference, but not all background work should keep a task busy. A dev server may remain available after the requested task is done. The task projection must explain which category is active.

**Stop design:** expose Stop current turn and, where applicable, Stop task-owned work. Resolve exact owners across bindings before dispatch; do not kill by process-name pattern. Later independent turns in a child chat must not be cancelled merely because an earlier delegated task is cancelled.

Luna's current interrupt targets the live run handle for a chat. Native task controls are additional paths. Verify the combined behavior before claiming a bug or introducing a second stop mechanism.

**Delivery design:** retain Luna's stable outcome IDs and current continuation policy. Add explicit acknowledgement/disposal only where needed to prevent duplicate parent wakes. Preserve Claude's existing peer-origin envelope and permission-class holds in claude_child_reports.rs; never relabel child output as the user's authorization.

**Failures:** child turn completes while a grandchild runs; status is read at the same time as parent wake; parent is archived or stopped; late completion from a superseded binding; provider switch leaves a shell alive; server restart interrupts delivery; a child is reused for a later independent run.

**E2E artifact:** a work ledger before/after Stop, scoped process/port evidence for owned test processes, exactly one delivered result, and an independently controlled child turn surviving cancellation of an older task.

## 4. Improve durable effects only where existing receipts leave a gap

**Value:** recover safely between persisted intent, provider acceptance and completion.

Luna already has durable commands, payload hashes, task revisions, queue claims, atomic projection commits, a host run journal and acceptance-aware recovery. Replacing this with T3's tables would add migration risk without proving a benefit.

Audit the actual effect boundaries instead:

1. Accept a command and record the exact request.
2. Claim execution under the existing task/binding authority.
3. Record whether delivery was rejected, accepted or unknown.
4. Observe native completion separately from checkpoint/UI finalization.
5. Retry only the operation that is safe for that acceptance state.
6. Reject stale run/attempt/binding updates.

Where an effect is still only in memory, add a typed local effect record tied to the existing command/run identity. Preserve the rule that **unknown acceptance cannot authorize replay of the original request**. Background recovery and metadata generation should not block each other unnecessarily, but file restore must block later mutation until its outcome is resolved.

**Failures:** crash after commit/before dispatch; crash after native acceptance/before receipt; duplicate command with different payload; lease expiry during a long call; stale event after a new attempt; disk-full journal write; Stop racing a queued start; failed checkpoint followed by a new prompt.

**E2E artifact:** deterministic crash-point runs in a disposable QA profile, event/receipt timelines, exactly one native user request when acceptance was known, and an explicit unknown state when it was not.

## 5. Add an app scheduler, then PR watch

**Value:** “check this later” remains real work when the current model turn ends.

Luna's Claude schedules are provider-owned and depend on that runtime. They are not an app scheduler. Add a host-owned schedule service using the same command, permission, workspace and provider routing as a normal user-approved send.

Persist schedule identity, owner host, timezone, policy, next due time, enabled state, target conversation versus new conversation, and an execution ledger. Atomically claim a due slot using its scheduled occurrence identity. Define overlap, sleep/missed-run behavior, user changes during a sweep, pause/resume, retry, cancellation and retention before UI work.

Keep recurring schedules and event-driven watches distinct. A PR watch should persist repo/PR/head identity, last observed checks and remark IDs, a deduplicated wake cursor and a bounded wake budget. Ignore its own responses and avoid repeated wakes for unchanged state. A new head invalidates stale check results. Read failures are unknown, never “checks passed.”

**Sequence:** scheduler ledger and read-only UI → one-shot tasks → recurring tasks/timezones → agent tools → PR-watch producer. Keep external side effects subject to normal task authorization.

**Failures:** DST changes, machine asleep, two consumers claim one slot, paused/deleted schedule from a stale snapshot, offline execution host, new user prompt before an old retry, rate limit without a reset time, same-second PR comments, own-comment feedback loops.

**E2E artifact:** a clock-controlled replay plus one real short run in a disposable profile; exact occurrence IDs and one provider dispatch per occurrence; no activity after pause; explicit missed/failed runs. No jobs were created by this research.

## 6. Apply BB's performance and composer lessons to the current workbench

**Value:** large histories and several panes do not multiply work or misroute input.

Before adding caches, measure:

- 1,000+ conversation items, large tool outputs, nested child streaming.
- Reopen the same chat and switch among multiple panes.
- Keep a hidden chat streaming while the focused composer accepts input.
- Invalidate history after rewind/compaction/fork, including same-ID rewritten items.
- Separate durable full-output storage from preview/wire budgets.

Reuse settled projections when the event sequence, rewrite generation, context boundary and binding still match. Limit cache entries and bytes, and make hidden views release unnecessary work. Luna already has virtualized rows and incremental Markdown; the target is the remaining host/query/subscription work, not a rewrite of rendering.

Use an explicit composer/chat/pane identity for keyboard actions, popups, model selection and queued-message editing. BB's composer-owned command routing is a particularly useful reference for the dirty workbench code in this checkout.

**E2E artifact:** captured frame/interaction timing, host CPU and payload measurements, bounded memory after repeated navigation, and a two-pane test proving that only the intended chat receives a sent message. Set thresholds after establishing a reproducible baseline.

## Delivery order and scope

| Order | Deliverable | Size / dependency | Exit condition |
| --- | --- | --- | --- |
| 0 | Reconcile this plan with current persistent-fork and workbench changes | Small review; avoid editing another task's work | Agreed source baseline and no duplicate implementation |
| 1 | Capability/transition diagnostics plus active-permission regression scenarios | Medium; existing adapters and diagnostics | Requested/effective state and transition outcome agree |
| 2 | Visible-text portable handoff and fork-result merge-back | Medium–large; existing boundary and native history | A→B→A and fork→parent continuity with durable receipts |
| 3 | Unified work status, stop ownership and result delivery | Large; depends on explicit identities | Nested work and stop/restart scenarios pass |
| 4 | Targeted durable-effect hardening | Scope determined by boundary audit | Crash matrix passes without replaying unknown requests |
| 5 | App schedules, then PR watches | Large; relies on 1/3/4 | One durable execution per occurrence, meaningful wakes only |
| 6 | Workbench performance, prompt-history and presentation refinements | Measured slices; some can precede later phases | Quantified improvements and correct composer routing |

Model catalog/tier propagation, native question behavior and installer-aware updates are verification/refinement tracks, because Luna already has much of the functionality.

Pi, ACP Registry and other retired provider IDs should remain a separate product decision. A saved enum variant is not a runnable integration: HarnessId::is_retired currently excludes those providers from active support. Finish the capability contract before considering provider breadth.

## Required implementation verification

For future changes, follow the repository's failure-first and E2E preference. Define the failure matrix before implementing an isolated subsystem. Do not add unit tests after writing its code as an afterthought.

Use the smallest relevant scripts/cargo.sh check --locked target, the appropriate existing integration/native E2E runner, and the configured formatting/lint checks. Budget builds with scripts/build-budget.py. Any native QA uses its own app identity, IPC port and data directory and preserves the shared build cache. Finish a meaningful implementation with the required LUNA Dev refresh, exact process/port/signature readback and repository delivery workflow.

None of those implementation builds or checks are claimed by this documentation-only research. The report's own evidence, links and fingerprints are verified separately.

[identity]: /Users/iulian/workspace/code/personal/luna/crates/proto/src/task_identity.rs:57
[receipts]: /Users/iulian/workspace/code/personal/luna/crates/proto/src/task_command.rs:93
[context-commit]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/doc_host.rs:1343
[rpc]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/rpc.rs:494
[recovery]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/recovery.rs:68
[children]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/child_threads.rs:1181
[tools]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/cross_task_tools.rs:1164
[runtime]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/runtime_contract.rs:26
[native-tasks]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/native_tasks.rs:20
[handoff]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/rpc.rs:4807
[context]: /Users/iulian/workspace/code/personal/luna/crates/proto/src/context_control.rs:12
[history]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/history_operations.rs
[checkpoints]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/git_checkpoints.rs:468
[model-options]: /Users/iulian/workspace/code/personal/luna/crates/harness/src/codex/mod.rs:416
[opencode]: /Users/iulian/workspace/code/personal/luna/crates/harness/src/opencode/mod.rs:459
[native-schedules]: /Users/iulian/workspace/code/personal/luna/crates/proto/src/goal.rs:59
[pull-requests]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/source_control/pull_requests.rs:17
[attention]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/attention.rs:18
[transcript]: /Users/iulian/workspace/code/personal/luna/crates/ui/src/transcript.rs:2956
[stash]: /Users/iulian/workspace/code/personal/luna/crates/engine/src/rpc/prompt_stash.rs:45
