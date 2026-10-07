# T3 Orchestrator V2 and BB: what Luna should adopt

Research completed **2026-10-04**. The local T3 and BB checkouts were fast-forwarded to latest available main and inspected directly. Relevant installed-app flows were exercised. This package contains findings and an implementation plan; it makes no Luna runtime changes.

**Recommendation:** evolve Luna's existing harness. Borrow T3's explicit execution lifecycle, portable context transfers and background-work coordination, plus BB's permission-transition, composer-ownership and history-performance lessons. A wholesale orchestrator rewrite would duplicate substantial Luna functionality.

## Read the research

1. [T3 Orchestrator V2 deep dive](t3-orchestrator-v2.md) — execution records, transaction/outbox, adapters, child delivery, provider handoffs, fork/merge-back, queues, Stop, recovery, questions, schedules, PR watches and the rest of the recent feature families.
2. [BB delta and code analysis](bb-delta.md) — provider fixes, eager forks, model options, timeline scaling, composer ownership, automation changes and full feature-family inventory.
3. [Luna integration plan](luna-integration-plan.md) — existing versus missing capabilities, specific source locations, implementation phases and failure-first E2E acceptance criteria.
4. [Live evidence and repeatable probes](evidence-and-tests.md) — versions, exact test conversations, prompts, screenshots, persisted readbacks and coverage limitations.

## The actual comparison window

| Repository | Last harness audit | Pulled revision | Commits in range |
| --- | --- | --- | --- |
| T3 | 8792f95762 | 4ee6bfd50e | 203 |
| BB | f1172452 | a2bbfaad | 196 |

Today's earlier product/UI study had newer baselines: T3 a21702d0e5 and BB 76a1ebca. There are 74 and 4 commits after those snapshots respectively. V2 was already in the T3 source used for that lighter UI review. The current work adds a much deeper execution/source comparison and fresh behavior probes.

BB's four newest commits include an automation-timeline change and its immediate revert. It is not listed here as an active new capability.

## What V2 means

T3's orchestrator is the application layer between the user/agent tools and provider runtimes. The user-visible conversation is separate from a requested run, each execution attempt, a native provider conversation, background work and context delivery.

That separation enables a practical workflow:

1. Start with Codex.
2. Switch to Claude with a bounded, attributed history packet.
3. Return to the original Codex conversation with only the missed context.
4. Fork a conversation and work independently.
5. Merge the fork's findings back as conversation context.
6. Keep queues, completion delivery and background controls owned by the app.

V1 already used events and projections. V2's value is its more explicit execution state and durable side effects, rather than merely introducing event sourcing.

The provider's own agent loop still belongs to Codex, Claude, OpenCode, etc. Porting T3's architecture does not mean embedding a new model loop inside Luna.

## What I verified in the apps

- Opened the user's existing Claude→Codex “Random number from Sol” result: persistent child, original task, result 73, provider/model and parent lineage.
- Created an isolated projectless T3 probe and verified **Codex → Claude → Codex context continuity**.
- Created a **native Codex fork with a distinct native ID** and verified it retained context.
- Verified **merge-back**: the parent recalled a marker introduced only in the fork. This transfers context; it does not merge Git changes.
- Queued a T3 follow-up during a harmless sleep, edited it before dispatch, and verified the edited request ran afterward.
- Created a projectless BB conversation, forked it before sending any new prompt, and verified the persistent fork could recall its source context.
- Independently checked T3's test-only persisted records and BB's fork metadata through read-only queries/CLI.

All recorded T3 runs completed, and BB's fork was idle. The labeled test chats remain available for inspection. Destructive rollback, forced crashes, live descendant cancellation, rate-limit exhaustion, schedules and PR watches were not tested against the user's normal profiles.

## Highest-value Luna opportunities

| Priority | Adopt / improve | Why |
| --- | --- | --- |
| First | One capability and transition snapshot | UI, tools, queue and recovery should agree on effective provider behavior |
| First product addition | Portable history transfer and fork-result merge-back | The clearest verified improvement to everyday cross-model work |
| Next | Unified background-work status, ownership and stopping | Avoid confusing a completed turn with a completed task or a persistent service |
| Next | Targeted durable-effect and completion-delivery hardening | Recover without duplicate prompts or duplicate parent wakeups |
| Later | App-owned schedules and PR-triggered wakes | Turn “check back later” into durable, inspectable work |
| Measured refinements | BB-style bounded history work and explicit composer ownership | Keep long conversations and multi-pane input fast and correctly routed |

Luna already has durable child creation, command receipts, acceptance-aware recovery, queue/steer, native history controls, isolated-worktree checkpoints, dynamic speed options, OpenCode 1/2 integration, native task observations, attention handling and a virtualized transcript. Those are foundations to extend.

Specific differences that matter:

- Luna's current provider handoff is explicit visible history capped at 8 messages / 12 KiB. T3 adds per-provider history coverage, capacity-aware selection, omission retrieval and delta delivery.
- Luna reports child turn completion and has separate native background ledgers. A shared task-level work state and stop scope are the useful next step.
- Luna observes Claude-native schedules; an application-owned scheduler independent of that runtime is still missing.
- BB's warm permission updates and narrow session-grant fixes are excellent regression scenarios, but this research does not assert the same bugs exist in Luna.
- Luna's current persistent-fork and workbench edits are in progress. Integrate with that work instead of duplicating it.

## What not to copy

Do not replace Rust/GPUI/CRDT infrastructure with T3's TypeScript/Effect implementation, equate durable intent with provider success, use a new scheduler to replay requests with unknown acceptance, or adopt a plugin marketplace/cloud service as a prerequisite for better orchestration.

Both reference projects are MIT at the inspected revisions; substantial copied implementation would require their notices. This package contains analysis, source references and test evidence, not imported implementation.

## Verification and next implementation step

The artifact verifier checks the source links against the pinned local Git objects, fingerprints, saved JSON receipts, sentinel outcomes and image integrity. It does not rerun providers. See [validation.json](evidence/validation.json) and [SHA256SUMS](SHA256SUMS).

No upstream suite, Luna build, native Luna QA, app refresh, commit or push was performed. Luna already had substantial unrelated edits, and they were preserved.

The implementation should start by reconciling the existing persistent-fork/workbench changes, then adding an inspectable capability/transition contract and the cross-provider continuity E2E matrix. The next user-visible slice is portable context plus fork-result merge-back.
