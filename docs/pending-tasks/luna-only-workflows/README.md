# LUNA-only workflows: prioritized implementation plan

**Status:** pending. This package defines tasks and acceptance gates; it does not claim that they have been implemented or tested.

**Requested:** 2026-10-10, Europe/Bucharest. **Planning baseline:** `06ea5cd56ad1c5874921413b020a54f3cedb0c27`. Recheck current source and provider capabilities in SW-00 before implementation.

## Outcome and scope

Iulian can carry his recurring desktop work through LUNA: investigate Moralis support and infrastructure using current evidence, implement and verify code, coordinate requested helpers, inspect and revise visual artifacts, preserve past context, and follow scheduled work. LUNA remains the interface over native provider engines; this plan does not replace Claude/Codex inference or introduce another orchestrator.

The usage audit makes integrations, computer use, transcript trust and task continuity the first priorities. See [evidence and limitations](evidence.md). The primary switching campaign covers Codex and Claude in the permission modes actually used. Existing plans that require OpenCode retain that scope; this package cannot mark their all-provider work complete using only two-provider evidence.

Read [implementation tasks](implementation.md) for ownership and delivery, then [E2E acceptance](verification.md) for observable pass conditions, failure cases and retained artifacts. Follow [AGENTS.md](../../../AGENTS.md), [test-audit](../../../.codex/skills/test-audit/SKILL.md), [QA infrastructure](../../reference/qa-infrastructure.md) and [commit-and-push](../../../.codex/skills/commit-and-push/SKILL.md).

## Existing work and duplicate prevention

This is a coordinating backlog. An external owner below keeps its implementation and test contracts. Link its evidence into this campaign; do not create a second renderer, runner, settings store, or equivalent test suite.

| Requirement | Existing owner and baseline state | What this plan adds |
| --- | --- | --- |
| Interactive HTML, artifact persistence and rendering | [Interactive chat surfaces](../interactive-chat-surfaces/README.md), IC-0–IC-3, pending | SW-07 consumes these phases for a real visual iteration journey. HTML tool/storage/isolation work stays there. |
| Interactive MCP Apps | [IC-4](../interactive-chat-surfaces/implementation.md#ic-4--mcp-apps-capture-and-live-request-host), pending | SW-14 verifies an actually needed connected-app journey. No second app bridge or Office-style frontend. |
| Find within the selected conversation | [IC-5](../interactive-chat-surfaces/implementation.md#ic-5--selected-conversation-search), pending | SW-11 connects it to imported/past work after it ships. Existing global history search remains the owner of cross-chat search. |
| Board, Side chats and review | [Project Board](../devin-project-board/README.md), PB-0–PB-3/PB-1R implemented | SW-10 certifies helper supervision; SW-12 verifies recent fixes. PB-4 remains future scope. |
| Automation editor/state, ticket deletion/badges, image attachments | [Delivered fixes](../../design/board-automation-ticket-fixes/verification.md), `dfba2f00` | SW-12 is verification-first. Reuse LUN-1, LUN-2, LUN-11, LUN-12 and LUN-13; do not duplicate their tickets or rebuild the features. |
| Skills lifecycle and picker | [Managed skills](../../reference/managed-skills-qa.md), [native picker](../../qa/integrations-picker/README.md), implemented | SW-01/SW-03 expose effective delivery in the actual permission mode; SW-11 uses existing copy/install/import paths. |
| Persistent helpers and workspace controls | [Agent workspace controls](../../design/agent-workspace-controls/README.md), implemented | SW-10 adds only a proven gap in scoped stop/readback; keep existing create/read/send/wait/layout ownership. |
| Timeline, input identity, queue, restart and history | Existing engine/harness suites named in [verification](verification.md#existing-owners-and-runnable-checks) | SW-05/SW-06 extend only uncovered contracts and add real native-session evidence. Existing fixes are not presumed broken. |
| Local recurrence | Existing scheduler, `automations_e2e` and `qa-automations-live.mjs` | SW-13 certifies actual runs and host lifecycle; no second scheduler. |
| Settings/attention, general workspaces, visual attachments | [Workspace opportunities](../agent-workspace-app-research/luna-opportunities.md), research | SW-01/SW-08/SW-11 own only the bounded migration slices; broader settings registry, layouts and feature bundles remain outside this plan. |
| Paired phone control | [Control this Mac](../control-this-mac/README.md), M0–M4 pending | SW-16 records whether it is needed and consumes its milestones. No duplicate remote transport. |
| Older provider parity packages | [Pending index](../README.md) records their completion/removal | Extend current owners; do not reopen the entire retired Codex/Claude/OpenCode parity backlog. Claude native workflow lanes remain deliberately deferred. |

## Priorities, dependencies and task ledger

Every SW task starts **not started**. External implemented components above retain their delivered status. Update a task only with a code/evidence link and the relevant E2E result; a verification-only task can close without a code change when current behavior passes.

| Task | Priority | Deliverable | Depends on |
| --- | --- | --- | --- |
| SW-00 | P0 | Current capability, coverage and ownership baseline | None |
| SW-01 | P0 | Accurate per-chat effective tool/skill inventory | SW-00 |
| SW-02 | P0 | Scoped Claude MCP delivery in Ask/Auto | SW-00, SW-01 |
| SW-03 | P0 | Approved skill guidance/delivery in intended modes | SW-00, SW-01 |
| SW-04 | P0 | Moralis evidence-to-draft/action gate | SW-02, SW-03 |
| SW-05 | P0 | Uncovered chronological/identity contracts fixed | SW-00 |
| SW-06 | P0 | Native long-session/restart/steering certification | SW-05 |
| SW-07 | P1 | Complete visual artifact iteration | SW-01, IC-0–IC-3 |
| SW-08 | P1 | Exact browser/native target and evidence handoff | SW-01, SW-02, SW-09 |
| SW-09 | P1 | Readable, authorized visual tool results | SW-00, IC-0 shared storage/presentation contract |
| SW-10 | P1 | Supervised helpers and scoped stop/readback | SW-01, SW-06 |
| SW-11 | P1 | Skills/history migration and general-work setup | SW-03; IC-5 for in-conversation Find slice |
| SW-12 | P1 | Confirm shipped Board/automation fixes | SW-00 |
| SW-13 | P1 | Local recurring work and restart proof | SW-01, SW-04 |
| SW-14 | P2 | Additional connected-app journey | SW-02, SW-09, IC-4 |
| SW-15 | P1 release gate | Evidence-backed desktop switching decision | SW-04, SW-06–SW-13 |
| SW-16 | P2 conditional | Voice/phone/offline requirement decision | SW-00; selected existing remote milestones if required |

## Delivery sequence

1. **Establish facts:** SW-00, then effective inventory SW-01. Record mode/account/platform gaps and select the real workflows to replay.
2. **Remove P0 barriers:** SW-02/SW-03 → SW-04; independently SW-05 → SW-06. Dependencies describe scheduling, not permission to launch parallel agents.
3. **Complete the desktop workbench:** drive the existing IC phases for SW-07; deliver SW-09 → SW-08; complete SW-10/SW-11; verify SW-12/SW-13. Land small coherent milestones with primary-app handoff after meaningful code changes.
4. **Decide switching readiness:** SW-15 reviews all core evidence. A required blocked workflow keeps the migration blocked; a documented optional feature does not invalidate passed desktop work.
5. **Broaden according to need:** SW-14 and selected SW-16 work. If the user makes one essential to switching, promote it and add it to SW-15's required set before claiming completion.

No fixed duration is promised before SW-00 discovers real provider contracts. HTML/MCP Apps retain IC-6's own complete-package gate even if a subset satisfies a switching journey earlier.

## Boundaries

Preserve native provider permissions, machine/parent ceilings, existing credentials and Executor ownership. A missing tool never silently switches a chat to Full. Hooks, executable custom commands, app mentions and instruction text have different authority; native-only behavior must remain accurately labelled. Installed/configured/inspectable is different from callable in the active session.

Keep projectless convenience within the existing space/folder model initially. Do not reinstate cancelled Plan/Build, add PB-4 dependencies, build a plugin marketplace, clone business-service UIs, or introduce a cloud scheduler as incidental work. Live support replies remain drafts, and production mutations need their existing exact action preview and authorization. This plan authorizes documentation; it does not authorize sending support messages or mutating production during QA.
