# LUNA's Claude Code integration: confirmed coverage and gaps

Pinned LUNA source: `22a6b23686a6bce0be75cc77f836ae2406582144`. All source paths below are relative to `/Users/iulian/workspace/code/personal/luna`. Reproduce a cited excerpt with `git show 22a6b23686a6bce0be75cc77f836ae2406582144:<path>`. Later checkout commits were observed during research; this file intentionally does not mix changing implementations into one baseline.

## Existing production path

`crates/harness/src/claude/mod.rs` launches the actual installed CLI, selects the native executable and cwd, sends JSONL input and consumes streaming JSONL output. Command construction at line 148 enables print mode, partial messages and stdio permission requests, adds model/effort/settings, appends engine runtime instructions, and resumes a native session when requested.

The warm-session loop at line 745 multiplexes stdout, the input mailbox, interruption and consumer shutdown. A native result is its authoritative turn boundary. Process cleanup and a stderr tail exist; this is not a one-shot text shell wrapper.

The adapter's normalized `AgentEvent` stream feeds `crates/engine/src/sessions.rs`, the host journal in `crates/engine/src/run_journal.rs`, document projection and the transcript in `crates/ui/src/transcript.rs`. The engine's `tool_output` path and journal lookup are already designed for lazy, host-local output retrieval. The UI's `tool_detail` at line 802 renders supplied output/diff. Improving only that UI cannot recover discarded provider content.

## Capability assessment

| Area | Confirmed baseline behavior | Remaining work |
| --- | --- | --- |
| Native launch and persistence | Real CLI, actual cwd, native resume, warm input stream, deterministic result boundary | Versioned protocol compatibility, recovery/correlation and richer terminal reasons |
| Models and commands | `live_models` uses initialization; `commands()` uses initialization with success caching; curated fallback remains | Account/cwd-sensitive invalidation, mid-session updates and validated option capabilities |
| Text and reasoning | Text and thinking deltas; reasoning summaries requested rather than private raw reasoning | Preserve message/block identity, non-text blocks and complete boundary reconciliation |
| Common tool calls | Bash/Read/Write/Edit/Grep/Glob/WebFetch/WebSearch/TodoWrite typed; MCP and unknown calls retained generically | Full arguments, newer built-ins, partial input progress and native result fidelity |
| Tool results | Completion ID and error flag only for root and child `tool_result` blocks | Result text, structured content, images/resources, metadata, output references and diffs |
| Images | First-turn base64 images for PNG/JPEG/GIF/WebP | Image-only prompts, warm turns and steer; explicit errors and per-block ownership |
| Native subagents | Parent-tool routing, agent/task ID mapping, opening prompt, tagged text and background completion | Nested task graph, progress/usage/output files, workflows, teams and native peer origin |
| Approval/questions | `can_use_tool`, AskUserQuestion, timeout/deny and engine-owned question emission | Full context/suggestions, cancellation, supported control subtypes and safe unknown responses |
| App tools | Engine-authored runtime instructions | Callable Claude transport for LUNA cross-chat/status/child-chat tools |
| History | Resume provider session; shared LUNA context-based continuation exists | Native Claude list/read/fork, UUID checkpoints, truncation semantics and file rewind |
| Integrations | Native configuration can be inherited; an init observation is parsed | Configured/effective inventory, actual session attachment, health/auth/reload readback |
| Usage/limits | Input/output totals and rejected-limit errors; model context window available in results | Cache/cost/model breakdown, warnings, reset/retry timing and background spend |
| New provider events | Narrow wire enum and normalization | Hooks, compaction details, task updates, auth/status, commands/reset, peer/control and future-event coverage |

These are source conclusions. Real installed-runtime validation remains pending.

## CC-G01: tool-result loss occurs before storage

`crates/harness/src/claude/wire.rs:198` defines `ContentBlock` without a nested `content` field or provider result metadata. `crates/harness/src/claude/normalize.rs:533` turns both child and root results into `AgentEvent::ToolResult` with no output/diff. `MessageFrame` also lacks a typed `tool_use_result` projection.

The journal's `tool_output` at `crates/engine/src/run_journal.rs:377` searches normalized tool events. It cannot find Claude's discarded output. This affects shell stdout/stderr, read results, edit metadata, notebook changes, web evidence, MCP payloads and child-agent tools. The tool may execute correctly and Claude may read its result while the user receives only a completion chip.

Fix the wire/normalization seam and connect it to the shared lossless output contract. Capture native result content once; do not rerun a native tool to manufacture display data. Preserve full host evidence while keeping synchronized transcript previews bounded.

## CC-G02: image delivery differs by turn type

`wire.rs:323` has an image-block serializer; `mod.rs:664` loads first-turn image bytes. Read failures, the current size cap and unsupported MIME result in a skipped image. `mod.rs:839` uses the text serializer for warm input and explicit steering, regardless of the initial attachment support.

Use one typed content-envelope builder for every delivery route. A path mention must remain a path mention; it must not masquerade as successful image delivery. Validate the model/CLI's actual accepted image formats and limits before sending. Reject or explicitly explain incompatible input rather than silently removing it.

## CC-G03: runtime instructions do not attach LUNA tools

`mod.rs:397` reports `RuntimeDelivery::ClaudeInstructionsOnly`. `mod.rs:490` rejects a Claude contract advertising callable tools. `crates/engine/src/runtime_contract.rs:219` identifies the missing verified Claude MCP transport. The cross-task catalog gate at `crates/engine/src/cross_task_tools.rs:1498` returns no tools for this delivery kind.

Introduce a session-bound callable transport, then extend the existing authority checks. Do not simply change the enum or remove its validation. Tool authorization must still derive from the owning LUNA task, binding generation, project, account and human-authorized scope.

## CC-G04: unknown controls can leave the provider waiting

`mod.rs:991` handles only `can_use_tool`; other subtypes are logged and abandoned. `wire.rs` treats control responses/cancellations and unknown message types as `Other`. This does not prove every native version will hang, but requests requiring an answer lack a response path.

Add a correlated dispatcher with one terminal outcome per request, explicit unsupported responses and provider cancellation. Maintain the engine input bridge as sole UI request owner. `parse_questions` at line 1094 drops option descriptions; approvals currently summarize a tool name, rather than showing the full blocked command/path/reason/suggested rule changes.

LUNA's Ask/Auto/Full startup mapping is at `mod.rs:350`: Ask uses restricted `acceptEdits`, Auto uses restricted `auto`, Full bypasses native permissions. Validate the actual effective semantics of those combinations; product labels alone do not prove filesystem/network enforcement. Permission changes must not be smuggled through plan-exit handling.

## CC-G05: child routing exists, but lifecycle coverage is narrower

`normalize.rs:235` tracks spawned agent tools and native task IDs. `task_started` is used for mapping; `task_notification` settles background child agents. This correctly distinguishes background shell tasks from agent tasks. `SystemFrame` has only a small subset of task fields, and unrecognized system subtypes return no events.

Retain this working routing while adding task progress/updates, nested ownership, independent usage, workflow membership and cancellation. Claude subagents, teams, native peer sessions and persistent LUNA child chats need separate identities; they must not all become one sidebar chat type.

## CC-G06: steering is locally accepted, not proven incorporated

`mod.rs:841` explicitly marks native stdin-write acceptance as unconfirmed. It rejects another explicit steer until the uncertainty clears at a result boundary. Ordinary warm input emits a local `Steered` boundary. Neither is proof that the model read a message. The source already recognizes this distinction; the remaining work is version-gated native IDs/receipts and complete delivery evidence, not replacing the current code with an optimistic acknowledgment.

Do not match repeated text to infer incorporation. On ambiguous interruption/reconnect, preserve uncertainty and ask for an explicit retry where necessary; automatic replay could duplicate mutations.

## CC-G07: native history requires a provider-specific implementation

`crates/engine/src/history_operations.rs:15` identifies no safe idle native fork for this adapter. Generic context transfer and local Git restore do not equal Claude history fork or Claude file checkpoint rewind. Implement session ID/message UUID mapping and native operations with previews and atomic binding changes. Review any newer shared history implementation first; another agent is modifying this area.

## CC-G08: discovery exists, but comprehensive readback does not

`mod.rs:266` initializes a short-lived discovery process; `live_models` at line 341 maps its response. `models()` at line 438 intentionally keeps a lightweight curated list, while the engine refreshes live models. `catalog.rs` has a stale introductory comment describing discovery as future work. Treat code as authoritative.

The init integration observation in `wire.rs:27` is deliberately not configured inventory or health proof. The normalizer stores an internal receipt, not a complete plugin/MCP administration surface. Preserve that trust boundary when connecting a shared inventory UI.

## Compatibility baseline

Installed CLI observed: **2.1.286**. Adapter source documents live checks on **2.1.228**. The installed help advertises `--permission-prompts stdio` and accepts the old `--permission-prompt-tool` alias; do not report the alias as broken without a probe. Audit all hidden/private assumptions against the exact selected binary.

No credential store was inspected and no native discovery/model query was launched during this research. Availability, subscription gates, real image delivery and actual tool output on 2.1.286 are verification tasks in [the validation plan](validation-plan.md).
