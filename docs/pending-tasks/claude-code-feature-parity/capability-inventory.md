# Full Claude Code capability inventory

This inventory defines the desired integration scope. It does not claim these capabilities are enabled for the installed account, implemented in LUNA, or fully implemented in T3/BB. Resolve each row against the selected CLI, transport, account, model, OS, settings and organization policy.

## Scope and terminology

There are three layers: locally embeddable Claude Code/Agent SDK features; LUNA product features that need a Claude adapter; and Anthropic-hosted or other-surface services. The third layer is part of the backlog but cannot be certified by local CLI tests. Terminal keybindings and another app's pane layout are not protocol capabilities to clone.

For every row, record native support, configured state, effective session state, LUNA presentation, control support and verification evidence separately. An exported type is not a loaded tool. A generic tool chip is not complete result/lifecycle fidelity. [Claude platform scope](https://code.claude.com/docs/en/platforms).

## Local session and input/output

| Capability | LUNA requirement | Owner |
| --- | --- | --- |
| Native executable/version/account selection | Same selected binary and provider home for discovery, launch, resume and maintenance; no credential copying | CC-00/01 |
| Native prompt/runtime preset | Preserve Claude's built-in coding behavior and append scoped app instructions; avoid replacing the preset accidentally | CC-01 |
| Multi-turn streaming | Persistent input, stable messages/blocks and deterministic per-turn completion; safe idle/reconnect handling | CC-02/03 |
| Text, images and local files | Typed images across initial/warm/steer/resume; ordinary file/path references retain their actual semantics | CC-04 |
| Image-only and mixed input | Accept valid image-only prompts, preserve ordered text/attachment blocks, reject invalid content clearly | CC-04 |
| Document/PDF and other content | Preserve supported read/MCP document results; do not invent native audio/video input support from unrelated API documentation | CC-02/04 |
| Streaming tool arguments | Visible progress for large partial inputs; reconcile one complete tool call without executing twice | CC-02/05 |
| Tool results and generated assets | Full text, structured values, image/resource blocks and safe clickable file references, with host-local lazy retrieval | CC-02/05 |
| Reasoning summaries | Display only provider-exposed reasoning; handle redacted/opaque blocks without requesting private chain of thought | CC-02 |
| Structured output | Preserve native schema-validated result separately from normal text; expose validation/retry failures | CC-05 |
| Model, effort, thinking, fast/context options | Discover supported choices, validate combinations and show effective live state; options are provider-specific | CC-06 |
| Usage, context, cache and limits | Account/model-aware totals, warnings/reset/retry context, background costs and clear unknown values | CC-06 |
| Session naming/import/export | Native ID/message identity linked to LUNA chat; read-only import and explicit ownership for later writes | CC-10 |
| Native history fork/truncation | Exact UUID boundary, independent destination identity and atomic binding reconciliation | CC-10 |
| File checkpoint rewind | Preview supported file restore separately from transcript history and Git checkpoints | CC-10 |

The official SDK supports streaming input and structured results; native session history and file checkpoints have distinct contracts. [Streaming input](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode), [structured outputs](https://code.claude.com/docs/en/agent-sdk/structured-outputs), [sessions](https://code.claude.com/docs/en/agent-sdk/sessions), [file checkpoints](https://code.claude.com/docs/en/agent-sdk/file-checkpointing).

## Permissions, tools and extensions

| Capability | LUNA requirement | Owner |
| --- | --- | --- |
| Built-in tool coverage | Classify common tools and retain every unknown native tool with full safe input/result/error/origin | CC-05 |
| Tool policies and approvals | Native rules plus LUNA authority; accurate command/path/reason, suggestion scope and one terminal response | CC-03 |
| Native automatic approval | Show native availability/effective enforcement; do not relabel bypass as auto or claim a sandbox from a prompt | CC-03/06 |
| Questions and other dialogs | Typed options/descriptions/free text/multi-select, form and URL elicitation, cancellation and timeout policy | CC-03 |
| Engine-owned app tools | Real session-scoped bridge for status, cross-chat coordination and authorized persistent child creation | CC-07/09 |
| External MCP | Reuse Executor and existing native connections; no duplicate global registrations; verify actual session attachment | CC-07/11 |
| MCP search/resources | Deferred tools, resource links/content, connection/auth failure and refresh readback | CC-05/11 |
| MCP Apps | Gated widget metadata/resource retrieval, sandboxed rendering and explicit unsupported fallback | CC-11/16 |
| Native memory and instructions | Effective CLAUDE.md/AGENTS.md/rules/auto-memory roots and precedence; preserve provider authority | CC-12 |
| Skills and commands | Native Claude roots, namespaced plugins, argument hints, provider-aware mention dispatch and live refresh | CC-12 |
| Hooks | Respect configured execution/policy, show meaningful progress/denial/failure and retain hook ownership | CC-12 |
| Plugins, marketplaces and LSP | Inventory source/scope/enabled/effective state, management readback, reload and language-server failures | CC-12 |
| Output styles | Discover and select native styles with accurate persistence scope; do not confuse them with model modes | CC-12 |
| Dynamic settings | Distinguish session-only changes from persistent user/project writes; read back effective settings | CC-06/12 |

Extensions can load from several native scopes, and settings precedence matters. The integration must preserve actual provider discovery instead of assuming Codex directories or injecting all local files. [Settings](https://code.claude.com/docs/en/settings), [memory](https://code.claude.com/docs/en/memory), [skills](https://code.claude.com/docs/en/agent-sdk/skills), [plugins](https://code.claude.com/docs/en/agent-sdk/plugins), [hooks](https://code.claude.com/docs/en/agent-sdk/hooks), [MCP](https://code.claude.com/docs/en/agent-sdk/mcp).

## Agents, ongoing work and advanced native features

| Capability | Distinction and desired connection | Owner |
| --- | --- | --- |
| Native subagents | Claude-owned child context with parent tool ID, agent/task IDs, model, tools, result and transcript; not automatically a persistent LUNA chat | CC-08 |
| Custom subagent definitions | Native filesystem/SDK definitions and skills/tool/permission configuration; respect loaded availability | CC-08/12 |
| Background shell/task lifecycle | Start/progress/update/output-file/usage/stop with stable task generation; root turn may finish while task remains active | CC-08 |
| Dynamic workflows | Preserve workflow/phase/member state, approval, pause/resume, consolidated result and usage | CC-13 |
| Agent teams | Experimental native multi-session coordination; distinguish teammate ownership from regular subagents | CC-13 |
| Native cross-session messaging | Preserve peer origin, destination, authentication evidence and delivery uncertainty; do not treat peer text as human authority | CC-09/13 |
| Persistent LUNA child chats | Engine-created app chat with lineage, fresh native session, separate branch/cwd and explicit authorized lifecycle | CC-09 |
| Native goals | Native condition/evaluator state and follow-up turns, with explicit start/status/clear; not a Codex RPC alias | CC-14 |
| Native schedules/monitors | Session timers/cron and Monitor task state; distinguish from app scheduler and cloud routines | CC-14 |
| Channels | Provider-managed incoming MCP/channel events, origin/trust, permission policy and cancellation | CC-13/14 |
| Review and findings | Normal chat review plus native structured findings; cloud ultrareview is a distinct service lane | CC-15 |
| Native workspace transitions | Observe cwd/repository changes and prevent stale task authority; automatic worktree creation stays disabled under current user instruction | CC-08/13 |
| End/reset conversation | Preserve historical LUNA records while applying an authoritative native reset to the active binding | CC-02/10 |

Agent teams are experimental and require opt-in; SDK subagents and persistent app children are separate mechanisms. [Teams](https://code.claude.com/docs/en/agent-teams), [SDK subagents](https://code.claude.com/docs/en/agent-sdk/subagents). Dynamic workflows, native messaging and channels each need their own lifecycle verification. [Workflows](https://code.claude.com/docs/en/workflows), [cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging), [channels](https://code.claude.com/docs/en/channels).

Claude currently documents a native `/goal` command that works non-interactively, and session-scoped scheduling. Preserve native status and stopping behavior instead of implementing an unlabelled imitation. A goal is not a claim that the host independently verified completion. [Goals](https://code.claude.com/docs/en/goal), [scheduled tasks](https://code.claude.com/docs/en/scheduled-tasks).

## Hosted, browser and other-surface integrations

| Surface | Plan and certification boundary | Owner |
| --- | --- | --- |
| Browser/Chrome integration | Discover enabled native integration, display calls/screenshots and permissions; no blanket browser access | CC-16 |
| Native computer use | macOS/runtime/account permission gates, tool/asset fidelity and one-app QA; never silently grant OS access | CC-16 |
| Remote Control/mobile | Explicit native attach/status/revocation, correct session origin and remote peer routing; distinguish native remote access from LUNA's own transport | CC-16 |
| Cloud sessions/teleport | Authenticate through supported provider interfaces, import/link native IDs and verify repository/account ownership | CC-16 |
| Cloud Projects | Link only if an embeddable interface is verified; otherwise provide accurate native handoff and a tracked external dependency | CC-16 |
| Hosted artifacts | Native publication/result/link metadata, privacy/sharing scope and mutable deployment state | CC-16 |
| Routines/cloud schedules | Account/organization gates, native schedule readback and explicit creation/disable controls; independent of session cron | CC-16 |
| Cloud ultrareview | Native invocation/job/result/cancellation and service gates; preserve findings and provenance | CC-15/16 |
| Enterprise providers | Anthropic, Bedrock, Google and Foundry configuration/auth/model gates; no invented uniform model list | CC-00/06/17 |
| Organization policy | Effective managed settings and excluded tools/features; explain restrictions without overriding them | CC-03/12/17 |
| CLI health/update/auth | User-visible native diagnostics and supported sign-in/update handoff, without reading or copying secrets | CC-00/17 |
| CI/editor/desktop integrations | Interoperate through session/file/results where documented; do not promise to recreate proprietary UI or backend services | CC-16/17 |

These are separate provider surfaces with their own requirements. [Computer use](https://code.claude.com/docs/en/computer-use), [Chrome](https://code.claude.com/docs/en/chrome), [Remote Control](https://code.claude.com/docs/en/remote-control), [cloud Projects](https://code.claude.com/docs/en/claude-projects), [artifacts](https://code.claude.com/docs/en/artifacts), [routines](https://code.claude.com/docs/en/routines), [ultrareview](https://code.claude.com/docs/en/ultrareview).

## Product exceptions and parity with Codex

**Plan/Build controls are excluded by explicit user decision.** No mode picker, proposal/implement flow or replacement `/plan`. Suppress native mode-entry tools in new sessions where supported by policy, and handle any unexpected/imported mode request explicitly; never drop its response or approve a permission escalation automatically. Users can still ask Claude to research and write a plan file in ordinary chat.

Codex-shaped request names and identities must stay inside its adapter. Map shared LUNA capabilities to verified Claude operations: images to Claude content blocks, history to Claude UUID/native session operations, goals to verified Claude goal behavior, and reviews to findings/native review or a clearly labelled ordinary chat workflow. Unsupported provider operations need explicit capability state rather than fabricated RPCs.

All LUNA-wide features also need Claude scenarios: composer attachments, durable queue delivery, cross-chat tools, output expansion, approvals/forms, child lineage, recovery, shared integration inventory, changes/terminals, diagrams/assets and review/history/goal surfaces. Reuse the other agent's merged contracts and add provider-specific validation.
