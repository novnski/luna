# LUNA project board and chat model: proposed feature contract

**Status:** product behavior/defaults confirmed; implementation not started. Engineering mechanisms may be refined within these contracts.

**Feature IDs:** PB-0 through PB-4, including PB-1R for temporary reviewer side chats and cleanup.

**Design basis:** [research](research.md); source inspection at `424e3e48ac230adbd608217b26e5980a3912ebf0` with unrelated working-tree changes present.

## 1. Product goal

From Board or an ordinary chat, the user can create an issue, start it with chosen implementation/reviewer settings, and receive an automatically checked/reviewed outcome in the same visible chat. The user intervenes for real blockers or chooses to inspect results; routine manual acceptance is not required.

**User requirements confirmed 2026-10-08:** Board is directly below Automations in the sidebar; normal chats can create saved issues and start them on request; manual and conversational authoring coexist; launches support model/reasoning selection; and the agent working on an issue can update its status. These requirements supersede the earlier project-only entry point and manual-only Start wording. They belong in PB-1. Detailed architecture below remains proposed.

The normal case is **one saved ticket, no working chat until Start, then one active working chat**. Additional attempts and related chats appear only when needed. Existing chat-first use stays available; users should not need to maintain a board to ask a question.

The [chat model](chat-model.md) defines main/side creation, fresh/context rules and ticket linkage. A main can create side chats; a side can create fresh siblings under its main or a distinct new main with a brief. Once a ticket gets a working chat, always preserve its original parent/container, even while idle. Other requesters open/continue it through a link. No UI or agent reattachment is allowed.

Success is lower coordination effort per accepted outcome. More cards, more concurrent agents, or more tokens consumed are not success metrics on their own.

The latest [review contract](automatic-review.md) and [board lifecycle](board-lifecycle.md) supersede earlier column/reviewer proposals. Backlog → Ready starts/resumes work, and Ready → Backlog uses shared Stop. Reviewers are temporary side chats: beneath a main ticket or beside a side ticket under its main parent. Successful review plus ticket completion triggers exact reviewer close/archive, with preserved evidence. Both main and side chats can be ticket workers.

The latest [authoring contract](ticket-authoring.md) makes Discuss with AI a temporary project-aware conversation outside every normal chat lifecycle. It contains no title/description editor. Manual creation has explicit execution controls; the discussion assistant obtains those choices conversationally through discoverable board/model tools. Temporary authoring sessions and reviewer side chats are different: reviewers are normal chats later archived, authoring sessions must never enter Archive or Settled.

## 2. User experience

### Sidebar entry point and project scope

Add a permanent **Board** page immediately below **Automations** in the main sidebar: New thread → Automations → Board. It is a peer navigation destination, not a Settings page or a section inside Automations. Keep it reachable when there are no automations, no issues, or no active chat. Add the normal route, selected-state handling, keyboard/command-palette navigation, and restart restoration.

Within Board, provide a project selector and Board/List switch. Opening it from a chat/project uses that project as the initial filter; an explicit All projects view aggregates accessible work without combining ownership or execution targets. With no active project, restore the last board scope or show All projects. Every issue still needs an explicit LUNA project and host before it can start; creating from All projects must resolve the project rather than guess. A project shortcut can open this same page prefiltered. Persist filters, ordering, selected card, and scroll position as presentation preferences.

The toolbar contains New issue, From GitHub issue, search, and filters for attention, provider, and source. A repository selector appears when a project has multiple possible remotes; do not assume a directory name identifies a repository. Show the selected GitHub account/host and source freshness where they affect issue selection.

Opening Board should not create tasks from every historical chat or import every issue. Offer **Add existing chat** and **From GitHub issue** as deliberate actions. The empty state explains those paths and allows a local task without GitHub access.

### Create a local card

1. In Write manually, enter title/description and select implementation harness/account/model, supported reasoning/options, effective access and reviewer profile. Show inherited project values explicitly. Acceptance criteria can be a description section rather than another mandatory form.
2. Save to Backlog. This stores a brief without creating a working chat or consuming provider usage for the save operation.
3. Press Start or ask an agent to start the issue when ready. The existing launch controls/tool receipt show effective provider/model/reasoning, permissions, host, project, and checkout. Do not introduce an extra confirmation if those choices are already supplied or inherited within the user's authorized scope.
4. First Start creates/links the worker, sets its ticket title and sends the agreed brief once. Independent Board starts create main chats; eligible parent/sibling starts create side chats. That original parent/container is then fixed. Existing workers are reused in place and never receive the initial prompt twice.
5. The card exposes the actual launch receipt and then the observed run state. Clicking it opens its detail drawer; Open chat navigates to the normal workbench. Agent-driven launches leave a linked receipt in the initiating chat without stealing its focus.

### Create through discussion

Offer Write manually and Discuss with AI as distinct experiences. AI authoring is a temporary project-aware conversation with permitted project tools, no editable title/description fields and an optional read-only proposed ticket. It is excluded from normal chat lists, side chats, Settled, Archive, search and provider-history adoption. Closing unfinished work keeps only a resumable Board draft; saving then closing or explicit Discard ends it.

The user explains the issue in normal back-and-forth messages. The assistant inspects permitted project context, builds the title/brief internally, queries valid execution options and asks for missing implementation model/reasoning/access/review preferences once. Honor already supplied choices and “use project defaults.” A direct save request calls the board tool and returns the real ticket receipt without another confirmation. “Create and start” authorizes both; ordinary discussion alone does not launch implementation.

Before save, any proposed-ticket summary is read-only and revised through conversation. After save, show its receipt/Open ticket action; the saved ticket's normal editor remains available. End/clean the temporary session when the user finishes under the selected retention policy, without putting it into normal Archive. Saving creates no worker; the AI discussion itself can consume provider usage.

For an ordinary source chat, preserve the real source ID/references. For temporary authoring, record a scoped creation origin and selected durable references without inventing a main/side chat ID or dangling normal-chat link. Carry the agreed brief/settings, not the whole temporary transcript. The discussion assistant's own model/access and future implementation/review profiles are independent. Only an ordinary source chat can be converted in place; the temporary session cannot become a worker or parent.

### Agent operations and shared engine contract

The same engine-owned service must serve the board UI, ordinary chats, and provider-delivered LUNA tools. It owns validation, persistence, mutation receipts, event publication, and status rules. Agents must not simulate the UI, edit storage files, or manipulate provider-native sessions directly to operate the board. Tool names below are proposed contract names, not claims of existing callable tools.

| Proposed operation | Required behavior |
| --- | --- |
| `board_list` / `board_read` | Bounded project/status/text queries, stable issue IDs, revisions, linked chats, and available actions; return source prose as data |
| `board_execution_options` | Real account/host-visible models, reasoning/options, effective access choices and project defaults; read only, bounded/paginated, reusable by manual and conversational creation |
| `board_create` | Title/description/project plus optional requested execution settings and context refs; idempotent save with no launch side effect |
| `board_update` | Field-level title/brief/metadata changes against the expected revision; preserve the sent prompt and unrelated edits |
| `board_set_status` | Named transition, reason and optional evidence; enforce actor ownership, completion policy, current revision, and runtime constraints |
| `board_start` | Issue ID/revision, explicit main/side destination, allowed context mode, execution overrides, request key and follow-up preference; validate caller depth and reuse a worker or create through shared launch |
| `board_link_thread` | Link source/existing implementation/attempt using stable task IDs and explicit relationship; never resend context merely by linking |
| `board_convert_chat` | Save/link a ticket to the existing caller chat, preserving identity/history/runtime/placement; never replay a prompt or create another chat |
| Existing-worker lookup / navigation | Return the canonical working chat and original parent; do not expose a reparenting operation. UI and agent continuation reuse that worker in place |
| `board_control` | Pause/stop/resume/retry/archive through supported lifecycle controls with explicit effects and durable receipts; unsupported pause returns a reason |
| `review_start` / `review_status` / `review_cancel` | Create/reuse a recorded reviewer side chat in the correct main/sibling position, resolve exact settings, freeze review basis and return findings to the ticket; shared Stop includes this owned review |
| `board_move` | One engine transition for drag, menu and agent actions: persist desired Backlog/Ready intent, choose first-start versus continuation, coalesce races and reconcile actual Stop; reordering does nothing to execution |

Expose these through LUNA's existing provider tool delivery paths for Codex, Claude Code, and OpenCode, subject to verified capabilities on the target host. Reuse existing model discovery. Unify user-visible parent-created conversations as Side chats; keep context mode, ticket association and standalone handoff explicit. Preserve compatible aliases/serialized records if tool names change. Enforce allowed destinations/context modes at the engine, not just by hiding UI actions. If unavailable, return the limitation rather than claiming success. GitHub remains an external source adapter.

Every mutation returns the issue ID, resulting revision/state, linked task ID when applicable, operation/launch receipt, and a navigable reference. Calls that time out remain reconcilable using the original key. UI and agents must observe the same canonical issue after restart and sync; repeated status updates are no-ops rather than duplicate activity.

The engine derives actor identity from the actual caller/run. A model cannot claim another task's ownership by passing an actor ID. The running issue thread has authority to report its progress, blockers, and readiness for review; an unrelated chat can manage the issue when the human asks it to. Reading an issue does not grant authority to change arbitrary other issues or launch their work. Use current LUNA permission modes and existing authorization; do not add an extra approval prompt to routine board operations already requested by the user.

Advertise and verify board tools through the actual internal runtime/MCP path for every supported ordinary main/side chat and resumed run. An internal prompt mentioning them is insufficient. Temporary authoring uses its own purpose/actor scope with project access and authoring tools; it cannot spoof a persistent main role. Extend the engine-owned catalog and current private `luna` endpoint delivery rather than introducing a duplicate external connector or changing user client registrations. The authoring contract records schemas, receipts, capability discovery and provider E2E requirements.

Inject compact canonical context into runs: stable chat ID and main/side role, main-parent ID when side, permitted creation/context/transfer actions, optional issue ID/role, project, brief revision, source refs, requested outcome and completion policy. Refresh after resume/handoff/compaction. Do not infer identity or authority from titles/layout. Agent-reported progress cannot overwrite engine-observed state. Parent resolution for a side-created sibling must not elevate permissions or overwrite creator provenance.

### Execution settings from UI or chat

Allow saved provider, model, reasoning, supported options such as service tier, execution host and checkout/branch preferences. New same-project ticket side chats inherit compatible parent configuration when no explicit/card choice exists. Independent Board starts use target-project defaults. Existing workers retain their configuration unless explicitly changed. A standalone handoff must resolve and show its settings without silently changing project or privileges.

Proposed new-chat precedence: explicit Start overrides → saved card overrides → compatible parent settings for a same-project side chat → target-project/provider defaults → advertised provider defaults. Validate the full combination on the target account. Provider changes invalidate incompatible options; unavailable choices produce an error or explicit alternative, never silent substitution. This planned inheritance differs from current fresh child tooling, which uses saved provider model/reasoning defaults when omitted.

Keep **requested**, **resolved for dispatch**, and **observed running** settings distinct. Freeze the resolved settings in the launch snapshot before queueing; changing defaults later must not alter an already accepted launch. A startup receipt proves admission/persistence, not that the provider has honored every option. Show unknown observations honestly.

Changing a card's settings during a run changes its next launch unless the user explicitly requests a supported live/next-turn transition. Provider changes, forks, and retries must follow existing LUNA task/binding rules. Model selection alone never increases permissions or grants credential access.

Keep implementation and review profiles separate, with exact harness/account/model and supported options. Project defaults avoid per-ticket setup. An unavailable reviewer or unobserved substitution cannot silently become another model. The reviewer runs in a temporary side chat with its own context/usage and returns results to the ticket. Its display parent and ticket workflow owner can differ.

### Whole-app behavior and result delivery

Add a clickable ticket badge and status control to the primary working chat header, with compact identity in sidebar rows and side-chat tabs/lists. Source discussions get reference/creation receipts, not a misleading working-ticket badge. Board → Go to chat opens the existing main chat or its main-parent workbench and exact side chat, without relaunch/promotion/reparenting. Search, Activity, notifications and command-palette navigation use shared identities/events and the same source/working/attempt distinctions.

The source chat can request “start it and report back” or “when it finishes, review the result.” Reuse durable result delivery and the existing opt-in parent continuation path. Without requested continuation, update the linked receipt/activity and leave the source chat idle. Reopening or restarting LUNA must not deliver the result twice or create an unsolicited new AI turn. Outcome delivery records success, failure, cancellation, or required intervention, not just successful completion.

Issues and implementation chats survive the source discussion being archived. Archive does not silently stop work or erase its provenance. A board status request does not automatically post a GitHub comment, close a remote issue, or change a recurring automation.

The ticket and current primary working chat share a canonical title. A rename through either surface updates both through the same engine operation; provider auto-titling cannot drift the working title away. Parent/source discussions and historical events keep their own labels. A local rename does not change a GitHub issue title; identity never depends on title matching.

### Card and detail design

Keep card content small: title, issue key if present, current execution/attention summary, provider/host when useful, PR/check indicator, and last meaningful activity. Show priority and labels only if the user uses them. Avoid exposing provider-native IDs or internal binding generations in the normal product surface.

The detail drawer has the editable brief, source issue, execution history, outcome/evidence, and clear actions. An active attention item opens the existing approval/question/error UI rather than a second implementation of it. A narrow window can switch to a list while preserving the same actions.

Provide keyboard equivalents for creating, opening, moving, starting, and filtering cards. Drag-and-drop is a convenience, not the only way to manage a task. Preserve focused card identity when a live update reorders the board; announce status changes accessibly without stealing focus.

### Start from a GitHub issue

1. Open From GitHub issue within the project. Resolve the repository and account through its owning host.
2. List/search issues with title, number, labels, assignees, state, update time, and whether a LUNA card already exists.
3. Open a preview: body, canonical source link, optional selected comments, and the editable initial prompt. Show any truncation or unavailable content.
4. Choose **Add to board** or **Start session**. Existing links offer **Open card** by default; a second attempt is a separate deliberate action.
5. Preserve a structured source reference and the exact context snapshot dispatched. Neither operation edits the GitHub issue in PB-2.

Example: issue `owner/repo#123` says “Search omits archived projects.” The user imports it, adds “Respect the Include archived toggle; retain current behavior when off,” and starts a chosen provider. When the agent finishes, the same card contains the original issue link, sent brief, chat, diff/PR, and verification evidence. The user can request changes without creating a second issue.

### Link an existing chat

**Convert this chat to a ticket** creates a ticket using its title and agreed brief, then links the same conversation as the primary worker. Preserve main/side placement, history, draft, active run and provider binding; send nothing again. A reference-only link is a different role and does not make the discussion a worker. Do not infer Done from idle/archived state or expose private history by default.

## 3. Work item, chat, and execution are separate concepts

### Recommended model

Use a small **WorkItem** record for the board and link it to existing LUNA task/chat identities. Usually there is one primary chat. This does not introduce another provider runtime or conversation store.

| Proposed record | Minimum responsibility |
| --- | --- |
| WorkItem | App-owned ID, project/host scope, title, editable brief, brief revision, requested execution settings, completion policy, workflow state, ordering, created/updated times |
| WorkItemTaskLink | Work item ID, stable chat ID, source/working/attempt role, link time, selected context refs and fixed original parent/container; keep source provenance and result subscriptions distinct from ownership |
| IssueReference | Provider and host, repository identity, issue identity/number, canonical URL, selected account reference, remote state/update time, last fetch time |
| LaunchSnapshot | Work item and brief revision, exact sent prompt/selected context, requested/resolved execution settings, launch request ID, linked task, effective target, result-delivery policy, receipt |
| OutcomeReference | Existing result, artifact, PR, or check reference with observed revision/time; later milestones can enrich this without copying all artifacts |
| WorkItemEvent | Revision, timestamp, actual actor/task/launch, operation, state transition or edited fields, reason and evidence refs; supports readback and duplicate-free activity |
| TicketAuthoringSession | Temporary purpose/actor ID, project/host, independent discussion profile, messages/candidate and requested execution settings; excluded from normal chat registry/projections/import, with scoped retention/cleanup |
| ReviewRun | Ticket/primary/reviewer chat IDs, display main parent, exact model profile, fixed basis, result/delivery/stop/close/archive receipts and cleanup state |
| BoardExecutionIntent | Desired Backlog/Ready, intent revision, execution generation, command/message IDs, first-prompt delivery state, launch snapshot and stop barrier; observed runtime is separate |

Names are provisional. Final persistence placement belongs in PB-0 after checking document migration and sync constraints. Do not confuse a new board WorkItem with the existing `TaskWorkItem`, which already means background execution work such as a child chat, shell process, or service.

### Alternatives and their tradeoffs

| Option | Benefit | Cost |
| --- | --- | --- |
| Board as a projection of existing chats only | Smallest UI prototype; little new persistence | No clean unstarted backlog; retries and external issue relationships become awkward |
| Work item plus links to existing chats **(recommended)** | Preserves an outcome across drafts, attempts, and imports | Requires a small persistent entity and launch/link transaction |
| Full tracker with epics, sprints, assignment, arbitrary workflows | Rich team planning | Much larger scope than the requested feature |

The column UI itself is bounded work. Durable launching, duplicate prevention, sync, and truthful completion make this a medium-sized cross-layer feature. A credible estimate requires PB-0; this research does not claim it is a one-day change.

### Description and prompt rules

- Before the first run, editing the description changes the draft that will be sent.
- At Start, freeze the exact brief revision and selected issue context into a launch snapshot.
- Editing the card after Start affects future work only. Offer **Send update to chat** through existing queue/steer behavior; do not modify the original user message.
- A remote issue edit updates the source preview and displays that it changed. It does not overwrite the local brief or inject a message into a running agent.
- Retrying can reuse the previous snapshot or explicitly adopt the latest brief. The choice is visible.
- Imported issue text/comments are source material. They cannot silently change execution permissions, select another project, authorize external messages, or start dependent work.

## 4. Workflow state and runtime state

Use Backlog, Ready and Done with execution phases and attention shown separately. Backlog/Ready express the user's execution intent; existing Idle/Starting/Running/Warm/Unavailable observations do not by themselves prove completion or termination. [Execution contract](../../../crates/proto/src/execution.rs).

| Workflow column | Meaning | Allowed automatic behavior |
| --- | --- | --- |
| Backlog | Work is held/stopped, or Stop is pending | Saving creates no worker; moving here cancels unsent launch or uses shared Stop; show Stopping/Unknown truthfully |
| Ready | Work is enabled to execute | Entering requests Start/Continue automatically; stay here through queueing, implementation, checks, review and repairs with phase badges |
| Done | Acceptance checks and configured review passed for the current result | Engine transitions from verified current job/check evidence; publishing/merging/remote closure have separate policies |

**Needs attention** is an overlay/filter: a pending question, approval, failure, auth problem, disconnected host, or compatibility block can happen in several columns. Display the actual reason and route to the existing engine-owned attention item. Acknowledging an alert is different from resolving its cause.

### Agent and human status control

The implementation agent reports progress/blockers and submits its result for automatic review. The reviewer returns findings; the engine schedules authorized bounded repairs and owns the final transition. Ordinary chats and UI share controls, but agent prose cannot bypass required check/review evidence. No manual drag is required.

Default completion policy is **Automatic checks and agent review**. Record criteria, reviewer profile and current result evidence. Optional Manual acceptance may exist for other project needs, but is not this user's default. Missing/stale review, skipped required checks, unavailable models and exhausted limits remain unresolved; they cannot be presented as successful completion.

Engine evidence wins over agent prose for runtime facts. A worker cannot declare itself stopped, erase a pending human approval, or force Done while blocking child work is active. A late update from an old attempt cannot overwrite a newer attempt's status. Return a revision/ownership conflict with current state, so the agent can adjust rather than blindly retry stale changes.

Transition rules:

- Dragging Backlog → Ready is an explicit Start/Continue instruction, with no second confirmation when settings/target are resolved. Ready → Backlog invokes the same Stop behavior as the chat. Within-column reordering has no execution effect.
- Failed/cancelled execution remains distinguishable from a successful outcome. Keep the card in its workflow with an attention/result badge and Retry/Continue choices.
- Reviewing means the recorded temporary reviewer side chat is checking the current result. Its verdict and required checks must match the current revision and Ready intent before Done.
- Done is unavailable while owned blocking work is active. Stop/pause uses the existing lifecycle operation first. A detached long-lived service is surfaced according to existing ownership rules rather than stopping every process blindly.
- Reopening a Done card into Backlog holds it; moving to Ready continues the existing chat with new result-bound review as needed. Preserve prior evidence as historical.
- Archive hides a local card and preserves links/history. It does not close an external issue or stop running work.
- Closing a PR without merge or closing an issue as duplicate is not proof the requested implementation succeeded.
- A future “mark done on merge” preference needs explicit semantics for required PRs, non-code tasks, pending children, and source freshness. Defer it from PB-1/PB-2.

For later feature groups, child completion produces a summary and any required integration checks/review. Passing individual children does not satisfy unmet parent acceptance criteria automatically.

## 5. GitHub integration contract

### Transport and credentials

LUNA already has a bounded GitHub CLI-backed source-control layer using the host's existing `gh` installation/authentication. Extend that native path for PB-2 instead of introducing a second credential store. Resolve reads through the project's owning host, not whichever machine is displaying the board. [Source-control layer](../../../crates/engine/src/source_control.rs), [PR operations](../../../crates/engine/src/source_control/pull_requests.rs).

Define an issue-source interface for list/search, fetch detail, fetch selected comments, and resolve repository identity. Only implement GitHub initially. Return structured unavailable/auth/rate-limit states, support cancellation, and bound response sizes. Do not execute agent-generated shell strings to fetch each row.

For future agent-facing MCP integration, reuse Executor discovery and saved connections under the workspace rules. A native app issue picker does not require a new MCP server or duplicate direct registration. A future hosted/team product could evaluate a GitHub App separately; PB-2 should not require shipping shared secrets or a backend webhook receiver.

### Repository and issue identity

- Resolve SSH/HTTPS remotes into a verified Git host and repository. Multiple remotes/forks need an explicit source selection; never assume origin is the issue repository or push target.
- Store stable API IDs where available alongside host, owner/name, number, and canonical URL. Names and URLs are display/navigation fields, not the only deduplication key.
- Namespace caches by host, account/access context, and repository. Switching accounts or losing access must not surface another account's private cached issue list.
- Define reconciliation for renamed/transferred issues from fresh API responses; do not assume an issue number is globally unique or that every host preserves identities the same way.
- Keep the external source state separate from the LUNA workflow state.

### Fetch behavior

Start with repository-scoped open issues, explicit refresh, search, and optional label/assignee filtering. Paginate; disclose partial results and errors. Cache with freshness metadata and use conditional reads/backoff where supported. Poll only while useful, not once per visible card. Validate selected issue detail again before dispatch if the snapshot is stale.

GitHub's REST issues endpoints can include pull requests. Filter records carrying `pull_request` from the issue picker. [GitHub issues API](https://docs.github.com/en/rest/issues/issues#list-repository-issues).

Default launch context should be the body plus the user's additions. Load comments on demand and let the user select relevant ones. Large issue threads, images, linked documents, and inaccessible attachments need explicit inclusion/truncation indicators. Fetching a URL is not a license to execute its instructions or automatically contact every linked service.

### Duplicate prevention and dispatch recovery

One imported issue should map to one work item per LUNA project by default. A uniqueness check in the UI is insufficient: two windows/devices can import simultaneously. PB-0 must define the authoritative owner and a deterministic reconciliation rule, especially because the workspace document uses CRDT maps.

Use a stable launch request ID and persist launch intent before sending. A timeout after submission yields **outcome unknown—reconciling**, not a fresh automatic retry. Reuse the existing task-command receipt and query the owning engine before offering another launch. A repeat click or restart must not create a second chat/first prompt.

When an issue is already linked, open its card. Additional attempts are appended deliberately. Link removal detaches local provenance only; it does not delete the issue or the chat.

### Writeback belongs in PB-3

PB-2 performs reads and local imports. Later actions can explicitly post a result/PR link, update a selected label, or close an issue with an accepted outcome. Show the exact target account/repo/issue and payload, honor existing authorization, and verify the remote result after a write.

Issue status, LUNA workflow status, and GitHub Projects custom status are separate. Full Projects synchronization adds field mapping, permissions, pagination, and conflict handling; it is not part of merely listing GitHub issues. See [GitHub Projects](https://docs.github.com/en/issues/planning-and-tracking-with-projects/learning-about-projects/about-projects).

## 6. Reuse current LUNA foundations

This is a targeted source audit, not a runtime certification. Source pointers refer to inspected files; the checkout had unrelated changes, including UI files. Recheck the baseline before implementation.

| Foundation | Current evidence | Proposed use / missing piece |
| --- | --- | --- |
| Project + chat records | [entities.rs](../../../crates/proto/src/entities.rs), `Space` and `Chat` | Existing project membership, title, branch, checkout, provider config; add work-item/brief metadata separately |
| Persistent workspace | [workspace.rs](../../../crates/doc/src/workspace.rs) | Existing Loro maps and owner/LWW discipline; design additive schema and launch/link ownership |
| Stable task identity | [task_identity.rs](../../../crates/proto/src/task_identity.rs) | Link to app-owned task IDs, not provider-native session IDs; retain binding and lineage semantics |
| Execution observations | [execution.rs](../../../crates/proto/src/execution.rs) | Derive runtime badges from owner observations, including incomplete/unavailable observations |
| Attention | [activity.rs](../../../crates/proto/src/activity.rs), [attention.rs](../../../crates/engine/src/attention.rs) | Reuse questions, approvals, failures, auth, and host availability; add board projection rather than a second lifecycle |
| Background work | [task_work.rs](../../../crates/proto/src/task_work.rs) | Prevent false completion with blocking/unknown child work; do not reuse this record as a ticket |
| Task command protocol | [task_command.rs](../../../crates/proto/src/task_command.rs) | Reuse command snapshots/events/receipts; verify its admission/recovery guarantees before extending launch |
| Context transfer | [context_transfer.rs](../../../crates/proto/src/context_transfer.rs) | Reuse explicit visible-text transfer boundaries for forks/provider handoff; avoid hidden automatic context copying |
| Agent launch/model/result tools | [cross_task_tools.rs](../../../crates/engine/src/cross_task_tools.rs), [wire types](../../../crates/proto/src/cross_task_tools.rs) | Existing `list_child_models`, `create_thread`, result reporting, and optional parent continuation; add board-aware operations over those paths |
| Fresh/fork identity and placement | [Chat model source audit](chat-model.md#6-current-luna-foundations-and-migration) | Preserve native IDs, context provenance, permissions and placements while unifying vocabulary and enforcing the new depth/context constraints |
| Sidebar destinations | [shell.rs](../../../crates/ui/src/shell.rs), `SidebarPage` and `SidebarNavItem::ALL` | Current working tree has New thread then Automations; add Board immediately after, with matching route/selection/restoration |
| Project launch defaults | [project_defaults.rs](../../../crates/proto/src/project_defaults.rs) | Seed provider/model/execution controls instead of parallel board-only settings |
| GitHub integration | [source_control.rs](../../../crates/engine/src/source_control.rs), [pull_requests.rs](../../../crates/engine/src/source_control/pull_requests.rs) | Existing remote parsing/auth/error patterns and PR workflow; add issue-specific API/model/cache operations |

Targeted searches did not find a dedicated Kanban board or structured GitHub issue entity in the inspected proto/engine/doc/UI code. That is a scoped finding, not proof no adjacent or in-progress implementation exists elsewhere.

The word **Space** already means a host-owned project folder in LUNA's model. Do not rename it to match Devin's feature grouping. If grouping is added later, use a distinct user-facing feature/group concept and preserve existing project identity.

## 7. Milestones and acceptance boundaries

| ID | Scope | Exit criterion | Deliberately deferred |
| --- | --- | --- | --- |
| PB-0 | State/data/tool contract, unified Side chat/ticket model, failure inventory | Engineer reviews draft-only save, creation, conversion, badges, fixed-parent reuse, independent handoff and migration before code | Unrelated scope; no extra product sign-off for confirmed decisions |
| PB-1 | Board below Automations, project filtering, Board-local drafting, UI/agent controls, ticket badges, main/context/fresh-side creation, conversion, independent handoff, bounded multi-ticket requests and safe reuse | Zero workers on save; correct main/side location and title/badge; conversion preserves identity; Go to chat finds the worker; side chats cannot own children or fork context; migration preserves existing work | GitHub, team assignment, arbitrary workflows, live transfer of active runs |
| PB-1R | Reviewer side chats, selected provider/model, bounded repair, automatic Done and cleanup | Correct main/sibling placement, result return to ticket, shared Stop and exact successful reviewer close/archive with preserved history | Unrestricted worker delegation, automatic deployment/merge |
| PB-2 | GitHub issue picker, preview, source reference, context snapshots, duplicate prevention, auth/freshness handling, basic blocked-by relationships | Representative live issue read → deliberate import/start → correct linked session; restarts/retries preserve identity; dependencies remain visible and do not trigger work | Issue writes, Projects sync, webhooks, automatic dependency dispatch |
| PB-3 | Result summary, diff/PR/check/evidence links, review/rework, explicit issue writeback | A reviewer can inspect evidence, request changes, and publish an authorized update with verified remote readback | Automatic merge/close policies |
| PB-4 | Optional feature groups, more trackers, templates, dependency-aware queues/automation | Each addition independently demonstrates demand and bounded execution semantics | Unbounded swarms and a full project-management replacement |

A prototype can show derived activity, but does not certify live ticket execution. PB-1 also requires the board-lifecycle contract for first start, Stop, continuation and races. The saved mock demonstrates the current design locally; real PB-1/PB-1R/PB-2 engine/provider acceptance remains separate.

Reuse current worktree choices. This feature must not silently create worktrees or enable parallel writers in one checkout. For this repository specifically, the build lock and single-GUI-app rule remain in force regardless of how many cards are running.

## 8. Failure cases to specify before implementation

These are requirements for future implementation and E2E fixtures; no new unit tests are proposed.

| Risk | Required behavior / evidence |
| --- | --- |
| Double-click Start; timeout after acceptance | One launch identity, one initial prompt; durable receipt reconciles uncertainty |
| Crash between saving card and linking chat | Restart recovers a valid link or a clearly retryable unstarted card; no orphaned running task |
| Two devices import the same issue | One canonical card or an explicit reconciled duplicate; no duplicate dispatch |
| Edit brief while a run is active | Sent snapshot stays immutable; new text remains draft until explicitly sent |
| Issue changes or closes during preview | Stale/source-changed state appears; user can inspect current details without silently changing the prompt |
| Branch/repository changes before Start | Re-resolve effective target and prevent dispatch to an unintended checkout |
| Private repo auth expires or account switches | Correct scoped error; no cross-account cached data exposure |
| Rate limit, timeout, partial pagination | Existing local cards remain usable; visible freshness/partial state and bounded retry |
| PR appears in issue API results | Excluded from issue picker unless a future explicit PR mode is selected |
| Large body/comments/attachments | Bounded inclusion with a visible summary of omitted content |
| Host disconnects; stale run observation | Unknown/unavailable state; never infer completion from missing updates |
| Provider switches or native session IDs change | Work item still links to stable LUNA task identity; capability-specific actions remain truthful |
| Parent turn ends while child work continues | Card does not imply accepted completion; active blocking work remains visible |
| Agent claims success but checks/review fail | Continue permitted bounded repair or surface Needs attention; never infer Done from the claim |
| Non-code task returns a document | Can be reviewed/completed without a PR; evidence is appropriate to the requested outcome |
| Drag between Backlog/Ready during execution | Explicit move triggers shared Start/Continue/Stop; repeated/reordered moves do not duplicate commands; external issue state stays separate |
| Malicious issue text asks for secrets/new permissions | Retain as source content; no elevation of authority or unauthorized sends |
| External write succeeds but response is lost | Reconcile existing result before retry; do not duplicate public comments |
| Concurrent rank/status edits | Deterministic merge or explicit conflict; preserve card identity and user focus |
| Older client reads upgraded workspace | Additive fields/capability negotiation preserve old chats and do not drop board data |
| Agent creates an issue from a discussion, then retries | One saved card, source link and receipt; no implementation run until Start is requested |
| Create-and-start partly succeeds | Retain the saved card and reconcile the original launch request; no lost issue or duplicate worker |
| Requested model/reasoning unavailable for target account | No silent fallback; preserve requested settings and return supported choices |
| Late worker or concurrent human changes status | Validate revision and active attempt ownership; prevent stale completion overwriting new work |
| Source chat closes before work finishes | Issue/worker survive; completion receipt delivered once and continuation follows saved user intent |
| Compaction/handoff loses board context | Restore the stable issue link and applicable completion policy without duplicating prompts |
| Provider omits board tool delivery | UI reflects unavailable capability; tool receipt never falsely claims an operation occurred |
| Sidebar page switch changes project unexpectedly | Preserve explicit filter and target identity; Board remains directly below Automations |

## 9. Proposed E2E verification

Use the project's existing native QA infrastructure. Write scenario failures before any isolated subsystem implementation. Prefer real end-to-end operations and retain a reproducible artifact; do not append implementation-mirroring unit tests after writing code. [QA infrastructure](../../reference/qa-infrastructure.md).

1. **Backlog → Ready → Stop → Continue → Done.** Save with no worker, drag to Ready for one initial prompt, drag back for shared Stop, then drag again for continuation in the same chat. Complete checks/review, close/archive exact reviewers and retain receipts/recording.
2. **Intervention and recovery.** Exercise a real question/approval, a provider failure, disconnected host, and active child work. Confirm board and normal chat agree, and that Done is not inferred from silence. Save attention/runtime snapshots and native UI readback.
3. **Duplicate and restart protection.** Repeat Start/import concurrently and restart at recorded failure boundaries. Prove one accepted first prompt and one canonical issue link. Save launch IDs, event counts, persisted state, and recovery output.
4. **Live GitHub read.** Use a deliberately chosen authorized repository with representative issues: labels, comments, Unicode, an empty body, closed state, PR entries, and pagination. Compare API fields to UI and dispatched context before blaming rendering. Save sanitized response fixtures, selected prompt hash, and issue/card IDs. No write is required for PB-2.
5. **Ownership and freshness.** Switch projects/accounts, change the source issue between preview and Start, and exercise unavailable access. Verify cache scope and that the execution target cannot silently change. Save source timestamps, effective target, and visible errors.
6. **Review and writeback, PB-3 only.** Produce a PR/check/evidence result, request a revision, then perform the explicitly authorized issue update. Read it back independently and verify no duplicate comment on retry. Save exact reviewed payload, receipt, canonical URL, and readback.
7. **Native usability and compatibility.** Run keyboard-only create/open/move/start, narrow-width list mode, focus-preserving live updates, and reopen after restart. Include a workspace created by the previous app version. Save recording, accessibility/readback evidence, and before/after document snapshots.
8. **Normal-chat authoring and launch.** Discuss an issue, ask the agent to save it for later, verify title/description/source link and zero implementation launches, then ask it to start with a catalog-valid model/reasoning selection. Verify requested/resolved/observed settings, one real task, one prompt, and no focus theft. Repeat via the Board's Discuss with AI path and manual form.
9. **Agent status and shared state.** Report blockers, enter Checking/Reviewing/Fixing phases and complete from verified current evidence. Confirm Board, chat, Activity and readback agree. Exercise automatic completion, optional Manual acceptance and a rejected stale attempt/result. Save actor/revision/job events.
10. **Provider and continuation parity.** Prove advertised board operations through each supported provider's real LUNA tool delivery path. Test unsupported options, compaction/resume context, source-chat archival, failure/cancellation return, and once-only requested parent continuation. Save model catalogs, receipts, readbacks and result-delivery records; do not claim a provider passed from schema inspection alone.
11. **Sidebar placement and scope.** Verify Board immediately follows Automations, including empty states and no active chat. Exercise project-filtered entry, All projects, command palette, chat backlinks, restart route restoration, and rejection of an ambiguous execution target. Save native UI/accessibility evidence.
12. **Dependencies and admission limits.** Link blocked-by issues, reject dependency cycles, and verify completing a prerequisite does not automatically launch dependent work. Fill the configured execution capacity or budget and confirm a visible bounded queue/refusal; cancel and retry across restart without duplicate dispatch. Preserve dependency snapshots, admission reasons, launch identities, and control receipts. Automatic dependency dispatch needs its own PB-4 verification before release.
13. **Brief revisions and external status.** Edit an issue during execution and confirm the original launch prompt remains unchanged; explicitly send the revision through the existing chat delivery path. Mark the local issue Done and independently verify the GitHub issue is unchanged. In PB-3, perform only an explicitly authorized remote update and verify it separately. Preserve before/after brief revisions, sent snapshots, delivery receipts, and remote readback.
14. **Chat identity, fixed parent and independent handoff.** Exercise main/side creation, temporary drafting, conversion, badges, context restrictions, batch requests, title sync and migration. Verify active and idle reparent attempts fail, original-parent navigation survives restart/unavailability, and an independent handoff creates a separate main chat without adopting the caller. Preserve identities, placements, prompt counts and result receipts.
15. **Temporary reviewer chats across providers.** Execute PB-1R for main and side tickets; verify correct reviewer parent, model readback, findings/repair, fixed diff basis, Stop and result delivery. After success verify exact archived IDs and closed views, with primary/unrelated chats preserved.
16. **Drag admission and continuation races.** Execute the board-lifecycle scenarios: rapid moves, two windows, unsent launch cancellation, unknown first delivery, Stop-pending restart, stale review completion and reordered cards. Verify one initial send, explicit continuations and no concurrent duplicate runs.
17. **Temporary authoring and real board tools.** Execute the authoring contract: chat-only UI, actual project reads, conversational configuration, manual model/reasoning/access selection, one-ticket creation, temporary-session exclusion/cleanup, model/access separation, and successful tool calls from ordinary and resumed Codex/Claude Code/OpenCode chats. Preserve real wire/identity/configuration evidence; mock replies do not certify this integration.

Each completed E2E run should preserve a scenario manifest containing app commit/version, selected provider/host, fixture setup and cleanup, exact steps, expected/observed results, command exit codes, artifact paths, and SHA-256 hashes. Redact credentials and private content before sharing. Record skipped scenarios and why; a mocked GitHub response is not proof of live account access.

During later implementation, run the required focused compile, lint/format, and E2E checks with `scripts/cargo.sh --locked` conventions, respect build-budget admission, then perform the normal signed LUNA Dev refresh and process/port/signature handoff. This documentation-only research requires no app build or restart.

## 10. Measure usefulness before expanding

Proposed measures: time from selected issue to first useful run; proportion of results accepted after review; number of manual context-copy steps; time spent awaiting input; duplicate-launch incidents; and time to find the next actionable task. Collect only the minimum local data needed for evaluation unless the user opts into broader telemetry.

### Required lifecycle scope

The user explicitly confirmed all six items below on 2026-10-08. They are required scope in this plan, not optional suggestions. Delivery remains phased; automatic dependency-driven execution can follow the basic dependency relationship later.

| Requirement | Required behavior and acceptance | Delivery boundary |
| --- | --- | --- |
| Completion criteria | Current checks and selected reviewer-side-chat result lead to automatic Done; then close/archive exact idle reviewers and preserve evidence. | Identity/drag PB-1; review/cleanup PB-1R; rich presentation PB-3; E2E 1, 9, 15 and 16 |
| Original discussion and result return | Retain source discussion, implementation chats, attempts, and results. “Start this and report back here” delivers once and resumes the source agent only when requested, including after restart/archival. | Core PB-1; E2E 8 and 10 |
| Dependencies | Support visible blocked-by relationships, detect cycles, and preserve links across restart. Clearing a dependency does not itself authorize or launch work. | Basic relationships PB-2; automatic scheduling PB-4; E2E 12 |
| Recovery and execution limits | Distinguish stop, supported pause/resume, cancel, retry, and archive. Bound explicit single/multi-ticket requests by concurrency/budget, show queue reasons, and recover without duplicate launches or unlimited retries. | Bounded explicit admission/recovery PB-1; automatic dependency scheduling PB-4; E2E 2, 3, 12 and 14 |
| Versioned context | Keep the current description separate from the exact prompt already sent. Editing a running issue offers an explicit update to its agent; retain original discussion, sent snapshots, revisions, and outcome history. | Core PB-1; E2E 13 |
| Local versus GitHub status | Local Done never silently closes or comments on GitHub. Import/link and remote writes are separate actions, with an explicit target/payload and independent remote readback for authorized writes. | PB-2 read; PB-3 writeback; E2E 6 and 13 |

Feature groups remain an additional PB-4 proposal: group multiple issues under shared acceptance criteria without automatically accepting the parent when children finish. They are separate from the six confirmed requirements above.

PB-0 implementation decisions include persistence/migration, deduplication, atomic title/link writes, authoring storage and evidence semantics. Product choices are confirmed in the README: Board-only draft retention, same-implementation-model reviewer fallback, at most two repair rounds, and immutable original ticket parent. Do not reopen them as optional scope questions.
