# Sources and evidence boundaries

Access date: **2026-10-08**, Europe/Bucharest. Links below are official documentation or first-party announcements unless labeled otherwise. Living documentation may change after this date.

## Method

The research reviewed public product documentation, official dated announcements, current pricing information, six adjacent products, and selected LUNA source. Public sources establish documented capabilities and vendor claims. They do not prove performance, usability, or entitlement in a particular authenticated account.

No Devin, competitor, or GitHub account was connected or modified. No authenticated board flow, coding run, issue import, or benchmark was executed. No vendor screenshot is presented as a live observation. Search results helped locate sources; recommendations are grounded in the linked primary material. A marketplace extension named “Devin Task Board” was not treated as Cognition's own product.

## Devin product sources

| ID | Source | Used for |
| --- | --- | --- |
| D01 | [Agent Command Center](https://docs.devin.ai/desktop/agent-command-center) | Session management surface |
| D02 | [Spaces](https://docs.devin.ai/desktop/spaces) | Grouping and context |
| D03 | [Introducing Devin Desktop, 2026-06-02](https://cognition.com/blog/introducing-devin-desktop) | Product transition and positioning |
| D04 | [Desktop product page](https://devin.ai/desktop) | Distribution and upgrade path |
| D05 | [ACP](https://docs.devin.ai/desktop/acp) | External-agent support |
| D06 | [Ask Devin](https://docs.devin.ai/work-with-devin/ask-devin) | Scoping before execution |
| D07 | [Environment blueprints](https://docs.devin.ai/onboard-devin/environment/blueprints) | Repeatable setup |
| D08 | [Linear integration](https://docs.devin.ai/integrations/linear) | Tracker handoff and feedback |
| D09 | [Automations](https://docs.devin.ai/product-guides/automations) | Event-driven execution |
| D10 | [Testing and recordings](https://docs.devin.ai/work-with-devin/testing-and-recordings) | Review evidence |
| D11 | [Devin Review](https://docs.devin.ai/work-with-devin/devin-review) | Diff/review workflow |
| D12 | [DeepWiki](https://docs.devin.ai/work-with-devin/deepwiki) | Repository understanding |
| D13 | [Memory and Dreaming](https://docs.devin.ai/product-guides/memory) | Persistence and scope boundaries |
| D14 | [Session Insights](https://docs.devin.ai/product-guides/session-insights) | Execution feedback |
| D15 | [Managed Devins, 2026-03-19](https://cognition.com/blog/devin-can-now-manage-devins) | Parallel execution model |
| D16 | [When to use Devin](https://docs.devin.ai/essential-guidelines/when-to-use-devin) | Scoping and verification limits |
| D17 | [Devin 2.2, 2026-02-24](https://cognition.com/blog/introducing-devin-2-2) | Product evolution |
| D18 | [Self-serve plans](https://docs.devin.ai/admin/billing/self-serve) | Current plan structure |
| D19 | [Pricing](https://devin.ai/pricing) | Price and dated promotion cross-check |
| D20 | [SWE-2 announcement](https://cognition.com/blog/swe-2) | Recent release visibility; no benchmark conclusion |

## Growth evidence

| ID | Source | Classification |
| --- | --- | --- |
| G01 | [May 27 announcement](https://cognition.com/blog/series-d) | Company-reported figures |
| G02 | [September 8 announcement](https://cognition.com/blog/series-e) | Company-reported figures |
| G03 | [September 25 milestone](https://cognition.com/blog/1b-run-rate) | Company-reported figures |
| G04 | [The Information, August 27](https://www.theinformation.com/articles/inside-cognitions-booming-growth-high-cash-burn) | Secondary reporting; limited public excerpt |

Do not convert run rate into recognized revenue, customer count, profitability, market share, or board retention. No causal adoption study was found in the reviewed material.

## Comparison and integration references

| ID | Source | Used for |
| --- | --- | --- |
| C01 | [Vibe Kanban overview](https://www.vibekanban.com/docs) | Product scope |
| C02 | [Vibe Kanban issue management](https://www.vibekanban.com/docs/issue-management) | Issue/workspace relationship |
| C03 | [Vibe Kanban GitHub integration](https://www.vibekanban.com/docs/integrations/github-integration) | PR-integration boundary |
| C04 | [Emdash issue import](https://emdash.com/docs/issues) | Source-to-task interaction |
| C05 | [Linear agents](https://linear.app/docs/agents-in-linear) | Ownership and delegation |
| C06 | [Linear coding sessions](https://linear.app/docs/coding-sessions) | Integrated execution/review |
| C07 | [Copilot on GitHub](https://docs.github.com/en/copilot/concepts/copilot-surfaces/copilot-on-github) | Repository-agent continuity |
| C08 | [GitHub Projects](https://docs.github.com/en/issues/planning-and-tracking-with-projects/learning-about-projects/about-projects) | Board/source-state distinction |
| C09 | [Cursor Linear integration](https://cursor.com/docs/integrations/linear) | Routing and context |
| C10 | [GitLab epic boards](https://docs.gitlab.com/user/group/epics/epic_boards/) | Hierarchical planning |
| C11 | [GitLab development flow](https://docs.gitlab.com/user/duo_agent_platform/flows/foundational_flows/software_development/) | Bounded execution versus chat |
| C12 | [GitHub issues REST API](https://docs.github.com/en/rest/issues/issues#list-repository-issues) | PR filtering in issue results |

## Automatic review and native subagents

| ID | Source | Used for |
| --- | --- | --- |
| R01 | [Codex subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents) | Model/effort configuration; no verified Codex-to-Claude-Code native spawn claim |
| R02 | [Claude Code subagents](https://code.claude.com/docs/en/sub-agents) | Configurable models, fresh-context versus fork distinction |
| R03 | [OpenCode agents](https://opencode.ai/v2/docs/agents) | Per-agent provider/model selection and configured subagent inheritance |
| R04 | [MCP tool definitions and discovery](https://modelcontextprotocol.io/specification/draft/server/tools) | Named tools, schemas, discovery and structured results; preserve the installed harness protocol versions |

These official pages were opened on 2026-10-08. Model examples do not establish installed/account availability. The initial no-extra-chat review proposal has been superseded: reviewers now use temporary side chats and automatically close/archive after ticket success. Source inspection confirmed native event routing, cross-harness chat fields and same-conversation provider handoff. Further reads confirmed descendant-scoped archive/close tools and ownership-aware Stop receipts. Sibling reviewer cleanup requires explicit workflow ownership rather than loosening arbitrary sibling controls; the new integration was not executed against real providers.

## LUNA baseline

The source audit used the current checkout with HEAD `424e3e48ac230adbd608217b26e5980a3912ebf0`. It covered the root README and Cargo manifest; proto entities, identity, execution, attention, background work, task commands, project defaults, and context transfer; workspace persistence; the GitHub source-control layer and PR operations; and project/activity UI entry points. Exact source links are collected in the [reuse matrix](proposal.md#6-reuse-current-luna-foundations).

Unrelated UI changes and an existing untracked agent-workspace research folder were present before this task. They were not edited, staged, or reverted. Findings from UI files are treated as working-tree observations; this report does not certify a shipped UI baseline.

The existing [pending-task index](../README.md) and [workspace-app research](../agent-workspace-app-research/README.md) determined the documentation destination and complementary scope. This feature remains pending.

The user's follow-up on 2026-10-08 established sidebar placement and full UI/agent integration as requirements. Additional current source reads covered the engine's cross-task tool catalog and wire types, including model discovery, persistent child creation, result delivery, optional parent continuation, and caller-derived authority; and the sidebar page/navigation definitions. These source reads justify reusing the existing paths, not claiming board operations already exist. No child agent or application session was launched to perform this research.

Further user decisions unified fresh/context conversations under Side chat terminology, required ticket badges and conversion in place, kept Board AI drafting inside the draft, and limited side-chat ownership to one main-parent level. A side chat can create a fresh sibling under the same main or a distinct new main chat with a written handoff, but cannot fork context, create nested children, promote itself or reattach itself. Current source inspection of sidebar filtering, placement, fork creation, workbench rendering, child metadata and the persistent-fork design grounded the migration plan. Current nested-fork support is acknowledged as existing behavior that the new design deliberately restricts; no runtime behavior was changed here.

The latest requirement makes Backlog/Ready movement actionable: first start, shared Stop, and continuation in the existing chat. Reviewer results and ticket success precede exact reviewer close/archive. The updated mock, its editable source and an interaction manifest are saved in the plan. Browser checks cover local simulation only; launch/stop/archive receipts from real LUNA/provider executions remain required before implementation can be certified.

The subsequent authoring requirement separates a temporary project-aware conversation from ordinary chat storage, and requires manual harness/model/reasoning/access controls plus real internal board-tool delivery. Current reads of `runtime_contract.rs`, Codex's `luna_mcp.rs`, Claude's `luna_tools.rs`, and the engine tool catalog verified the existing private internal-MCP foundation. Temporary-purpose identity and actual board handlers remain planned. The mock uses local scripted conversation/configuration flows and does not establish real AI project access or MCP execution.

The user then resolved the remaining product choices: keep unfinished discussions only as Board drafts; use the implementation model for review when no reviewer is configured; stop after two repair/re-review rounds if blockers remain; and always retain a ticket worker's original parent. The last decision explicitly replaces the earlier idle-reattachment recommendation. These are direct user decisions, not inferred vendor behavior. Engineering choices remain within that confirmed contract.

## Verification boundary

Documentation validation covers local links/anchors, external-source inventory and whitespace. Mock source/export hashes and observed browser outcomes are stored alongside the saved design. The pending-task index links to the current plan, which includes drag-driven execution, temporary reviewer placement/cleanup and preserved chat identity. Proposed real-engine scenarios are in the [E2E plan](proposal.md#9-proposed-e2e-verification); they have not been run.

Before implementation, refresh current competitor docs and inspect the latest LUNA schema/runtime contracts. Before assessing Devin task quality, use representative authorized tasks with explicit success criteria, review time, cost, and retained artifacts; do not substitute vendor revenue or benchmark marketing for that evaluation.
