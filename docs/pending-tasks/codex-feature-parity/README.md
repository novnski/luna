# Codex feature parity in LUNA

**Status:** Pending implementation. Research completed on 2026-09-30.

LUNA should expose the useful capabilities of the Codex harness it embeds, including correct tool activity, native steering, installed plugin/app selection, and the common coding workflows. The review used four subagents across Codex, T3 Code, BB, and LUNA, followed by focused investigations of tool output, steering, and plugin mentions.

The underlying integration is already substantial: LUNA uses the real `codex app-server`, with native sessions, resume/import/fork, steering, compaction, model/reasoning/service-tier selection, approvals, structured user questions, usage, child agents, file changes, and viewed/generated images. The main gaps are structured prompt inputs, lossless activity/output handling, accurate delivery state, and a few missing user-facing workflows. T3 and BB provide useful implementation examples but are not complete parity references.

This document and its supporting audits preserve research for future implementation. They do not claim that the proposed changes or the new E2E scenarios have been implemented or run.

## User requirements

- Preserve Codex's ordinary steering behavior: accept a follow-up while work continues, then incorporate it at the native completion boundary. Do not implement steering by interrupting and restarting the turn.
- Support the common Read/Search/path exploration, Write/Edit/Patch, Run, Web, ViewImage, generated-image, MCP, and subagent activity paths, including their results, failure states, and reopened history.
- Allow users to select installed Codex plugins and connected apps in the composer, similar to the Codex picker shown during the review. Installation and management can remain in the CLI or Codex app; a plugin manager is outside this feature.
- Preserve native plugin/app/skill identity and image inputs through drafts, queued messages, steering, remote dispatch, and account selection.
- Prioritise useful everyday capabilities. Internal maintenance, terminal themes, and unrelated experimental endpoints do not define the product backlog.
- Keep research and planning in ordinary chat, including requests to write a plan file. The explicit Plan/Build feature was cancelled by the user on 2026-09-30; do not implement or reintroduce it.
- Use meaningful E2E verification with retained, repeatable evidence. Follow the repository's build, one-app, data-preservation, and handoff instructions when implementing.

## Evidence baseline and limits

| Source | Inspected revision | Purpose |
| --- | --- | --- |
| Codex | `ceea67163f93b4317c9352d1cd2af9fee32537b9` | Native app-server contract, engine behavior, feature gates, recent history |
| T3 Code | `8792f95762092ea33e74d41a07e8818befd84970` | Reference integration and Plan/rewind workflows |
| BB | `f1172452bb0ca60c042ce884b3f8b98112fe377d` | Reference tool timelines, retained output, permissions and dynamic tools |
| LUNA | `e71d90caad0798ca6dcec28da4b352d8f8c39c0c` | Harness → engine → document → UI trace |

LUNA advanced to `2a50e9b4def45854499622ac504795e3d046c74c` through an unrelated documentation-only commit during the review. Runtime source remained unchanged. Source links in the supporting audits are pinned to the original inspected revisions; recheck them when implementation starts.

The installed CLI was `codex-cli 0.159.0`. Its stable and experimental JSON schemas were generated successfully. A read-only feature listing reported `apps`, `plugins`, and `goals` enabled, and `instant_interrupt` disabled. These are the local CLI's effective values, not proof of every LUNA account/project overlay or account entitlement.

The review traced code and existing documented QA. It did not run new model calls, native GUI checks, builds, or timing tests. The September 25 real-provider steering exercise is historical successful-path evidence, not complete verification of 0.159 or the failure cases below. BB's checkout was shallow, limiting claims about older BB history.

## Supporting research

| Audit | Detail |
| --- | --- |
| [Native Codex capabilities](codex-source-audit.md) | API inventory, inherited/TUI/experimental boundaries, dates and version comparison |
| [T3 integration](t3-integration-audit.md) | Runtime overrides, accounts, Plan, rewind, questions and event projection |
| [BB integration](bb-integration-audit.md) | Dynamic tools, permissions, questions, account routing and lifecycle |
| [LUNA coverage](luna-coverage-audit.md) | Existing support and ranked gaps across harness, engine, protocol and UI |
| [Native steering semantics](native-steering-audit.md) | Acceptance, pending input, tool boundaries, instant interrupt and correlation |
| [LUNA tools and steering](luna-tools-steering-audit.md) | Protocol → journal → document → transcript and delivery failure paths |
| [Plugin/app/skill mentions](plugin-mentions-research.md) | Native input shapes, installed catalogs, cache scope, metadata and icons |
| [BB/T3 tool and plugin comparison](competitor-tools-plugins-audit.md) | Streaming, full-output retrieval, rich results, badges and competitor limits |

The original local evidence bundle is under `Documents/Codex/2026-09-30/luna-codex-parity-review/`. It contains the eight original reports, their SHA-256 hashes, repository snapshots, installed method inventory, exported schemas, selected feature output, and `review-manifest.json`. Raw schemas and logs are deliberately outside this source documentation. The linked research above contains the implementation-relevant conclusions independently of that local bundle.

## Current capability matrix

| Capability | LUNA | Reference or limit |
| --- | --- | --- |
| Native session start/resume/import/fork | Supported | Provider IDs, managed continuity and checkpoint forks exist |
| Native model/reasoning/service-tier choices | Supported | Live paginated model catalog drives the Traits picker |
| Ordinary steering | Native path supported | Acceptance is currently labelled applied too early; default follow-ups use Queue |
| Queued follow-ups | Supported through LUNA's durable queue | Different from Codex's experimental editable thread queue |
| Read/Search/list-files | Supported typed activity | Multiple or unrecognised command actions remain Run |
| Write/Edit/Patch | Supported per-file chips and diff references | Large diffs are truncated before journalling; progress events are ignored |
| Command result/error/output | Supported at completion | No live stdout/stdin activity; received output is capped before journal storage |
| Web search/open/find | Supported | Expandable text/JSON rather than dedicated result cards |
| Viewed/generated output images | Supported | Some inline results without a saved path are unsupported |
| Native image prompt input | Missing | Attached image paths are currently sent as text |
| MCP lifecycle | Supported | Only text result blocks survive; richer content and detail are lost |
| Generic MCP elicitation | Missing | Only the narrow computer-use form path is accepted |
| LUNA dynamic tool execution | Supported | Most cross-task call/result activity is hidden in the transcript |
| Native plugin/app selection | Missing | Current `@` picker searches files, and Codex input is text-only |
| Explicit native skill selection | Partial | Slash picker inserts text rather than a guaranteed native Skill item |
| Native Plan/Build workflow | Dropped by user | Ordinary chat, plan files and existing todo activity remain; no added mode switch or replacement slash command |
| Native Review workflow | Missing | No `review/start` dispatch or review-mode item mapping |
| Native same-thread conversation revert | Missing | LUNA has provider forks and local file checkpoint restore instead |
| Goal engine | Potentially inherited | Dedicated controls/status are missing; continuation integration needs E2E verification |
| Rich permission decisions | Partial | Core approvals work; context, scopes and policy amendments are simplified |
| Mermaid in chat | Missing | Files already has a native Mermaid renderer that chat does not connect |

## Recommended implementation sequence

1. **Native composer inputs:** unify image, plugin/app and explicit skill selection, retain their identity durably, and support ordinary send, queue, and native steer.
2. **Tool activity fidelity:** retain complete received results on the host, add live command activity, expose dynamic tools, and preserve rich MCP/unknown item information.
3. **Steering delivery semantics:** retain native scheduling, separate acceptance from later recording/incorporation, correlate messages, and verify error/Stop/reconnect races.
4. **Common workflows:** implement native Review, then general MCP prompts, conversation revert, Mermaid chat rendering, and goal controls/readback in focused features. Explicit Plan/Build modes are dropped.

Generic MCP prompts may need to move earlier when testing selected plugins: an installed integration that requires a form or URL prompt cannot complete reliably through LUNA's current narrow handler. Native Review can be a separate milestone rather than a prerequisite for the input/output work.

## Tool activity and result preservation

Codex sends item lifecycle and additional output/progress notifications. LUNA normalises lifecycle items to AgentEvent, journals the normalised events on the owning host, folds safe metadata into the document, and renders grouped transcript chips. Lazy FetchToolBlob retrieves retained details from the host journal. This design already provides real durable tool history and avoids copying every output into the synced transcript.

Preserve these existing behaviors:

- A single native command action classified as read, search or list-files becomes Read/Search/Glob. Composite shell commands or unknown actions remain Run.
- Do not relabel a shell command that writes a file as Write merely from its text. Native fileChange add/update/delete provides the truthful Write/Edit/Patch path.
- Multi-file patches fan out to stable per-file activity identities. Add events carry raw new-file content; updates can carry unified patches.
- Tool completion must resolve the original item even if a steer has split the assistant entry.
- Viewed/generated images are imported into profile-owned storage and rendered after the asset import is validated.
- Failure state, invocation labels, expandable detail, diff statistics and reopened history must survive.

### Concrete losses to address

1. Command aggregatedOutput is cut at 64 KiB before AgentEvent and journal storage. The lazy output control retrieves only the retained portion. Diff/web/MCP text also has normaliser limits. Move preview limits after durable host retention, or explicitly communicate a retained-output limit; do not present truncated content as lossless full output.
2. Stable command outputDelta and terminalInteraction notifications are ignored. File-change output/progress is also ignored. Add activity without repeatedly syncing or repainting the entire transcript.
3. MCP projection copies only text blocks from result.content. Preserve typed image/audio/resource blocks, structuredContent, useful error detail and app UI resource metadata. Full interactive MCP Apps rendering can be a later milestone; retaining a useful typed fallback is still necessary.
4. LUNA's list_threads/read_thread/send_message_to_thread/wait_threads dynamic calls execute, but only luna_status currently gets dynamic-tool lifecycle events. Show call/result status and a bounded useful receipt while retaining existing argument sanitisation.
5. Unknown items and notifications currently disappear. Current examples include hookPrompt, functionCallOutput, sleep, plan and review transitions. Map relevant types explicitly and retain a bounded diagnostic/fallback for unknown types so a CLI update does not silently remove activity.
6. Web results are usable but raw. Optional source-card/link presentation improves browsing detail; it must remain distinct from browser/computer-use MCP activity.

Sources and exact line references: [LUNA tools and steering](luna-tools-steering-audit.md), [competitor tool comparison](competitor-tools-plugins-audit.md), and [native item contract](codex-source-audit.md). BB's streamed command and lazy-output implementation is useful here; neither BB nor T3 is a complete rich-MCP rendering reference.

## Steering contract

The required behavior is Codex's native turn/steer contract, not an application-level interrupt/resume approximation.

1. The client sends threadId, the exact active expectedTurnId, input and optionally clientUserMessageId.
2. Codex atomically validates the active regular turn and appends pending input.
3. The matching RPC reply confirms acceptance only. Hooks, user-message recording, history update and the next model request can occur later.
4. With default instant_interrupt disabled, the current response continues and all already-launched in-flight tools drain before the next request incorporates pending input.
5. Several accepted steers preserve input order and can be incorporated together. There is no fixed one-or-two-tools rule; the boundary is completion, not necessarily tool success.

LUNA already uses turn/steer with expectedTurnId for explicit steering, without calling turn/interrupt. Its late-result ownership handling preserves the original chips. Keep both.

### Delivery states and correlation

LUNA currently emits Steered immediately on a matching RPC reply, creates the user bubble, and settles the queued row/command receipt. Its queue model already describes an accepted-but-pending state, but that state is retired at acknowledgement.

Use distinct meanings for accepted, recorded/incorporated, rejected and unknown delivery. A provider userMessage with the submitted clientId can confirm recording; it still does not prove successful model consumption. A terminal failure or Stop between acceptance and recording needs truthful status and a recoverable message.

LUNA has a stable SteerMessage.message_id but does not forward Codex's optional clientUserMessageId. It also ignores parent userMessage items. Carry the ID and reconcile later item notifications/readback.

**clientUserMessageId is correlation, not idempotency.** Codex appends accepted input without suppressing a duplicate ID. Never blindly replay an unknown-outcome request assuming that the same ID makes it safe.

### Queue, Steer, Send now and Stop

- Follow-ups default to Queue, including the busy composer path. This is intentionally different from native mid-turn steering.
- Explicit Steer requires an active turn, rejects attachments, and does not automatically start a substitute turn on rejected/unknown delivery.
- Ordinary warm-session delivery may fall back to a later turn on a correlated rejection. Orphan recovery for unknown ordinary delivery favours availability but can duplicate an input whose acknowledgement was lost. Review this separately from explicit steering.
- Send now and Stop are separate interrupting actions. Preserve clear labels and do not make them interchangeable with Steer.
- Native turn/interrupt cancels the turn and clears pending in-turn input. It is not a promise to kill every detached background process.

### Faster steering and terminal states

Codex 0.159's instant_interrupt is opt-in and under development. It can preempt an unfinished model response or skip tool calls not yet emitted, but already-launched local tool work still drains. Long code-mode observations can yield without killing their live cell. LUNA inherits feature configuration on ordinary runs; do not turn this flag on merely to implement the default behavior.

The current v2 wire reports turn/completed with turn.status completed, failed or interrupted. Core TurnAborted is an internal event, not a separate v2 notification. An additional static edge case exists in LUNA: interrupted without its local interrupt token can be reported Completed. The future verification pass should check externally interrupted turns and use the authoritative native status.

See [native steering](native-steering-audit.md) and [LUNA steering](luna-tools-steering-audit.md) for exact source paths, race handling and historical successful-path evidence.

## Installed plugin/app/skill selection

The user wants to use integrations already installed or connected through Codex. Catalog discovery and invocation are in scope; installing, uninstalling, managing marketplaces or changing credentials are not required.

### Catalogs and native IDs

Plugin bundles and connector apps are distinct. Names and logos in the reviewed Desktop screenshot cannot establish the exact backing ID, because that Desktop picker is not part of the public source inspected here.

| Entry | Catalog | Native input |
| --- | --- | --- |
| Installed plugin bundle | plugin/installed with selected cwds; filtered plugin/list fallback | Mention with name and path plugin:// plus PluginSummary.id |
| Callable connected app | app/installed with threadId when available; batched app/read metadata | Mention with name and path app:// plus InstalledApp.id |
| Explicit skill | skills/list for effective workspace roots | Skill with native name and path |

Prefer installed-only discovery without suggestions. A wider plugin/list includes uninstalled marketplace rows, so eligibility must be explicit: installed, enabled and allowed by effective policy. App callable status may depend on the loaded thread. Stable IDs can be opaque; do not manufacture IDs from a display name.

Codex uses a plugin mention to resolve its available skill/app/MCP capabilities, load required MCP servers for the turn and inject capability instructions. App mentions select explicit app capabilities. A display label alone cannot provide this behavior.

Current LUNA @ searches files and inserts file links. Its slash command catalog derives Codex skills but inserts /skillName as ordinary text. Codex's explicit skill selector supports a native Skill item or recognised skill syntax; the slash text alone does not guarantee injection. Current turn and steer input is text-only.

### Smallest durable input path

- Add a host-routed read-only catalog using the selected execution account, effective CODEX_HOME, cwd/worktree and thread context. The existing Settings inventory is local-only and is not a sufficient remote composer endpoint as it stands.
- Reuse the existing composer popup behavior, with separate file/plugin/app/skill identity and descriptions/icons. Preserve the actual native kind, ID/path and label in edit-safe chips.
- Persist selections in draft and queued-message contracts. Generate the native typed input at dispatch for turn/start and turn/steer, alongside ordinary user text and staged localImage input.
- Keep identities through save/restart, queue reorder/edit, account changes and remote dispatch. Deduplicate identical selections without deriving identity from labels.
- When a selected entry becomes unavailable, preserve the draft and explain that state. Do not silently dispatch it under another account or replace it with a similarly named entry.
- Metadata errors, older-CLI method errors, policy-disabled entries and an actually empty catalog must remain distinguishable.

### Cache and icon behavior

Cache scope should include host device, selected provider account/effective home, canonical cwd and native CLI version. Include thread identity or effective thread-policy state for callable apps. A cache keyed only by the Codex harness name can leak stale or incorrect choices across projects/accounts.

A short freshness interval, initially around five minutes, is a proposal rather than an observed native guarantee. Return fresh cached rows promptly; refresh on opening after expiry and support explicit retry/refresh. Invalidate on account/home/project/worktree changes, CLI update, reconnect and relevant app/account/thread-settings notifications. There is no general plugin-list-updated notification, so external CLI/app installs need bounded TTL and explicit refresh.

Keep the last successful rows with visible stale state during temporary failures. Do not turn an offline or failed read into No plugins. Cache metadata rather than tokens, auth files, tool payloads or whole plugin packages.

Plugin interface metadata and app/read provide names, descriptions and light/dark icons. Local icon paths belong to the Codex host, not the device displaying a remote thread. Load icons lazily, use a fallback on failure, and use host-owned asset retrieval for local-only icons when required.

Older CLI fallbacks must be capability-tested. Actual installed catalog reads for the user's auth mode, disabled-policy behavior and remote local-icon retrieval were not exercised during this source review.

Full evidence and extension points: [plugin mention research](plugin-mentions-research.md).

## Other valuable workflows

### Native Plan/Build feature — dropped

The user cancelled this feature on 2026-09-30 before it reached main. Keep the existing ordinary chat workflow: users can ask the model for a deep dive, research, or a plan written to a file. Do not add Plan/Build switches, mode-specific proposal actions, or a replacement `/plan` command as part of this backlog. The supporting source audits remain historical reference material, not an active implementation request.

### Native Review

Native review/start supports working changes, branch comparison, a commit and custom instructions, with inline or detached delivery. LUNA lacks the dispatch and entered/exited review item mapping. A Git diff panel or asking for a review in prose is a different affordance. Neither T3 nor BB exposes the native reviewer in the inspected integration.

### MCP prompts and permission detail

LUNA accepts MCP elicitation only for a narrowly recognised computer-use form and otherwise declines form/URL requests. Add generic schema-aware forms and URL-flow responses with exact server/turn binding. T3's form auto-fill is limited and URL prompts are declined; BB's native request decoder does not implement these prompts.

Command/file/permission approvals already work. LUNA's native permissions handler grants the requested object once or denies it, with JSON/reason presentation. Structured permission categories, turn/session choices, network context, proposed rules and richer command context deserve a focused fidelity pass. BB provides useful scope handling but also loses some macOS grants and policy amendments. Do not describe basic approvals as missing.

### Conversation revert, goals and diagrams

thread/revert changes provider conversation history without reverting local files. LUNA currently offers provider forks and separate Git checkpoint restore; a same-thread rewind would add a distinct workflow. T3 demonstrates a native revert combined with separately optional file restore. Keep those choices explicit.

Goals are stable/default-on in Codex, and stock app-server supplies goal tools plus automatic idle continuation for eligible persistent threads. LUNA may already inherit agent-initiated goals from an explicit prompt. It lacks first-class goal controls/status/readback, and its parked warm-session behavior needs an E2E check for early follow-on output and history checkpoints. Do not claim that the goal engine is absent.

Files already renders Mermaid with the native adapter. Chat passes no diagram media handler, so diagrams remain source fences. Connecting the existing renderer to chat is a useful display feature, separate from harness semantics.

Background-terminal visibility/control and live turn model/effort/tier updates are later candidates. Their native APIs exist but are experimental. Memory can be inherited when configured; lack of a management UI is not proof that memory, hooks, plugins or apps do not execute.

Attach Chrome, Record a skill and Sketch in the screenshot are product affordances, not proof of a public one-call CLI API. A sketch can use the native image-input path; browser attachment and skill recording require their actual context/recording integration to be traced before copying those controls.

## Reference implementations and recent Codex changes

T3 adds managed sign-in/CLI acquisition, shared-home account continuation, injected browser/device MCP servers, custom application instructions, and Plan/rewind UX. BB adds dynamic plugin tools, skill-root discovery, checkpoint-based message editing, account routing and rich operation timelines. LUNA already adds its own runtime contract, dynamic cross-task tools, account-scoped execution and durable document/queue behavior. The important differences are in integration and projection, not a replacement for Codex's model/tool loop.

T3's Plan support is optional and disabled by default. BB's Plan entry is textual rather than native collaborationMode. BB disables native user questions and has an optional replacement AskUserQuestion plugin that is off by default. Neither currently has a native Codex plugin/app mention picker, native Review, goal controls or full rich-MCP projection. Generated protocol types in either repository do not establish working UI support.

Codex rust-v0.159.0 includes opt-in instant interruption, item-anchored history pagination and TUI Mermaid rendering. Main was 92 commits ahead during the audit, but its method map and turn/model wire shapes were unchanged; the small OAuth protocol addition was optional loginId for correlating concurrent logins. New model catalog defaults and the TUI MCP login command did not introduce new app-server methods.

Useful older additions include native conversation revert and editable queue in August, live turn settings in August, durable thread attachments in September and image fileId input references in September. The detailed native audit records the dates and commit IDs. Most proposed work is already-shipped parity, not waiting for a future CLI.

## E2E acceptance and evidence

Use representative real Codex runs for common behavior, plus a deterministic app-server/MCP fixture for rare failure frames and rich result types. Write plausible isolated failure modes before implementing fixtures. Use the repository's Cargo wrapper with --locked and the documented native QA identity/one-app process when a compiled app is required.

Each milestone should retain CLI version, selected feature state, source SHA, fixture inputs, native protocol payloads or redacted thread/read, LUNA WatchDocMessages/WatchQueue/command receipts, owning-host journal/blob readback, and relevant screenshots. Save hashes and reproduction commands. A screenshot alone cannot prove delivery or retained bytes.

| Scenario | Required result |
| --- | --- |
| Read/search/list and shell-created file | Truthful typed actions; composite/shell writes remain Run unless native fileChange exists |
| Add/edit/delete patch | Correct paths, content/diff representation, stable per-file IDs and failure state |
| Large command/diff result | Distinctive head/tail beyond 64 KiB survives host retention and lazy retrieval; preview can remain bounded |
| Live command and stdin | Progress/interaction appears before completion without duplicate final output or excessive transcript updates |
| Web search/open/find | Correct action labels, retained results and inspectable source URLs |
| Native image input and output | Image-only and text-plus-image prompts receive real visual content; viewed/generated assets reopen correctly |
| MCP mixed result | Text plus image/resource/structured content/error remains useful; unknown content has a bounded fallback |
| LUNA dynamic tools | Authorised execution has visible call/result lifecycle with sanitised details and a durable receipt |
| Held sequential/parallel tools plus two steers | Native default does not abort launched tools; tool results occur once, steer order survives, same turn continues |
| ACK then terminal failure or Stop | Accepted remains distinct from recorded/incorporated; no false applied status or blind replay |
| Stale turn, wrong reply ID, lost reply | Explicit steering fails/unknown safely without silently creating another turn |
| Ordinary delivery recovery | Availability behavior and duplicate-risk policy are explicit and reconciled |
| Explicit interrupt or external interruption | Current v2 turn/completed status interrupted is respected; Stop remains different from Steer |
| Plugin/app/skill selection | Exact native identities reach both turn/start and turn/steer; capability use and skill injection can be read back |
| Draft/queue/restart/remote account scope | Selected identities survive and dispatch on the owning host/account; unavailable selections remain recoverable |
| Catalog refresh and collision | Same names with different IDs remain distinct; account/cwd/policy changes and external installs invalidate appropriately |
| Native Plan and Review | Structured progress, proposals, refine/implement and review output survive reopen |
| Goal continuation | Idle follow-on turns, early output, Stop/budget states and checkpoints remain visible and durable |

Use turn/completed with status failed or interrupted in current v2 fixtures. Do not invent turn/failed or turn/aborted wire methods. Correlation through userMessage.clientId verifies recording, not duplicate suppression or guaranteed model consumption.

The existing [September 25 chat-ordering QA](../../qa/chat-ordering-comparison-2026-09-25.md) provides a real sleep/steer/queued-follow-up exercise and RPC readback examples. Its statement about unavailable full output predates later lazy-journal work; the current normaliser's retained-output cap is the relevant limit. See [LUNA tools/steering](luna-tools-steering-audit.md) and [native steering](native-steering-audit.md) for the expanded source-derived scenarios.

## Before implementation starts

Recheck the installed CLI schema and current LUNA source, then select one bounded milestone. Build from a capability/input/event coverage matrix rather than endpoint counts or generated types alone. Treat unsupported experimental capabilities explicitly and preserve a useful fallback for new item types.

Keep plugin installation/management outside the installed-use picker. Coordinate cross-task activity work with [agent-created child threads](../agent-created-child-threads.md), since visible dynamic-tool receipts and parent/child results overlap.

Follow [LUNA instructions](../../../AGENTS.md) and the [commit-and-push workflow](../../../.codex/skills/commit-and-push/SKILL.md) after each completed implementation and relevant checks. Refresh the primary LUNA Dev app after meaningful code changes; this research documentation itself does not require an application rebuild.
