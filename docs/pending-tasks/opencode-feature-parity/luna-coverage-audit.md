# LUNA OpenCode coverage audit

Research date 2026-10-06/07.

| Item | Pin |
| --- | --- |
| LUNA | `main` @ `9b3e9a2e`. HEAD advanced to `e6723e1b` (UI-only) during research; the OpenCode adapter is unchanged. Another session holds uncommitted edits in `crates/engine/src/{rpc.rs, sessions.rs, child_threads.rs, …}` and `crates/harness/src/lib.rs`; line refs use HEAD numbering. |
| OpenCode | v2.0.22 `527f0b93` (installed and released) |

Sources:
- [notes/C-luna-integration.md](notes/C-luna-integration.md): full traces.
- [notes/B-protocol-lifecycle.md](notes/B-protocol-lifecycle.md): wire contract.
- Coordinator re-checks, marked *(coordinator)*.

Abbreviations follow [capability-inventory.md](capability-inventory.md#conventions).

**What this audit is.** It traces source along LUNA's real production path, from adapter to engine, journal, doc, RPC and UI. Nothing was built, run or tested. Every Verification value is `unverified`; existing coverage is fixture-only (§9). "Native executes it" is never taken as proof that LUNA preserves its data or lets the user control it.

## 1. Production path

```
launchd / terminal / luna-headless
  └─ LUNA engine (in-process in LUNA Dev.app unless a daemon owns the IPC port; crates/ui/src/state.rs:1-15)
      └─ Harness::run (one per user turn after idle)                      oc:419-444
          ├─ resolve_opencode_executable()  (every run, probe, fork, compaction)   oc:146-156, :264-271
          ├─ Server::spawn: opencode serve --port <free> --hostname 127.0.0.1
          │     env OPENCODE_SERVER_PASSWORD=<uuid>, OPENCODE_CLIENT=luna; stdin null   oc:515-569
          │     readiness+generation = Protocol::detect (/api/health | /global/health)   oc:474-490, :576-597
          ├─ session create/resume + rules + set_model + command list + bus connect     oc:1614-1745
          ├─ POST prompt / command; GET /api/event SSE                                    oc:2418-2484, :3935-4031
          ├─ normalize_v2_frame (2.x → 1.x shapes) → handle_bus_event                    oc:3632-3855 → :2556-3098
          └─ AgentEvent stream (TextDelta, ReasoningDelta, ToolCall, ToolResult, Subagent, Usage,
             ContextUsage, Error, InputRequested/Resolved, Steered, ProviderCheckpoint, Done)
      ├─ engine run pump → journal (JSONL, uncapped) + broadcast                       ses:~8490-9950, run_journal.rs
      ├─ doc fold (MessagePart::*) with previews (tool 64 KiB, diff 64 KiB) and sanitize_tool_call   ses:65-130, parts:1023-1060
      ├─ RPC (FETCH_TOOL_BLOB → journal)                                               rpc:12233-12250, ses:1567-1598
      └─ UI (transcript, composer approval/question wizard, pickers, queue)            tr:1603-1830, crates/ui/src/composer.rs:680-775
```

## 2. Ranked findings

Severity: **S0** blocks every v2 run or approval; **S1** breaks a whole feature or silently leaves a safety setting unapplied; **S2** narrower breakage; **S3** fidelity loss. "WP" refers to [implementation-plan.md](implementation-plan.md).

| # | Finding | Sev | Evidence | WP |
| --- | --- | --- | --- | --- |
| G-0 | **v2 never becomes ready.** The readiness and generation probe uses the removed `/api/health` (since 2.0.4). `/global/health` returns HTML, so every spawn and discovery probe times out after 300 s, and attached mode is misdetected as v1. | S0 | E2 `oc:474-510,576-597`; OpenCode OpenAPI at `d44b52ca` (has `/api/health`) vs `466b3e594d`/`527f0b93`/`e7a34f09`/`4a0f273f` (absent; `/api/info` present) *(coordinator)*; `oc2:cli/src/services/web-ui.ts:17-54` | OC-02 |
| G-1 | **Approvals hang.** The reply sends `{reply}`; v2 requires `{decision}` (`required`, `additionalProperties:false`). The 400 is swallowed, the tool blocks and the turn hangs; the stall watchdog is already disarmed. | S0 | E2 `oc:2986-3006`; OpenAPI `session.permission.reply` *(coordinator)* | OC-03, OC-04 |
| G-2 | **Questions, MCP elicitation and the websearch provider picker hang invisibly.** v2 sends them as `form.created{form}` with no top-level `sessionID`; LUNA drops them before matching. | S1 | E2 `oc:3634-3641,3849`; `oc2:schema/src/form.ts:170` *(coordinator)*; `oc2:core/src/tool/plugin/question.ts:49-96`, `websearch.ts:36-75`, `mcp/index.ts:160-248` | OC-05 |
| G-3 | **LUNA's permission policy is never applied on v2.** It uses key `permission` (v2: `permissions`), v1 rule shape and v1 action names. v2 defaults govern Ask and Full; "Full" still auto-answers asks one at a time. | S1 | E2 `oc:2145-2175,2206-2226,1626-1634`; `oc2:core/src/permission.ts:86-97` | OC-03, OC-04 |
| G-4 | **Agents can bypass approvals (E5).** In `default` serve mode the server password stays in the environment that agent tools inherit, so an agent shell command could call the reply endpoint itself. | S1 | E2 `oc2:cli/src/server-process.ts:74-77`, `oc2:core/src/environment/exec-defaults.ts:83-84`; LUNA `oc:526-529` | OC-02 |
| G-5 | **Slash commands silently do nothing on v2.** LUNA sends `{command}`; v2 needs `{name}`. The status is never checked, so the turn stalls 60 s. Discovery is stale (cached for the app's lifetime, from a home-dir server) and attachments on the slash route are dropped. | S1 | E2 `oc:305-318,399-404,2418-2461` | OC-03, OC-14 |
| G-6 | **v2 tool vocabulary mismatch.** `shell` becomes `Unknown` and its command is stripped from the synced doc. `subagent` never registers a spawn, so **every v2 child transcript is dropped**; grandchildren are dropped on both wires. | S1 | E2 `oc:3534-3624,3130,3313,3339,2703-2706`; `parts:1056-1059` | OC-06, OC-11 |
| G-7 | **Fork is broken on v2.** The body key is wrong (`messageID`; v2 wants `before`). The saved checkpoint is probably the synthetic `"usage"` id. Readback is a newest-first 50-item page with the v1 shape. The result is an orphaned native fork. | S1 | E2 `oc:793-856,2737-2739,3805-3815,1128-1159`; E5 ordering | OC-03, OC-12 |
| G-8 | **Turns can run unseen after Stop or quit (E5).** SIGTERM without `interrupt` keeps the restart claim, and the user's managed background service (same DB) resumes such turns at its next boot. LUNA-owned servers don't sweep; corrected from C by the coordinator, E2 `oc2:server/src/process.ts:104-108,232-239`. | S1 | E2 `hl:607-631`; `oc2:core/src/session/execution/restart.ts:55-59` | OC-02, OC-08 |
| G-9 | **Tool output is cut to 4 KiB before the journal sees it.** Diffs, stdout/stderr streams, metadata, image and file content are dropped. MCP and plugin tools show only a name. The full-output viewer shows at most 4 KiB. | S2 | E2 `oc:3520-3532,3354-3367,3764-3772`; `parts:1023-1060` | OC-06 |
| G-10 | **Retry, finish-reason and turn-error visibility is lost on v2.** `session.retry.scheduled` is dropped, and LUNA's retry policy keys on a `session.status{retry}` that v2 never emits. `step.ended` finish reasons are dropped, error status and body are dropped, and an error after content settles as Completed. | S2 | E2 B §2.9, §3.2; `oc:2622-2657,1903-1921` | OC-07 |
| G-11 | **No delivery truth and no native steering.** The prompt response (inbox id) is discarded, and no caller id or `delivery` is sent. `session.inbox.*` and `session.synthetic` are dropped. The adapter advertises `TurnBoundary` although v2 steers natively at step boundaries. | S2 | E2 `oc:334-336,2478-2484`; `oc2:core/src/session/prompt.ts:50` | OC-07 |
| G-12 | **No replay or gap detection.** `durable.seq` is discarded; reconnect resyncs only liveness; pending asks and forms are never read back. | S2 | E2 `oc:3632-3640,2062-2077`; B §2.6 | OC-08 |
| G-13 | **Compaction reports false success.** It posts `null` (likely 400), records "completed" on admission, kills the server immediately, and the 30 s limit includes a cold boot. Compaction events are dropped. | S2 | E2 `oc:406-417,1161-1183`; `rpc:4845-4900` | OC-03, OC-10 |
| G-14 | **Usage, cost and context are wrong on v2.** The meter shows cumulative session tokens with no window. Cost is never shown. | S2 | E2 `oc:3795-3817,2264-2294`; `oc2:core/src/session/projector.ts:60-104` | OC-10 |
| G-15 | **Images are refused on steer and queue for OpenCode;** typed @file and skills are rejected; there is no admission check; paths are duplicated into the text. | S2 | E2 `pi:14-40`; `ses:972-977,5126-5130`; `oc:2358-2372` | OC-09 |
| G-16 | **No LUNA runtime contract or tools.** Delivery is `Unsupported`: no instructions, no `luna` MCP tools, so no agent-created child chats or workspace controls from OpenCode. | S2 | E2 `hl:228-230`; `crates/engine/src/runtime_contract.rs:212-216` | OC-13 |
| G-17 | **Silent binary and generation selection.** GUI LUNA picks v2 only while the interactive login-shell probe succeeds; otherwise it silently picks Homebrew v1 sharing the same data dir. Nothing is pinned per chat or recorded. The update checker has no v1/v2 notion. | S2 | E1 timings (C §6); E2 `crates/harness/src/shell_env.rs:28-43,71,126-138`, `oc:118-156,264-271`, `crates/engine/src/provider_updates.rs` | OC-00, OC-17 |
| G-18 | **Session state changes made by tools or other clients are invisible** (rename, move, delete, revert, agent and model selection). | S3 | E2 `oc:3849` | OC-12, OC-14 |
| G-19 | **The permission capability descriptor overstates v2 behaviour.** `supports_workspace_sandbox: true` and "session-scoped OpenCode permission rules" are not true while G-3 stands. It is latent: only a UI fixture reads the flag. | S3 | E2 `perm:51-60` | OC-04 |
| G-20 | **Native-fork side chats are offered for OpenCode and fail on v2.** The history capability is `Checkpoint`, so the workbench chooser offers `nativeFork`; the broken fork path (G-7) then fails the side chat and leaves an orphaned native fork. A user-visible failure in a shared surface. | S1 | E2 `crates/engine/src/history_operations.rs:21-29`; `crates/ui/src/shell/workbench/chooser.rs:390`; `crates/engine/src/side_chats.rs:349-433` (@`36cd741a`) | OC-12 |
| G-21 | **Snapshot side chats send the parent context as a quoted user-message prefix for OpenCode,** because it has no runtime-contract channel. Codex and Claude carry the seed in the stable instruction section. | S3 | E2 `crates/engine/src/side_chats.rs:772-801` (@`36cd741a`) | OC-13 |

## 3. Feature traces (condensed)

Each trace gives the path, what survives, what is lost, and the dimension statuses. The full per-step tables are in C §1.

### 3.1 Boot and discovery (F-LIFE-01, F-MOD-01)
- **Path:** `Server::spawn` → `Protocol::detect` (`oc:474-490`) loops every 150 ms until 300 s. `probe_models` and `probe_commands` (`oc:283-319`) boot their own servers with no cwd (home, or `/` under launchd).
- **Lost:** v2 never detected (G-0). Discovery location mismatch. Commands cached in a `OnceCell` (`oc:220,399-404`).
- **Fidelity** n/a · **Product** missing (v2) · **Verification** unverified.

### 3.2 Text and reasoning (F-OUT-01)
- **Path:** `session.text.*`/`session.reasoning.*` → `normalize_v2_frame` (`oc:3709-3735`), synthetic part ids `{msg}:t|r{ordinal}` → `part_snapshot_events`/`part_delta_events` (`oc:3220-3305,3442-3479`) → `TextDelta`/`ReasoningDelta` → journal → `MessagePart::Text`/`Reasoning` → transcript.
- **Lost:** rewritten snapshots (`oc:3286-3292`); part boundaries; `state`; text whose message role never arrives (`oc:3176-3197`).
- **Fidelity** partial · **Product** supported.

### 3.3 Tool calls and full output (F-OUT-02/03/04, F-TOOL-*)
- **Path:** `session.tool.input.started` (name) → `called` (input) → `success`/`failed` → `oc_tool_call` (`oc:3534-3624`) → `ToolCall` → `ToolResult{output ≤4 KiB, diff: None}` (`oc:3354-3367`) → preview (64 KiB) and `sanitize_tool_call` (removes `Unknown`/`Mcp` input) → `FETCH_TOOL_BLOB` returns the same ≤4 KiB → viewer.
- **Lost:** see G-6 and G-9. Tools the adapter doesn't recognise: `list`, `todoread`, `multiedit`, `lsp*`, `codesearch`, `skill`, `batch`, `question`, plugins, all MCP tools, and v2 `shell`/`subagent`/`execute`/`opencode_*` → `Unknown`.
- **Fidelity** partial · **Product** partial.

### 3.4 Permission ask → approval → reply (F-PERM-01..03, 13..15)
- **Path:** rules out (`oc:2145-2175`, wrong on v2). `permission.asked` is passed through (`oc:3840-3848`) → adapter reads `permission`/`patterns` (`oc:2906-2921`) → engine `request_input` bridge (`ses:4832-4871`) with a 120 s expiry (`ses:715-770`) → composer approval (`crates/ui/src/composer.rs:719-775`) → reply `{reply}` (`oc:2986-3006`).
- **Lost:**
  - On v2, the kind, target, metadata, `source` call link and `message` (ask reads "OpenCode requests tool permission").
  - Deny, Cancel and timeout all send a bare `reject`.
  - `permission.replied` dropped.
  - Child asks shown without child attribution.
  - The reply 400s (G-1).
- **Fidelity** missing (v2) / partial (v1) · **Product** missing (v2: hang) / partial (v1).

### 3.5 Questions and MCP elicitation (F-PERM-07/08, F-TOOL-10)
- **Path:** v2 `form.created` is dropped (`oc:3634-3641`). The v1 `question.asked` path (`oc:3009-3094`, `map_questions` `:3481-3518`) maps into the engine bridge and shared wizard (`crates/ui/src/composer.rs:680-718`). v1 replies go to `/question/{id}/reply`; failures are logged at debug only.
- **Lost:** everything on v2. On v1: option descriptions (LUNA has `UserInputDetails.option_descriptions`), child origin, the MCP input model (`UserInputQuestion.mcp` is never filled, `oc:2932,3509`).
- **Fidelity** missing (v2) · **Product** missing (v2).
- **Reuse:** the shared question panel and the MCP form contract (`docs/reference/mcp-elicitation.md`) already support schema-validated fields, a pending map, retransmit dedupe, limits, expiry and URL secrecy. The contract binding is Codex-only ("active native Codex thread and turn"), so OC-05 generalizes the binding rather than adding a new dialog.

### 3.6 Images and files (F-INP-05/06/07)
- **Path:**
  - UI staging appends path text (`crates/ui/src/attachments.rs:55-70`).
  - Engine `provider_inputs::admit` (`pi:14-34`): OpenCode images are not validated; typed `input_items` are rejected.
  - Steer and queue gates (`pi:38-40`, `ses:5126-5130`, `crates/engine/src/doc_host.rs:5660-5668`, `rpc:3434-3437`) and warm routing (`ses:972-977`) push images onto a fresh run.
  - Adapter `prompt_body_v2` `{text, files:[{uri, name}]}` (`oc:2358-2372`); steers post `attachments: &[]` (`oc:1872-1884`).
- **Lost:** images on steer and queue; typed mentions and skills; `description`/`mention`; output images.
- **Fidelity** partial · **Product** partial.

### 3.7 Slash commands and skills (F-INP-11, F-EXT-08)
- See G-5. Skills install to OpenCode directories (`crates/engine/src/integration_settings.rs:131-140`) but are never discovered back. `SlashCommandKind::Skill` is never set, and the composer shows skills for Codex only (`crates/ui/src/composer.rs:4712-4721`).
- **Fidelity** partial · **Product** missing (v2 execution) / partial (v1).

### 3.8 Models, variants, effective config (F-MOD-01..03, 11)
- **Path:** `GET /api/model` → `catalog_from_v2_models` (`oc:1406-1450`) → picker. Variants map to effort only for `minimal…max` (`oc:161-198`). `set_model` is called once per run (`oc:935-952`). `SessionStarted.model = request.model` (`oc:1712-1720`).
- **Lost:** capabilities, cost, provider display name, custom variants, effective-model and agent readback (`session.model.selected` and `agent.selected` are dropped).
- **Fidelity** partial · **Product** partial.

### 3.9 Usage, cost, context, compaction (F-OUT-07, F-MOD-06/07)
- See G-13 and G-14. The engine's live accounting reads only `ProviderAccounting` (`crates/engine/src/live_usage.rs:1-14`), which OpenCode never emits. Historical usage is explicitly unsupported (`crates/engine/src/usage.rs:421-424,1632-1640`).
- **Fidelity** missing (v2 context and cost) · **Product** partial.

### 3.10 Turn lifecycle, steering, stop, retry, stall (F-TURN-*)
- **Path:**
  - Steering mode `TurnBoundary` (`oc:334-336`) → engine offers after-turn steer and queue "Send now" (`crates/ui/src/queue.rs:85-96`).
  - Queued steers are held in the adapter until idle (`oc:1825,1866-1896,1999-2001`).
  - Turn end = first execution terminal after busy (`oc:2599-2618`).
  - Stop → interrupt with a 5 s timeout, 2 s grace, then teardown (`oc:1942-1972`).
  - Retry: v1 only (`oc:2622-2657`).
  - Stall: first event only (`oc:1925-1934,2029-2060`).
- **Lost:** inbox correlation; interrupt reason; error status and body; retries (v2); finish reasons; post-activity stalls.
- **Fidelity** partial · **Product** partial.

### 3.11 Native subagents (F-AGT-06/07/14)
- **v1 path:** `task` part → spawn chip, `register_spawn` by `metadata.sessionId`, then title prefix, then FIFO (`oc:3375-3440`) → `session.created{parentID}` → `bind_child` (`oc:3149-3174`) → child feed wrapped in `AgentEvent::Subagent` (`oc:2523-2528`) → engine per-subagent docs `{chat}--sub--{id}` (`ses:7446-7470,8535-8600`) → agent cards.
- **v2:** `subagent` never matches, so all children are unbound and dropped (G-6). Background children and synthetic completions are invisible. Child usage isn't emitted. Stop aborts only the root (`oc:2530-2554`).
- **Fidelity** missing (v2) / partial (v1) · **Product** missing (v2) / partial (v1, one level).

### 3.12 Fork, rewind, checkpoints (F-SES-06/16)
- **Path:** `fork_capability = Checkpoint` (`oc:351-353`); checkpoint = last assistant `message.updated` id (`oc:2737-2739`) → `ProviderCheckpoint` → persisted on completed entries (`ses:9866-9871`); fork `{messageID}` (`oc:793-856`) → readback mapper (`oc:1128-1159`).
- **UI:** fork from assistant rows (`tr:7698-7705`); user-row fork refused (`hops:165-170`); rewind and edit gated to Codex/Claude (`tr:7728-7760`); file rewind Claude only.
- **Lost:** see G-7. Revert events dropped.
- **Fidelity** missing (v2) · **Product** partial (fork) / gated (rewind, checkpoints).

### 3.13 Resume, restart, reopen, import (F-SES-02/04/15)
- Resume reuses the native session and fails visibly on a lookup error (E4 fix 2026-09-26).
- Reopened history comes only from the LUNA doc and journal, with no native backfill (`ti:931-940`).
- Native import is Codex-only (`ti:1402-1406`).
- A mid-turn app restart stamps the entry aborted; the native turn dies with the server (`kill_on_drop`) but its claim survives (G-8).
- **Fidelity** partial · **Product** partial.

### 3.14 Remote and multi-device (F-WS-11)
- OpenCode is always a local child of whichever engine owns the run (luna-headless on remote hosts). There is no attach path in production (`oc:252-256`) and external servers are called unsupported (`crates/engine/src/usage.rs:1249`).
- The binary and generation on the remote host are unknown (OCF-08).
- **Product** partial.

### 3.15 LUNA runtime contract and tools (F-EXT-12, F-AGT-12/13)
- `luna_runtime_delivery()` keeps its default `Unsupported` (`hl:228-230`). The runtime contract is compiled only for Codex and Claude (`ses:4496-4530`). `invoke_luna_tool` is never read.
- Native v2 has the pieces: runtime `PUT /api/experimental/mcp/{server}` is an in-memory per-location override that doesn't write the user's config (*coordinator*, E2 `oc2:core/src/mcp/index.ts:635-639`). Session instruction entries exist (experimental). T3 uses both (D §4.1).
- **Product** missing.

## 4. Shared-feature provider gates (OpenCode)

From C §2, re-reviewed. "Supported" means the gate admits OpenCode, not that the path is verified.

| Shared LUNA feature | OpenCode status | Gate (`file:line`) | WP |
| --- | --- | --- | --- |
| Run / resume / stop | supported (behind G-0 on v2) | `reg:747-766`; `perm:319-321` | OC-02 |
| Permission modes | Ask and Full; **Auto unavailable** (no native automatic reviewer) | `perm:51-66,200-205`; `crates/ui/src/pickers.rs:1746-1748` | OC-04 |
| "Allow for task" | v1 only at runtime; engine capability false | `oc:2177-2191`; `perm:57-59` | OC-04 |
| Mid-turn steer | unavailable (`TurnBoundary`) | `reg:48-50,755`; `crates/engine/src/sessions_execution.rs:404-419` | OC-07 |
| Images or typed inputs on steer and queue | gated (Codex/Claude) | `pi:38-40`; `ses:5126-5130`; `rpc:3434-3437`; `crates/engine/src/doc_host.rs:5660-5668` | OC-09 |
| Typed composer selections | rejected | `pi:26-30` | OC-09 |
| Warm routing with attachments | fresh run instead | `ses:972-977` | OC-09 |
| Native compaction | `ProviderCommand` (no exclusive transition) | `crates/engine/src/context_controls.rs:13-18`; `rpc:4792-4853` | OC-10 |
| Provider fork | assistant rows and tip | `hops:21-28,165-170`; `tr:7698-7705` | OC-12 |
| Edit earlier message / rewind | gated (Codex/Claude) | `tr:7728-7760` | OC-12 |
| Native file checkpoints | gated (Claude) | `crates/engine/src/rpc/claude_history.rs:1-8` | OC-12 |
| Native session import and reconciliation | gated (Codex) | `ti:931-940,1402-1406` | OC-12 |
| Goals | gated (Codex/Claude); native unavailable | `ses:4258-4264`; `crates/ui/src/composer/goals.rs:53-60` | — (correct) |
| Native review | gated (Codex native; Claude via its `/code-review` command line). **Target: like Claude, via OpenCode `/review`** | `ses:4283-4303`; `crates/ui/src/shell/native_review.rs:50` | OC-15 |
| LUNA instructions and tools | missing (`Unsupported`) | `hl:228-230`; `crates/engine/src/runtime_contract.rs:212-216,299` | OC-13 |
| OpenCode as child-chat destination | **offered** (installed and enabled), blocked in practice by G-0..G-3 | `crates/engine/src/cross_task_tools.rs:33-37`; `crates/engine/src/child_threads.rs:119-264` (@`36cd741a`) | OC-02..04, OC-10 |
| OpenCode as child-chat parent (`create_thread`) | gated: no LUNA tool delivery | `hl:228-230`; E4 `docs/qa/child-chats.md:10` | OC-13 |
| Side chats from an OpenCode parent | offered; native fork fails on v2 (G-20); snapshot seed as user prefix (G-21); read-only parent refused like Claude | `crates/engine/src/side_chats.rs:426-433,772-801`; `crates/ui/src/shell/workbench/chooser.rs:390` | OC-12, OC-13 |
| Account selection | gated (Codex; Claude for execution) | `ses:3712-3716`; `crates/engine/src/sessions_execution.rs:116-126` | OC-16 |
| Read-only side chats / children | refused (no sandbox), which is correct | `crates/engine/src/sessions_execution.rs:183-187`; `crates/engine/src/side_chats.rs:426-432` | — |
| Integrations catalog | gated (Codex) | `rpc:10490-10498`; `crates/ui/src/composer/integrations.rs:396` | OC-14 |
| Skills in composer | Codex only | `crates/ui/src/composer.rs:4712-4721` | OC-14 |
| Live cost rows | missing | `crates/engine/src/live_usage.rs:1-14` | OC-10 |
| OpenCode Go limits | supported (reads the credential DB) | `crates/engine/src/usage_opencode.rs:30-50` | OC-16 |
| Strict resume, native thread name, client message ids, delivery-unknown steer guard | Codex only | `ses:4689-4693,4737-4810` | OC-07 |
| Conversation deep link | gated (Codex) | `crates/ui/src/links.rs:75` | OC-14 (P3; F-SES-19) |
| Historical usage scan | unsupported, with an outdated note that T3/BB lack it | `crates/engine/src/usage.rs:421-424,1625-1645` | OC-10 (F-MOD-15): adopt T3's read-only database scan |
| Multiple pending steers | single-flight for non-Codex | `ses:5172-5174` | OC-07 |
| Permission capability descriptor | **untruthful on v2**: `supports_workspace_sandbox: true`, "session-scoped OpenCode permission rules", while the rules are never applied (G-3) | `perm:51-60` | OC-04 |
| Other OpenCode-specific UI | skill library install (filesystem, supported); accounts card (usage only); picker cold-boot retry; 60 s harness-settings probe timeout; provider updates (same resolver) | C §2 | OC-14, OC-16, OC-17 |

## 5. Silent drops

All 20 drop sites are in C §3 (D1–D20). The material ones:
- `normalize_v2_frame` drops every unmapped v2 type (`oc:3849`) and any frame without a top-level `sessionID` (`oc:3634-3641`).
- The v1 router catch-all (`oc:3097`).
- Unmapped part types (`oc:3364-3366`).
- Rewritten snapshots (`oc:3286-3292`).
- Command POST status ignored (`oc:2439-2461`).
- Question and permission reply failures swallowed (`oc:3043-3093`, `oc:2988-3006`).
- Detection failure → assumed v1 (`oc:505-510`).
- Catalog and command failures → empty defaults (`oc:1640,1729-1736`).
- Unparseable SSE lines skipped (`oc:4009-4011`).

## 6. Truncation and caps

| Cap | Value | Where | Effect |
| --- | --- | --- | --- |
| Adapter tool output | **4,096 B** + "…" | `oc:3520-3532` | Binding cap for the journal, sidecar and viewer |
| Doc preview | 64 KiB (tool), 64 KiB/field (diff) | `ses:65-118` | Moot behind the 4 KiB cap |
| Error body | 300 B | `oc:1208-1222` | Provider diagnostics clipped |
| Pending v2 tool map | 4,096 entries → run errors | `oc:3630,4014-4017` | — |
| Approval wait | 120 s → silent `reject` | `oc:2949-2976` | A slow user loses the turn |
| Stall | 60 s, first event only | `oc:90-116` | Later blocks never time out |
| Startup | 300 s | `oc:66-67` | G-0 symptom |
| Compaction RPC | 30 s including boot | `rpc:4845-4853` | False failure or false success |
| Bus reconnect | 40 × 250 ms | `oc:81-82` | Then the run fails |
| UI attachment | 24 MiB (native 20 MiB) | `crates/ui/src/attachments.rs:32` | No re-validation |

## 7. Resolver and environment

- **Order:** `OPENCODE_EXECUTABLE` (an existing file; **not** passed through `validate_native_override`, unlike Codex and Claude; the doc comment at `oc:142-145` is stale) → process PATH → login-shell PATH → install paths (`~/.opencode/bin` first, then `~/.local/bin`, `~/.bun/bin`, `~/.npm-global/bin`, `/opt/homebrew/bin`, `/usr/local/bin`) → node-manager bins (`crates/harness/src/executable.rs:296-333`).
- **Login-shell probe:** once per process, cached even when it fails (`crates/harness/src/shell_env.rs:28-43`). `-l -i -c` first, then `-l -c`, 5 s per attempt (about 10.5 s worst case). `LUNA_NO_LOGIN_SHELL` disables it.
- **Timings (fresh E1):** `zsh -l -i -c` → `~/.opencode/bin/opencode` (v2.0.22) in 0.10 s; `zsh -l -c` → `/opt/homebrew/bin/opencode` (v1.18.34) in 0.05 s.
- **Launch:** `scripts/run-macos-dev.sh` launches with `open --env`, so LUNA Dev runs with launchd env plus `LUNA_*` keys. Its process PATH has no opencode.
- **Conclusion:** v2 normally; **silently v1 if the interactive probe fails** (E5, strongly supported). Terminal-launched engines inherit the terminal PATH. luna-headless hosts are unknown.
- **No pinning:** each run, probe, fork and compaction re-resolves; no version is recorded or gated. The desktop OpenCode app merges the login-shell environment before spawning its CLI (A §4.3, `oc2:desktop/src/main/lifecycle/environment.ts:20-33`), which is a reference pattern for consistent resolution.
- **Update checker** (`crates/engine/src/provider_updates.rs`): has no v1/v2 notion. It compares `~/.opencode/bin` against npm `opencode-ai@latest` (dist-tag unknown) and assumes Homebrew installs are `anomalyco/tap/opencode`.

## 8. Data ownership and security notes

- **Password leak (G-4):** fixed by `--stdio`.
- **Credential read:** `usage_opencode.rs` reads a credential from `~/.local/share/opencode/opencode.db` (`:30-50`). D marked the equivalent BB pattern as not to adopt. **Decided (PD-17): keep it as is**; don't copy the pattern elsewhere. Earlier wording: decide whether LUNA keeps this (Q-15).
- **Shared storage:** v1 and v2 share `~/.local/share/opencode` unless invoked via the `opencode1` wrapper. The historical v1 lazy-migration failure is recorded at `67c960f4`. QA runs need OpenCode storage isolation (`XDG_*`, `OPENCODE_CONFIG_DIR`, `OPENCODE_DB`, `OPENCODE_DISABLE_PROJECT_CONFIG`), which `docs/reference/qa-infrastructure.md` doesn't document yet.
- **Spawn environment:** the spawned server inherits the app's environment (possibly provider keys) plus the login-shell PATH. This research read no process environment.

## 9. Existing test and probe coverage

| Asset | Class | Version assumed | Gaps |
| --- | --- | --- | --- |
| `crates/harness/src/opencode/tests.rs` (about 45 fns; fake HTTP/SSE `TurnWire`) | T0 fixture | health 1.18.31 and 2.0.3 | No v2 `form.*`, no `shell`/`subagent`, no `before` fork, no `permissions` body, no checkpoint-id assertion, no permission prompt text/kind, no compaction against a server, no `/api/info` |
| `crates/harness/tests/opencode.rs` | fixture | — | as above |
| `examples/opencode_turn_probe.rs` | manual real binary | 2.0.3 (E4 `67c960f4`) | not re-run; harness only |
| `examples/opencode_models_probe.rs` | manual real binary | unrecorded ("8 live models", 2026-09-17) | — |
| `examples/opencode_subagent_probe.rs` | real binary + mock OpenAI-compatible provider | v1 wire | v2 variant needed |
| Engine E2E (`crates/engine/tests/e2e.rs:2516-2522`, `task_commands.rs:4827-4900`, `workspace_sync.rs`, `model_cache_lifecycle.rs`, `provider_scope_e2e.rs`, `pane_chat_mcp.rs:202`, `skill_library_e2e.rs`) | synthetic harness labelled OpenCode | n/a | not the adapter |
| UI fixtures (`crates/ui/src/queue_steer_e2e.rs`, picker tests) | UI fixture | n/a | — |
| Live QA scripts (`scripts/qa-*.mjs`, 25) | — | — | **none accept OpenCode** (D §5.2) |

## 9a. Parity refinement (PD-14, 2026-10-07)

The user clarified that parity means OpenCode should behave like Codex and Claude through LUNA's existing shared mechanisms, without OpenCode-only features. [provider-parity-contract.md](provider-parity-contract.md) compares the shared trait hooks and events provider by provider and specifies subagents (§2), side chats (§3), child chats (§4) and LUNA tool delivery (§5) against what Codex and Claude do. Consequences for this audit:
- **New gaps:** G-20 and G-21 (side chats).
- **Gate table corrected:** OpenCode is already a child-chat destination; the parent path stays gated until OC-13. Review's target is Claude's command-line route.
- **Shared hooks OpenCode lacks** compared with Claude: `luna_runtime_delivery`, `can_reconfigure_live`/`stage_live_reconfiguration`, `native_history_turns`/`rewind_native_conversation`, `rewind_native_files`, `models_live`, `commands_in`, `mcp_control`/`session_mcp_observation`/`native_extensions`, `stop_native_child` and `run_title`. Shared events it never emits: `ChildStatus`/`ChildProgress`, `NativeTaskChanged`, `ProviderTurnStarted`, `ToolActivity`, `ProviderAccounting`, `ContextCompaction`, `ProviderNotice`, `GeneratedImage`.
- **Not pursued:** OpenCode-only features (contract §7). Their silent drops are fine once bounded diagnostics exist; `opencode_session_move` is contained by a deny rule.

## 10. Corrections to earlier claims

| Earlier claim | Correction | Evidence |
| --- | --- | --- |
| Adapter header: "2.x `/api/*` wire (verified against 2.0.3)", readiness via `/api/health` (`oc:12-18`) | Accurate for 2.0.3 only; every release from 2.0.4 on breaks boot | OpenAPI per tag *(coordinator)*; B §3.5 |
| Orchestrator plan: "OpenCode adapter supports both 1.x and 2.x wire layouts — Existing" (`docs/pending-tasks/orchestrator-v2-and-bb-2026-10-04/luna-integration-plan.md:27`) | True in structure. Behaviourally broken for every installed-era v2 release | G-0..G-7 |
| Adapter header: ACP "settles a prompt on the FIRST `session.status{idle}`" | Describes 1.18 ACP. v2 ACP correlates via `session.inbox.delivered`. HTTP+SSE still wins on fidelity. | B T6 |
| C: "the next boot of any LUNA-owned server may resume a stopped turn" | Only the **managed service** sweeps claims in Node `serve`; `fetch.ts` resume applies to the embedded SDK. The hazard is the user's background service resuming LUNA's turn. | `oc2:server/src/process.ts:104-108,232-239` *(coordinator)* |
| C: "Auto permission mode … excluded-by-product-decision" | Prior LUNA docs call it "Unavailable: no verified native automatic reviewer" (`docs/qa/thread-approvals-2026-10-03.md:37`). It is a capability gap, not a product decision. | E4 |
| `docs/reference/native-composer-inputs.md:5`: "Claude/OpenCode retain their existing image handling" | Existing OpenCode handling refuses images on steer and queue and adds no admission check | `pi:14-40` |
| `oc:3519` comment: "journal keeps the rest" | False for OpenCode: the cap applies before the event exists | `oc:3520-3532` |
| `oc:142-145`: override "validated at launch through `validate_native_override`" | Not called for OpenCode | C §6 |
| First delivery: "OpenCode as cross-provider child destination: gated" (F-AGT-13) | Already offered (`CHILD_PROVIDERS`). The cited QA note is about *creating* providers. | `crates/engine/src/cross_task_tools.rs:33-37` @`36cd741a` |
| `crates/engine/src/usage.rs:1636`: "OpenCode historical session usage is not in the T3/BB usage adapters" | Outdated: T3 reads OpenCode history since `e5a46d6c5d` (2026-09-25) | T3 `apps/server/src/usage/opencodeUsageReader.ts` @`4ee6bfd5` (added `e5a46d6c5d`, 2026-09-25, #10409) |
| First delivery: "agent-card model label Codex-only" | Other providers show the raw model id; OpenCode just needs to emit `ChildProgress{model}` | `crates/ui/src/transcript/agent_cards.rs:75-100` |
| Line references for `crates/harness/src/lib.rs`, `crates/engine/src/sessions.rs` and `rpc.rs` | Pinned at `9b3e9a2e`. Later commits and other sessions' uncommitted edits shift them; for example the `luna_runtime_delivery` default is `lib.rs:228-230` at the pin and `:232-234` at `346f6825`. Re-resolve before editing. | E's review §7 S9; baseline JSON |
