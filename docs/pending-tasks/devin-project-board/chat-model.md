# Chat identity, side chats, and board tickets

**Status:** product decisions confirmed on 2026-10-08; not implemented. Board-only drafts, one-level Side chats, fresh sibling creation, independent new-main handoff and no side-origin context forks remain required. Ticket working chats always retain their original parent, even while idle; UI and agents cannot reattach them. This replaces the earlier idle-transfer recommendation.

## 1. One conversation model, three independent properties

Use **chat** as the general term. A **main chat** appears as an independent sidebar entry. A **side chat** is created from another chat and normally opens inside that main chat's workbench. Retire the separate user-facing categories “child thread” and “side chat”; retain native forks and fresh-session mechanisms where needed internally.

Side chats have **one main parent and no side-chat children**. This is a creation/ownership constraint, not just a flattened drawing of a deeper hierarchy. All of a main chat's side chats are displayed in a flat group. A side chat may create a fresh sibling under that same main parent. Preserve the immediate creator as provenance, but never make the creating side chat the new chat's parent.

| Property | Meaning | Values |
| --- | --- | --- |
| Placement | Where the conversation is normally found | Main sidebar chat, or side chat in a parent's workbench |
| Starting context | What the new agent conversation receives | Without inherited conversation history, or with a copied context snapshot/native fork where supported |
| Board relationship | What work this conversation represents | No ticket, primary working chat for a ticket, or reference/source discussion |

A ticket association is independent of context inheritance and placement. A side chat working on a ticket may start with or without inherited context. A main sidebar chat may also work on a ticket. Avoid a mutually exclusive “fresh / fork / ticket” type: it would make “a ticket side chat with context” impossible to express cleanly.

**A main parent chat can itself be a ticket chat.** Independent Start from Board and conversion of an existing main chat both preserve main/sidebar identity. It may still coordinate side chats. The ticket badge does not create another category of conversation.

The latest [review contract](automatic-review.md) uses temporary reviewer side chats. A main ticket gets a reviewer beneath it; a side ticket gets a fresh reviewer sibling under its existing main parent. The reviewer reports to the ticket's workflow and is closed/archived after both review and ticket success. It does not become another ticket, a main sidebar chat or a nested side chat. This replaces the earlier invisible-job proposal.

“Without context” means no inherited transcript. The issue brief, selected references, project instructions, execution configuration, and explicit prompt still apply. “With context” means a snapshot from a specified point, not live shared memory; the result must identify whether it was a native fork or a visible-context copy. Preserve provider-specific capability and permission boundaries.

The fresh/context choice is available when a **main chat** creates a side chat. A **side chat cannot create a native fork or copy its transcript into another context-carrying conversation**. A concise, explicitly requested work handoff brief remains allowed; it is a new prompt, not a fork or inherited transcript. Enforce the restriction in menu actions, message-level Fork actions, agent tools, RPC and the engine, with no hidden parent-proxy workaround.

## 2. Confirmed creation and placement rules

**Saving a ticket creates no working chat and starts no implementation.** Where its first working chat belongs is decided when it starts, not when the ticket was saved or discussed.

| User action | Board result | Chat result and location |
| --- | --- | --- |
| Save a ticket from Board | An unstarted ticket | No working chat |
| Start an unstarted ticket from Board in a new session | Ticket linked to its working chat | New main chat in the sidebar |
| In a normal chat: “Create a ticket based on this; I'll start it later” | Saved ticket with discussion provenance | Existing discussion stays as it is; no extra chat |
| In a main chat: “Create a ticket and work on it in a new side chat” | Saved ticket and linked execution | One new side chat inside the main parent's workbench; no automatic sidebar root |
| “Convert this chat to a board ticket” | Create a ticket and link the current chat as its working chat | Same chat ID, history, provider binding, placement, draft, and active execution; no duplicate chat or replay |
| Main chat: “Work on these three unstarted tickets” | Link each selected ticket to its own execution | Three side chats under the requesting main chat, subject to dependencies, capacity and checkout constraints |
| Side chat: “Create another side chat for this work” | Optional selected ticket/brief | A fresh sibling under the same main parent, never a child of the calling side chat |
| Side chat: “Create a new main chat and pass the remaining work there” | Optional explicit new ticket or continuation link; no silent reassignment of an existing ticket | New independent sidebar root with a handoff brief, no inherited transcript and no child lifecycle ownership |
| Board → Go to chat | No state mutation | Navigate to the linked main chat, or open its parent workbench and select the exact side chat |

For the last case, one click must reveal the actual conversation, even if its tab is closed. Navigation reopens a view; it does not create a new agent session, resend the issue, reparent the chat, or move it into the sidebar. Launching several issues reports per-issue queued/started/reused/failed results instead of an ambiguous blanket success.

The saved ticket's source discussion is provenance, not its execution parent. A ticket saved in chat A and first started independently from Board becomes a main chat. If main chat B first starts the unstarted ticket, its worker is a side chat under B. On first working-chat creation or conversion, freeze that original parent/container. Later requests reuse it in place.

### Independent handoff from a side chat

**New main chat** must use the same standalone creation semantics as Cmd+N, with a real independent sidebar identity. It is not a child record merely promoted into the sidebar. Preserve an optional `Handed off from…` source link without setting child ownership or misclassifying that link as a context fork.

The new root must be a **different chat**, with a newly allocated identity. A side chat cannot turn itself into a main chat, create a new container parent for itself, attach itself beneath the new root, or move itself to another existing parent. Its current parent and identity remain unchanged. A sequence of tools or a prompt sent to the new root must not circumvent this caller restriction.

The handoff brief states the remaining goal, completed work, relevant files/artifacts, decisions, constraints and next step. It does not clone the complete transcript or hidden context. Return the new chat's link and preserve the source conversation unchanged. Creating the new root does not automatically archive/stop the source, wake the original parent, or grant stronger permissions. The new main chat may later create its own side chats under ordinary main-chat rules.

Distinguish **prepare a new chat with this brief** from **start working there**. Create/save alone does not submit an implementation turn; explicit “start/work on it there” can create and send through the existing durable launch path. Do not ask twice when execution is already requested.

Separate follow-up work can be handed to a distinct main chat, but continuing the same ticket uses its existing worker in the original location. Any explicit replacement attempt must remain in the ticket's original container and preserve earlier attempts; it is not a reparenting route. A separately requested follow-up ticket is independent, and checkout admission still prevents conflicting writes.

A new root does not inherit the original parent's right to stop, archive or receive results from it. Requested later follow-up is recorded explicitly. Permission modes and model settings are resolved through normal standalone launch controls; the handoff never upgrades privileges simply because the chat is now a root.

### AI drafting inside Board

Discuss with AI is a temporary, project-aware multi-turn conversation. It has no editable title/description fields and is never a main chat, side chat, settled chat or archived chat. The assistant builds a candidate internally, asks for execution settings and saves through real board tools. Manual title/description plus execution controls belong only to Write manually. See [ticket authoring](ticket-authoring.md) for lifecycle and retention details.

Use the existing project/harness infrastructure with a distinct temporary purpose and scoped identity. It can access permitted project context like a normal conversation, while remaining excluded from ordinary chat listings, Archive/Settled, history import and chat search. The discussion profile is separate from the future ticket's implementation/review configuration. Start creates the working chat from the saved brief/settings only when execution is requested. Unlike reviewer side chats, an authoring session is cleaned up without normal-chat archival.

## 3. Visible identity and badges

Give each local ticket a stable, compact human-readable key, for example `LUN-42` (illustrative format; final numbering scheme is a PB-0 decision). Never use the title as identity or confuse a local key with a GitHub issue number.

| Surface | Required presentation |
| --- | --- |
| Working chat header | Clickable ticket badge, such as `Ticket LUN-42`, plus a separate workflow-status control |
| Main sidebar row | Compact ticket icon/key for a standalone working chat; accessible label and tooltip identify the ticket |
| Side-chat tab and side-chat list | Same compact ticket badge; parent/workbench placement remains unchanged |
| Board card/detail | Working-chat link with location: `Main chat` or `Side chat in <parent title>`; unstarted tickets show Start |
| Source discussion | A linked creation/result receipt; optional `Related ticket` reference, not the primary-working-ticket badge |
| Activity/search/deep links | Stable ticket/chat association and navigation to the exact existing conversation |

Ordinary mentions or links to a ticket do not make a chat its working chat. A parent coordinating three tickets remains the coordinator; its three working side chats carry the three ticket badges. A details tooltip may show context origin, provider and host without crowding every tab with several colored labels. Do not encode meaning through color alone.

Recommended cardinality: one primary working ticket per chat and one active working chat per ticket. An issue can retain several historical attempts and source/reference chats. One coordinator can reference many issues. These constraints prevent an unrelated discussion from accidentally appearing to own every ticket it mentions.

## 4. Titles, models, and conversion

The ticket and its current primary working chat share the ticket title. Their IDs remain distinct. A rename through either primary surface uses one engine operation and updates both labels; provider auto-titling must not rename the working chat away from its ticket. Do not rename the source/parent discussion. Historical attempts may retain a recorded title with an attempt indicator, while preserving their ticket link.

When converting the current chat, propose its current title as the ticket title or use the title explicitly requested by the user. Save a brief distilled from agreed context without rewriting prior messages. Conversion changes the board relationship only; it does not send a first prompt again, fork the conversation, change its parent, or change model/settings. If the chat is running, record the exact task/binding/run association and derive current state honestly; conversion cannot mark existing work complete.

For a new ticket side chat, inherit the parent's agent configuration when no ticket-specific or explicit launch override is present. Preserve the earlier capability validation: provider/model/reasoning/options must be compatible on the execution host, and permissions cannot increase just because the conversation becomes a ticket. Start overrides and saved ticket choices take precedence over inherited settings. For an independent Board launch, use saved ticket choices then the selected project's compatible defaults. Freeze resolved settings in the launch receipt.

Creating a ticket from context, converting the current chat, and creating a new side chat are separate user intents. Make the tools and receipts unambiguous:

- **Save ticket:** persist a brief and source references; no working chat.
- **Convert this chat:** create/link the ticket to the caller's existing conversation.
- **Start ticket:** reuse/link or create its worker using an allowed destination and context mode for the caller's main/side role.
- **New side chat:** from a main chat, create fresh or with context; from a side chat, create a fresh sibling under the same main parent only. Either may work on a ticket.
- **New main chat:** create an independent sidebar root; from a side chat, hand off a written brief without inherited conversation history.

Ordinary language should map to those operations without requiring users to learn internal tool names. If a request genuinely does not distinguish conversion from starting elsewhere, ask that narrow question before creating another conversation.

## 5. Shared engine and navigation contract

Extend the proposed board operations with explicit relationship intent. Conceptual fields include `ticketId`, stable `chatId`, `sourceDiscussionId`, `workingChatId`, destination (`newMain` or `sideOf` a validated chat), context mode/source/checkpoint, and idempotency key. Exact schema names are not final.

The engine must distinguish:

1. Who created or discussed the work.
2. Which chat is performing it now.
3. Which main workbench displays that chat.
4. Which history/context snapshot it inherited.
5. Which active run owns execution controls and outcome delivery.

A model-supplied `parentId` is not authority to control another chat. Use caller-bound identity and existing scope checks. Return issue, working-chat and display-container IDs plus requested/resolved settings so the UI and agent can prove what happened.

Support multiple selected issues as a bounded batch of independently idempotent Start operations. A parent may request several side chats without authorizing unlimited retries, worktree creation, or simultaneous unsafe edits in one checkout. Preserve the earlier concurrency/budget rules. Board navigation must follow canonical relationships, not tab labels, title matching, or a stale remembered layout.

Shared creation must validate the main/side role from canonical relationships, not the currently focused workbench tab or a model-supplied claim. A side chat selected as the visible Main tab or promoted into the sidebar does not automatically become a genuine main parent. A true independent root is created explicitly, with separate source provenance. For side-origin sibling creation the engine resolves the existing main parent itself; the agent cannot supply a different parent or request inherited context.

### Role-aware tools and harness context

Publish a compact canonical context at startup, resume and context refresh: current chat ID, `main`/`side` role, main-parent ID when side, optional ticket ID and role, current display container, source/creator provenance, and explicit permitted creation/context/transfer actions. Use provider-delivered LUNA tools across all supported harnesses; do not require an agent to infer role from titles, a screenshot, or old transcript text.

For ticket workers, include their canonical original parent/container and an explicit parent-locked capability. No role receives permission to reparent them. Context handoff or independent new-chat creation must not be misread as authority to change that existing ticket relationship.

| Caller | Create side chat | Create independent main chat | Change a ticket chat's parent |
| --- | --- | --- | --- |
| Main chat | Fresh or with permitted context; new side belongs to this main | New independent identity when requested | Not allowed; link to/open/continue the original worker |
| Side chat | Fresh sibling only under its current main | Distinct new main plus a written brief; caller stays in place | Not allowed; no self-promotion or reattachment |
| User through UI | Choices valid for the selected main/side context | Ordinary Cmd+N or explicit handoff | Not allowed for ticket workers; preserve original parent |

Conceptual operations may be `create_side_chat` with a capability-checked context mode and `create_main_chat` with an optional handoff brief; these are proposed names, not live tool claims. Board Start must call the same creation/identity service. A side-origin ticket launch can create a sibling under its existing main or a distinct main worker if requested, but cannot turn the caller into either a new parent or a child of another parent.

Tool schemas/descriptions should show only applicable modes when feasible, while engine checks enforce them for all callers, including stale or manually formed requests. Return the actual new chat ID, role, main-parent ID or null, creator/source link, context mode, ticket link, launch state, and settings. Reject invalid `withContext`, arbitrary-parent and self-adoption requests with a clear allowed alternative.

Resolve effective models from the explicit/card choice or compatible requesting chat configuration; main-parent ownership must not silently replace a side creator's chosen model. Parent resolution also must not elevate sibling permissions: preserve the creating side chat's effective access ceiling and existing parent/grant constraints through the normal admission mechanism. Provenance is not a permission grant or automatic result-recipient choice. Default sibling result routing remains tied to the actual requesting conversation, with the main parent able to see the work; continuation still requires the user's requested follow-up policy.

A ticket link/conversion preserves its existing identity and placement. Board actions cannot subsequently change the ticket worker's original parent/container. Pins and navigation shortcuts must lead to that original chat without masquerading as adoption by another parent. Do not rewrite unrelated existing non-ticket placement behavior. Provider-native subagents remain distinct from LUNA side chats.

## 6. Current LUNA foundations and migration

Source inspection on 2026-10-08 found:

- [Sidebar filtering](../../../crates/ui/src/shell/spaces.rs) already treats both child metadata and fork provenance as parent-origin chats; they remain out of the main sidebar unless promoted.
- [Placement](../../../crates/ui/src/shell/placement.rs) separates sidebar promotion/docking from lineage. Docking a normal chat does not create a parent relationship.
- [Side-chat creation](../../../crates/ui/src/shell/side_chats.rs) currently means a fork or context snapshot. [Workbench rendering](../../../crates/ui/src/shell/workbench/render.rs) still presents “New thread or side chat” as distinct choices.
- [Child-thread records](../../../crates/proto/src/cross_task_tools.rs) retain durable startup, parent identity, execution permissions and result delivery. [TaskLineage](../../../crates/proto/src/task_identity.rs) already distinguishes parent, source and lifecycle ownership.
- [Workbench layout](../../../crates/proto/src/chat_workbench.rs) is explicitly presentation-only. The [persistent-fork design](../../design/persistent-fork-threads/README.md) states that source deletion must not cascade into surviving forks.

These are current source observations, not a fresh native UI test. The checkout contains unrelated changes.

Simplify vocabulary and creation while retaining fresh/fork launch machinery and durable IDs for permitted main-origin operations. The current persistent-fork design supports nested creation, so the new depth/context restriction is a deliberate behavior change; do not claim it already exists. For old nested records, preserve identities/history/native context and original provenance, stop further nested creation, and plan a non-destructive ownership/placement migration under the top-level main chat. Active descendants may require deferred migration after verified quiescence; never rewrite their control/result ownership mid-run just to flatten the UI.

Migrate additively. Do not recreate chats, rewrite provider IDs, replay prompts, drop permissions, or infer stop authority from a renamed category. Preserve saved workbenches, explicit placements, archived history and receipts. Update labels, command palette, accessibility, tools, help, notifications and errors together. Ordinary source handoff references must remain distinct from fork provenance so new independent chats do not disappear from the sidebar.

A closed/archived/deleted parent must not make surviving ticket work inaccessible. Preserve the original parent reference, including an unavailable/tombstoned reference, and allow direct opening from Board without rewriting it or creating a replacement parent. A recovery viewing surface is not a role/parent change. Do not restart, cancel or reparent the worker merely to recover navigation.

## 7. Acceptance scenarios

Add these to PB-1's native E2E coverage before claiming the integrated board is complete:

| Scenario | Required proof |
| --- | --- |
| Save from Board or normal chat | Ticket exists; zero working chats or implementation prompts created |
| Start from Board | One main sidebar chat, correct title/settings and ticket badge |
| Start from parent with/without history | One side chat, no duplicate sidebar root, correct context snapshot or fresh brief |
| Convert main chat and convert side chat | Original identity/history/run/draft/placement retained; exactly one ticket link |
| Work on three issues | Three distinct linked side chats; bounded admission; parent remains coordinator |
| Go to chat after closing tabs/restart | Correct main chat or parent-plus-side-chat selected without relaunch |
| Rename ticket or working chat | Both primary titles agree; parent/source title remains unchanged |
| Reference versus working badge | Source discussion does not acquire a misleading execution badge |
| Duplicate launch/conversion and old attempts | One primary association; no second prompt; old attempt cannot overwrite current state |
| Legacy child/fork migration | Context, provider bindings, permissions, delivery receipts, and placements survive |
| Missing/archived parent | Ticket remains navigable; no unrequested restart/cancel or lost evidence |
| Side-chat depth/context restriction | UI and real agent/RPC calls cannot create nested children or context forks, including from a promoted side chat or an alternate tab |
| Side → new main handoff | One independent sidebar root with a written brief, preserved source link, no inherited transcript/child ownership, and a single send only when requested |
| Side → sibling creation | New fresh chat has the same main parent, actual side creator provenance, bounded permissions and truthful result routing; no nested ownership |
| Side self-promotion/self-move attempts | UI/tool/RPC requests cannot make the caller a new root or attach it to a new/different parent, including indirect create-root-then-adopt sequences |
| Harness role context | Main and side tools receive correct allowed modes after restart/compaction; changing selected tab never changes canonical authority |
| New main creates side chats | New root can own its own fresh/context side chats; original parent has no implicit control |
| Legacy nested active work | No data loss or changed mid-run ownership; migration waits for safe conditions and retains provenance |

Retain screen recordings/accessibility readback and before/after ticket, task, lineage, launch and placement snapshots with hashes. Verify all supported provider tool paths. No runtime scenario above has been executed for this documentation change.

## 8. Existing ticket chat: reuse with a fixed parent

**Confirmed:** always keep the original parent. A side ticket remains under its original main chat; an independently started main ticket remains a main chat. Inactivity, Stop/Continue, another requester or a UI gesture cannot change that relationship. Reuse the existing worker instead of duplicating it.

| Current state | Another main chat requests work | Result |
| --- | --- | --- |
| No working chat yet | Create under the requesting main according to normal first-start rules | New side chat; original parent becomes fixed |
| Active or waiting | Reuse the current worker without moving or resending its initial prompt | Link to the original chat and actual status |
| Idle, stopped, failed with confirmed termination, or complete | Send an authorized continuation to the same worker | Existing history/configuration/parent retained |
| Host/runtime unknown | Reconcile before another run | No duplicate worker or timeout-based takeover |

Offer Go to chat and Continue where applicable. Do not offer Move here, idle reattachment, or deferred reattachment for ticket workers. “Work on this here” from another main chat continues the original worker and returns its link. An explicit conflicting destination returns the canonical location rather than moving or duplicating the chat.

This does not prevent reading, editing the brief, or explicitly requested queue/steer messages. Preserve the ticket/chat IDs, original parent, source/creator provenance, provider binding, checkout, outcomes and permission grants. Another requester does not silently change model, inject its entire history, redirect existing result delivery or gain parent ownership. Separately requested result subscriptions do not change the parent.

Record the original parent/container at first working-chat creation, or capture the existing canonical relationship during conversion. Enforce immutability through UI, tools, RPC and engine validation. Navigation resolves that relationship; a presentation field cannot be used to bypass it. Independent main tickets remain main tickets even if another parent asks to work on them.

Reparenting is excluded from this feature. Adding it later needs a new explicit product decision; it is not an implementation gap to fill with a transfer button. A deliberately requested replacement attempt stays in the same original container and retains the previous attempt as history.

### Fixed-parent acceptance

Verify second-parent requests reuse the worker without duplicate initial prompts; both idle and active reparent attempts are rejected; standalone main tickets stay standalone; Stop/Continue and restart retain the original parent; and unavailable-parent recovery remains navigable without reassignment. Preserve before/after relationship snapshots, launch counts, result-delivery receipts and native UI readback.

The user explicitly rejected the earlier idle-reattachment recommendation. This fixed-parent contract is authoritative.

## 9. Confirmed boundary

The user explicitly selected **both** side-origin creation capabilities: a fresh sibling under the same main parent, or a distinct new independent main chat. A side chat cannot fork context, own nested side chats, become a parent itself, create a replacement parent for itself, or reattach itself to a different parent. Main chats can create fresh/context side chats. No question remains about those boundaries.

Ticket workers keep their original parent regardless of which actor requests a move. The rule applies to UI/main-chat reattachment as well as side-chat self-moves.
