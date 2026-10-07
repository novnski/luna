# Claude Code feature parity for LUNA

Original research date: 2026-09-30. Reassessed against refreshed LUNA, BB and T3 main branches on **2026-10-01**.

**Start with the [current gap assessment](current-gap-assessment.md).** The ten shared/Codex features have merged; Claude's adapter still needs its own integration. The reassessment maps every CC package to the current code, corrects the follow-up-image and shared-infrastructure assumptions, and records a confirmed Ask/Auto configuration mismatch on the installed CLI. Its [baseline](reassessment-baseline.json) supersedes the old source pins for current planning. Full Claude E2E certification remains pending.

The objective is a complete Claude Code integration: native execution must remain intact, every relevant result and lifecycle event must survive into LUNA, and LUNA's own chat features must be callable and usable with Claude. A tool executing successfully inside Claude does not prove that LUNA preserves its output, attribution, approval or completion state.

## Start here

- [Current gap assessment](current-gap-assessment.md): fresh source findings, shipped foundations, 19-package status ledger, priorities and additional acceptance scenarios.
- [Implementation plan](implementation-plan.md): original 19 work packages and completion gates; apply the reassessment's reduced scope and revised ordering.
- [LUNA coverage audit](luna-coverage-audit.md): confirmed strengths and gaps, with pinned source references.
- [T3 and BB integration audit](reference-integrations.md): what their adapters actually implement and what to adopt.
- [Capability inventory](capability-inventory.md): the full scope, including advanced native and hosted features.
- [Tool and event matrix](tool-and-event-matrix.md): built-in tools, MCP payloads, messages, controls and hooks.
- [Validation plan](validation-plan.md): failure cases and repeatable end-to-end evidence.
- [Portable handover](HANDOVER.md): a short starting prompt for any coding agent.
- [Research baseline](research-baseline.json): source commits, CLI/SDK versions, artifact hashes and verification limits.

Repository root: `/Users/iulian/workspace/code/personal/luna`. Reference repositories: `/Users/iulian/workspace/code/personal/t3code` and `/Users/iulian/workspace/code/personal/bb`.

## Original baseline findings

The findings below describe the 2026-09-30 source pin. Current completion and routing corrections are in the [reassessment](current-gap-assessment.md), including normal follow-up images taking a fresh resumed process.

1. **LUNA has a substantial native integration already.** It launches the installed Claude CLI, keeps a streaming session alive across turns, resumes native sessions, streams text and reasoning summaries, handles common tools and approvals, loads initial images, and routes child-agent content. It also has live model and slash-command discovery; an older catalog comment understates this implementation.
2. **Tool-result bodies are discarded.** The Claude normalizer emits root and child tool completion with `output: None` and `diff: None`. The wire content-block type also omits nested result content. LUNA's downstream journal and expandable detail cannot reconstruct information that never reached them.
3. **Images are only included in the initial request path.** The warm-session/steering path serializes text. Initial unreadable, oversized or unsupported images are skipped, leaving path references rather than a dependable vision request.
4. **LUNA's custom cross-chat tools are unavailable to Claude.** Its runtime contract is explicitly `ClaudeInstructionsOnly`, with no callable app-tool catalog. T3 registers a scoped app MCP endpoint; BB supplies an SDK MCP bridge. Instructions alone are not an equivalent connection.
5. **The control and event vocabulary is incomplete.** Unknown control requests receive no response; many newer event types disappear. Child routing exists, but task progress, workflow ownership, hooks, native reset and richer results need explicit treatment.
6. **Native history operations need a Claude implementation.** LUNA's history capability gate identifies no safe Claude idle fork. SDK-backed T3 and BB use native session-fork operations; file checkpoint rewind is a separate feature, not automatically covered by their fork support.

Evidence for these findings is in the [LUNA audit](luna-coverage-audit.md), not inferred solely from public SDK types.

## Coverage contract

Track four independent dimensions for each capability:

| Dimension | Question |
| --- | --- |
| Execution | Does the selected native Claude runtime support and execute it? |
| Fidelity | Do arguments, output, errors, origin, identities and lifecycle survive normalization and storage? |
| Product connection | Can users invoke, inspect, answer, resume or stop it through LUNA? |
| Verification | Has the production adapter/engine/UI path been exercised with retained evidence? |

Use `supported`, `partial`, `unverified`, `gated`, `unavailable` and `excluded by product decision` deliberately. A documented feature, exported SDK type or successful fake-process replay is not a real-session pass. Hosted features require their own account/service verification and are not implicitly delivered by local CLI support.

Claude Code's documented surface extends beyond core tools to subagents, workflows, messaging, goals, hooks, plugins and hosted integrations. Those are included in the inventory rather than assumed present in T3, BB or LUNA. [Claude extension overview](https://code.claude.com/docs/en/features-overview), [platform overview](https://code.claude.com/docs/en/platforms).

## Recommended order

| Stage | Outcome | Work packages |
| --- | --- | --- |
| A | Pin current compatibility and decide transport using evidence | CC-00, CC-01 |
| B | Preserve complete native data and fix everyday interaction gaps | CC-02 through CC-06 |
| C | Make LUNA tools, native agents and session history usable | CC-07 through CC-10 |
| D | Connect configuration, extensions and ongoing work | CC-11 through CC-15 |
| E | Connect hosted/native integrations and certify the full matrix | CC-16 through CC-18 |

The transport decision is a gate, not a predetermined rewrite. T3 and BB use the TypeScript Agent SDK; LUNA uses Rust and the CLI protocol. Keep the current adapter if it can meet a versioned compatibility contract. Introduce an SDK sidecar only if the comparison demonstrates that it is needed and its packaging/lifecycle cost is acceptable.

## Existing work and product decisions

Another agent is integrating shared Codex/LUNA features while this audit is written. Before implementation, refresh the baseline and reuse merged image/composer, tool-output, permission, history, integration-inventory, review, goal and child-chat contracts. The [Codex backlog](/Users/iulian/workspace/code/personal/luna/docs/pending-tasks/codex-feature-parity/README.md) describes related work; it is historical research, not an authoritative current completion list. The earlier orchestration's portable handovers are at `/Users/iulian/workspace/code/personal/luna-feature-checkouts/2026-09-30-codex-parity/handovers/README.md`; do not resume those tasks from this plan.

The user cancelled the native Plan/Build feature. This plan preserves ordinary chat for research and plan files. Do not add mode switches, a proposal/implement workflow or a replacement `/plan` command. Native mode-entry/exit tool records still need safe handling so imported sessions and unexpected requests do not hang or bypass approvals.

No worktree creation is authorized by this research. Observe native workspace changes when present; do not enable automatic worktree creation as an implementation shortcut. Preserve existing source, data, checkouts, credentials and build caches.

## What this audit verified

Read pinned source in all three repositories; read current official Claude documentation; inspect the installed CLI's version/help; compare local reference commits with immutable upstream heads. No model prompt, authenticated SDK query, new build, E2E execution, app refresh or GUI interaction was performed. No production implementation or merge is claimed. Local runtime findings are tied to the source commit in [the baseline](research-baseline.json); another agent is moving the checkout, so line numbers and shared behavior must be refreshed before coding.

Durability: the original snapshot is at `/Users/iulian/workspace/code/personal/luna-feature-research/2026-09-30-claude-code-parity`. The reassessment, original-doc backup, source snapshots and initialization evidence are at `/Users/iulian/workspace/code/personal/luna-feature-research/2026-10-01-claude-code-parity-refresh`. These research drafts remain uncommitted and unpushed. Existing work in the pending-task index was preserved.
