# C — LUNA integration and data fidelity (OpenCode adapter)

Owner: workstream C. Research date: 2026-10-06. LUNA pin `9b3e9a2e` (main). Conventions, statuses, evidence levels and category codes follow `00-baseline.md`.
Research only: no builds, tests, app launches or edits. **Line numbers are for the pinned commit `9b3e9a2e`.** During this research HEAD advanced to `e6723e1b` ("Even sidebar gutters…"), a UI-only commit that landed the baseline's dirty UI files (`crates/ui/src/shell.rs`, `shell/activity_center.rs`, `shell/spaces.rs`, `shell/usage.rs`, `state.rs`); UI refs to those files were read from that working-tree state and re-checked against `e6723e1b` (`shell.rs:4645,11435,11446`, `shell/usage.rs:1663` unchanged). Another session also began uncommitted edits in `crates/engine/src/{rpc.rs (+3 at 8531), sessions.rs (1 line at 4931), child_threads.rs, chat_workbench*.rs, cross_task_tools.rs, rpc/agent_workspace.rs}`, `crates/harness/src/lib.rs` (+4 at 42) and harness/engine test fixtures; none touch `crates/harness/src/opencode/`. Refs into those files use HEAD numbering (working tree is offset by +3/+4 after the insertion points).

Status: COMPLETE (2026-10-06). All traces are source-level (E2) unless marked; nothing was executed except read-only `ps`, `git`/file reads and two login-shell `command -v` timings.

## 0. Production path overview

### 0.1 Adapter facts (E2, `crates/harness/src/opencode/mod.rs` @ `9b3e9a2e`, clean file)

- **One `opencode serve` per `Harness::run`** (`mod.rs:419-444` → `server()` `:273-280` → `Server::spawn` `:515-602`). The server lives for the run (initial prompt + any turn-boundary steers) and is shut down at the end of `run_session` (`:2138-2142`). Discovery (`probe_models` `:283-303`, `probe_commands` `:305-318`), compaction (`compact_context` `:406-417`) and fork (`fork_session_staged` `:362-381`) each boot their own short-lived server. Spawn env adds only `OPENCODE_SERVER_PASSWORD` and `OPENCODE_CLIENT=luna` (`:526-529`), plus `compose_child_path` (`:530`); probes boot with no cwd (home).
- Wire generation chosen by health probe (`Protocol::detect` `:474-489`; a `version` field on `/api/health` = V2). Undetectable → defaults to V1 (`:505-510`).
- 2.x frames are rewritten into 1.x payloads by `normalize_v2_frame` (`:3632-3855`); every frame kind it does not list falls into `_ => Vec::new()` (`:3854`). The 1.x-shaped router `handle_bus_event` (`:2556-3098`) ends in `_ => BusOutcome::Continue` (`:3097`).
- **Events the adapter emits** (complete list from `mod.rs`): `SessionStarted` (tools always `[]`, `:1712-1720`), `AvailableCommands` (`:1738-1744`), `TextDelta`, `ReasoningDelta`, `ToolCall`, `ToolResult` (`diff: None` always, `:3359-3364`), `UserMessage` (child feeds only), `Subagent{…}` wrapper, `Usage{input,output}`, `ContextUsage{tokens,window}`, `Error`, `InputRequested`/`InputResolved` (questions only), `Steered`, `AssistantMessageCompleted`, `ProviderCheckpoint`, `Done`.
- **AgentEvent variants LUNA has but OpenCode never emits** (`crates/proto/src/agent.rs:961-1253`): `ToolActivity` (stdout/patch/progress streaming), `GeneratedImage`, `ProviderAccounting` (cost), `ContextCompaction`, `ChildStatus`/`ChildProgress`, `ProviderNotice`, `ProviderEvidence`, `NativeTaskChanged`, `ProviderTurnStarted`, `GoalChanged`, `NativeReview`, `NativeSchedules`, `AsyncQuestions`, `PeerMessage`.
- **RunRequest fields the adapter ignores** (`rg` of `mod.rs` for `input_items|model_options|provider_account|sandbox|review|goal` = no hits): `input_items` (Mention/Skill composer picks), `model_options`, `provider_account_id`, `sandbox`, `review`, `goal`. It does read `prompt`, `model`, `reasoning`, `cwd`, `resume`, `attachments`, `auto_approve`, `permission_policy`.

### 0.2 Shared downstream path (E2)

`Harness::run` stream → engine run pump (`crates/engine/src/sessions.rs` ~`:8490-9950`) → `inner.publish` = journal append (`crates/engine/src/run_journal.rs`, JSONL `{seq,event}` per chat, **no size cap, whole file kept**, header `:1-11`) + broadcast → doc fold (`crates/doc/src/parts.rs`, `MessagePart::*`) with transport previews from `tool_event_preview` (`sessions.rs:65-130`: tool output capped **64 KiB**, diffs 64 KiB per field) and `sanitize_tool_call` (`parts.rs:1023-1060`: drops `WriteFile.content`, `EditFile` old/new, `WebFetch.prompt`, and **all `Unknown`/`Mcp` input** except a spawn badge) → RPC (`FETCH_TOOL_BLOB` `crates/engine/src/rpc.rs:12233-12250` → `Sessions::tool_output` `sessions.rs:1567-1598` → journal) → UI (`crates/ui/src/transcript.rs` part rendering `:1603-1830`, tool labels `:984-1040`, icons `:9085-9099`, blob fetch `:5796-5830`; composer approval/question wizard `crates/ui/src/composer.rs:680-760`).

Engine input bridge: the engine's `request_input` closure (`sessions.rs:4832-4871`) mints its own request id, parks the resolver, emits `InputRequested`, and arms `schedule_approval_expiry` (`:715-770`). Any harness-emitted `InputRequested` with an unknown id is dropped (`sessions.rs:8783-8804`).

## 1. Per-feature traces

### C-OUT-01 Text and reasoning streaming

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| Native (v1) | `message.part.updated` (text/reasoning snapshots) + `message.part.delta{field:"text"}` | text, part id, message id | — |
| Native (v2) | `session.text.started/.delta/.ended`, `session.reasoning.started/.delta/.ended` | text, `assistantMessageID`, `ordinal` | — |
| v2 normalize | `mod.rs:3709-3729` → synthetic part id `{msg}:{t|r}{ordinal}` (`:3878-3886`) | text/delta | reasoning/text metadata, timings |
| Adapter | `part_snapshot_events` `mod.rs:3220-3305`, `part_delta_events` `:3442-3479` | suffix beyond already-emitted bytes → `TextDelta`/`ReasoningDelta` | (a) a shorter/rewritten snapshot is **dropped** (`:3286-3292`); (b) deltas with `field != "text"` dropped (`:2853-2855`); (c) text parts whose message role is unknown are held, then replayed on `message.updated` (`:3176-3197`) — never flushed if the role never arrives; (d) part boundaries collapse into one stream (no per-part identity on `TextDelta`) |
| Proto | `AgentEvent::TextDelta/ReasoningDelta` (`agent.rs`) | text | no part/message id |
| Engine/journal/doc | journal verbatim; fold into `MessagePart::Text`/`Reasoning` | full text | — |
| UI | `transcript.rs:1666` (reasoning), text markdown | rendered | — |

- **Data fidelity: partial** — streamed text survives verbatim; snapshot rewrites (native text replaced in place) are silently dropped, and multi-part boundaries/metadata are flattened. E2.
- **Product connection: supported** — visible live and in history. E2 (no fresh runtime evidence; E4 adapter header claims 2.0.3 live verification).

### C-OUT-02 Tool calls: args, result, error, stdout/stderr, diff

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| Native v1 | `message.part.updated{type:"tool", tool, callID, state{status,input,output,error,title,metadata,time,attachments}}` | — | — |
| Native v2 | `session.tool.input.started{name}` → `.called{input}` → `.success{content[]}` / `.failed|.error{error}`; other `session.tool.*` frames | — | — |
| v2 normalize | `mod.rs:3730-3794`; chip id `{sessionID}:{assistantMessageID}:{callID}` (`:3903-3933`) | name, input object, text content | **non-text `content[]` items dropped** (`:3764-3772` keeps only `.text`); tool progress/input-delta/metadata frames fall to `_ =>` (`:3854`); a tool whose `input.started` was missed has an empty name → rendered `Unknown{name:""}` |
| Adapter | `part_snapshot_events` tool arm `mod.rs:3306-3367`; `oc_tool_call` `:3534-3624` | one `ToolCall` (on first non-empty input or terminal status), one `ToolResult{is_error, output}` | **`ToolResult.output` capped at 4,096 bytes + "…"** (`OUTPUT_CAP` `:3520`, `cap_text` `:3522`); `diff: None` always (`:3361`); `state.metadata` (bash stdout/stderr live output, edit `diff`/`filediff`, truncation flag, output file path), `state.title`, `state.time`, `state.attachments` all dropped; no `ToolActivity` streaming while running; a tool with empty input appears only at completion (`:3330-3332`) |
| Arg mapping | `oc_tool_call` | bash→`Exec{command}`; read→`ReadFile{path}`; write→`WriteFile{path,content}`; edit→`EditFile{path,old,new}`; patch→`ApplyPatch{path}`; grep→`Search{pattern,path}`; glob→`Glob{pattern}`; webfetch→`WebFetch{url}`; websearch→`WebSearch{query}`; todowrite→`Todo{items}`; task→`Unknown{"Agent: …"}` | bash `description/timeout/workdir`; read `offset/limit`; grep `path`+`include` collapse to one (`path` wins, `:3570`); glob `path`; webfetch `format` and `prompt:None`; patch: 1.x `patchText` has no `filePath` so `path` is `None` (E5); todo `priority/id` and `in_progress/cancelled` → `done:false`; **`list`, `todoread`, `multiedit`, `lsp*`, `codesearch`, `skill`, `batch`, `question`, plugin and every MCP tool → `ToolCall::Unknown{name,input}`** (`:3619-3622`), never `ToolCall::Mcp` (no server/tool split) |
| Engine preview / doc | `tool_event_preview` `sessions.rs:88-105` (64 KiB, already moot at 4 KiB); `sanitize_tool_call` `parts.rs:1023-1060` | Exec command, paths, patterns, url, query, todo items | `Unknown` input stripped from the synced doc, so every unmapped OpenCode tool (incl. MCP) shows only its name in the card; full input only in the host journal |
| Journal | `run_journal.rs` | the capped event | the comment "journal keeps the rest" (`mod.rs:3519`) is **false for OpenCode**: the cap is applied before the event exists, so the journal only ever holds 4 KiB |
| UI | `transcript.rs:984-1040` labels, `:9085-9099` icons, `:1603` part | typed cards for the mapped kinds | no diff view, no stdout pane, generic widget card for Unknown |

- **Data fidelity: partial** — args for mapped kinds survive; results truncated at 4 KiB; diffs, stdout/stderr streams, metadata, attachments and non-text v2 content are dropped; MCP tools lose server identity. E2.
- **Product connection: partial** — user sees a card and a truncated result; no diff review, no live stdout, no file-change hand-off to LUNA's diff/changes surfaces from OpenCode tool data. E2.

### C-OUT-03 Full-output retrieval and output viewer

| Step | Location | Notes |
| --- | --- | --- |
| Adapter cap | `mod.rs:3354-3358` | output already truncated to 4,096 B |
| Sidecar ref | `parts.rs:990-998` sets `output_ref = "{chat}/{id}"`; `sidecar_payload` `:1011-1028` | ref created when output exists |
| RPC | `FETCH_TOOL_BLOB` `rpc.rs:12233-12250` → `Sessions::tool_output` `sessions.rs:1567-1598` (validates id charset `[A-Za-z0-9._:#~-]`, ≤200 B; v2 chip ids `ses_…:msg_…:call…` ≈ 90 B pass) → `journal.tool_output` (`run_journal.rs:377`) | returns the journal copy = the same 4 KiB |
| UI | `transcript.rs:5796-5830` (20 s timeout) | viewer opens, shows the truncated text with trailing "…" |

- **Data fidelity: partial (effectively missing beyond 4 KiB)** — native full output (v1 `state.output` beyond 4 KiB, v1 truncated-output file path in metadata, v2 full `content[]`) is never retained anywhere LUNA can reach. E2.
- **Product connection: partial** — the viewer works but cannot show more than the adapter kept; no "open native output file" fallback. E2.

### C-PERM-01 Permission ask → LUNA approval UI → reply (deny / cancel / stale / always)

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| LUNA rules out | `opencode_permission_rules` `mod.rs:2145-2175`, sent at create (`create_session` `:2201-2262`) and PATCH on resume (`:1626-1634`) | v1: `[{permission, pattern, action}]` | **v2 mismatch:** LUNA sends key `permission` with `{permission,pattern,action}`; v2.0.0 and v2.0.22 `session.create`/PATCH read **`permissions`** with `{action, resource, effect}` (v2.0.22 `packages/protocol/src/groups/session.ts:220-230`, `packages/server/src/handlers/session.ts:125-136,275-277`; v2.0.0 same). Excess key → ignored (Effect Schema default; E5). So LUNA's Ask/Full ruleset never reaches a v2 server; v2's own evaluation applies (`packages/core/src/permission.ts:88-97` falls back to `ask` for unmatched actions). |
| Native ask v1 | `permission.asked{id, sessionID, permission, patterns, metadata, always, tool{messageID,callID}}` | | |
| Native ask v2 | `permission.asked{id, sessionID, action, resources, save?, metadata?, source?{messageID,id}, message?}` (v2.0.22 `packages/schema/src/permission.ts:24-44`) | | |
| v2 normalize | `mod.rs:3832-3842` passes `data` through as `properties` | all fields | — |
| Adapter | `mod.rs:2858-3007` | request id, owning session (own, live child, or unbound child only), `messageID` → `turn_id` | reads **`permission`/`patterns` only** (`:2906-2921`): on v2 these are absent, so every ask renders "OpenCode requests tool permission" with no target and `action_kind = Tool` (bash/edit/network/external-path classification lost). Always dropped on both wires: `metadata` (bash command text beyond the pattern, edit diff), `always`/`save` patterns, tool `callID` link to the tool card, v2 `message`; `details: None` |
| Options | `opencode_approval_options` `:2184-2191` | v1: Deny / Allow once / Allow for task / Cancel; v2: Deny / Allow once / Cancel | v2 "always" deliberately withheld (persists project-wide) |
| Reply | `opencode_permission_reply` `:2193-2199`; posted `:2986-3006` | once / always(v1 only) / reject | **Deny and Cancel both → `reject`** (no distinct cancel); timeout (120 s, `:2972-2976`) or dropped resolver → `reject`; v1 falls back to the session route; v2 has no fallback and a failed POST is swallowed (`let _`) |
| Auto | `auto_approve` (Full mode, `permissions.rs:307`) → "Allow once" without asking (`:2969-2971`) | | |
| Engine | `request_input` bridge `sessions.rs:4832-4871`; expiry `schedule_approval_expiry` `:715-770` (OpenCode: fixed 120 s, not `native_choices` because `details` is None and expiry is set) | | `permission.replied` (resolution elsewhere) not consumed → card lingers until expiry (low impact: server is LUNA-owned with a per-run password) |
| UI | `composer.rs:719-751` `approval_decision` (labels → `ApprovalDecision`), expiry row `:755-775` | | — |

- **Data fidelity: partial on v1, missing on v2** — v1 keeps permission kind + patterns; v2 loses kind, target, metadata and message entirely, and LUNA's ruleset is not applied at all. E2 (field names), E5 (runtime effect of ignored rules).
- **Product connection: partial (v1) / partial, degraded (v2)** — user can allow/deny every ask; on v2 the prompt is uninformative, and Ask mode does not behave as the policy table (`permissions.rs:200-205`) claims. Deny vs Cancel are indistinguishable natively. E2.

### C-PERM-02 Native question tool → LUNA QuestionPanel / form

| Step | Location | Preserved | Lost |
| --- | --- | --- | --- |
| Native v1 | `question.asked{id, sessionID, questions[{question, header, options[{label, description}], multiple, custom?}], tool?}` | | |
| Native v2 | **No `question.*` events.** The v2 question tool (`packages/core/src/tool/plugin/question.ts:51,119`) goes through `Form` → `form.created{form}` / `form.replied` / `form.cancelled` (`packages/schema/src/form.ts:170-172`), answered at a session form route (`packages/server/src/handlers/session.ts:670-681`) | | |
| v2 normalize | `form.*` not listed → `_ => Vec::new()` (`mod.rs:3854`) | — | **everything** |
| Adapter (v1 only) | `mod.rs:3009-3094`, `map_questions` `:3481-3518` | question, header, option labels, multiple; owning session check | option `description` (LUNA has `UserInputDetails.option_descriptions`, `agent.rs:850-858`, but the adapter sets `input: None`); questions without `question` text dropped; child-session origin not attributed |
| Reply | v1 `POST /question/{id}/reply {answers:[[labels]]}` or `/reject` (`:3060-3077`); failure only logged at debug (`:3083-3087`) | | v2 path would be wrong even if events arrived |
| Engine/UI | adapter's own `InputRequested{native id}` dropped as an unknown-id twin (`sessions.rs:8783-8804`); the bridge's copy drives `composer.rs:680-718` `pending_input_request` | | |

- **Data fidelity: partial (v1) / missing (v2)**. E2.
- **Product connection: partial (v1) / missing (v2)** — on v2 a native question **blocks the turn invisibly**: the stall watchdog is already disarmed by earlier activity (`mod.rs:1925-1934`), so the run waits until the user presses Stop. Same for the v2 websearch provider-selection form (`packages/core/src/tool/plugin/websearch.ts:36-75` calls `forms.ask` when no default provider). E2 (source), E5 (runtime hang).

### C-PERM-03 MCP elicitation

- v2.0.22 MCP elicitation is implemented on `Form` (`packages/core/src/mcp/index.ts:160-171,239,786`, `toElicitationField`) → `form.created`, dropped by `normalize_v2_frame` (`mod.rs:3854`). LUNA has a full MCP-input model (`UserInputQuestion.mcp`, `crates/proto/src/mcp_input.rs`) that the OpenCode adapter never fills (`mcp: None` at `:2932`, `:3509`). v1: workstream A/B own whether 1.18 has elicitation; the adapter has no handler for it either way.
- **Data fidelity: missing. Product connection: missing** (same invisible-block failure mode as C-PERM-02 on v2). E2 (source), E5 (runtime).

### C-INP-01 Images and files: initial prompt, follow-up, queued, steered; @file; validation

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| UI staging | `crates/ui/src/attachments.rs:32` (24 MiB cap), `with_attachments` `:55-70` appends "Attached images (local files — open them to view): …" to the prompt text | paths in text + `RunRequest.attachments` | — |
| Engine admission | `provider_inputs::admit` `crates/engine/src/provider_inputs.rs:14-34`: Codex/Claude validate; **OpenCode: images unvalidated; any typed `input_items` (Mention/Skill) → rejected** ("need Codex (Claude Code runs skills only)", `:26-30`) | | typed @file / skill picks unusable for OpenCode |
| Steer/queue gates | `steers_native_inputs` `provider_inputs.rs:38-40` (Codex/Claude only); steer with images → `SteerOutcome::Rejected` (`sessions.rs:5126-5130`); queue steer held `AttachmentsUnsupported` (`doc_host.rs:5660-5668`); RPC steer rejected (`rpc.rs:3434-3437`); warm routing refused when attachments/input_items exist for non-Codex (`sessions.rs:972-977`) so they go to a fresh resumed run | | images only travel on a fresh run |
| Adapter v1 | `prompt_body` `mod.rs:2323-2356`: text part + `{type:"file", mime, filename, url:"file://…"}` | path, filename, mime by extension (png/jpg/gif/webp/pdf else octet-stream, `:2374-2390`) | no size check; non-image files get `application/octet-stream` |
| Adapter v2 | `prompt_body_v2` `:2358-2372`: `{text, files:[{uri:"file://…", name}]}` | uri, name | v2 also accepts `description`, `mention`, plus `agents[]` and `skills[]` attachments (v2.0.22 `packages/schema/src/prompt-input.ts`) — none used |
| Steer path in adapter | queued steers post `attachments: &[]` (`mod.rs:1872-1884`, `:2005-2018`); `SteerMessage.attachments/input_items/message_id` (`crates/harness/src/lib.rs:92-108`) ignored | prompt text | images (moot: engine refuses them first), user message id (`Steered{user_message_id: None}`) |
| Slash route | `post_prompt` `:2418-2460` → command endpoint | command + args | **attachments silently dropped** when the prompt starts with a known `/command` |
| Output images | — | — | assistant/tool image output (`file` parts, v2 non-text tool content) never becomes `GeneratedImage` |

- **Data fidelity: partial** — initial/fresh-run images reach OpenCode twice (text refs + native file parts); steer/queue images are refused upstream; typed mentions/skills are refused; image output is dropped. E2.
- **Product connection: partial** — attach-and-send works on a fresh turn; queue/steer with images degrades to a held row or a fresh turn; no @file or skill chips for OpenCode. E2.

### C-EXT-01 Slash command discovery and execution; skills

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| Discovery | `probe_commands` `mod.rs:305-318` (server booted in **home**, no project dir, `:273-280`); `commands_wire` `:882-892` (`/command` v1, `/api/command` v2) | name, description | v1 extra fields (`agent`, `model`, `template`, `subtask`, `hints`, source) dropped by `commands_from_wire` `:1353-1376` (`input_hint: None`, `kind: Command` always); v2 `Command.Info` is only `{name, description}` (v2.0.22 `packages/schema/src/command.ts`) |
| Cache | `commands_cache: OnceCell` (`:220`, `:399-404`) | | **never refreshed** for the harness lifetime; `command.updated` (v2) not consumed; project-scoped commands absent when the cache is warm because runs reuse it (`known_commands`, `:434`, `:1729-1736`) |
| Advertise | `AvailableCommands` `:1738-1744` → composer popup | | — |
| Execute | `post_prompt` `:2418-2460`: only names in the cached list route to `POST …/command` (v1 `{command, arguments}`, v2 `{command, text}`) | | unknown/project-only `/foo` is sent as plain prompt text; command HTTP status is never checked — transport errors logged at debug only (`:2448-2455`), a 4xx/5xx is invisible until the 60 s stall watchdog |
| Skills | — | — | no OpenCode skill discovery (`SlashCommandKind::Skill` never set), typed skill selection rejected by `provider_inputs.rs:26-30`; v2 `skills[]` prompt attachments and `session.skill.activated` unused. Integration settings do install skills into OpenCode dirs (`crates/engine/src/integration_settings.rs:131-140`) |

- **Data fidelity: partial** — names/descriptions survive; argument hints, agent/model bindings and command source are dropped; list is stale. E2.
- **Product connection: partial** — global commands run; project commands and skills are not first-class; command failures surface late or not at all. E2.

### C-MOD-01 Model, provider and variant selection/discovery; effective config

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| Discovery v1 | `GET /provider` → `models_from_providers` `mod.rs:1302-1351` (filtered to `connected`) | `provider/model` id, label, provider display name (as `description`, shown as picker attribution `crates/ui/src/pickers.rs:7640-7648`), context limit, variant ids | cost, capabilities (attachment/reasoning/tool support), release dates, status, modalities |
| Discovery v2 | `GET /api/model` → `catalog_from_v2_models` `:1406-1450` | same | provider display name (= id), no `connected` filter (only `enabled:false` skipped), variant `settings` |
| Variant → effort | `variant_to_level` `:188-198`; ladder `REASONING_LEVELS` `:161-167` (Low..Max) | minimal/low/medium/high/xhigh/max | any other variant id (`none`, `thinking`, provider-custom) dropped; `Minimal` mapped but not in the advertised ladder; `Model.options` left empty (`:1342`) though LUNA's `ModelOption` could carry arbitrary variants |
| Apply | v1: `model`+`variant` in each prompt body (`:2343-2354`); v2: `POST /api/session/{id}/model` once per run (`set_model` `:935-951`, called `:1641-1655`) | | `request.model == None` → nothing set, server default used, `SessionStarted.model = ""` (`:1716`) |
| Effective config | `SessionStarted{model: request.model}` `:1712-1720` | requested id | no readback of the server's actual model/agent (`session.model.selected`, `session.agent.selected` dropped at `:3854`); agent choice not exposed |
| Picker retry | `pickers.rs:2576-2590` retries `LIST_MODELS` up to 3× for OpenCode only (cold boot) | | — |

- **Data fidelity: partial** — ids/labels/limits survive; non-standard variants, provider names (v2), capabilities and costs are dropped. E2.
- **Product connection: partial** — users can pick a model and standard effort; no effective-config confirmation, no agent selection (product decision excludes Plan/Build UI; general agent choice is still absent). E2.

### C-MOD-02 Usage, cost, context window; compaction

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| Native v1 | assistant `message.updated{info.tokens{input,output,reasoning,cache{read,write}}, cost, providerID, modelID}` | | |
| Native v2 | `session.usage.updated{cost, tokens{input,output,reasoning,cache}}` — **session-cumulative** (v2.0.22 `packages/core/src/session/projector.ts:60-104` sums every step into the session row and republishes the totals); `session.step.ended{cost, tokens, snapshot, files}` | | |
| v2 normalize | `mod.rs:3795-3815`: synthetic `message.updated{id:"usage", role:"assistant", tokens}` | tokens | `cost`, providerID/modelID (so **no context window**), `step.ended` entirely |
| Adapter | `message.updated` handler `:2723-2779`; `context_usage_event` `:2264-2294` | `Usage{input, output}` (last message before idle wins, `:2750-2757`); `ContextUsage{tokens = total or input+output+cache.read+cache.write, window}` | `reasoning` tokens excluded from the fallback sum; cost never mapped (`ProviderAccounting` never emitted); v1 multi-step turns report only the last step's Usage (E5); **v2 context meter shows lifetime session tokens, not occupancy, with no window** (E2+E5) |
| Side effect | same handler sets `turn.provider_checkpoint_id = "usage"` on v2 (`:2737-2739`) — see C-HIST-01 | | |
| Engine | `ContextUsage` → `doc_ref.update_context_usage` (`sessions.rs:8805-8810`); root `Usage` is journaled but not aggregated anywhere visible (live accounting reads only `ProviderAccounting`, `crates/engine/src/live_usage.rs:1-14`); OpenCode history usage explicitly unsupported (`crates/engine/src/usage.rs:421-424,1632-1640`); OpenCode Go limits via `usage_opencode.rs` (reads credential from `~/.local/share/opencode/opencode.db`, `:30-50`) | | |
| Compaction | capability `ProviderCommand` (`crates/engine/src/context_controls.rs:13-18`) → `Harness::compact_context` `mod.rs:406-417` boots a **separate** short-lived server → v1 `POST /session/{id}/summarize {providerID, modelID}`; v2 `POST /api/session/{id}/compact` with JSON body `null` (`compact_http_request` `:1161-1183`) → server shut down immediately | | v2 compact is "durably admit … runs at the next step boundary" (v2.0.22 `packages/protocol/src/groups/session.ts:499-515`): LUNA kills the server right after admission, so the compaction runs (if at all) during the next run's boot — E5. A `null` body against a `Struct` payload may be rejected — E5, untested beyond the route shape (`tests.rs:352-364`). `session.compaction.*`/`session.compacted` never surface; no `ContextCompaction` event |

| Engine compaction command | `crates/engine/src/rpc.rs:4792-4800` (exclusive native-compaction transition only for Codex), `:4845-4853` (non-Codex: `compact_context` under a **30 s timeout** that includes the cold `opencode serve` boot), `:4856-4874` (on HTTP success records "The provider reported that context compaction completed"), `:4876-4900` (error → `Unknown`, blocks further compaction until Clear context) | | v1 `summarize` runs the summarization synchronously, so a long summary or slow plugin boot hits the 30 s bound and the server is dropped mid-operation (E5); on v2 the "completed" receipt is written after mere admission (E2 for the claim, E3/E2 for v2 semantics) |

- **Data fidelity: partial (v1) / missing for context and cost (v2)**. E2.
- **Product connection: partial** — context meter and a compact button exist; on v2 the meter is wrong and compaction completion is unobservable; no cost anywhere. E2/E5.

### C-TURN-01 Steering vs LUNA queue; abort/cancel; retry and stall surfacing

| Concern | Location | Behaviour | Loss / gap |
| --- | --- | --- | --- |
| Mode | `steering_mode() = TurnBoundary` `mod.rs:334-336`; registry `crates/engine/src/registry.rs:747-766`; `steers_mid_turn` false (`registry.rs:48-50`) | engine offers `AfterTurn` steer (`sessions_execution.rs:404-419`); queue primary action "Send now (interrupts…)" (`crates/ui/src/queue.rs:85-96`, asserted `queue_steer_e2e.rs:102`) | no native in-turn injection used; v2 natively supports inbox `delivery` (steer at step boundary, cf. compact docs) — not used |
| Warm follow-up | adapter queue `queued_steers` `:1825,1999-2001`; posted at idle in `settle_idle!` `:1866-1896` | message rides the same run/server | `RequireActiveTurn` steers rejected (`:1986-1996`); single-flight for non-Codex (`sessions.rs:5172-5174`) |
| Per-turn process | run ends after `Done` (`:1909-1921`) → `server.shutdown` (`:2141`) | each new user turn after idle = **new `opencode serve` boot** (+ session lookup, PATCH, catalog, set_model, command list, bus connect) | cold-start latency per turn (startup budget 300 s, `:66`) |
| Abort | interrupt → `abort_session` (v1 `/abort`, v2 `/interrupt`) with 5 s timeout `:1944-1968`; idle-wait grace `interrupt_grace` 2 s (`:229`); failure → immediate `Done{Interrupted}` + teardown | | child chips force-settled `Interrupted` (`settle_children` `:2530-2554`) regardless of native child state |
| Retry (v1) | `session.status{retry}` `:2626-2657`: attempt ≥3 → `AgentEvent::Error` chip; ≥8 → abort + `Errored` | | retries become **persistent `MessagePart::Error`** rows (`crates/doc/src/parts.rs:808-814`) instead of LUNA's transient `ProviderNotice{Retrying}` (`agent.rs:941-953`, consumed `sessions.rs:8500-8534`) |
| Retry (v2) | — | v2 `session.status{retry}` and `session.retry.scheduled` fall to `_ =>` (`:3854`) | **v2 retries invisible; the attempt-8 abort guard never fires on v2** |
| Warnings | `session.warning` and v2 `session.step.failed` (non-abort) → `Error` chip (`:2660-2703`, `:3696-3706`) | | warnings are rendered as errors |
| Stall | first-event watchdog 60 s (`LUNA_OPENCODE_STALL_MS`) `:1925-1934`, `:2029-2060`; disarmed by any session event (`:1579-1582`, `:2617-2619`) | | a turn that stalls after first activity (e.g. blocked on an unsurfaced v2 form) never times out |
| Bus loss | 40 reconnects × 250 ms (`:81-82`, `:3935-3976`); reconnect re-syncs via `session_running` (`:2062-2077`) | | missed frames during reconnect are not replayed (transcript gap) |
| Errors with content | `settle_idle!` `:1903-1921` | a `session.error` after any content → `Done{Completed}` with `error: None` | terminal error only visible as the earlier `Error` chip |

- **Data fidelity: partial** (retry/warning semantics flattened into errors; v2 retry lost). E2.
- **Product connection: partial** — queue, send-now and Stop work; mid-turn steer is not offered; v2 retry storms and post-activity hangs are unguarded. E2.

### C-TOOL-01 v2 tool-name drift (cross-cutting; explains C-OUT-02 and C-AGT-01 losses on v2)

`oc_tool_call` (`mod.rs:3534-3624`) and the spawn logic key on **1.x tool names**. v2.0.22 core tool plugins are named `shell`, `read`, `edit`, `write`, `patch`, `grep`, `glob`, `webfetch`, `websearch`, `question`, `skill`, **`subagent`**, plus `list_mcp_resources`, `read_mcp_resource`, `opencode`, `session_rename`, `session_move`, `models` (`packages/core/src/tool/plugin/*.ts`, e.g. `shell.ts:22`, `subagent.ts`). No `bash`, `task`, `todowrite` or `list` in v2 core.

- `shell` → `ToolCall::Unknown{name:"shell"}` → generic card, and `sanitize_tool_call` strips its input from the synced doc (`parts.rs:1056-1059`), so **v2 shell commands are not visible in the transcript card** (only in the host journal) and approvals are not typed (C-PERM-01).
- `subagent` → `Unknown{name:"subagent"}`: not `is_subagent_spawn()` (`agent.rs:712-719` requires `Agent`/`Agent: …`), not registered as a spawn (`mod.rs:3313`, `:3339` test `tool == "task"`), not settled (`task_completion` `:3128-3145`). See C-AGT-01.
- v2 `edit` success carries structured `files: FileDiff.Info[]` (`edit.ts:36-39`) via `session.tool.success.metadata`/content (`packages/schema/src/session-event.ts:532-545`) — dropped.
- Test fixtures for v2 only use `read` (`tests.rs`), so none of this is covered.
- **Data fidelity: partial→missing for shell/subagent/todo on v2. Product connection: partial.** E2.

### C-AGT-01 Native subagent child sessions → nested transcript, attribution, stop propagation

| Step | Location | Preserved | Lost / changed |
| --- | --- | --- | --- |
| Spawn chip (v1) | `task` tool part on our session → `ToolCall::Unknown{"Agent: {description}"}` with chip id = **part id** (`mod.rs:3313-3321`, `:3593-3599`); `register_spawn` `:3375-3440` binds by `state.metadata.sessionId`, else title prefix match, else FIFO | description, input (journal only; doc keeps spawn badge) | `subagent_type`/agent and `prompt` only in journal |
| Child session | `session.created` with `parentID == ours` (`:2701-2722`; v2 normalize `:3818-3831`) → `bind_child` `:3149-3174` (title prefix, else FIFO) or `unbound_children` | direct children only | **grandchildren never bind** (`:2703-2706`): their traffic is dropped although the child's own `task` chip appears (engine `NestedSpawns` would link it, `crates/engine/src/sessions/native_subagents.rs`, but no events for the grandchild arrive) |
| Child feed | `message.updated`/`message.part.*` for a bound child → `part_snapshot_events(…, is_main=false, spawn_ctx=None)` wrapped in `AgentEvent::Subagent{parent_tool_use_id}` (`tag` `:2523-2528`) | text, reasoning, tools, user-role text as `UserMessage` (spawn prompt / steer) | child `Usage`/`ContextUsage` not emitted (usage only for the main feed, `:2729-2760`); no `ChildStatus`/`ChildProgress`; late traffic after the chip settles is dropped (`:2835-2837`) unless a new user message re-opens it (`:2768-2772`) |
| Settle | completed/errored `task` part → tagged `Done{Completed|Errored}` (`:2795-2830`); run end → `settle_children(Interrupted)` (`:2530-2554`) | status | child error text, result summary |
| v2 | tool is `subagent`, not `task` (C-TOOL-01) | — | **no spawn registered → every v2 child stays in `unbound_children` → its whole transcript is dropped**; parent shows a generic `subagent` card with ≤4 KiB output; background (`status:"running"`) subagents (`subagent.ts:50-54`) invisible |
| Engine | `Subagent` routing `sessions.rs:8535-8600` → per-subagent doc `{chat}--sub--{id}` (`subagent_doc_id` `:7446-7470`), nested index `native_subagents.rs:1-60` | | — |
| UI | agent chips `crates/ui/src/transcript/agent_cards.rs`, subagent doc fetch `shell.rs:4630-4655` | | model label helper only for Codex (`agent_cards.rs:97`) |
| Stop | root interrupt → `abort_session(root)` only; children force-marked Interrupted locally | | no per-child stop; native child cancellation relies on OpenCode cascading the parent abort (workstream A) |
| Permissions/questions from children | answered through the root chat's approval/question UI (`:2872-2882`, `:3010-3016`) | owning-session check | not attributed to the child in the prompt text |
| LUNA child chats | separate system (`crates/engine/src/child_threads.rs`); OpenCode can be the provider of a user-created child chat, but agent-initiated child-chat transport is omitted for OpenCode (E4: `docs/qa/child-chats.md:33`, item 12) | | — |

- **Data fidelity: partial (v1) / missing (v2)**. E2.
- **Product connection: partial (v1: nested transcript works one level deep) / missing (v2)**. E2; E4 for v1 live probe existence (`crates/harness/examples/opencode_subagent_probe.rs`, mock provider, not re-run).

### C-HIST-01 Provider fork / revert / rewind; file checkpoints

| Step | Location | Behaviour | Loss / gap |
| --- | --- | --- | --- |
| Capability | `fork_capability() = Checkpoint` `mod.rs:351-353`; engine `history_operations.rs:21-28` | fork supported | — |
| Checkpoint capture | `turn.provider_checkpoint_id` = last assistant `message.updated` id (`mod.rs:2737-2739`) → `ProviderCheckpoint` before `Done` (`:1905-1908`) → persisted on Completed entries only (`sessions.rs:9866-9871`) | v1 OK | **v2: the synthetic usage frame has `id:"usage"` and `role:"assistant"` (`mod.rs:3805-3815`)**; v2 publishes `session.usage.updated` after each step's usage is recorded (`projector.ts:74-104`), i.e. after the last `step.started`, so the persisted checkpoint is very likely the literal `"usage"` → later fork fails "checkpoint usage is not in source session" (`mod.rs:976-984`). E2 (code) + E5 (ordering) |
| Fork call | `fork_session_staged` `mod.rs:793-856`: v1 `POST /session/{id}/fork {messageID}`; v2 `POST /api/session/{id}/fork {messageID}` | | **v2.0.22 payload is `{before}`** (`packages/protocol/src/groups/session.ts:312-315`): `messageID` is ignored → native fork from tip → LUNA's readback (`map_opencode_fork_history` `:1128-1159`) fails on length mismatch and leaves an orphan forked native session. Tip forks (`{}`) are unaffected. E2 + E5 |
| Readback | `session_messages` `:778-791` expects `info.id`/`id` per message | v1 shape | v2 message list shape unverified for this mapper (workstream B) |
| UI eligibility | fork from **assistant** rows for OpenCode (`crates/ui/src/transcript.rs:7698-7705`); engine refuses user-row fork for OpenCode (`history_operations.rs:165-170`); sidebar `native_history_fork` = Codex|OpenCode (`shell.rs:11433-11436`) | | edit-earlier-message and rewind are Codex/Claude only (`transcript.rs:7728-7760`) |
| Revert / rewind / file checkpoints | — | — | OpenCode v1 `revert`/`unrevert` and v2 `session.revert.*` + `snapshot`/`patch` parts and `step.ended.snapshot/files` are all dropped (`mod.rs:3364-3366`, `:3854`); LUNA's native file-rewind surface (`crates/ui/src/shell/native_file_rewind.rs`, `crates/engine/src/rpc/claude_history.rs:1-8`) is Claude-only |

- **Data fidelity: partial (v1) / missing (v2, checkpoint id and fork boundary both wrong)**. E2/E5.
- **Product connection: partial (v1 assistant-row and tip forks) / gated for rewind, edit and file checkpoints**. E2. Engine fork E2E (`crates/engine/tests/task_commands.rs:4827-4900`) uses a synthetic harness labelled OpenCode, not the adapter.

### C-SES-01 Session resume after restart; history from journal vs native; import

| Concern | Location | Behaviour | Loss / gap |
| --- | --- | --- | --- |
| Resume id | engine remembers `Done.session_id`/`SessionStarted` (`sessions.rs:9719-9725`, `:9540-9546`); task identity `prepare_resume` (`sessions.rs:4752-4782`) | `RunRequest.resume` = native session id | — |
| Adapter resume | `run_session` setup `mod.rs:1614-1640`: `session_info(resume)` then PATCH `{permission}` | reuses the native session; a failed lookup errors the run (no silent new session) | v2 PATCH key mismatch (C-PERM-01); binary is re-resolved per run, so a chat can hop between v1 and v2 binaries (E5, see §6), and the v1 adapter comment documents a shared DB rewritten by 2.x (`mod.rs:2228-2236`) |
| Transcript history | LUNA doc + journal are the only source on reopen; native `/session/{id}/message` is read only for fork | complete for what LUNA captured | anything the adapter dropped (see above) is unrecoverable from LUNA; no native backfill/reconcile ("provider does not expose a verified native reconciliation API", `crates/engine/src/task_identity.rs:931-940`) |
| Import | `task_identity.rs:1402-1406` | **native imports are Codex-only** | OpenCode sessions created outside LUNA cannot be imported |
| Orphan replay (v2) | v2 replays orphaned executions on next boot (`packages/server/src/fetch.ts:36-39`) | LUNA's hard-teardown paths (abort POST failure `mod.rs:1953-1967`, missing idle after grace `:2029-2041`, app crash) SIGTERM→SIGKILL the server (`crates/harness/src/lib.rs:607-629`) | the next boot of **any** LUNA-owned server (including home-dir model probes) may silently resume a turn the user stopped; its events would race LUNA's next prompt. E5 |
| Mid-turn app restart | journal without `Done` → boot recovery stamps the entry aborted (`run_journal.rs:5-8`) | | native turn killed with the server (`kill_on_drop`, `mod.rs:539`) |

- **Data fidelity: partial**. **Product connection: partial** (resume works; import/reconcile gated to Codex). E2/E5.

### C-WS-01 Remote and multi-device execution

- The engine runs **in-process in the LUNA app** unless a daemon already listens on the IPC port (`crates/ui/src/state.rs:1-15`, `crates/ui/src/lib.rs:165-190`); remote environments run `apps/luna-headless` (SSH-reached, `apps/luna-headless/src/main.rs:1-20`). Each chat runs on its owning device (`device_id`; UI routes blob fetches with `targetDeviceId`, `transcript.rs:5799-5815`).
- OpenCode is always a **local child of whichever engine owns the run**: `opencode serve --hostname 127.0.0.1 --port <free>` with a per-run password (`mod.rs:515-602`). There is no attach-to-existing/remote OpenCode server in production (`with_base_url` is a test seam, `:252-256`), and LUNA's usage page calls external OpenCode servers unsupported (`usage.rs:1249`).
- Environment = that engine process's env + PATH composed from exe dir, process PATH and login-shell PATH (`crates/harness/src/lib.rs:496-522`). On a launchd-started app this is the GUI env; on luna-headless it is the SSH/daemon env.
- Remote-device defaults are not edited from the local settings panel (E4: `docs/qa/agent-workspace-controls-2026-10-05.md:103-105`).
- **Data fidelity: n/a. Product connection: partial** — OpenCode works on any LUNA host that resolves a binary, but there is no OpenCode-native remote/share/attach integration. E2.

### C-EXT-02 Callable LUNA tools; runtime contract vs Executor conventions

| Step | Location | Behaviour |
| --- | --- | --- |
| Delivery | `OpencodeHarness` does not override `luna_runtime_delivery` → default `RuntimeDelivery::Unsupported` (`crates/harness/src/lib.rs:228-230`) | |
| Compile | runtime contract is compiled only for Codex/Claude (`sessions.rs:4496-4530`); `Unsupported` disables both instructions and tools (`crates/engine/src/runtime_contract.rs:212-216`, reason text `:299`) | OpenCode receives **neither LUNA instructions nor LUNA tools** |
| Tool executor | `RunControls.invoke_luna_tool` (`crates/harness/src/lib.rs:163-165`) is never read by the adapter (no `invoke_luna_tool` in `mod.rs`); claim fence `provider_turn_fence` = Codex/Claude (`sessions.rs:1210-1216`) | |
| MCP / Executor | LUNA does not inject an MCP server (e.g. a `luna` app-tool server or the Executor gateway) into OpenCode; OpenCode uses its own `opencode.json` MCP config; LUNA's integration inventory states this (`integration_settings.rs:668-670`) and tells users to manage integrations in the provider or "the chosen MCP gateway" (`:681`) | consistent with the Executor-gateway convention (no duplicate registrations), but no LUNA tools at all |
| Skills | LUNA can install skills into `~/.config/opencode/skills` / `.opencode/skills` (`integration_settings.rs:131-140`, dirs `agent_accounts.rs:488-497`) | file-level only; not discovered back into the composer (C-EXT-01) |
| E4 | `docs/qa/agent-workspace-controls-2026-10-05.md:102-104`: "The existing OpenCode adapter does not yet deliver these tools" | |

- **Data fidelity: n/a. Product connection: gated/missing** (instructions-only is not even available; v2 natively supports MCP so an app-tool MCP path is plausible — workstream A). E2.

### C-ADV-01 Goals, review and other shared LUNA features

- Goals: engine rejects `goal` for non-Codex/Claude ("Goals need a Codex or Claude Code task", `sessions.rs:4258-4264`); UI `GoalProvider::of` returns `None` for OpenCode (`crates/ui/src/composer/goals.rs:53-60`). **gated**.
- Native review: requires Codex (`sessions.rs:4298-4303`, Claude converts to a prompt `:4283-4296`); UI `native_review_available` Codex/Claude (`shell.rs:11446-11455`, `crates/ui/src/shell/native_review.rs:50`). **gated** (no OpenCode review path; OpenCode `/review`-style commands would only run as an ordinary slash command).
- Account selection: Codex only at chat level (`sessions.rs:3712-3716`); execution accounts resolved only for Codex/Claude (`sessions_execution.rs:116-126`, `child_threads.rs:178-184`); `provider_account_id` ignored by the adapter. **gated**.
- Read-only side chats/children: refused for OpenCode (`sessions_execution.rs:183-187`, `side_chats.rs:426-432`). **gated**.
- Conversation deep link: Codex only (`crates/ui/src/links.rs:75`). **gated**.
- Integrations composer catalog: Codex only (`crates/ui/src/composer/integrations.rs:396`). **gated**.
- Auto permission mode: unsupported for OpenCode by design (`permissions.rs:51-66`, UI copy `pickers.rs:1746-1748`; E2E `crates/engine/tests/e2e.rs:2516-2522`). **excluded-by-product-decision** (no native reviewer).
- **Product connection: gated** for all of the above. E2.

## 2. Provider-gate table

Method: `rg "HarnessId::Opencode"` (all non-test hits reviewed), `rg -c "HarnessId::Codex"` per file with production hits inspected, plus capability descriptors (`registry.rs:747-766`, `permissions.rs:51-66`, `context_controls.rs:13-18`, `history_operations.rs:21-36`, `provider_inputs.rs:14-40`, `lib.rs:228-230`). E2 throughout. = file has another session's uncommitted edits (read as-is).

| Shared LUNA feature | OpenCode status | Gate / evidence (`file:line`) |
| --- | --- | --- |
| Run / resume / stop | supported | registry `crates/engine/src/registry.rs:747-766`; `provider_can_run` `permissions.rs:319-321` |
| Permission modes | Ask + Full only; Auto **excluded-by-product-decision** | `crates/engine/src/permissions.rs:51-66`, mapping `:200-205`; UI `crates/ui/src/pickers.rs:1746-1748`; E2E `crates/engine/tests/e2e.rs:2516-2522` |
| "Allow for task" approval | gated to v1 at runtime; engine capability false | `mod.rs:2177-2191`; `permissions.rs:57-59` |
| Automatic reviewer ("Approve for me") | unavailable | `permissions.rs:54`, limitation text `:60-62` |
| Mid-turn steer / queue "Steer now" | unavailable (TurnBoundary) → "Send now" | `registry.rs:48-50,755`; `sessions_execution.rs:404-419`; `crates/ui/src/queue.rs:85-96` |
| Images/typed inputs on steer & queue steer | gated (Codex/Claude only) | `provider_inputs.rs:38-40`; `sessions.rs:5126-5130`; `rpc.rs:3434-3437`; `doc_host.rs:5660-5668`; UI mirror `queue.rs:347-352` |
| Typed composer selections (mention / skill / plugin) | gated (rejected) | `provider_inputs.rs:26-30` |
| Warm-runtime routing with attachments | gated (fresh run instead) | `sessions.rs:972-977` |
| Multiple pending steers | single-flight (non-Codex) | `sessions.rs:5172-5174` |
| Native compaction | supported via `ProviderCommand`; no exclusive transition (Codex only) | `context_controls.rs:13-18`; `rpc.rs:4792-4800,4845-4853` |
| Provider fork | supported (assistant rows + tip); user-row fork refused | `history_operations.rs:21-28,165-170`; UI `transcript.rs:7698-7705`; sidebar `shell.rs:11433-11436` |
| Edit earlier message / rewind | gated (Codex/Claude) | `crates/ui/src/transcript.rs:7728-7760`; Codex rewind `rpc.rs:4030-4040`; Claude rewind `rpc.rs:5928-5940` |
| Native file checkpoints | gated (Claude) | `crates/engine/src/rpc/claude_history.rs:1-8`; `crates/ui/src/shell/native_file_rewind.rs` |
| Goals | gated (Codex/Claude) | `sessions.rs:4258-4264`; `crates/ui/src/composer/goals.rs:53-60`; `rpc.rs:4562-4572` |
| Native review | gated (Codex; Claude via prompt) | `sessions.rs:4283-4303`; `shell.rs:11446-11455`; `crates/ui/src/shell/native_review.rs:50` |
| LUNA runtime instructions + tools (app-tool MCP) | missing (`Unsupported`) | `crates/harness/src/lib.rs:228-230`; `sessions.rs:4496-4530`; `runtime_contract.rs:212-216,299`; claim fence `sessions.rs:1210-1216` |
| Agent-initiated LUNA child chats | gated (transport omitted) | E4 `docs/qa/child-chats.md:33` |
| Chat-level / execution account selection | gated (Codex; Claude for execution) | `sessions.rs:3712-3716`; `sessions_execution.rs:116-126`; `child_threads.rs:178-184`; `rpc.rs:8605` |
| Side-chat / child read-only sandbox | gated (Codex/Mock) | `sessions_execution.rs:183-187`; `side_chats.rs:426-432` |
| Side-chat context seeding | different path (prompt-injected context) | `side_chats.rs:780-792` |
| Native integrations catalog / enable | gated (Codex) | `rpc.rs:10490-10498`; `crates/ui/src/composer/integrations.rs:396`; inventory reason `integration_settings.rs:668-670` |
| Skill library install | supported (filesystem) | `integration_settings.rs:131-140`; `agent_accounts.rs:488-497`; E2E `crates/engine/tests/skill_library_e2e.rs` (synthetic) |
| Skills shown as skills in composer | Codex only | `crates/ui/src/composer.rs:4712-4721` |
| Native session import | gated (Codex) | `crates/engine/src/task_identity.rs:1402-1406` |
| Native reconciliation of bindings | gated (Codex) | `task_identity.rs:931-940` |
| Conversation deep link | gated (Codex) | `crates/ui/src/links.rs:75` |
| Live cost / accounting rows | missing (no `ProviderAccounting`) | `crates/engine/src/live_usage.rs:1-14` |
| Historical usage scan | unsupported | `crates/engine/src/usage.rs:421-424,1632-1640` |
| OpenCode Go limits on Usage page | supported (read-only endpoint) | `crates/engine/src/usage_opencode.rs:1-50`; `rpc.rs:1413-1415`; UI `crates/ui/src/shell/usage.rs:1663` |
| Accounts settings card | usage card instead of accounts/sign-in | `crates/ui/src/settings/accounts.rs:1865,1995-1997` |
| Model picker attribution + cold-boot retry | OpenCode-specific (supported) | `pickers.rs:1350,2584,7644` |
| Provider updates | supported (same resolver) | `crates/engine/src/provider_updates.rs:19,33,42,154-182` |
| Harness settings probe timeout | 60 s for OpenCode (25 s others) | `crates/ui/src/settings/harnesses.rs:2444-2448` |
| Strict resume / native thread name / client message ids | Codex only | `sessions.rs:4737-4745,4772-4777,4783-4797,4798-4810` |
| Delivery-unknown steer guard | Codex only | `sessions.rs:4689-4693` |


## 3. Silent drops

All in `crates/harness/src/opencode/mod.rs` unless noted. E2.

| # | Where | What is dropped | Impact |
| --- | --- | --- | --- |
| D1 | `normalize_v2_frame` `_ => Vec::new()` `:3854` | every v2 kind not mapped. Of the v2.0.22 `session.*` vocabulary only `execution.{started,interrupted,succeeded,failed}`, `step.{started,failed}`, `text.*`, `reasoning.*`, `tool.input.started`, `tool.called`, `tool.success`, `tool.failed`, `usage.updated`, `created` are mapped (plus `permission.asked`). Dropped: `session.status` (retry), `retry.scheduled`, `step.ended` (cost, tokens, snapshot, files), `tool.progress`, `tool.input.delta/ended`, `compaction.*`, `compacted`, `revert.*`, `forked`, `renamed`, `model.selected`, `agent.selected`, `skill.activated`, `shell.*`, `synthetic`, `message.content.updated`, `inbox.*`, `permissions`, `instructions.updated`, `metadata.updated`, `moved`, `deleted`, `viewed`, `usage.recorded`; `permission.replied`; all `form.*` | questions/elicitation/websearch forms hang; no cost, retry, compaction, revert, title, live tool progress |
| D2 | `:3634-3641` | any v2 frame without top-level `data.sessionID` (e.g. `form.created{form:{sessionID}}`, `mcp.*`, `command.updated`, installation/vcs/lsp) is discarded before the match | same as D1 for forms; no catalog refresh |
| D3 | `handle_bus_event` `_ => BusOutcome::Continue` `:3097` | v1 kinds other than session.status/error/warning/created/idle/interrupted, message.updated, message.part.updated/delta, permission.asked, question.asked: e.g. `todo.updated`, `session.diff`, `session.compacted`, `session.updated`, `message.removed`, `message.part.removed`, `permission.replied`, `question.replied/rejected`, `file.edited`, `lsp.*` (names per workstream B) | stale questions/approvals; no diff/todo/title updates |
| D4 | `:2577-2580` | `sync` frames and frames without `type` | — |
| D5 | session filter `:2582-2620` and child maps | foreign sessions (correct), **unbound children and all grandchildren** | nested/v2 subagent transcripts lost |
| D6 | `part_snapshot_events` `_ => Vec::new()` `:3364-3366` | `step-start`, `step-finish` (per-step cost/tokens), `snapshot`, `patch` (changed files), `file` (images/attachments out), `agent`, `subtask`, `retry`, `compaction` parts | checkpoints/file lists/image output/cost lost |
| D7 | `:3286-3292` | shorter/rewritten text snapshots | edited text keeps the stale version |
| D8 | `:2853-2855`, `:3465-3467` | deltas whose `field != "text"`; deltas targeting a tool part | streamed tool input never shown |
| D9 | `:2835-2837`, `:2862-2864` | child traffic after its chip settled (unless re-opened by a user message) | late child output lost |
| D10 | `:3997-4013` | non-UTF-8 SSE frames, non-JSON `data:` lines; framing only on `\n\n` (a `\r\n\r\n` server would never yield frames — E5) | — |
| D11 | `:2439-2461` | `/command` POST: response status never checked, transport error only `debug!` | failed command looks like a 60 s stall |
| D12 | `:3043-3093` | question reply/reject failure only `debug!`; `InputResolved` still sent | user believes the answer was delivered |
| D13 | `:2988-3006` | permission reply failure: v1 tries a fallback route, v2 swallows (`.is_err()` with no else) | tool waits natively, LUNA shows resolved |
| D14 | `:1729-1736`, `:1640` | command list and provider catalog failures → empty defaults | no commands; no variant/context window, silently |
| D15 | `:505-510` | protocol detection failure → assume V1 | wrong wire without a visible error |
| D16 | `:2689-2697` | error de-duplication by first-line containment | distinct errors sharing text are hidden |
| D17 | `:682`, `:712` | non-JSON success bodies → `Value::Null` | — |
| D18 | `:2047` | abort result ignored on the stall path | possible orphan turn (v2 replay, C-SES-01) |
| D19 | `:2668-2671` | `MessageAbortedError` (by design) | — |
| D20 | engine `sessions.rs:8783-8804` | harness-emitted `InputRequested` with native id (by design; the bridge's copy is used) | — |

## 4. Truncation and caps

| Cap | Value | Where | Effect for OpenCode |
| --- | --- | --- | --- |
| Tool output into the event stream | **4,096 B** + "…" | `mod.rs:3520-3532` | the binding cap: journal, sidecar, viewer all see ≤4 KiB |
| Transport/doc tool preview | 64 KiB | `crates/engine/src/sessions.rs:65-105` | moot after the 4 KiB cap |
| Diff preview | 64 KiB/field | `sessions.rs:80-87` | moot: OpenCode never sends `ToolDiff` |
| Tool activity preview | `luna_doc::TOOL_ACTIVITY_PREVIEW_BYTES` | `sessions.rs:106-118` | moot: no `ToolActivity` |
| Journal | none (whole file kept) | `run_journal.rs:10-11` | — |
| Error body text in messages | 300 B | `truncate_body` `mod.rs:1208-1222` | server error details clipped |
| Pending v2 tool map | 4,096 entries → run errors | `mod.rs:3630`, `:4014-4017` | — |
| Nested spawn index | 4,096 | `native_subagents.rs` `MAX_NESTED_SPAWNS` | — |
| Approval wait | 120 s (adapter `:2972-2976`; card expiry `:2949-2955`) | | slow user → silent `reject` |
| Stall (first event) | 60 s (`LUNA_OPENCODE_STALL_MS`) | `mod.rs:90-91` | — |
| Startup | 300 s (`LUNA_OPENCODE_STARTUP_TIMEOUT_SECS`) | `mod.rs:66-67` | — |
| Ordinary HTTP call | 60 s | `mod.rs:76` | cold `/provider` catalog |
| Compaction command | 30 s incl. boot | `rpc.rs:4845-4853` | see C-MOD-02 |
| Bus reconnect | 40 × 250 ms | `mod.rs:81-82` | — |
| Attachment size (UI) | 24 MiB | `crates/ui/src/attachments.rs:32` | not re-validated for OpenCode |
| `FETCH_TOOL_BLOB` UI timeout | 20 s | `transcript.rs:5810-5816` | — |

## 5. Unsupported content

- **Images in:** initial/fresh-run attachments sent as v1 file parts or v2 `{uri,name}` (`mod.rs:2323-2372`); refused on steer/queue (C-INP-01). No per-model image-capability check (catalog capabilities dropped, C-MOD-01).
- **Images out:** assistant `file` parts (v1) and non-text v2 tool/assistant content dropped (D6, `mod.rs:3764-3772`); `GeneratedImage` never emitted.
- **Resources:** MCP resources (`list_mcp_resources`/`read_mcp_resource` v2 tools) render as `Unknown` with input stripped; resource content beyond text dropped.
- **Files / @mentions:** typed mentions rejected (`provider_inputs.rs:26-30`); v2 `files[].mention`/`description`, `agents[]`, `skills[]` unused.
- **Diffs:** never produced (`diff: None`), v1 `metadata.diff`/v2 `edit` `files` dropped.
- **Structured tool output / metadata:** dropped on both wires.
- **Todos:** v1 `todowrite` input → `ToolCall::Todo` (lossy statuses); v1 `todo.updated` events and v2 todo state dropped.


## 6. Resolver and environment

**Where the engine runs (E2 + E1).** The UI embeds the engine in-process unless a daemon already listens on the IPC port (`crates/ui/src/state.rs:1-15`; `crates/ui/src/lib.rs:165-190`). Remote environments use `apps/luna-headless` over SSH (`apps/luna-headless/src/main.rs:1-20`). At 23:3x on 2026-10-06, `ps` showed `LUNA Dev.app/Contents/MacOS/luna` pid 15998 (ppid 1, started 23:34:54 — restarted since the baseline's pid 95440), no `luna-headless`, and **no `opencode serve` child** (E1). So OpenCode would be spawned by the app process itself.

**Which environment it sees (E2).** `scripts/run-macos-dev.sh:296-317` launches via `open -g -n --env LUNA_DATA_DIR=… --env LUNA_IPC_PORT=… [LUNA_WORKTREES_DIR, LUNA_OPEN_ROUTE]`, i.e. LaunchServices/launchd env plus those keys (and `LSEnvironment` keys written at `:209-218`). Baseline: launchd PATH is the default `/usr/bin:/bin:/usr/sbin:/sbin` (E1, `00-baseline.md`). The child PATH is composed as exe dir → process PATH → login-shell PATH (`crates/harness/src/lib.rs:496-522`). Spawn adds only `OPENCODE_SERVER_PASSWORD`, `OPENCODE_CLIENT=luna` (`mod.rs:526-529`).

**Resolution order (E2).** `OPENCODE_EXECUTABLE` if it is a file (`mod.rs:146-156`; **not** passed through `validate_native_override`, unlike Codex `codex/mod.rs:103,188-193` and Claude `claude/mod.rs:199-204` — the doc comment at `mod.rs:142-145` is stale) → `find_on_paths` (`executable.rs:81-89`): process PATH → login-shell PATH → install paths (`mod.rs:118-132`) → node-manager bins (`executable.rs:296-333`).

**Login-shell probe (E2).** `crates/harness/src/shell_env.rs`: captured **once per process** in a `OnceLock`, including a negative result (`:28-43`); prewarmed at registry build (`crates/engine/src/registry.rs:651`); shell = `$SHELL` → passwd → `/bin/zsh` → bash → sh (`:85-107`); flag sets zsh/bash `-l -i -c` then `-l -c` (`:126-138`); **5 s hard timeout per attempt** (`ATTEMPT_TIMEOUT` `:71`) + 250 ms flush grace (`:73`), so worst case ≈10.5 s; 2 MiB output cap (`:70`); `LUNA_NO_LOGIN_SHELL=1` disables (`:76-78`). The probe runs `/usr/bin/env` but keeps only `PATH`.

**Fresh timing (E1, this research, minimal `env -i HOME USER SHELL=/bin/zsh TERM=dumb`, stdin `/dev/null`).** `zsh -l -i -c 'command -v opencode'` → `~/.opencode/bin/opencode` (v2.0.22) in 0.10 s; `zsh -l -c …` → `/opt/homebrew/bin/opencode` (v1.18.34) in 0.05 s. Conclusion (E5, strongly supported): a launchd-started LUNA Dev resolves **v2.0.22** via the interactive probe; it would silently resolve **v1.18.34** only if the `-l -i -c` attempt yields no parseable PATH (timeout/rc failure) or `LUNA_NO_LOGIN_SHELL` is set, because login-shell PATH candidates precede the `~/.opencode/bin` install-path entry. A terminal-launched engine inherits the terminal PATH (v2 here). luna-headless over SSH would depend on that host's shells.

**Per-run re-resolution (E2).** Every `run`, probe, compaction and fork calls `resolve_executable()` (`mod.rs:264-271,277,385`); nothing pins a chat to a binary or records the version. There is no version gate: the only version use is the health discriminator (`mod.rs:474-489`). Verified versions in code comments are 2.0.3 and 1.18.31 (`mod.rs:12-14`); installed 2.0.22 is 19 patch releases newer and the mismatches found here (permission/fork payload keys, question→form, tool names, usage semantics) are consistent with v2 having drifted since or never matched (v2.0.0 already uses `permissions`/`action`/`resources`, E2).

**`OPENCODE_EXECUTABLE` exposure (E2).** No settings UI or persisted setting; only the process env (`rg OPENCODE_EXECUTABLE crates` → adapter + probe docs). With `open --env` launching, a user can only set it via `launchctl setenv` or `LSEnvironment`.

**Provider updates (E2, `crates/engine/src/provider_updates.rs`).** No v1/v2 notion. It resolves the same binary (`:29-36`), runs `--version` (3 s, `:15,56-69`) and compares against: Homebrew `brew info --json=v2` when the real path is under a Cellar/Caskroom (`:71-109,165-183`; Cellar installs are upgraded/queried as **`anomalyco/tap/opencode`** even if the formula came from elsewhere — E5 risk), otherwise npm `opencode-ai@latest` (`:111-135`). `~/.opencode/bin/opencode` maps to `opencode upgrade` (`:154-159`) but its "latest" still comes from npm `opencode-ai` — whether that dist-tag tracks v2 is unverified (§8). `update_available` only when latest > installed (`:267-270`).

**Environment exposure note.** The spawned server inherits the app's environment (including any provider keys in it) plus the login-shell PATH; this research did not read any process environment.

## 7. Test classification

None were run (hard limit). Versions are what the fixtures claim.

| Test / probe | Location | Class | Exercises | Version assumed |
| --- | --- | --- | --- | --- |
| Adapter unit + wire tests (≈45 fns) | `crates/harness/src/opencode/tests.rs` (fork wire `:7-113`, `TurnWire` fake server `:420-632`) | **fixture** (in-process fake HTTP/SSE servers; no binary) | fork remap/progress (v1 only), compaction route shapes, permission rules/allowance, queued idle, v2 text/tool/failure/interrupt settle, catalog decode, variants, prompt bodies (v1+v2), reasoning, role hold/replay, tool open/resolve, task spawn bind (v1 `task`), questions map, tool typing, commands map, env parsing, v2 normalize, v2 model fold, v2 permission routing (auto-approve, ownership), v2 tool scoping/overflow | health fixtures `1.18.31` (`:91,515`) and `2.0.3` (`:506`) |
| Context usage unit | `mod.rs:4034-4065` (`context_tests`) | fixture | cache inclusion, model limit | n/a |
| Gaps in fixtures | — | — | no v2 `form.*`, no v2 `shell`/`subagent` names, no v2 fork `before`, no v2 `permissions` body, no v2 checkpoint id assertion, no v2 permission prompt text/kind assertion, no compaction against a server | — |
| `opencode_models_probe` | `crates/harness/examples/opencode_models_probe.rs` | **real-binary probe** (manual `cargo run`) | boot + catalog | whatever PATH resolves; E4 `docs/research/model-discovery-reliability.md:48` (2026-09-24): "8 live models", version unrecorded |
| `opencode_turn_probe` | `crates/harness/examples/opencode_turn_probe.rs` | **real-binary probe** (manual; arg selects binary/model) | one full turn, exit 0 on Completed with text | header example uses `~/.opencode/bin/opencode` (v2 line) |
| `opencode_subagent_probe` | `crates/harness/examples/opencode_subagent_probe.rs` | **real-binary probe** with a **mock OpenAI-compatible provider** in `opencode.json` | task chip correlation + child transcript | v1 wire (`/global/event` in header) |
| Engine E2E (OpenCode-labelled) | `crates/engine/tests/e2e.rs:2516-2522`, `task_commands.rs:4827-4900`, `workspace_sync.rs:95-108,493-504`, `model_cache_lifecycle.rs:22,59`, `provider_scope_e2e.rs:26,145`, `pane_chat_mcp.rs:202`, `skill_library_e2e.rs` | **E2E with synthetic/scripted harness** (no adapter, no binary) | policy rejection (Auto), staged fork receipts, sync, model cache, provider scope listing, skill install | n/a |
| UI fixture E2E | `crates/ui/src/queue_steer_e2e.rs:1-110` (feature `appshots-fixture`), `pickers_project_defaults_tests.rs:123-132`, `pickers.rs` tests (`:8908-9870`) | **UI fixture** | queue primary action = Send now for OpenCode; defaults; picker attribution | n/a |
| Historical live QA | E4: `docs/qa/parallel-audit-2026-09-26.md:55-56` ("Claude/OpenCode lifecycle fault injection remains fixture coverage"); `docs/qa/agent-workspace-controls-2026-10-05.md:102-104` | — | — | — |

**Verification dimension for every trace in §1: unverified** (no retained real-binary evidence against v2.0.22 for any path; fixture coverage only, E2/E4).

## 8. Unresolved questions

1. Does v2.0.22 actually ignore the excess `permission` key on `POST /api/session` and PATCH (Effect Schema default), or reject it? Either way LUNA's ruleset is not applied; which one decides whether v2 runs fail outright. (Workstream B wire probe.)
2. Does `POST /api/session/{id}/compact` with a JSON `null` body pass payload decoding on v2.0.22? If it passes, does the admitted compaction run before LUNA kills the server, at the next boot, or never?
3. Ordering: is `session.usage.updated` always the last `message.updated`-shaped frame before `execution.succeeded`, making the persisted v2 checkpoint the literal `"usage"`? (Needs one live v2 turn with a fork attempt.)
4. Does v2 replay an orphaned execution after LUNA's SIGTERM→SIGKILL teardown, and could a home-dir model probe server resume it?
5. Does the v2 SSE stream include `session.status` retry frames on `/api/event` (ServerDefinitions lists `SessionStatusEvent`), and with what `data` shape?
6. v2 `/api/session/{id}/message` item shape vs `opencode_message_id` (`info.id` / `id`) used by fork readback.
7. v1 1.18.34 `/command` fields (hints/agent/source) and `question.asked` `custom` flag — confirm with workstream A/B to size C-EXT-01/C-PERM-02 losses.
8. npm `opencode-ai@latest` dist-tag: v1 or v2? Determines whether LUNA ever offers 2.0.23/2.0.24 for the `~/.opencode/bin` install and whether it misreports the Homebrew v1 install.
9. Does OpenCode (both lines) cascade a parent abort to running child sessions, given LUNA only aborts the root?
10. Whether the in-app login-shell probe in the real launchd environment (no TERM, possibly no SHELL) behaves like the `env -i` timing test here.

