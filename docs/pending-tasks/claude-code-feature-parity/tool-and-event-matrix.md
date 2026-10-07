# Tool, content, event and control coverage

The matrix is an implementation checklist, not a declaration that every name appears in every Claude session. Current official catalogs, exported types and legacy transcript names differ. The selected native session's advertised tools/capabilities are authoritative for availability; an unadvertised name is not automatically a protocol error.

## Built-in tools

The name inventory combines the [current tools reference](https://code.claude.com/docs/en/tools-reference) and [SDK tool schemas](https://code.claude.com/docs/en/agent-sdk/typescript#tool-input-types). Descriptions below specify LUNA's desired projection. Native execution belongs to Claude; LUNA must not execute a native tool a second time.

| Native names / family | Baseline LUNA mapping | Required LUNA evidence and interaction |
| --- | --- | --- |
| `Bash`, `PowerShell` | Bash typed; PowerShell generic | Command/cwd/shell, stdout/stderr, exit/error, background task/output file, permission context; OS gating |
| `Read`, `Write`, `Edit` | Typed calls, missing result bodies | Full arguments, native read blocks, edit/write metadata and diffs; preserve pagination/offset and multi-file results |
| `MultiEdit`, legacy shell-output names | Generic/legacy | Replay old transcripts faithfully; availability gated, never advertise removed tools based on old fixtures |
| `Glob`, `Grep` | Typed subset of arguments | Full query/options, matches and path provenance; tolerate sessions that do not expose these tools |
| `NotebookEdit` | Generic | Cell identity/type, notebook path, native edits and errors; meaningful diff without corrupting notebook data |
| `WebFetch`, `WebSearch` | Typed call, missing result bodies | URL/query, search/fetch result evidence, sources/citations and errors |
| `LSP` | Generic | Operation, symbol/location results, diagnostics and server failure; plugin-dependent availability |
| `Agent`, legacy `Task` | Generic named spawn plus child routing | Native agent/task/parent IDs, prompt/model/tools, child transcript, result, resume/messaging and usage |
| `ListAgents`, `SendMessage`, `SubagentHandback` | Generic; SendMessage has some child attribution | Keep destination and authenticated origin distinct from display text; delivery/result and late messages |
| `Workflow` | Generic | Script/run, approval, phases/member graph, progress, stop/pause/resume and consolidated result |
| `TaskCreate`, `TaskUpdate`, `TaskGet`, `TaskList`, `TodoWrite` | Todo subset; other names generic | Stable native task-list IDs/dependencies/status; distinguish checklist entries from background processes and app chats |
| `TaskStop`, legacy `TaskOutput` | Generic | Exact task generation, terminal outcome and output reference; support old transcripts without assuming current availability |
| `Monitor` | Generic | Source/command/watch identity, live output, deadline and stop, bounded display with complete host evidence |
| `CronCreate`, `CronDelete`, `CronList`, `ScheduleWakeup` | Generic | Native scheduled ID/condition/time/status and creation/disable readback; no conversion to app automation without explicit scope |
| `AskUserQuestion` | Question callback, labels only | Preserve question/options/descriptions, multi-select, free text, cancellation and one correlated response |
| `EnterPlanMode`, `ExitPlanMode` | Generic + approval path | Product-policy exclusion; safe response and imported-state handling, no Plan/Build UI or automatic escalation |
| `EnterWorktree`, `ExitWorktree` | Generic | Observe native cwd/repository transitions, revalidate authority; creation disabled under current instruction |
| `Skill` | Generic/native command discovery | Native resolved skill/source, argument expansion, lifecycle/result and cancellation |
| `ToolSearch`, `WaitForMcpServers`, `RefreshMcpTools` | Generic | Search/load/wait/refresh outcomes, effective tool catalog and server health; feature gating |
| `ListMcpResourcesTool`, `ReadMcpResourceTool`, `ReadMcpResourceDirTool` | Generic | Resource identity, URI, MIME, content/link and permissions; no arbitrary URI loading by the UI |
| `mcp__<server>__<tool>` | Typed generic MCP name/input | Full MCP content/structured content/error/meta, exact server origin and app widget eligibility |
| `ReportFindings` | Generic | Structured findings, severity/file/line, review provenance and repeated-result reconciliation |
| `Artifact`, `SendUserFile` | Generic | Native asset/reference/deployment metadata, MIME, safe opening and explicit publish/share scope |
| `Projects`, `RemoteTrigger`, `PushNotification` | Generic | Hosted job/project/schedule/notification results with account/service gates and explicit external effects |
| `EndConversation`, `SendFeedback`, `ShareOnboardingGuide`, `ShowOnboardingRolePicker` | Generic or unknown | Native lifecycle/dialog/result, provider availability and effect scope; no silent upload or conversation erasure |
| Any future/custom native tool | Generic unknown call | Original name and safe complete input, matching result/error, progress/origin and capability state |

Version drift to verify: SDK documentation says TaskOutput was removed in CLI `2.1.277`, while the tools page still includes a deprecated entry. Keep a legacy decoder and probe actual availability; do not retain a deny policy on a removed name as an effective safeguard. The Agent/Task alias also differs between advertising and invocation. These conflicts belong in the compatibility ledger.

## Tool/content result envelope

Preserve, when native output supplies it: provider/session/turn/message/block/tool IDs; parent/agent/task/workflow ownership; ordered text/image/document/resource content; structured content; native result metadata; `is_error`; output file/reference; terminal status; usage; citations; safe UI metadata. Unknown content blocks need a bounded visible fallback plus a host-owned artifact reference.

Normalize canonical complete results once, reconciling partial blocks and envelope `tool_use_result` rather than concatenating duplicate copies. Explicitly distinguish an empty result from missing/unavailable output. Every blob has an owner and lifetime; lazy retrieval validates both. The host retains full permitted output, while synchronized documents keep bounded previews and references.

MCP tools can return text, images, resources and structured data; test each through the app bridge and external Executor route. [Custom tools](https://code.claude.com/docs/en/agent-sdk/custom-tools), [MCP integration](https://code.claude.com/docs/en/agent-sdk/mcp).

## Provider message families

This list groups current SDK message identifiers; retain the native identifier in captured evidence rather than assuming its wire `type` matches the TypeScript name. It is derived from the current [SDK message union](https://code.claude.com/docs/en/agent-sdk/typescript#message-types); native fixtures must validate discriminators and field ownership.

| Messages / subtypes | Desired LUNA behavior | Baseline |
| --- | --- | --- |
| Assistant, user and user replay | Stable UUID/block ordering and origin, child attribution, replay dedupe | Text/calls and tagged child data partly handled; UUID/origin coverage incomplete |
| Partial assistant | Reconcile text/thinking/input JSON and all native content blocks | Text/thinking and JSON liveness; complete structured partial tool input missing |
| Result | Authoritative turn outcome with result/schema/error/cost/usage and native identity | Basic outcome and token totals supported |
| System init | Capability/tool/model/cwd/account/session snapshot with provenance | Basic session start; internal integration observation only |
| Compact boundary and memory recall | Context lifecycle, native identity/checkpoint transitions and usable readback | Dedicated coverage not established |
| Status, session state, notification and informational | Working/idle/waiting/compacting/auth state with explicit ownership | Most variants dropped |
| Task started/progress/updated/notification; background tasks changed | Distinct task graph/generation, late terminal, usage and output files | Start mapping and child notification terminal only |
| Tool progress and tool-use summary | Correlated tool liveness/summary, no second execution or fabricated completion | Dedicated coverage not established |
| Hook started/progress/response | Hook source/request/output/result and effect on approval/turn | Dedicated coverage not established |
| Auth status, rate limit and API retry | Explain native gates, warning/reject/reset/retry state; no invented successful delivery | Rejected limit surfaced; richer state incomplete |
| Thinking tokens | Usage/effort information only; do not turn counters into reasoning text | Dedicated coverage not established |
| Plugin install and commands changed | Refresh effective inventory and native command cache | Dedicated live-event coverage not established |
| Local command output | Native command result separate from model prose, retaining its origin | Dedicated coverage not established |
| Permission denied and elicitation complete | Settle the corresponding request exactly once, update UI and journal | Current control subset only |
| Files persisted and mirror error | Persistence/recovery evidence without treating an observer's failure as model failure | Dedicated coverage not established |
| Worker shutting down | Graceful native lifecycle and pending-task reconciliation | Generic process termination only |
| Prompt suggestion | Optional UI suggestion, never silently submitted as a human message | Dedicated coverage not established |
| Conversation reset | Atomic active binding reset, preserve prior audit history, clear stale active state | Currently unrecognized frame |
| Unknown future frame/subtype/content | Explicit classification, retained safe host evidence and bounded visible fallback | Unknowns become Other/zero events or parse-skip |

Conversation reset is particularly important for `/clear` and native fresh-session transitions: an active LUNA binding cannot continue pretending to own the previous provider conversation. Implement reset reconciliation with the same durability guarantees used for history changes. [SDK session lifecycle](https://code.claude.com/docs/en/agent-sdk/sessions), [streaming output](https://code.claude.com/docs/en/agent-sdk/streaming-output).

## Control and administrative surface

The current SDK reference exposes operations beyond turn input. The transport decision must establish equivalent support or an explicit unavailable state for these names:

- Input/lifecycle: `streamInput`, `interrupt`, `stopTask`, `close`, initialization and reinitialization.
- Runtime selection: `setModel`, `setPermissionMode`, thinking configuration, ephemeral flag settings.
- Discovery: supported commands/models/agents, account and context usage.
- Extensions: reload plugins/skills/output styles; MCP status/reconnect/toggle/replace; gated widget resource read.
- History: list/read/info/name/tag/fork native sessions and checkpoint file rewind.
- Persistent settings: supported, allowlisted provider settings writes with correct scope and readback.
- Interactive requests: approval, question, MCP form/URL elicitation, supported native dialogs and provider cancellation.

Request IDs, abort signals and one terminal response must span adapter → engine → persisted pending state → RPC → native UI → provider. An unknown inbound request must receive a supported explicit rejection or compatibility failure; it must not remain unanswered. Late responses after cancellation cannot grant permissions.

Native interrupt receipts require `interrupt_receipt_v1`; do not infer them from a completed stdin write. The alpha MCP Apps methods/metadata require SDK `0.3.280+` and runtime capabilities; the audited T3/BB dependencies predate that version. Probe before enabling a control. [SDK reference](https://code.claude.com/docs/en/agent-sdk/typescript#query-object).

## Hooks

Inventory native hook families without installing new hooks merely to observe them. Each configured hook retains its source, matching conditions, cancellation and effect. Current families include:

- Tool/permission: PreToolUse, PostToolUse, PostToolUseFailure, PostToolBatch, PermissionRequest, PermissionDenied.
- Prompt/session/turn: UserPromptSubmit, UserPromptExpansion, SessionStart, SessionEnd, Stop, StopFailure, Setup, Notification.
- Agents/tasks: SubagentStart, SubagentStop, TeammateIdle, TaskCreated, TaskCompleted.
- Context/configuration: PreCompact, PostCompact, PreModelSwitch, PostModelSwitch, ConfigChange, InstructionsLoaded, DirectoryAdded.
- Requests/workspace: Elicitation, ElicitationResult, WorktreeCreate, WorktreeRemove, CwdChanged, FileChanged, MessageDisplay.

The hook vocabulary is version-dependent. Rendering a hook event and supporting an SDK callback are different capabilities. Preserve native execution and permission policy; do not shadow organization hooks with app-generated replacements. [Hooks reference](https://code.claude.com/docs/en/hooks), [SDK hooks](https://code.claude.com/docs/en/agent-sdk/hooks).

## Required coverage artifact

For each observed native tool/event/control, store `nativeName`, version/capability prerequisites, sample hash, normalization outcome, owner IDs, persistence/RPC/UI outcome and E2E evidence. Mark deliberate exclusions explicitly. Fail compatibility certification if a new semantic request or result disappears silently. Unknown progress-only events may use the generic fallback, but must remain counted and inspectable.
