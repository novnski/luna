# Interactive HTML replies, MCP Apps, and conversation search

**Shared tracking:** [queue and current status](../README.md) · [agent workflow](../WORKFLOW.md).
Queue ownership: IC-0–IC-6. SW-07/SW-09/SW-14 consume the shared contracts and acceptance; they do not own another renderer or app bridge. Status statements below describe the planning/delivery baseline; use the CLI for live claims and progress.

**Status:** pending; plan only. No phase has been implemented or runtime-certified by this document.

**Requested:** 2026-10-10, Europe/Bucharest. Build interactive HTML replies across Codex, Claude Code and OpenCode, add MCP Apps hosting, and add find within the current conversation.

**Research baseline:** LUNA `790584471a790cc57d2f4612841f99a77f905e68`; T3 Code `50647de0143e68fde051eb1a0ccf4fe63f9947b2`. These are inspected source snapshots, not installed-provider capability claims. Refresh the relevant source and installed CLI schemas during IC-0.

## Intended result

A user can ask any of LUNA's three active providers for an interactive chart, diagram, comparison, gallery or mockup, and receive it directly inside the conversation. The agent can inspect a screenshot and console diagnostics before publishing. The result remains available after the provider stops and after LUNA restarts.

An MCP tool that supplies an app can display its interface in the same conversation. The interface uses its original provider/server connection for permitted operations and clearly reports when that connection is unavailable. Codex is the first required live integration. Claude and OpenCode support is determined by verified live-session operations, not provider names or resource-inspection results.

The user can find text in the selected conversation with a scoped Find bar, move between matches, and reach matches outside the mounted viewport. Searches include user and assistant messages, code and plan text already present in ordinary messages. This does not reintroduce the cancelled Plan/Build workflow.

## Read order

1. [Design and contracts](design.md): product behavior, ownership, storage, tool contracts, provider boundaries and search semantics.
2. [Implementation phases](implementation.md): dependencies, proposed production owners, deliverables and completion gates.
3. [Verification](verification.md): failure matrix, E2E ownership, live-provider acceptance and evidence requirements.

Follow the repository [AGENTS.md](../../../AGENTS.md), [test-audit skill](../../../.codex/skills/test-audit/SKILL.md), [QA infrastructure](../../reference/qa-infrastructure.md) and [commit-and-push skill](../../../.codex/skills/commit-and-push/SKILL.md) when implementing. This package authorizes planning; it does not claim that application changes have been made.

## Scope and chosen defaults

| Area | First delivery |
| --- | --- |
| HTML replies | Actual callable `html_preview` and `html_render` LUNA tools in Codex, Claude Code and OpenCode; one engine implementation and normalized presentation contract. Instructions alone do not satisfy support. |
| Rendering | One isolated document renderer shared by HTML replies and MCP Apps, separate from the normal browser's cookies, privileges and navigation policy. First native acceptance is macOS. |
| Persistence | Immutable, host-owned documents and small synced references; reopen, expand, view source and export. Follow-up publications create another card. Retry keys prevent duplicate cards. |
| HTML interaction | JavaScript, SVG, canvas, forms handled within the page, embedded datasets and approved local images. V1 documents are self-contained and make no direct network requests. |
| MCP Apps | Standards-based host with live Codex capture and interaction; explicit capability/fallback states for Claude and OpenCode. No duplicate direct MCP registrations or copied credentials. |
| Search | Literal, case-insensitive by default; optional case-sensitive toggle, match count, next/previous, wrapping, keyboard/menu/palette entry points, complete authorized conversation history. |
| Existing surfaces | Main chats, Side chats and Board-linked chats use the same implementation. Owning task/run/part identity is never inferred from whichever tab has focus. |
| Other platforms | Preserve existing builds and readable fallbacks. Advertise interactive support only on platforms whose isolation and native layout have been verified. Do not describe macOS-only evidence as Linux or phone certification. |

Not included: an artifact library, document collaboration, editable revision history, artifact-template skill authoring, external-agent OAuth, webhook automations, in-app app marketplaces, cross-conversation full-text search, regex search, replacement of the existing orchestrator, or arbitrary JavaScript execution against LUNA APIs. Public CDN dependencies and live network data in ordinary HTML replies are deferred; agents can embed data and library code within the documented limits.

## Current LUNA foundations and actual gaps

| Existing owner | Reusable foundation | Work still needed |
| --- | --- | --- |
| [Cross-task tools](../../../crates/engine/src/cross_task_tools.rs) and [runtime contract](../../../crates/engine/src/runtime_contract.rs) | Scoped runtime tool discovery, exact caller authority, provider delivery and durable receipts. | Add HTML tools through these established admission/delivery paths. |
| [Codex private MCP endpoint](../../../crates/harness/src/codex/luna_mcp.rs), [Claude bridge](../../../crates/engine/src/claude_mcp.rs), [OpenCode bridge](../../../crates/engine/src/opencode_mcp.rs) | LUNA-owned tools reach real provider sessions. | Prove both new tool calls and their normalized results for every provider, including resume and cancellation. |
| [Provider MCP types](../../../crates/proto/src/provider_mcp.rs) and [control contract](../../../crates/harness/src/mcp_control.rs) | App tool/resource metadata and inspectable resource facts. `ProviderMcpAppResourceView.rendered` is currently false. | An executable isolated host; live-session resource/tool operations and capability negotiation. |
| [Claude MCP inspection](../../../crates/harness/src/claude/mcp.rs) | Some resource inspection through an administrative process. | This is not an active conversation's app bridge. Never route app actions through this unrestricted probe. |
| [OpenCode adapter](../../../crates/harness/src/opencode/mod.rs) | MCP inventory and session observation. | Current `ReadAppResource` control is refused. Discover actual upstream live-session operations before advertising app interaction. |
| [Tool output contract](../../reference/tool-output.md), [message parts](../../../crates/doc/src/parts.rs) | Host journal retains full results; synced previews remain small and sanitized. | Typed visual references, lazy content retrieval, stable visual row projection and compatible fallback. |
| [Native browser](../../../crates/ui/src/browser/macos.rs) and [attachments](../../../crates/ui/src/attachments.rs) | Platform WebView embedding and host-directed attachment transfer. | A distinct isolated renderer and read-only artifact transport. Existing browser behavior is not the sandbox. |
| [Transcript](../../../crates/ui/src/transcript.rs) and [Markdown](../../../crates/ui/src/markdown/render.rs) | Virtualized rows, scroll anchors, streaming Markdown, media and native Mermaid. | Inline interactive documents and search-to-row/range mapping that preserves these contracts. |
| [Activity search types](../../../crates/proto/src/activity.rs) | Existing history/activity search. | A selected-conversation body search; metadata/activity results do not satisfy this feature. |

## Completion tracking

All phases below were **not started at the planning baseline**. Record live status through the shared queue with code and retained evidence; this table defines ownership and dependencies. Preserve the distinction between offline protocol evidence and live/pixel evidence.

| Phase | Deliverable | Depends on |
| --- | --- | --- |
| IC-0 | Provider, storage, renderer and search discovery; failure contracts | None |
| IC-1 | Immutable artifact storage, reference and isolated renderer | IC-0 |
| IC-2 | HTML tools and preview execution for all three providers | IC-1 |
| IC-3 | Native inline presentation and lifecycle | IC-1, IC-2 |
| IC-4 | MCP Apps live capture and request bridge | IC-1, IC-3 |
| IC-5 | Selected-conversation search and highlight projection | IC-0; integrate artifact titles after IC-3 |
| IC-6 | Integrated E2E, live-provider/native acceptance and handoff | IC-2 through IC-5 |

IC-5 can be implemented independently after its IC-0 contract is settled. This dependency does not authorize subagents or parallel builds. Follow the user's delegation and build-budget rules.

## Upstream evidence

These sources establish the inspiration and protocol vocabulary. Their claims do not prove LUNA support.

- T3 [HTML replies, PR #15968](https://github.com/pingdotgg/t3code/pull/15968): preview and publish tools, added October 5.
- T3 [MCP Apps, PR #16236](https://github.com/pingdotgg/t3code/pull/16236): tool-supplied apps, added October 7; Codex integration.
- T3 [conversation search, PR #10439](https://github.com/pingdotgg/t3code/pull/10439): find messages and plans, added October 8.
- T3 immutable source: [HTML tools](https://github.com/pingdotgg/t3code/blob/50647de0143e68fde051eb1a0ccf4fe63f9947b2/apps/server/src/mcp/toolkits/html/tools.ts), [artifact preparation/storage](https://github.com/pingdotgg/t3code/blob/50647de0143e68fde051eb1a0ccf4fe63f9947b2/apps/server/src/htmlRender/HtmlRender.ts), [app snapshots](https://github.com/pingdotgg/t3code/blob/50647de0143e68fde051eb1a0ccf4fe63f9947b2/apps/server/src/mcpApps/McpAppSnapshot.ts), [host bridge](https://github.com/pingdotgg/t3code/blob/50647de0143e68fde051eb1a0ccf4fe63f9947b2/packages/client-runtime/src/mcpApps/host.ts), [searchable text](https://github.com/pingdotgg/t3code/blob/50647de0143e68fde051eb1a0ccf4fe63f9947b2/packages/shared/src/threadFindText.ts).
- [MCP Apps stable specification, 2026-01-26](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx). Record the exact spec/SDK revision and negotiated features during IC-0; do not silently claim compatibility with later drafts.

## Handoff instruction

Implement this package phase by phase. First record IC-0 findings and failure cases, then use the existing engine/harness/document/RPC paths. HTML support must pass real Codex, Claude Code and OpenCode acceptance. MCP Apps must pass a real Codex/server round trip and report honest capabilities for the other providers. Search must work beyond mounted rows and during streaming. Retain repeatable E2E artifacts, refresh and verify LUNA Dev after meaningful code changes, and finish through the repository commit-and-push workflow. Do not count saved documents, mock interfaces or tool acknowledgements as runtime completion.
