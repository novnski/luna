# Automatic review with temporary reviewer side chats

**Status:** user requirements updated on 2026-10-08; not implemented. Reviewers are now ordinary temporary side chats using a selected provider/model. After review and the ticket both succeed, LUNA closes their views and archives them automatically. This explicitly replaces the previous proposal for invisible internal reviewer jobs. The ticket retains its primary chat and durable review results.

## 1. Recommended experience

Use **Backlog → Ready → Done**. Backlog means held/stopped; moving to Ready enables execution and immediately requests Start or Continue. Keep a card in Ready while it is queued, implementing, checking, reviewing or fixing, and show that actual phase separately. Moving back to Backlog uses the shared Stop operation. See [drag/start/stop/resume](board-lifecycle.md). Needs attention identifies a real blocker, not a routine manual-review gate.

The ordinary path is:

1. Save a ticket without creating its working chat.
2. Start with the project's implementation and review defaults, optionally overridden on the ticket.
3. The implementation agent produces changes and runs the required checks.
4. A temporary reviewer side chat examines the exact result and sends findings back to the ticket chat.
5. The implementation agent addresses blocking findings; checks and review repeat within a bounded policy.
6. LUNA marks Done after current checks/review pass, persists and delivers the result, then closes and archives its idle reviewer chats. Results remain linked from the ticket.

The user can inspect the evidence at any time, but routine manual review is not a completion requirement. An optional Manual acceptance policy can remain for projects that need it; it is not this user's default. Done means the configured verification/review contract passed, not a claim that software is certainly defect-free. Publishing, merging, deployment, and closing GitHub issues retain their own explicit project policies.

Keep configuration small:

| Setting | Default / behavior |
| --- | --- |
| Implement with | Existing project provider/account/model/reasoning defaults |
| Review with | Project review profile; choose provider/account/model and supported effort once, with an optional per-ticket override |
| Completion | Automatically after required checks and review pass |
| Advanced limits | Enforce applicable time/cost/concurrency bounds and the confirmed cap of two repair rounds, then Needs attention if blockers remain |

**Confirmed default:** without a separate project/ticket review profile, use the implementation model in a separate reviewer side chat. Resolve it from the implementation configuration for this workflow and show the actual selected model; preserve the review-specific access constraints rather than inheriting broad write access. No extra reviewer-selection question is required. Never replace an explicitly selected reviewer silently; a requested exact model must be available through the target account/harness.

**Confirmed repair cap:** allow the initial review, then at most two cycles of implementation repair followed by re-review. If blocking findings remain, stop automatic repairs and mark Needs attention. This is up to three review passes, not three fixes. Persist the counter across restarts and Backlog/Ready stop/resume so those operations cannot silently reset the cap. A further cycle requires an explicit user-authorized retry; ordinary model-driven retries cannot grant themselves more rounds. Auth failure, cancellation or an earlier budget limit may stop work sooner.

## 2. Reviewer placement and visible identity

The ticket has one primary working chat. A reviewer is a different persistent LUNA chat with a temporary review role; it never becomes the ticket's primary worker or a new board issue.

| Ticket working chat | Reviewer location | Result recipient / workflow owner |
| --- | --- | --- |
| Main chat T | Side chat R directly under T | Ticket chat T |
| Side chat T under main M | Fresh sibling R under the same main M | Ticket chat T, not automatically M |

This preserves the one-level chat hierarchy. A ticket side chat does not acquire nested children. Record creator, display parent and workflow owner separately. The reviewer receives an explicit review brief rather than inheriting the ticket chat's complete context.

Name it `Review · LUN-42 · <ticket title>` with a **Reviewer · LUN-42** badge, distinct from the primary working-chat ticket badge. Show its provider/model and phase. Its link appears in the ticket's activity/result area, and its side-chat tab/list entry is visible while needed. It never adds an independent main-sidebar entry.

Example visible sequence:

> Implementation — selected OpenAI model
>
> Reviewing changes — Claude Code / Opus 5.5 — running
>
> Review result — two blocking findings
>
> Implementation — addressing findings
>
> Review passed · required checks passed · Done

Those names illustrate the workflow, not a live run. Summary/result cards remain in the ticket's primary chat, while detailed review execution is inspectable in the temporary side chat. The selected reviewer has its own provider session, context, account and usage.

Use **Review** for the action and **Reviewer** for the side-chat role. Creation, result return and successful cleanup are automatic under the ticket's configured workflow, without requiring the user to organize or archive the reviewer manually.

## 3. What exists today, and what needs building

The distinction between **model provider** and **harness** matters. OpenAI/Anthropic describe model services; Codex, Claude Code and OpenCode are the agent runtimes LUNA drives.

| Capability | Evidence | Boundary |
| --- | --- | --- |
| Native subagents in a chat | LUNA normalizes nested subagent events and gives them their own detail stream while keeping a spawn chip in the parent transcript | Existing rendering/event foundation; not a generic cross-harness worker launcher |
| Different subagent model/effort | Official Codex docs describe model/effort overrides; Claude Code docs describe model selection | Do not assume either launches the other harness as a native subagent |
| Multiple model providers within one harness | OpenCode supports per-agent `provider/model` selection, including configured subagents | Depends on configured providers/account access; it does not prove Codex → Claude Code delegation |
| Cross-harness LUNA chat creation | `CreateChildThreadRequest.provider` can choose another harness, with its own fresh session/account/defaults | Reuse this foundation for reviewer chats; shared-main sibling placement and workflow ownership need integration |
| Switch provider in the same LUNA chat | ProviderHandoff validates the same conversation ID with a different provider and applies a context boundary | Sequential handoff, not a parallel reviewer; native context is not automatically shared |
| Successful reviewer cleanup | Existing tools close views and archive exact idle descendants | A reviewer sibling is not the ticket side chat's descendant; cleanup must use verified workflow ownership, not broaden arbitrary sibling authority |

Current source: [nested event contract](../../../crates/proto/src/agent.rs), [Codex adapter](../../../crates/harness/src/codex/mod.rs), [OpenCode subagent routing](../../../crates/harness/src/opencode/subagents.rs), [cross-task request types](../../../crates/proto/src/cross_task_tools.rs), [same-chat provider handoff](../../../crates/engine/src/rpc/context_transfer.rs), [context-transfer payloads](../../../crates/proto/src/context_transfer.rs). These are source observations, not fresh multi-provider E2E results.

Official references: [Codex subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents), [Claude Code subagents](https://code.claude.com/docs/en/sub-agents), [OpenCode agents](https://opencode.ai/v2/docs/agents).

Subagents do **not** universally inherit conversation history. Claude Code documents fresh isolated context for normal subagents and inherited history for forks. For cross-harness review, always construct an explicit input packet instead of depending on native inheritance. Model/effort inheritance and conversation-history inheritance are different properties.

## 4. Recommended implementation choice

Create a **workflow-owned reviewer side chat** through shared LUNA conversation creation and the selected harness. Keep the implementer's provider binding intact. Store a review-run record linking ticket, primary chat, reviewer chat, display main parent, requested/resolved/observed model, reviewed revision and result delivery. Reuse an existing reviewer across repair rounds where compatible; do not create a fresh chat for every status update.

The required reviewer is a side chat. Provider-native subagents remain available for other capabilities, but they are not a substitute for this specified review-chat placement/cleanup contract. A different reviewer model/provider uses the appropriate available harness account and is observed in the review run.

The review path uses fresh bounded review context, even when a provider also supports full-history forks. It must not create a native context fork from a side chat as a shortcut around the established restriction.

Sequentially switching the ticket chat's provider is not necessary for this design. Preserve its history/configuration and send findings back through the existing durable result/continuation path.

The old generic inline-worker proposal is superseded. Do not build a second hidden-agent framework as a prerequisite for this feature.

## 5. Review input and result contract

Build a versioned, size-bounded review packet containing:

- Ticket ID, brief revision, requested outcome, acceptance criteria and selected project guidance.
- Exact repository/checkout identity, base and result revisions, diff hash and any included uncommitted-file manifest.
- Relevant source locations and readable code context beyond the diff, with declared omissions.
- Check commands, outcomes and evidence references tied to that same revision.
- Constraints and the review task: find concrete correctness/regression/requirement gaps, return evidence-backed findings, and do not modify the implementation.

The engine assembles the canonical snapshot; the implementer may contribute a summary but cannot control the only evidence the reviewer sees. Do not copy hidden reasoning, secrets, unrelated chat history or permission grants. A reviewer can request additional permitted repository context if the packet is insufficient.

The reviewer returns a structured `passed`, `changesRequested`, `blocked`, `failed`, or `cancelled` result with snapshot identity, observed execution profile, findings and evidence references. Findings include file/location, explanation, severity, required correction or validation, and whether they block completion. A “looks good” string, provider exit, or empty result is insufficient for automatic Done.

The implementer receives findings as an attributed review result or durable queued continuation, not a forged user approval. The reviewer cannot broaden scope, change permissions, launch extra channels, reparent itself/the ticket, merge, deploy, or mark arbitrary tickets complete. It reports to its ticket workflow; the engine owns transitions and cleanup.

## 6. Stable revisions and bounded execution

The first implementation should serialize implementation writes and review. Quiesce the implementation turn and owned blocking work, capture the review basis, and run the reviewer without source-edit privileges. It may read code and inspect verification results; any necessary check execution must respect existing project commands, sandbox/access rules, build locks, ports and app-instance limits.

Verify the repository/diff basis again before accepting the review. If files changed during review, mark the result stale and review the new basis. Git commit ID alone is insufficient when uncommitted changes exist. A read-only review instruction is not a filesystem guarantee; enforce available capabilities and fail clearly if the selected runtime cannot meet the required contract.

Do not run reviewer and implementer as competing writers in one checkout. Do not automatically create worktrees or launch a second LUNA GUI app to support review. An independently authorized isolated environment can be used when the project's policies require it.

Finite repair cycles prevent an endless reviewer/implementer disagreement. Persist findings across rounds, detect repeated unresolved findings/no progress, enforce cost/time limits, and move the ticket to Needs attention when the agents cannot satisfy the contract. Passing a different snapshot, hitting a timeout, losing authentication, or skipping required checks must never yield Done.

## 7. Durable lifecycle and ownership

Use a review-run identity distinct from ticket, primary chat, reviewer chat and provider session IDs. Record owner task/turn, display main parent, role, requested/resolved/observed settings, review basis/input hashes, result, stop state and once-only delivery/cleanup receipts. Its reviewer is an ordinary side chat, with explicit role metadata rather than a disguised native subagent.

States include queued, starting, running, waiting, completed, failed, cancelled and outcome-unknown. Recover uncertain starts by the original key. A primary turn ending does not complete its ticket while owned review/repair remains active. The shared Stop path must include the exact workflow-owned reviewer, even when it is a sibling. A late result cannot complete a ticket returned to Backlog or a different execution generation.

Project/ticket scope and reviewer permissions come from trusted engine configuration, not arbitrary model arguments. Expanding a result shows its actual actor and evidence. If usage is unavailable, say so rather than fabricate a combined cost.

### Automatic cleanup after success

Cleanup requires **all** of: the ticket's current result satisfies completion policy; current review succeeded; result/evidence are persisted and delivered; the exact reviewer and its owned work are observed idle/terminal. A startup/send receipt or the reviewer saying “done” is insufficient.

1. Persist the review result, reviewed snapshot, transcript/evidence references and ticket Done transition with the current intent/execution generation.
2. Resolve only reviewer chat IDs created/registered for that ticket's successful workflow. Never select by title or archive the primary chat, main parent or unrelated siblings.
3. Use existing lifecycle guards to archive those idle reviewers and close their open views. The current archive-with-close operation archives first and closes views after successful archival; preserve that safe ordering. This fulfills “close and archive” without discarding a conversation if archive fails.
4. Read back archived state and absence from active side-chat views. Preserve transcript, results, snapshots and drafts; archiving is not deletion. Keep a Review details link from the ticket to the archived history without silently unarchiving it.
5. Journal partial/unknown outcomes and retry the same cleanup identity. The ticket can show **Done · cleanup pending** if archival fails; do not rerun paid review or claim cleanup succeeded.

Do not archive merely because a review pass ended with requested changes while implementation is still unfinished. Reuse it for the next round. Stopped/failed tickets retain clearly associated review state for recovery; they are not falsely treated as successful cleanup. User inspection does not grant new worker authority; protect any unsaved view state when closing.

Existing `manage_workspace.archiveThreads` is descendant-scoped. In the side-ticket case, the ticket and reviewer are siblings, so letting the ticket call that unchanged is insufficient. Implement an engine-owned cleanup path authorized by the recorded review-run ownership (or the main coordinator's appropriately scoped operation). Do not weaken the ordinary guard to allow arbitrary sibling archives. The same ownership distinction is needed for Stop and result routing.

Source anchors: [cross-task lifecycle tools](../../../crates/engine/src/cross_task_tools.rs), [work ownership](../../../crates/proto/src/task_work.rs), [shared work Stop](../../../crates/engine/src/rpc/task_work.rs).

## 8. Relationship to main and side chats

**A main chat can be a ticket's primary working chat.** Starting a ticket independently from Board already creates that main chat. Converting an existing main chat adds the ticket link/badge without changing identity or placement. A side chat can likewise work on a ticket. A main ticket chat can still have side chats under the established creation rules.

Reviewers are always side chats. A main ticket owns the reviewer's display parent relationship. For a side ticket, the reviewer is attached to that ticket's existing main parent while the ticket remains the result recipient and workflow owner. Neither case creates nested side chats or permits self-promotion/reattachment.

Explicit review requests in non-ticket chats can reuse this mechanism with their own completion scope. They do not silently create board tickets. Automatic ticket-review cleanup applies only to the recorded ticket workflow, not all similarly named chats.

## 9. Delivery and E2E acceptance

Add **PB-1R** after basic ticket/chat identity and lifecycle. It requires cross-harness reviewer-side-chat creation, correct main/sibling placement, result return, shared Stop and verified successful auto-archive. A basic board alone does not complete this scope.

Before implementation, specify failures for unknown model/effort, unsupported read-only execution, incomplete context, stale diffs, reviewer crash, lost launch response, parent restart, repeated findings, exhausted budget, cancelled owner, late results, and wrong-provider attribution.

Required E2E scenarios:

1. A main ticket using Codex runs review through the selected Claude Code model. Verify one reviewer side chat under that ticket, selected model readback, an unchanged primary chat, result return and no new main-sidebar entry.
2. Repeat with a side ticket: reviewer is a fresh sibling under the same main parent, reports to the ticket, and receives no inherited transcript or nested ownership.
3. Reviewer finds a seeded real defect; implementer repairs it; checks and a new review pass for the final diff; ticket reaches Done without a human acceptance click.
4. Mutate the diff during review. The old review must be stale and unable to complete the ticket.
5. Interrupt/restart at launch and delivery boundaries. Prove once-only reviewer creation and result delivery, correct owner association, and no false success after Stop.
6. Simulate unavailable reviewer, omitted context/checks, an unobserved model substitution, and exhausted repair rounds. Each produces a clear unresolved state rather than fallback success.
7. Convert an existing main chat into a ticket and perform the same workflow without replacing its conversation identity or history.
8. After successful review and ticket completion, verify exact reviewer IDs are archived and their views closed; primary/main/unrelated siblings remain. Reopen archived review details without restarting/unarchiving it.
9. Inject lost/partial cleanup responses, a late reviewer turn, and layout revision conflict. Retry cleanup idempotently with truthful pending state; no premature archive or duplicate review.
10. Stop a side ticket during sibling review by moving it to Backlog. Verify shared Stop reaches both owned executions, no unrelated sibling is stopped, and a late pass cannot mark Done.

Retain a scenario manifest, exact model/account capability observations with secrets removed, snapshot hashes, launch/result/cancel receipts, provider event evidence, chat-row counts, and native UI recordings. No such live execution was performed for this research.

The [saved mock and interaction evidence](mockup/README.md) demonstrate the updated layout and local start/stop/resume/reviewer-cleanup simulation. They are design evidence, not live provider or LUNA runtime certification.
