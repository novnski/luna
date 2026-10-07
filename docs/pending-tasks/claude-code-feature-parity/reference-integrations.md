# How T3 and BB integrate Claude Code

This is an adapter source audit, not a claim that either application supports every Claude feature. Both use the official TypeScript Agent SDK and select a native Claude executable. Their mature behavior comes from the surrounding identity, streaming, callbacks, tool bridge and persistence code, rather than from importing the SDK alone.

## Immutable sources and freshness

| Repository | Inspected local commit | Verified upstream main during audit | SDK resolved locally |
| --- | --- | --- | --- |
| T3 | `8792f95762092ea33e74d41a07e8818befd84970` | `35be904f2fc40aa6d7a42778b6895e8274f3097f` | `0.3.276` |
| BB | `f1172452bb0ca60c042ce884b3f8b98112fe377d` | `d7a6d74e87f55b80243667c67f68644b4737e77a` | `0.3.245` |

GitHub compare returned T3 12 commits ahead and BB 42 ahead. No directly audited Claude-provider source changed in those ranges. T3's lockfile changed, so its immutable upstream lockfile was read separately: the Claude SDK remains `0.3.276`. Shared app/server changes in those upstream ranges were not exhaustively re-audited. Local T3 has pre-existing untracked `docs/research/`; neither reference checkout was modified or pulled.

## T3: strong native input, event and history handling

Root: `/Users/iulian/workspace/code/personal/t3code`.

### Session construction and app tools

[`apps/server/src/provider/Layers/ClaudeAdapter.ts:4901`](https://github.com/pingdotgg/t3code/blob/8792f95762092ea33e74d41a07e8818befd84970/apps/server/src/provider/Layers/ClaudeAdapter.ts#L4901) constructs SDK query options with the exact binary, cwd, native Claude system-prompt preset plus app instructions, selected model/effort, native resume/session identity and partial messages. It explicitly loads user, project and local settings.

The same path registers `canUseTool`, `onUserDialog` and supported resume-return dialogs. It supplies a scoped HTTP MCP server named `t3-code`, using an engine-owned endpoint and authorization header from `apps/server/src/mcp/McpProviderSession.ts`. Additional filesystem access is limited to the working directory and the attachment leaf directory, rather than the whole application data root.

**Adopt:** native preset/configuration inheritance; per-session callable tools; exact process/cwd/account ownership; narrow attachment grants. The endpoint is evidence of a real tool connection. Do not copy its credentials or globally duplicate MCP configuration.

### Image content and skill invocation

[`ClaudeAdapter.ts:1602`](https://github.com/pingdotgg/t3code/blob/8792f95762092ea33e74d41a07e8818befd84970/apps/server/src/provider/Layers/ClaudeAdapter.ts#L1602) builds structured SDK content, inserts base64 image blocks for supported MIME types, and errors on invalid attachment paths/types. Other files are represented as readable path references, with different semantics from image input.

`apps/server/src/provider/Drivers/ClaudeSkillDispatch.ts` handles Claude's native command-expansion requirement: the last text block must begin with the slash command. It preserves surrounding prose while translating a skill mention into native invocation. `ClaudeSkills.ts` discovers native Claude skill roots for the selected cwd/config home. This is provider-aware dispatch, not sending a Codex mention identifier to Claude.

**Adopt:** one typed input builder for all turns; explicit attachment failure; native command/skill dispatch based on selected provider. Verify expansion against the installed CLI instead of relying solely on T3's comments or old fixtures.

### Tool and stream normalization

[`ClaudeAdapter.ts:2884`](https://github.com/pingdotgg/t3code/blob/8792f95762092ea33e74d41a07e8818befd84970/apps/server/src/provider/Layers/ClaudeAdapter.ts#L2884) covers `tool_use`, `server_tool_use` and `mcp_tool_use` blocks. It buffers partial JSON input and reconciles complete blocks. Tool-use results are extracted at line 1160. Parent tool IDs retain child ownership.

Hook started/progress/response handling begins around line 3667. Task start/progress/notification processing keeps native task IDs, agent ownership, models and workflow members. Workflow fingerprints suppress repeated member arrays without deleting changed progress.

**Adopt:** stable item IDs; input-progress buffers; deterministic deduplication; parent-aware completion and workflow updates. Confirm full image/resource result rendering separately; this audit does not establish that every non-text SDK payload reaches every T3 view.

### Interactive requests and mutable session controls

Approval/question processing around lines 4519–4824 retains pending requests and resolves native callbacks. Model changes use `query.setModel()` around line 5166. T3 also implements its own Plan workflow; that part is not an adoption target because LUNA's user cancelled Plan/Build controls.

**Adopt:** complete request ownership and cancellation; live controls with an effective-state readback. Do not copy permission transitions coupled to T3's plan implementation.

### Native history

`apps/server/src/provider/Drivers/ClaudeDriver.ts` uses native session messages and fork operations through scoped provider-environment operations. The adapter tracks assistant UUIDs and resume checkpoints, rather than reconstructing native history from UI text alone.

**Adopt:** native session/message identity, environment-correct list/read/fork and explicit checkpoint boundaries. No inspected `rewindFiles` integration was found; native file checkpoint restoration remains a separate requirement for LUNA.

## BB: strong bridge, permissions and task translation; weaker image submission

Root: `/Users/iulian/workspace/code/personal/bb`.

### Resident provider bridge

[`plugins/provider-claude-code/src/bridge/sdk-session.ts:237`](https://github.com/get-bb/bb/blob/f1172452bb0ca60c042ce884b3f8b98112fe377d/plugins/provider-claude-code/src/bridge/sdk-session.ts#L237) constructs a persistent SDK input stream with native executable/cwd, Claude prompt preset, user/project/local settings, partial messages, persistence, native resume/session IDs, plugins, hooks, permissions and SDK MCP servers. Methods at lines 188 and 197 change native permission mode and model.

`bridge/tool-proxy-mcp.ts` creates the `bb-bridge` tool proxy. The bridge connects BB-defined tools to the running native session instead of describing unavailable tools in prompt text. `native-roots.ts` maintains provider/session roots and `provider-maintenance.ts` separates native maintenance from turn execution.

**Adopt:** resident session ownership, a callable bridge, capability-dependent live control and scoped native maintenance.

### Typed tools, output and tasks

`tool-classification.ts:412` explicitly handles NotebookEdit as well as the common filesystem/shell/web tools. Agent/Task and generic MCP names are classified separately. `delta-translation.ts:943` combines native tool-result content with envelope `tool_use_result`; command execution output gets specific extraction, and close events retain status and presentation.

`task-translation.ts` and `schemas.ts:231` cover native task start, update, progress and notification. Task records include type/generation, usage, summary/error, output file, phases and workflow agents. Background shell tasks are not blindly treated as chat threads.

**Adopt:** native-result extraction, explicit task generations, output-file references and richer workflow/background lifecycle. Use LUNA's existing output store and task model rather than copying BB's entire delta schema.

### Permission and question contract

`interactive-contract.ts` carries provider/tool IDs, permission suggestions, blocked paths and decision context. The session callback receives an abort signal and validates the question response shape. This is more actionable than an approval showing only a tool name.

`bridge/session-options.ts:115` enables BB's workspace sandbox with `failIfUnavailable: false` and allows unsandboxed commands. Those settings are a deliberate BB policy; they are **not proof of strict isolation** and should not be copied as LUNA's effective-security guarantee.

**Adopt:** context-rich requests, one-shot resolution and cancellation. Verify actual filesystem/network enforcement and show downgraded protection explicitly.

### History and input limitations

`bridge/bridge.ts:2250` calls native `forkSession`. Provider checkpoint/fork operations are integrated into bridge commands. A full SDK file-rewind UI was not established by the inspected source.

Importantly, `bridge/bridge.ts:2457` builds **text** from input items: images become an attached-image URL marker; local images/files become path markers asking Claude to use Read. No direct base64 image-block builder was found in the inspected Claude plugin. BB's SDK usage therefore does not make it the model for LUNA's direct vision transport. Use T3's typed content path as the stronger reference and test it independently.

Skill mentions are normalized in this text builder; this audit does not prove equivalence to T3's last-text-block dispatch for every mixed-content prompt.

## What should not be inferred from these applications

- SDK dependency presence does not prove full API coverage or latest-version support. Their pinned SDKs predate the documented alpha MCP Apps resource API introduced in `0.3.280`.
- Native subagents do not automatically become persistent app chats; session lineage needs an application contract.
- Native forks are not file rewinds, and text path references are not submitted image bytes.
- A feature may execute inside Claude while its UI/result representation is partial.
- T3/BB plan controls and permissive sandbox fallbacks are not LUNA requirements.

The implementation proposal is to reuse their verified patterns, then cover the current native capability inventory with LUNA-specific contracts and evidence. It is not to clone either adapter wholesale.
