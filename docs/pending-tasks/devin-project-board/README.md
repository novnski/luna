# Project board, ticket chats, and unified Side chats

**Status:** PB-0 through PB-3, including PB-1R, implemented on 2026-10-09. Only PB-4 (feature groups, more trackers, dependency-driven automation) remains open. The engineering contract is [pb0-contract.md](../../design/project-board/pb0-contract.md).

**Verification (2026-10-09):**

- **Engine E2E:** `crates/engine/tests/board_*_e2e.rs` cover store, lifecycle, chat model, Codex/Claude/OpenCode tools, review, GitHub and authoring.
- **Headless UI fixtures:** `crates/ui/examples/board-*-fixture.rs`.
- **Native QA:** run on a signed LUNA QA Board app with real providers.
  - Codex implemented two tickets and Claude Code Haiku reviewed them, both in a main and in a side-ticket placement; the second ticket was stopped and then continued in the same chat.
  - An agent created and started tickets through LUNA tools.
  - Discuss with AI saved a ticket and kept, resumed and discarded a draft.
  - An existing chat was converted to a ticket.
- **Not run live:** GitHub issues (no authorized repository was chosen; covered by the fake-`gh` E2E and fixture).

**Researched:** 2026-10-08, Europe/Bucharest.

**Scope:** Devin, six related products, and a source-grounded proposal for LUNA.

## Confirmed defaults

The user resolved the remaining product choices on 2026-10-08. These decisions override earlier recommendations in the research and should not be reopened during implementation.

| Decision | Confirmed behavior |
| --- | --- |
| Unfinished AI discussion | Keep only as a resumable Board draft. Never add it to sidebar chats, side chats, Settled or Archive. Discard removes it; successful save plus close ends it. |
| No configured reviewer | Use the implementation model in a separate reviewer side chat. Project/ticket review overrides remain available; reviewer access stays constrained to review. |
| Unresolved review findings | At most two repair rounds, each followed by another review, then Needs attention. The initial review is separate, so at most three review passes occur in this cycle. |
| Ticket chat parent | Always keep its original parent, including when idle. Another chat opens/continues the existing worker through a link; neither UI nor agents reattach it. An independently started main ticket stays a main chat. |

No product clarification is outstanding for the current scope: PB-0 through PB-3, including PB-1R. PB-4's explicitly future ideas remain deferred. Storage, identifiers, API shape and race handling are implementation decisions constrained by the specifications. [Claude Code handoff prompt](handoff.md).

**Recommendation:** add an app-wide **Board** entry immediately below **Automations** in the sidebar, with project-scoped work. A card describes an outcome, Start opens a normal LUNA chat, and the card keeps the issue, execution, review, and evidence together. Manual controls and agents use the same engine operations. Preserve LUNA's provider choice and local-first behavior.

## Required app integration

The user clarified these product requirements on 2026-10-08; they are part of the first release, not optional future enhancements:

- A normal chat can create a board issue from a discussion and leave it unstarted for later.
- The user can ask an agent to start a saved issue with an explicit provider, model, reasoning level, and other supported execution settings.
- The agent working on the linked issue can update its workflow status and report progress or blockers through native LUNA tools.
- Manual creation exposes title/description plus implementation harness/model, reasoning, access and review settings. Discuss with AI is a separate temporary, project-aware conversation with no editable title/description fields; the assistant collects settings and creates the ticket through internal tools.
- The main **Board** navigation item sits directly below **Automations**. Project context filters this shared surface; it is not confined to project-only navigation.

The implementation must connect board, chat, search, activity, launch controls, persistence/sync, and agent tools. A UI-only board does not satisfy this feature. See the [agent operation contract](proposal.md#agent-operations-and-shared-engine-contract).

The user also confirmed all six lifecycle requirements: completion criteria, links/result delivery back to the original discussion, dependencies, recovery and execution limits, versioned context, and separate local/GitHub status. They are required plan scope with phased delivery and acceptance checks in the [lifecycle requirements](proposal.md#required-lifecycle-scope).

## Chat and ticket identity

- Saving a ticket creates no working chat. AI authoring uses a temporary Board-only session, never an entry in sidebar, side chats, Settled, Archive or normal chat search/import.
- Starting independently from Board creates a main sidebar chat; starting from a main parent creates a side chat in that parent's workbench.
- Use **Side chat** for parent-created conversations, with a fresh/context choice at the main parent. Side chats cannot own other side chats or create context forks.
- A side chat can create a fresh sibling under the same main parent, or hand work to a distinct new main sidebar chat using a written brief. It cannot promote itself, create a parent for itself, or reattach itself to another parent.
- Ticket linkage is a visible badge on either a main or side working chat. The ticket and current working chat share a title.
- **Convert this chat to a ticket** links the same chat in place. **Create a ticket and work elsewhere** creates a separate working chat only when requested.
- Existing ticket work reuses the same chat and always keeps its original parent/location contract. Requests from another main chat link to the existing worker without reattachment.

The [chat model](chat-model.md) defines badges, navigation, handoff, conversion, migration and role-aware harness tools. The creation boundaries are confirmed; no clarification remains on them. It supersedes earlier wording about independent ticket/chat titles or automatic scoping chats in the sidebar.

## Automatic review and cleanup

Use **Backlog → Ready → Done**. Moving to Ready starts or continues work; moving back to Backlog uses the chat's shared Stop operation. Cards stay in Ready while queued, working, checking, reviewing or fixing, with their actual phase shown separately. The next move to Ready continues the same chat instead of replaying the original prompt.

Reviewers are temporary side chats using the configured provider/model. For a main ticket, its reviewer is a side chat under that ticket. For a side ticket, its reviewer is a sibling under the same main parent. After both review and ticket success, persist results, close reviewer views and archive the exact idle reviewers. Keep the primary chat and linked review history. This replaces the previous invisible-review-job proposal. See [automatic review](automatic-review.md) and [drag/start/stop/resume](board-lifecycle.md).

The most useful idea in Devin is the continuity between scoping a task, running an agent, seeing what needs attention, and reviewing the result. Its Agent Command Center is the visible management surface for that workflow. [Devin Desktop announcement](https://cognition.com/blog/introducing-devin-desktop), [Agent Command Center](https://docs.devin.ai/desktop/agent-command-center).

The proposed title-to-chat and description-to-prompt interaction is a good first experience. Keep one distinction underneath: **the work item describes what should be achieved; the chat records work toward it.** A work item can exist before execution and survive a failed run, a retry, or several related chats. This avoids turning every new attempt into a duplicate ticket.

## Read the detail

1. [Research and comparison](research.md): what Devin does well, evidence of growth, six alternatives, and what LUNA should borrow.
2. [LUNA feature proposal](proposal.md): UX, state transitions, GitHub integration, current foundations, delivery milestones, failure cases, and E2E acceptance.
3. [Sources and evidence limits](sources.md): official references, dated business claims, local source baseline, and what was not tested.
4. [Chat and ticket model](chat-model.md): Side chat terminology, depth limit, ticket badges, conversion, independent handoff and reuse with a fixed original parent.
5. [Automatic review](automatic-review.md): reviewer-side-chat placement, selected model, bounded repairs, result return and automatic close/archive after success.
6. [Board execution lifecycle](board-lifecycle.md): drag-to-start/stop, first versus continuation prompts, rapid moves, restart and ownership.
7. [Saved interactive mock](mockup/README.md): runnable design, editable source, reproduction steps and interaction evidence.
8. [Ticket authoring and internal tools](ticket-authoring.md): temporary project conversations, manual execution settings, authoring instructions and real board-tool delivery.

## Proposed first release

| User action | Result |
| --- | --- |
| Open a project's Board | See saved tasks and their execution/review state |
| New issue | Enter a title and description; save without starting an agent |
| Discuss with AI / ask in a normal chat | Capture the agreed problem as a board issue with a link back to the discussion |
| Start | Review effective project/provider/checkout context and send the description as the first prompt of a linked chat |
| Ask an agent to start a saved issue | Launch through the same path using requested, validated model/reasoning settings |
| Drag Backlog → Ready | Start once, or continue the existing chat with a resume instruction |
| Drag Ready → Backlog | Use the same Stop path as the chat; retain conversation and partial work |
| Agent updates its issue | Persist progress/status with provenance and refresh the board and chat together |
| Convert this chat to a ticket | Preserve its conversation and placement; attach the ticket and badge |
| Main chat works on selected tickets | Create/reuse ticket side chats with bounded admission and per-ticket results |
| From GitHub issue | Search the selected repository, preview an issue, and create a linked card or start it |
| Open a running card | Resume the actual chat, including its existing questions and approvals |
| Agent completes implementation | Run checks and the configured reviewer, fix findings within limits, and mark Done after the current result passes |
| Inspect a result | Optionally expand changes, checks, reviewer findings and artifacts inside the same chat |

Current columns: **Backlog → Ready → Done**, with separate observed phase and Needs attention indicators. Ready means enabled to run. Backlog can temporarily show Stopping until termination is verified; the column is not proof the agent has stopped. Review is automatic, and successful cleanup is verified independently.

Vibe Kanban is the closest reference for issue descriptions becoming prompts. Emdash is a close reference for importing tracker issues into a task. Linear demonstrates keeping human ownership while delegating execution. [Vibe Kanban](https://www.vibekanban.com/docs/issue-management), [Emdash](https://emdash.com/docs/issues), [Linear](https://linear.app/docs/agents-in-linear).

## Delivery order

- **PB-0:** settle the data and state contracts with a small UX prototype.
- **PB-1:** Board, draft-only ticket creation, unified Side chats, badges, conversion, main/side launch, independent handoff, shared UI/agent controls, bounded multi-ticket requests, and reuse with a fixed original parent.
- **PB-1R:** selected-provider reviewer side chats, revision-bound verification, bounded repairs, shared Stop, and verified close/archive after ticket success.
- **PB-2:** GitHub issue discovery, context preview, durable linking, duplicate prevention, and basic blocked-by relationships; automatic dependency dispatch comes later.
- **PB-3:** review summaries, result evidence, and explicit GitHub writeback.
- **PB-4:** optional feature groups, additional trackers, and bounded automation.

The integration requirements above are explicit user requirements. Architecture, milestone details, and additional suggestions remain proposals for later implementation. A future in-app request to create/start an issue is actionable without redundant confirmation within its authorized scope; merely discussing work does not launch it. This documentation task itself has not launched agents or changed runtime code, third-party configuration, or issues.

The scope complements the existing [agent workspace research](../agent-workspace-app-research/README.md), especially attention clarity, project context, review continuity, and feature groups. It does not replace that backlog.
