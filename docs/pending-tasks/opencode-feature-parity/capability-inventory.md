# OpenCode certification checklist

OC-18 certifies each row below from the shipped code and retained E2E evidence. The rows come from the 2026-10-06 research against **OpenCode v2.0.22** (commit `527f0b93`). `24` marks v2.0.24 (`e7a34f09`) and `head` the upstream `v2` branch (`4a0f273f`) where they differ.

The research rated each row on LUNA fidelity, product connection and verification, with a route/gap note and LUNA file references. Those columns described the code before the parity work merged, so they were removed on 2026-10-08. The original matrix and its workstream notes are at `c7622350` (`git show c7622350:docs/pending-tasks/opencode-feature-parity/capability-inventory.md`); evidence citations such as `B §2.7`, `B T6` or `D §4.1` point to the workstream notes (`notes/B-…`, `notes/D-…`) in that commit.

| Column | Meaning |
| --- | --- |
| Native | What OpenCode v2.0.22 exposes, with later changes in parentheses |
| Evidence | E1 runtime check, E2 pinned source, E3 official docs (retrieved 2026-10-06), E4 historical LUNA report, E5 inference |
| Exec | Whether native OpenCode supports and executes it, independent of LUNA. `n/a (LUNA-owned)` marks LUNA features |
| Scope | `In scope (OC-xx)`: delivered by that package and still to certify. `OC-16 (open)`: not implemented yet. `Not pursued (PD-14)`: OpenCode-only, see the last section. `None`: nothing to integrate, with the reason |
| E2E | Certifying scenarios in [validation-plan.md](validation-plan.md) |

Prefixes: `D:x/` is `https://opencode.ai/v2/docs/x/`, and `oc2:` is OpenCode `packages/` at `527f0b93`.

## SES — sessions and persistence

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-SES-01 | Create a server-owned session with location and session rules; client-chosen id | `POST /api/session {id?, location, permissions?, …}` | E2 `oc2:protocol/src/groups/session.ts:220-237`; E3 D:api | supported | In scope (OC-03, OC-04) | OCE-05, OCE-12 |
| F-SES-02 | List native sessions; import a native session into LUNA | `GET /api/session` (cursor, `parentID`) | E2 `session.ts:55-63`; E3 D:api | supported | Not pursued (PD-14) | — |
| F-SES-03 | Session record: title, metadata, outcome, cost, tokens, revert pointer | `GET`/`PATCH /api/session/{id}` | E2 `oc2:schema/src/session.ts:31-58` | supported | In scope (OC-03, OC-10, OC-12) | OCE-09, OCE-19 |
| F-SES-04 | Resume a native session on a later turn or after app restart | session id plus new execution | E2 | supported | In scope (OC-00, OC-08) | OCE-09, OCE-12 |
| F-SES-05 | Delete a session, cascading to children | `DELETE /api/session/{id}` | E2 `session.ts:299-307` | supported | In scope (OC-12) | OCE-19 |
| F-SES-06 | Fork before a message, or the full history | `POST /api/session/{id}/fork {before?}` | E2 `session.ts:312-323` (body object required; `before` optional, **exclusive**, `oc2:core/src/session/projector.ts:136-138`; `additionalProperties:false`) | supported | In scope (OC-03, OC-12) | OCE-10 |
| F-SES-07 | Move a session to another directory or worktree, including an agent-callable move tool | `POST /api/session/{id}/move`; Code Mode `opencode.session_move` | E2 `session.ts:381-390`, `oc2:core/src/tool/plugin/opencode.ts:110-131`; E3 D:tools/ | supported | In scope (OC-04 (contain)) | OCE-19 |
| F-SES-08 | Rename, auto-title from the hidden `title` agent, agent-callable rename | `session.update`, `opencode.session_rename`, `session.renamed` | E2 `opencode.ts:90-106`; E3 D:agents/ | supported | In scope (OC-14 (`run_title`)) | OCE-20 |
| F-SES-09 | Export or import session JSON, with optional sanitize | `experimental.session.export/import`; CLI `session export/import` | E2 `oc2:core/src/session/transfer.ts:36,62,180`; E3 D:cli/commands/ | supported (experimental) | Not pursued (PD-14) | — |
| F-SES-10 | Session stats and tool reliability | `experimental.session.stats`; `opencode stats --tools` | E2 `session.ts:201-215` | supported | Not pursued (PD-14) | — |
| F-SES-11 | Active sessions and busy discovery | `GET /api/session/active` | E2 `oc2:server/src/handlers/session.ts:180-188` | supported | In scope (OC-08) | OCE-04 |
| F-SES-12 | Message list, single message and active context | `GET …/message?limit&order&cursor`, `…/message/{id}`, `…/context` | E2 `oc2:protocol/src/groups/message.ts:68`; default limit 50, order desc (`oc2:server/src/handlers/message.ts:9,41-46`) | supported | In scope (OC-08, OC-12) | OCE-10 |
| F-SES-13 | Durable per-session event log with replay after `seq` | `GET /api/experimental/session/{id}/log?after&follow` | E2 `session.ts:727-742` | supported (experimental) | In scope (OC-08) | OCE-04 |
| F-SES-14 | Viewed/unread attention marker shared across clients | `POST /api/session/{id}/view`; `session.viewed` | E2 `session.ts:889-898` | supported | None: LUNA owns its own unread state, so dropping `session.viewed` is harmless. Revisit only if LUNA shares sessions with the native TUI. | — |
| F-SES-15 | Durable resume of an in-flight turn after a server restart (budget 10) | Restart claims are swept only by the **managed service** at boot | E2 `oc2:server/src/process.ts:104-108,232-239`; `oc2:core/src/session/execution/restart.ts:33,55-59` | supported (managed service only) | In scope (OC-02, OC-08) | OCE-09 |
| F-SES-16 | Snapshots: undo, redo, revert stage/commit/clear, with optional file restore | `POST …/revert/stage {messageID, files?}`, `DELETE …/revert`, `POST …/revert/commit` | E2 `session.ts:534-559`; OpenAPI `files` boolean (coordinator, E2); E3 D:snapshots/ | supported | In scope (OC-12) | OCE-10 |
| F-SES-17 | Session sharing | `/share` reports "unavailable" | E2 `oc2:tui/src/routes/session/index.tsx:895-903`; E3 D:sharing/ | unavailable | None: Native v2 has no sharing; it is still unavailable at head. Record only. | — |
| F-SES-19 | Conversation deep link to the native session (LUNA-owned) | OpenCode desktop has its own deep links | LUNA-owned | n/a (LUNA-owned) | Not pursued (PD-14) | — |
| F-SES-20 | LUNA side chats from an OpenCode parent (LUNA-owned; shared surface) | native fork via `session.fork {before}` or a fresh session with a seed | LUNA-owned | n/a (LUNA-owned) | In scope (OC-12, OC-13) | OCE-24 |
| F-SES-18 | v1 → v2 data migration and shared storage | `migration.v1.status`; DB migration | E2 `oc2:core/src/database/v1-migration.bun.ts`; E4 `67c960f4` | supported | In scope (OC-00) | OCE-12 |

## INP — inputs, attachments, queue and drafts

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-INP-01 | Text prompt | `POST /api/session/{id}/prompt {text, …}` | E2 `session.ts:395-412` | supported | In scope (OC-03) | OCE-17 |
| F-INP-02 | Durable admission acknowledgement and caller message id | response = inbox item `{id, delivery}`; `session.inbox.enqueued/delivered`; 409 on id collision | E2 B §2.8, `oc2:server/src/handlers/session.ts:306-340` | supported | In scope (OC-07) | OCE-17 |
| F-INP-03 | Native mid-turn steering at step boundaries | `delivery: "steer"` (server default) / `"queue"` | E2 `oc2:core/src/session/prompt.ts:50`, `oc2:schema/src/session-inbox.ts:11,37-44`; E3 D:cli/tui/ | supported | In scope (OC-07) | OCE-17 |
| F-INP-04 | List, cancel and re-prioritise pending inbox items | `GET`/`PATCH`/`DELETE …/inbox/{id}` | E2 `session.ts:608-644` | supported | Not pursued (PD-14) | — |
| F-INP-05 | Image and file attachments on the initial or a fresh run | prompt `files[{uri, name?, description?, mention?}]`; `file:` or `data:` URIs; 20 MiB; PNG/JPEG/GIF/WebP | E2 `oc2:schema/src/prompt-input.ts`, `oc2:core/src/image.ts`; E3 D:attachments/ | supported | In scope (OC-09) | OCE-08 |
| F-INP-06 | Images on follow-up, queued, steered, retry and restart paths | `files` accepted on any `delivery` | E2 | supported | In scope (OC-09) | OCE-08 |
| F-INP-07 | Typed agent mentions and explicit skill selection in a prompt | prompt `agents[]`, `skills[]`; `experimental.session.skill` | E2 `prompt-input.ts`, `session.ts:437-451`; E3 D:skills/ | supported | In scope (OC-09, OC-14) | OCE-20 |
| F-INP-08 | Named reference directories and cached Git repos as context | config `references`; `reference.list` | E2 `oc2:core/src/reference.ts`; E3 D:references/ | supported | Not pursued (PD-14) | — |
| F-INP-09 | Synthetic (non-user) input injection | `POST …/synthetic`; `session.synthetic` | E2 `session.ts:456-474` | supported | In scope (OC-07, OC-11) | OCE-01, OCE-21 |
| F-INP-10 | User shell command inside the session (`!cmd`) | `POST …/shell`; `session.shell.started/ended` | E2 `session.ts:479-493` | supported | Not pursued (PD-14) | — |
| F-INP-11 | Slash commands: custom, built-in `/init` and `/review`, MCP prompts, `$ARGUMENTS`, shell blocks | `GET /api/command`; `POST …/command {name, text, files?, …}` | E2 `oc2:core/src/plugin/command.ts:30-69`; E3 D:commands/ | supported | In scope (OC-03, OC-14) | OCE-20 |
| F-INP-12 | Per-session API-managed instruction entries | `PUT /api/experimental/session/{id}/instructions/entries/{key} {value}` | E2 `session.ts:663-703` (coordinator re-checked OpenAPI) | supported (experimental) | In scope (OC-13) | OCE-18 |
| F-INP-13 | Per-session shell environment | `POST …/environment`; injects `OPENCODE_SESSION_ID` | E2 `oc2:core/src/session/environment.ts` | supported | None: Record only: not needed unless LUNA must scope environment variables per chat. | — |
| F-INP-14 | `$EDITOR` drafting; IDE selection bridge | TUI only | E2 `oc2:tui/src/context/editor.ts:117-121` | supported (client) | None: Native-client-only. LUNA's composer is the surface. | — |
| F-INP-15 | Draft, retry and restart fidelity of composed messages (LUNA-owned) | — | E2 shared composer/queue contracts | n/a (LUNA-owned) | In scope (OC-09) | OCE-08, OCE-09 |

## TURN — completion, steering, interrupt, retry, stalls and recovery

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-TURN-01 | Turn completion: one terminal per busy period (`session.execution.succeeded/failed/interrupted`) | execution events | E2 B §2.7 | supported | In scope (OC-07) | OCE-17, OCE-01 |
| F-TURN-02 | Interrupt | `POST …/interrupt` → `{interrupted}`; `execution.interrupted{reason}` | E2 `session.ts:747-764` | supported | In scope (OC-07, OC-11) | OCE-03 |
| F-TURN-03 | Interrupt and resume pending steers (`?resume=true`) | query flag | E2 | supported | Not pursued (PD-14) | — |
| F-TURN-04 | Move running foreground tools to the background | `POST …/background` | E2 `session.ts:768-778` | supported | Not pursued (PD-14) | — |
| F-TURN-05 | Wait-for-idle barrier | `POST /api/experimental/session/{id}/wait` | E2 `session.ts:519-529` | supported (experimental) | In scope (OC-10) | OCE-14 |
| F-TURN-06 | Provider retry with bounded backoff, visible retry status | `session.retry.scheduled{attempt, at, error}`; at most 10 retries | E2 `oc2:core/src/session/runner/retry.ts:43-60,104-110` | supported | In scope (OC-07) | OCE-07 |
| F-TURN-07 | Context-overflow recovery: compact and retry once | runner compaction | E2 `oc2:core/src/session/runner/step.ts:102-113` | supported | In scope (OC-10) | OCE-14 |
| F-TURN-08 | Max steps and finish reasons (`length`, `content-filter`, `tool-calls`) | `session.step.ended{finish, cost, tokens, snapshot, files}` | E2 B §2.7 | supported | In scope (OC-07) | OCE-07 |
| F-TURN-09 | Session outcome (succeeded/failed/interrupted) | session record `outcome` | E2 `schema/src/session.ts:44` | supported | In scope (OC-08) | OCE-09 |
| F-TURN-10 | Stall guard after first activity (LUNA-owned) | — | LUNA-owned | n/a (LUNA-owned) | In scope (OC-07) | OCE-07, OCE-16 |
| F-TURN-11 | Doom-loop detection | removed in v2 | E3 D:permissions/ | unavailable | None: v1 had `doom_loop`. Nothing to integrate. | — |
| F-TURN-12 | Structured turn errors (provider rate limit, auth, quota, content filter, transport; tool execution; aborted) with status and body | `session.step.failed`, `session.execution.failed`, `session.tool.failed` with `{type, message, status?, response?}` | E2 `oc2:core/src/session/to-session-error.ts:29-85` | supported | In scope (OC-07) | OCE-07 |

## OUT — text, reasoning, tool results, diffs, images, usage

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-OUT-01 | Streaming text and reasoning | `session.text.*`, `session.reasoning.*` (ordinal, `state`) | E2 B §3.2; E3 D:models/ | supported | In scope (OC-06) | OCE-02 |
| F-OUT-02 | Tool result body and full output, with native truncation to a managed file | `session.tool.success{content[], metadata, resultState}`; `tool_output` 2000 lines / 50 KiB | E2 `oc2:core/src/tool-output.ts:16,55,139`; E3 D:config/ | supported | In scope (OC-06) | OCE-06 |
| F-OUT-03 | Live tool progress and streamed tool input | `session.tool.progress{metadata}`, `session.tool.input.delta/ended` | E2 B §3.2 | supported | In scope (OC-06) | OCE-06 |
| F-OUT-04 | Per-tool file diffs (edit, write, patch) and per-turn diff | edit/write/patch `files: FileDiff[]`; `GET …/diff` | E2 `oc2:core/src/tool/plugin/file-diff.ts:4-22`, `session.ts:583-602` | supported | In scope (OC-06) | OCE-06 |
| F-OUT-05 | Images in tool results and image output | read returns images or PDF to the model; file content items | E2 `oc2:core/src/tool/plugin/read.ts` | supported | In scope (OC-06) | OCE-06, OCE-08 |
| F-OUT-06 | Transient side questions `/btw` | `POST …/generate` (no history change) | E2 `session.ts:709-722`; E3 D:cli/tui/ | supported | Not pursued (PD-14) | — |
| F-OUT-07 | Usage and cost: tokens including reasoning and cache, USD | `session.usage.updated` (session-cumulative), `session.step.ended{cost, tokens}` | E2 `oc2:core/src/session/projector.ts:60-104` | supported | In scope (OC-10) | OCE-13, OCE-14 |
| F-OUT-08 | Structured JSON-schema final output | not found | E5 | unavailable | None: No native feature. | — |
| F-OUT-09 | Markdown, mermaid and LaTeX rendering | client rendering | E2 | n/a | None: LUNA renders its own transcript. | — |
| F-OUT-10 | Bounded diagnostics for unknown events and parts (LUNA-owned) | — | E2 | n/a (LUNA-owned) | In scope (OC-02, OC-06) | OCE-02 |

## TOOL — built-in tool families (v2 names)

v2 renamed tools: `bash` → `shell`, `task` → `subagent`, `apply_patch` → `patch`. The todo, plan and LSP tools were removed.

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-TOOL-01 | `read` (offset, limit; media ≤20 MiB) | ask on `.env*` and external paths | E2 `read.ts:17-25,122-170`; E3 D:tools/ | supported | In scope (OC-06) | OCE-06 |
| F-TOOL-02 | `glob` |  | E2 `glob.ts:17-28` | supported | In scope (OC-06) | OCE-06 |
| F-TOOL-03 | `grep` |  | E2 `grep.ts:17-38` | supported | In scope (OC-06) | OCE-06 |
| F-TOOL-04 | `edit` | runs the formatter; hidden for GPT models | E2 `edit.ts:24-34,110-123` | supported | In scope (OC-06) | OCE-06 |
| F-TOOL-05 | `write` |  | E2 `write.ts:23-28` | supported | In scope (OC-06) | OCE-06 |
| F-TOOL-06 | `patch` (GPT-family models only) | per-file operations, partial apply reported | E2 `patch.ts:21-25,98-199,296-307` | supported (GPT models) | In scope (OC-06) | OCE-06 |
| F-TOOL-07 | `shell` (timeout, workdir, background; scanner-derived resources) | host authority, no OS sandbox | E2 `shell.ts:23,47-60,125-138,192,213` | supported | In scope (OC-06) | OCE-06, OCE-16 |
| F-TOOL-08 | `webfetch` |  | E2 `webfetch.ts:15,23-31` | supported | In scope (OC-06) | OCE-06 |
| F-TOOL-09 | `websearch` with provider failover; asks for a provider through a form when none is configured |  | E2 `websearch.ts:24-75` | supported | In scope (OC-05, OC-06) | OCE-16 |
| F-TOOL-10 | `question` (header, prompt, choices, multiple; free-form always allowed) | backed by Forms | E2 `question.ts:23-27,49-96` | supported | In scope (OC-05) | OCE-16 |
| F-TOOL-11 | `skill` |  | E2 `skill.ts:12-54` | supported | In scope (OC-06, OC-14) | OCE-20 |
| F-TOOL-12 | `subagent` (agent, prompt, `model?`, `sessionID?` to continue, `background?`) | depth limit 1 by default | E2 `subagent.ts:19-48,79-142` | supported | In scope (OC-11) | OCE-01, OCE-15 |
| F-TOOL-13 | `execute` (Code Mode JavaScript calling catalog tools; MCP tools go through it by default) |  | E2 `oc2:core/src/codemode/tool.ts:76`, `oc2:core/src/tool.ts:238-277` | supported | In scope (OC-06) | OCE-22 |
| F-TOOL-14 | `opencode_session_rename`, `opencode_session_move`, `opencode_models` | Code Mode namespace | E2 `opencode.ts:27-44,84-196` | supported | In scope (OC-06, OC-14) | OCE-19 |
| F-TOOL-15 | MCP resource tools | `opencode_list_mcp_resources`, `opencode_read_mcp_resource` | E2 `mcp-resource.ts:10-92` | supported | In scope (OC-06) | OCE-22 |
| F-TOOL-16 | Desktop-attached `browser.*` namespace (CDP, Lighthouse, profiling) | attached only by the OpenCode desktop app | E2 `oc2:plugin-browser/src/tools.ts:35-49` | gated (OpenCode desktop) | None: A LUNA-spawned server has no attached browser, so agents deny `browser` by default. LUNA's own computer-use and browser tooling is separate. Record only. | — |
| F-TOOL-17 | MCP tools `<server>_<tool>` | session id sent in `_meta["ai.opencode/sessionID"]`; execution timeout 12 h | E2 `oc2:core/src/tool/mcp.ts` | supported | In scope (OC-06) | OCE-22 |
| F-TOOL-18 | Plugin-defined tools |  | E2 `oc2:plugin/src/effect/tool.ts` | supported | In scope (OC-06) | OCE-22 |
| F-TOOL-19 | Tool input repair | internal | E2 | supported | None: Internal to OpenCode. | — |
| F-TOOL-20 | v1-only tools: `todowrite`/`todo`, `task`, plan enter/exit, `lsp`, `batch` | absent in v2 (v1 records survive only as migrated history) | E2 `internal.ts:228-245`; E3 D:migrate-v1/ | unavailable (v2) | In scope (OC-06, OC-12) | OCE-10 |

## MOD — models, providers, accounts, variants, usage, compaction, effective config

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-MOD-01 | Model catalog discovery | `GET /api/model` (503 while models.dev loads), `/api/model/default`, `/api/provider`; catalog and credentials are **location-scoped**; 24: non-text models excluded | E2 `oc2:core/src/models-dev.ts:16,252,270`; E3 D:models/ | supported | In scope (OC-02, OC-10) | OCE-13 |
| F-MOD-02 | Per-session model selection `provider/model#variant` | `POST …/model {model:{id, providerID, variant?}}` | E2 `session.ts:345-356` | supported | In scope (OC-10) | OCE-13 |
| F-MOD-03 | Variants: reasoning effort and budgets per model | `variants[]` with per-variant settings | E2 `oc2:core/src/variant.ts`; E3 D:models/ | supported | In scope (OC-10) | OCE-13 |
| F-MOD-04 | Provider and model sources: custom providers, 30+ built-in provider plugins, local Ollama/LM Studio/vLLM discovery | config `providers`; internal plugins | E2 `oc2:core/src/plugin/provider/*.ts`; E3 D:providers/ | supported | In scope (OC-10) | OCE-13 |
| F-MOD-05 | Integrations and credentials: API key, OAuth, device code; multi-account switch, export, import | `integration.*` (11 ops), `credential.*` (5); CLI `auth` | E2 `oc2:protocol/src/groups/integration.ts:16-192`; E3 D:cli/providers/ | supported | Not pursued (PD-14) | — |
| F-MOD-06 | Compaction: auto near the limit, manual with idempotent id, keep/buffer, native provider compaction | `POST …/compact {id?, delivery?}` (object required) → inbox item; `session.compaction.started/delta/ended/failed`; 23: compaction agent's model honoured | E2 `oc2:core/src/session/compaction.ts`, `session.ts:499-515`; E3 D:compaction/ | supported | In scope (OC-03, OC-10) | OCE-14 |
| F-MOD-07 | Context window and occupancy | model `limit`; `GET …/context`; step tokens | E2 B §2.3 | supported | In scope (OC-10) | OCE-14 |
| F-MOD-08 | Session warming | config `warming`; costs tokens | E2 `oc2:core/src/plugin/warming.ts:11-24`; E3 D:warming/ | supported | None: Native config only; LUNA needn't control it. Recorded because it spends real tokens while idle. | — |
| F-MOD-09 | Provider transport timeouts and WebSocket transport | config | E2; E3 D:providers/ | supported | None: Native config only. | — |
| F-MOD-10 | Per-agent model and request overlays | `agents.<id>.model`; `request` stored but not sent | E2 `oc2:schema/src/config/agent.ts:12-21`; E3 D:agents/ | partial | None: Native limitation (docs warn). No LUNA work. | — |
| F-MOD-11 | Effective-config introspection | `GET /api/config`, `/api/location`; CLI `debug config`, `reload` | E2 `oc2:protocol/src/groups/config.ts:9-35` | supported | In scope (OC-10, OC-14) | OCE-13 |
| F-MOD-12 | Config layering and env overrides | global < project < `.opencode`; `OPENCODE_CONFIG*`; hot reload | E2 `oc2:core/src/config/discovery.ts:11,30-75`; E3 D:config/ | supported | In scope (OC-00, OC-01, OC-14) | OCE-12 |
| F-MOD-13 | Provider-use policies (`provider.use`) | config `experimental.policies` | E2; E3 D:policies/ | supported (experimental) | In scope (OC-07) | OCE-07 |
| F-MOD-14 | OpenCode Go/Zen limits on LUNA's Usage page (LUNA-owned) | hosted | LUNA-owned | gated (hosted plan) | Kept as is (PD-17) | OCE-13 |
| F-MOD-15 | OpenCode usage **history** on LUNA's Usage page (tokens and cost over time; LUNA-owned, like Codex and Claude) | per-reply model, tokens and cost in OpenCode's local database (`session_message` rows with `type='assistant'`; v1 `message` table and legacy JSON) | E2 `oc2:core/src/session/sql.ts:78-97`, `oc2:schema/src/session-message.ts:214-227`; T3 `apps/server/src/usage/opencodeUsageReader.ts` @`4ee6bfd5` (added `e5a46d6c5d`, 2026-09-25, #10409) | n/a (LUNA-owned) | In scope (OC-10) | OCE-13 |

## PERM — permissions, sandbox, approvals, questions, elicitation

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-PERM-01 | LUNA Ask/Full modes compiled to OpenCode session rules | ordered `{action, resource, effect}`, last match wins, no match → ask; session `permissions`; head: default `external_directory: ask` removed | E2 `oc2:core/src/permission.ts:86-97`; E3 D:permissions/ | supported | In scope (OC-03, OC-04) | OCE-05, OCE-16 |
| F-PERM-02 | Approval request details (action, resources, metadata such as command or diff, source call, message) | `permission.asked{id, sessionID, action, resources[], save?, metadata?, source?, message?}` | E2 `oc2:schema/src/permission.ts:24-44` | supported | In scope (OC-04) | OCE-16 |
| F-PERM-03 | Approval replies: once, always (project-scoped save), reject with optional feedback message | `POST …/permission/{id}/reply {decision, message?}` | E2 OpenAPI `required:["decision"]` (coordinator, E2) | supported | In scope (OC-03, OC-04) | OCE-05, OCE-16 |
| F-PERM-04 | Saved approvals list and remove | `permission.saved.list/remove` | E2 `oc2:protocol/src/groups/permission.ts:37-49` | supported | Not pursued (PD-14) | — |
| F-PERM-05 | Auto-approve modes `--auto`, hidden `--yolo` and `--dangerously-skip-permissions`; TUI autoaccept | CLI flags | E1, E2 `oc2:cli/src/commands/commands.ts:26-36` | supported | In scope (OC-04) | OCE-05 |
| F-PERM-06 | Hard-deny policy layer, with Console last | `experimental.policies` | E2; E3 D:policies/ | supported (experimental) | In scope (OC-06) | OCE-16 |
| F-PERM-07 | Forms: generic structured questions, session-scoped and location-global | `form.created{form}`, `form.replied`, `form.cancelled`; `POST …/form/{id}/reply {answer}`; field types `string`, `number`, `integer`, `boolean`, `multiselect`, `external` | E2 `oc2:schema/src/form.ts:52-102` (coordinator), `session.ts:797-870` | supported | In scope (OC-05) | OCE-16 |
| F-PERM-08 | MCP elicitation, form and URL modes, under a global pseudo-session | MCP client capability mapped to Forms | E2 `oc2:core/src/mcp/client.ts:84-101,165-190`, `oc2:core/src/mcp/index.ts:91-94,160-248`; undocumented in D:mcp-servers/ | supported | In scope (OC-05) | OCE-16 |
| F-PERM-09 | Shell command scanner: per-command resources and inferred directories | tree-sitter scanner by default; portable scanner opt-in (head: default on local/dev) | E2 `shell.ts:125-138` | supported | In scope (OC-04) | OCE-16 |
| F-PERM-10 | Plugin permission-evaluation hook | plugin | E2 `oc2:plugin/src/effect/permission.ts` | supported | None: Native extension point. LUNA ships no OpenCode plugin. | — |
| F-PERM-11 | OS-level sandbox | missing: shell runs with host authority | E2; E3 D:tools/ | unavailable | None: LUNA correctly refuses read-only side chats and children for OpenCode. It must never claim a sandbox. | OCE-15 |
| F-PERM-12 | Child-session permission inheritance | child uses **its own agent rules**, not a subset of the parent; the parent's `subagent` rule gates the launch | E2 `subagent.ts:118-142` | partial | In scope (OC-04, OC-11) | OCE-05, OCE-01 |
| F-PERM-13 | Approvals answered elsewhere or gone stale | `permission.replied` | E2 B §3.2 | supported | In scope (OC-04, OC-08) | OCE-05 |
| F-PERM-14 | Server password exposure to agent tools (LUNA-owned security) | `default` serve keeps the password in its env; `--stdio` deletes it | E2 `oc2:cli/src/server-process.ts:74-77`, `oc2:core/src/environment/exec-defaults.ts:83-84` | n/a (LUNA-owned) | In scope (OC-02) | OCE-05 |
| F-PERM-15 | Deny vs Cancel vs skip semantics, and timeout behaviour (LUNA-owned mapping) | reject without message stops the execution; reject with message lets the model continue | E2 `oc2:core/src/permission.ts:66-85,276-292`; T3 precedent (D §4.1) | n/a (LUNA-owned) | In scope (OC-04) | OCE-16 |

## EXT — MCP, plugins, skills, instructions, memory, hooks, LUNA tools

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-EXT-01 | MCP servers (local stdio, remote HTTP, OAuth, timeouts, Code Mode routing) | config `mcp.servers`; `GET /api/mcp`; `mcp.status.changed` | E2 `oc2:schema/src/config/mcp.ts:7-22`; E3 D:mcp-servers/ | supported | In scope (OC-14) | OCE-22 |
| F-EXT-02 | MCP prompts as `/<server>:<prompt>` commands | commands registry | E2 `command.ts:57-69` | supported | In scope (OC-03, OC-14) | OCE-20 |
| F-EXT-03 | MCP resources and server instructions | `mcp.resource.catalog`; `<mcp_instructions>` | E2 `oc2:core/src/mcp/instructions.ts:12-105` | supported | In scope (OC-06, OC-14) | OCE-22 |
| F-EXT-04 | MCP list-changed and roots; sampling | sampling not implemented | E2 `oc2:core/src/mcp/client.ts:143-190` | partial | None: Native internals. No sampling to integrate. | — |
| F-EXT-05 | Plugins: v2 API, npm/Git/local sources, hooks, storage | config `plugins`; `GET /api/plugin`; v1 plugins don't run | E2 `oc2:plugin/src/effect/plugin.ts:27-58`; E3 D:plugins/, D:build/plugins/ | supported | In scope (OC-14 (P3, via `native_extensions`)) | OCE-22 |
| F-EXT-06 | Plugin RPC callable by clients | `POST /api/rpc`; `rpc.*` events | E2 `oc2:protocol/src/groups/rpc.ts:14`; E3 D:build/plugins/rpc/ | supported | None: Not needed. Dropping `rpc.*` is harmless. Revisit only if LUNA ever ships an OpenCode plugin. | — |
| F-EXT-07 | TUI and desktop GUI extensions | client | E2 | supported (client) | None: Native-client-only. | — |
| F-EXT-08 | Skills: discovery, typed selection, activation; 23: `disable-model-invocation` | `GET /api/skill`; `skill.updated`; prompt `skills[]`; `.claude/skills` and `.agents/skills` compatible | E2 `oc2:core/src/config/plugin/compatibility.ts:40-41`; E3 D:skills/ | supported | In scope (OC-09, OC-14) | OCE-20 |
| F-EXT-09 | Instructions: `AGENTS.md` only; `instructions` config ignored; no `CLAUDE.md` fallback | files; `session.instructions.updated` | E2 `oc2:core/src/config/plugin/instruction.ts:37-71`; E3 D:instructions/ | partial | In scope (OC-13) | OCE-18 |
| F-EXT-10 | Built-in `/init` and `/review` | commands | E2 `command.ts:30-44` | supported | In scope (OC-03, OC-15) | OCE-20, OCE-23 |
| F-EXT-11 | Persistent memory | not present | E5 | unavailable | None: No native memory subsystem. | — |
| F-EXT-12 | LUNA runtime instructions and callable LUNA tools (app-tool MCP) (LUNA-owned) | native support: runtime `experimental.mcp.add` (in-memory per location, coordinator E2 `oc2:core/src/mcp/index.ts:635-639`), session instruction entries | LUNA-owned | n/a (LUNA-owned) | In scope (OC-13) | OCE-18 |
| F-EXT-13 | Formatters after edits; LSP | formatters off by default; LSP not run | E2; E3 D:formatters/, D:migrate-v1/ | partial | None: Native. Formatter effects show up as file changes. | — |
| F-EXT-14 | Web-search providers and failover | config `websearch`; `websearch.providers/query` | E2; E3 D:websearch/ | supported | In scope (OC-05) | OCE-16 |
| F-EXT-15 | Model-specific system prompts and optimization plugins | internal | E2 | supported | None: Internal to OpenCode. | — |

## AGT — native agents, subagents, parent-child, LUNA child chats

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-AGT-01 | Default primary agent `build` |  | E2 `oc2:core/src/plugin/agent.ts:87-92` | supported | In scope (OC-10) | OCE-13 |
| F-AGT-02 | Primary agent `plan` (read-only except the plan dir; enter/exit reminders) |  | E2 `oc2:core/src/plugin/plan.ts:12-60,93`; E3 D:agents/ | supported | In scope (OC-12, OC-14) | OCE-10, OCE-13 |
| F-AGT-03 | Built-in subagents `general` and `explore` (24: explore may run shell) |  | E2 `agent.ts:94-131` | supported | In scope (OC-11) | OCE-01 |
| F-AGT-04 | Hidden maintenance agents `compaction`, `title`, `summary` |  | E2 `agent.ts:133-154` | supported | None: Internal. Note that title generation spends tokens. | — |
| F-AGT-05 | Custom agents and per-session agent selection | `GET /api/agent`; `POST …/agent`; `session.agent.selected` | E2 `session.ts:329-340`, `oc2:protocol/src/groups/agent.ts:10-24`; E3 D:agents/ | supported | Not pursued (PD-14) | — |
| F-AGT-06 | Native subagent child sessions, foreground or background; continue a child via `sessionID` | `subagent` tool; `session.created{parentID}`; completion via `session.synthetic` | E2 `subagent.ts:19-48,118-142`, `oc2:core/src/session/subagent-completion.ts` | supported | In scope (OC-11) | OCE-01, OCE-15 |
| F-AGT-07 | Nested subagent depth (default 1; `experimental.subagent_depth`) |  | E2 `oc2:schema/src/config/experimental.ts:11-13` | supported | In scope (OC-11) | OCE-01 |
| F-AGT-08 | Delegated commands (`subagent: true`) reporting back to the parent |  | E2 `oc2:core/src/session/command.ts` | supported | In scope (OC-11) | OCE-01 |
| F-AGT-09 | Subagent model override; agent-callable model search | `subagent.model`, `opencode_models` | E2 `subagent.ts:36-39,79-80` | supported | In scope (OC-11) | OCE-01 |
| F-AGT-10 | Child-session visibility in native clients | TUI filter; ACP `_meta` gate | E2 | supported (client) | None: Native clients only. LUNA's nested transcript is F-AGT-06. | — |
| F-AGT-11 | Agent teams, peer agents, shared task lists | not present | E5 | unavailable | None: No native feature. LUNA child chats are a separate LUNA system (PD-6). | — |
| F-AGT-12 | LUNA agent-created child chats from an OpenCode parent (LUNA-owned) | needs LUNA tools inside OpenCode | LUNA-owned | n/a (LUNA-owned) | In scope (OC-13) | OCE-15, OCE-18 |
| F-AGT-13 | OpenCode as the destination of a cross-provider LUNA child chat (LUNA-owned) | fresh session with a brief; model catalog; ceiling | LUNA-owned | n/a (LUNA-owned) | In scope (OC-02..04, OC-10, OC-13) | OCE-15 |
| F-AGT-14 | Stop propagation from parent to native children | foreground child interrupted with the parent step; background children are independent | E2 `subagent.ts:233-240`, B §2.10 | supported (foreground) / partial (background) | In scope (OC-11 (`stop_native_child`, like Claude)) | OCE-03 |

## ADV — review, goals, background work, side questions

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-ADV-01 | Code review via the built-in `/review` command (uncommitted, commit, branch, PR) | prompt-template command; findings are free text | E2 `command.ts:43-44`, `oc2:core/src/plugin/command/review.txt` | supported | In scope (OC-15) | OCE-23 |
| F-ADV-02 | Native review UIs (`/diff` viewer, desktop Review panel) | client, backed by `vcs.*` and `session.diff` | E2 | supported (client) | None: Native-client-only. LUNA's changes pane is the surface; per-turn diff data is F-OUT-04. | — |
| F-ADV-03 | Background shell jobs and background subagents with completion notifications | `background:true`; `shell.*`; synthetic completion | E2 `oc2:core/src/job.ts:15-28` | supported | In scope (OC-11) | OCE-21 |
| F-ADV-04 | Goals, continuations, schedules, monitors | not present | E2/E3 | unavailable | In scope (OC-15 (truthful unavailable)) | OCE-23 |
| F-ADV-05 | GitHub agent | the repo action runs the v1 `opencode github run` | E1, E2 `root:github/action.yml:44-71` | gated (v1-only) | None: Not part of the v2 CLI. | — |

## WS — workspace, project, worktree, remote, multi-device, credentials

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-WS-01 | Shared per-user background service (auto-started; replaced on version mismatch) | `opencode` (TUI) default target; `service *` | E2 `oc2:cli/src/commands/handlers/default.ts:35-49` | supported | In scope (OC-02) | OCE-09 |
| F-WS-02 | Remote attach to any server (`--server <url>`); serve modes `default`, `--stdio`, `--service` |  | E1, E2 `oc2:cli/src/server-process.ts:20,58-62,163-210` | supported | Not pursued (PD-14) | — |
| F-WS-03 | Device pairing links and QR, web UI | `opencode pair` | E2 `oc2:server/src/pairing.ts` | supported | In scope (OC-02) | — |
| F-WS-04 | Projects and locations: one process, many directories | `x-opencode-directory` or `location[directory]` | E2 `oc2:server/src/location.ts:39-66` | supported | In scope (OC-02, OC-14) | OCE-13 |
| F-WS-05 | Worktrees: create, list, remove, strategies, startup scripts | `worktree.*`; `worktree.resolved` | E2 `oc2:core/src/worktree.ts:93-97,230-252`; E3 D:config/ | supported | In scope (OC-14) | OCE-19 |
| F-WS-06 | VCS info and diff, Git and Mercurial | `vcs.*`; 23: `vcs.init` | E2 | supported | None: LUNA has its own Git surfaces. | — |
| F-WS-07 | Remote workspace drivers | internal groundwork, no public surface | E2 `oc2:core/src/workspace.ts` | unavailable | None: Not user-facing in v2.0.22. | — |
| F-WS-08 | Filesystem API, PTYs, persistent PTYs, file watching | `fs.*`, `pty.*`, `persistentPty.*` | E2 | supported | None: LUNA has its own terminal and filesystem. Dropping `pty.*`/`persistent-pty.*` is harmless. | — |
| F-WS-09 | Credential and data ownership: server-side DB, `auth export`, uninstall keep flags |  | E1, E2 | supported | **OC-16 (open)** | OCE-12 |
| F-WS-10 | ACP additional directories, SSH remote servers, WSL | ACP and desktop only | E2 | supported (client) | None: Native clients only. | — |
| F-WS-11 | LUNA remote-device execution of OpenCode (LUNA-owned) | local child of the owning engine | LUNA-owned | n/a (LUNA-owned) | **OC-16 (open)** | OCE-11 |
| F-WS-12 | Executable resolution and generation selection (LUNA-owned) | v1 and v2 co-installed | E1 baseline | n/a (LUNA-owned) | In scope (OC-00) | OCE-12 |

## HOST — hosted services, updates, compatibility

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-HOST-01 | OpenCode Console connection: workspace providers, MCP, policies, websearch | `/connect`; internal provider plugin | E2 `oc2:core/src/plugin/provider/opencode.ts:20-94`; E3 D:console/ | gated (hosted account) | In scope (OC-07) | OCE-07 |
| F-HOST-02 | Zen and Go inference subscriptions | provider `opencode` / `opencode-go` | E2 `oc2:ai/src/providers/opencode-zen.ts:20`; E3 D:console/go/ | gated (paid plan) | **OC-16 (open)** | OCE-13 |
| F-HOST-03 | Console workspaces, members, SSO, SCIM, budgets, billing, Console API | hosted web | E3 D:console/* | gated (hosted) | None: Out of band for a local client. | — |
| F-HOST-04 | Session sharing service | unavailable | E2 | unavailable | None: See F-SES-17. | — |
| F-HOST-05 | Updates: `upgrade`, update channels, `update` config; LUNA provider-update checker | `opencode upgrade --method`; channel API | E2 `oc2:cli/src/services/updater.ts:133-433` | supported | In scope (OC-17) | OCE-12 |
| F-HOST-06 | Telemetry, OTLP opt-in | env | E2 `oc2:cli/src/index.ts:125-133` | supported (opt-in) | None: LUNA sets `OPENCODE_CLIENT=luna` and must not enable OTLP. | — |
| F-HOST-07 | Distribution, Docker, Cloudflare/workerd build |  | E2 | supported | None: Not relevant to LUNA's local integration. | — |
| F-HOST-08 | v1 compatibility layer (v1 config normalized, v1 run bridge, v1 plugin dual support) |  | E2 | supported (bounded) | In scope (OC-00, OC-02 (remove v1)) | OCE-12 |
| F-HOST-09 | Simulation and HTTP recording modes | `OPENCODE_SIMULATE`, `http-recorder` | E2 `oc2:cli/src/server-process.ts:99` | supported (dev) | In scope (OC-01) | — |

## CLIENT — native-client-only surfaces and alternative transports

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-CLIENT-01 | Full-screen TUI: slash commands, tabs, themes, keybinds, `cli.json`, attention sounds, diagnostics | supported | E2/E3 D:cli/tui/ | supported (client) | None: Native-client-only (PD-4). LUNA equivalents are its own UI. | — |
| F-CLIENT-02 | `opencode mini`, `opencode run`, `run --format json` | supported | E1/E2 | supported | None: Not a LUNA transport: lossy, one-shot, no approvals or forms. | — |
| F-CLIENT-03 | Desktop app and its GUI extensions | supported | E2 | supported (client) | None: Native-client-only. | — |
| F-CLIENT-04 | Web app | supported | E2 | supported (client) | None: Native-client-only. | — |
| F-CLIENT-05 | ACP agent (`opencode acp`) | supported | E2 B T6 | supported | In scope (OC-02, decision) | — |
| F-CLIENT-06 | VS Code extension; IDE selection bridge | supported (thin) | E2 | supported (client) | None: Native-client-only. | — |
| F-CLIENT-07 | `@opencode/client` and `@opencode/sdk`; `opencode api` | supported | E2/E3 D:build/client/ | supported | In scope (OC-02) | — |

## LIFE — transport and lifecycle (from workstream B)

| ID | Capability | Native v2.0.22 (later) | Evidence | Exec | Scope | E2E |
| --- | --- | --- | --- | --- | --- | --- |
| F-LIFE-01 | Readiness and generation detection | `GET /api/info` (503 booting, 200 ready, 500 failed; `version`) | E2 B §2.2; coordinator OpenAPI check | supported | In scope (OC-02) | OCE-12 |
| F-LIFE-02 | Launch mode and ownership: stdio lease, port, password, orphan safety | `serve --stdio --port 0` prints a `{"url"}` line and exits on stdin EOF | E2 `oc2:cli/src/services/standalone.ts:18-33`, `server-process.ts:74-77,163-169,197-210` | supported | In scope (OC-02) | OCE-09 |
| F-LIFE-03 | Authentication | HTTP Basic `opencode:<pw>`; `OPENCODE_PASSWORD` (legacy `OPENCODE_SERVER_PASSWORD`) | E2 `oc2:cli/src/env.ts:10-13`, `oc2:server/src/auth.ts:20` | supported | In scope (OC-02) | OCE-12 |
| F-LIFE-04 | Event subscription and envelope (`id`, `created`, `durable.seq`, `location`) | single global `GET /api/event`; `server.connected`; 15 s heartbeat; 4,096-frame overflow queue | E2 B §2.5 | supported | In scope (OC-08) | OCE-04 |
| F-LIFE-05 | Reconnect and replay | no replay on `/api/event`; per-session durable log only | E2 B §2.6 | supported (log, experimental) | In scope (OC-08) | OCE-04 |
| F-LIFE-06 | Versioned compatibility contract (OpenAPI operation and field fingerprint; event vocabulary; known-good/known-bad/untested classification) | `/openapi.json` served live (auth) | E2 `oc2:server/src/routes.ts:176` | supported | In scope (OC-02) | OCE-12 |
| F-LIFE-07 | Shutdown and restart-claim release | `interrupt` releases the claim; SIGTERM keeps it | E2 B §2.11 | supported | In scope (OC-02, OC-08) | OCE-09 |
| F-LIFE-08 | Process model: one server per run, probe, fork and compaction vs one long-lived multi-location server | v2 is built for one process with many locations | E2 B §3.4 | supported | In scope (OC-02 (measure), OC-14) | OCE-13 |
| F-LIFE-09 | HTTP error envelope (`_tag`, `message`, `kind`, `field`; pre-ready 503 codes) | tagged JSON | E2 B §2.9 | supported | In scope (OC-03) | OCE-07 |
| F-LIFE-10 | Transport choice | HTTP+SSE canonical; ACP, SDK and `run` are clients of it | E2/E3 B §1, §5 | supported | In scope (OC-02) | — |

## Not pursued (PD-14)

Recorded so they aren't reopened by accident. These get no package, UI or scenario; their events are ignored with bounded diagnostics. Where one would *break* LUNA's model, it is **contained** instead (last column).

| OpenCode-only feature | Matrix rows | Handling |
| --- | --- | --- |
| `/btw` transient side questions (`session.generate`) | F-OUT-06 | not pursued (LUNA side chats are the shared surface) |
| Move running tools to background (`session.background`) | F-TURN-04 | not pursued |
| Interrupt-and-resume pending steers (`?resume=true`); inbox list and reprioritise UI | F-TURN-03, F-INP-04 | not pursued. Inbox events are used only as receipt evidence. LUNA's queue is the UI. |
| User shell commands (`!cmd`, `session.shell`) | F-INP-10 | not pursued |
| Reference directories and cached repos | F-INP-08 | not pursued |
| Agent picker, agent mentions, custom-agent switching | F-AGT-05, F-INP-07 (agents) | not pursued. The effective agent is shown read-only; the `plan` agent is handled safely (PD-1). |
| Session move and rename tools (`opencode_session_move`, `opencode_session_rename`); `session.moved`/`renamed` | F-SES-07/08, F-TOOL-14 | **contained:** deny `opencode_session_move` in LUNA's rules so the chat's workspace stays LUNA-owned. Native titles ignored; LUNA uses `run_title`. |
| Session export/import JSON, session stats and tool reliability | F-SES-09/10 | not pursued |
| Saved-approval management | F-PERM-04 | not pursued. LUNA never writes "always" rules (PD-8). |
| Per-turn diff endpoint | F-OUT-04 (turn part) | not pursued. Per-tool diffs (shared) are in scope. |
| Code Mode-specific presentation | F-TOOL-13 | Generic tool card only; nested calls shown only if emitted as ordinary tool events |
| External server attach, managed background service, pairing, web UI | F-WS-01/02/03 | not pursued. LUNA keeps private `--stdio` servers and disables the embedded web UI. |
| Multi-account credential management | F-MOD-05 | not pursued |
| Warming, transport settings, policies, plugin RPC and management, formatters, LSP config | F-MOD-08/09/13, F-EXT-05/06/13 | native config; n/a |
| Desktop browser namespace, persistent PTYs, worktree management, SSH/WSL, ACP, SDKs, TUI and desktop features | F-TOOL-16, F-WS-05/08/10, F-CLIENT-* | n/a or excluded (PD-5) |
