# OpenCode provider-parity contract

Refinement of 2026-10-07, at the user's direction:

> "have open code be on track with our other providers … behave the same rather than having custom features tailored specifically for open code … make it compatible with what we have."

**Definition used from here on (PD-14).** *OpenCode parity* means that an OpenCode chat works through **the same LUNA mechanisms Codex and Claude Code already use**: the same harness trait hooks, `AgentEvent` variants, engine gates and UI surfaces, with the same user-visible behaviour wherever OpenCode natively has an equivalent capability.
- OpenCode-native features with **no LUNA counterpart** are **not pursued**. They're recorded so they aren't rediscovered, but they get no package, UI or scenario.
- OpenCode-native APIs are used only as the *backing* for an existing shared LUNA surface. Examples: the prompt inbox backs shared steering, Forms back the shared question panel, `OPENCODE_CONFIG_CONTENT` backs shared LUNA tool delivery.
- Where Codex and Claude differ, OpenCode follows the **provider-neutral hook** (usually the Claude path, which goes through `Harness` trait methods rather than Codex-specific RPC). Where *neither* has a native counterpart, the feature stays unavailable for OpenCode too.

LUNA pin for this refinement: `36cd741a` (main). The OpenCode adapter is unchanged since `9b3e9a2e`. Line references below are at `36cd741a` unless marked. OpenCode source is v2.0.22 (`527f0b93`, `oc2:` = `packages/`).

This document supersedes the scope of the earlier backlog wherever they conflict. [implementation-plan.md](implementation-plan.md), [validation-plan.md](validation-plan.md) and [capability-inventory.md](capability-inventory.md) have been re-scoped to match.

## 1. The shared provider surface (what "behave the same" means in code)

LUNA describes a provider to the engine and UI through:
- the `Harness` trait (`crates/harness/src/lib.rs:228-470`)
- the runtime capability record `ProviderExecutionCapabilities` (`crates/engine/src/sessions_execution.rs:319-333`): steering mode, turn-end guarantees, safe recovery, native fork, runtime delivery, permission capabilities
- the static descriptor in `crates/engine/src/registry.rs:697-770`

Shared trait hooks, and which adapters implement them (script over the three adapters at `36cd741a`):

| Hook (`crates/harness/src/lib.rs`) | Codex | Claude | OpenCode | Shared LUNA surface it powers |
| --- | --- | --- | --- | --- |
| `steering_mode` / `supports_steering` | StepBoundary | StepBoundary | **TurnBoundary** | Queue "Steer" vs "Send now"; mid-turn steering |
| `deterministic_turn_end` | ✓ | ✓ | ✓ (claims it; wakes not handled) | Turn settling, watchdog retirement |
| `luna_runtime_delivery` | `CodexMcpTools` | `ClaudeMcpTools` | **default `Unsupported`** | LUNA tools, runtime contract, agent-created child chats, workspace controls |
| `supports_safe_recovery_continuation` | ✓ | — | — | Strict recovery resume |
| `fork_capability` / `fork_session_staged` | Checkpoint | Checkpoint | Checkpoint (**broken on v2**) | Fork, native side chats |
| `can_reconfigure_live` / `stage_live_reconfiguration` | — | ✓ | — | Live model/effort change without a restart |
| `native_history_turns` / `rewind_native_conversation` | (Codex RPC path) | ✓ | — | Edit earlier message, same-chat rewind |
| `rewind_native_files` | — | ✓ | — | Native file-checkpoint rewind |
| `models_live` / `models_for_execution` | partial (`models_for_execution`) | ✓ (`models_live`) | — | Picker, child model catalog |
| `commands_in(scope)` | — | ✓ | — (global, home-dir cache) | Per-project slash commands |
| `mcp_control` / `session_mcp_observation` / `native_extensions` | (Codex integration catalog) | ✓ | — | Integration inventory, MCP status |
| `compact_context` | (Codex native compaction path) | — | ✓ (broken on v2) | Context compaction control |
| `stop_native_child` | — | ✓ | — | Per-child Stop in the nested transcript |
| `native_goal_control` | (Codex `thread/goal`) | ✓ (native `/goal`) | — | Goals |
| `run_title` | ✓ | ✓ | — | Chat titles |

Shared `AgentEvent` variants and who emits them (`crates/proto/src/agent.rs`; counts of emission sites per adapter dir):

| Variant | Codex | Claude | OpenCode | Surface |
| --- | --- | --- | --- | --- |
| `Subagent{parent_tool_use_id, event}` | ✓ | ✓ | ✓ (v1 only in practice) | Nested subagent transcripts |
| `ChildStatus` / `ChildProgress` | ✓ / — | — / ✓ | — / — | Agent card status, model, tokens, last tool |
| `NativeTaskChanged` (kinds `Shell`, `Workflow`, `Teammate`, `RemoteAgent`; `crates/proto/src/native_tasks.rs:24-34`) | — | ✓ | — | Background work rows |
| `ProviderTurnStarted` | ✓ | ✓ | — | Provider-initiated turns (background wakes) |
| `ToolActivity` | ✓ | ✓ | — | Live tool output |
| `ProviderAccounting` | — | ✓ | — | Cost rows |
| `ContextCompaction` | ✓ | — | — | Compaction lifecycle |
| `ProviderNotice` (retrying, etc.) | ✓ | — | — | Temporary retry notices |
| `GeneratedImage` | ✓ | — | — | Image output |

The parity target for OpenCode is to fill the gaps in these two tables **using OpenCode-native backing**, so the engine and UI treat it exactly like the provider whose hook it copies.

## 2. Native subagents: handle them like Claude and Codex

**How LUNA handles native subagents today (shared contract):**
1. **Spawn decode.** Every driver decodes its spawn tool into `ToolCall::Unknown{name: "Agent: <description>"}`. `ToolCall::is_subagent_spawn` is the single gate (`crates/proto/src/agent.rs:703-719`; the doc comment lists "claude/codex `Task` … opencode `task`").
2. **Child traffic** is wrapped in `AgentEvent::Subagent{parent_tool_use_id, event}` (`agent.rs:1244-1247`). The engine routes it to a per-subagent doc `{chat}--sub--{id}` with a nested-spawn index (`crates/engine/src/sessions/native_subagents.rs`). The UI shows agent cards and nested transcripts.
3. **Status and progress.** Codex emits `ChildStatus` (sub-thread status). Claude emits `ChildProgress{model, total_tokens, tool_uses, duration_ms, last_tool, summary}` (`agent.rs:1064-1090`). Agent cards show the child model: Claude labels from its catalog, Codex from a pattern, any other provider shows the raw id (`crates/ui/src/transcript/agent_cards.rs:75-100`).
4. **Per-child Stop.** Claude implements `stop_native_child` (`crates/harness/src/claude/native_stop.rs`). The engine checks the launching runtime and forwards it (`crates/engine/src/sessions_work.rs:70-92`).
5. **Background work.** Claude reports background shells and workflows as `NativeTaskChanged` (`local_bash` → `NativeTaskKind::Shell`, `crates/harness/src/claude/coordination.rs:69`). Background-agent wakes become `ProviderTurnStarted` turns. `deterministic_turn_end` requires that even agent-initiated wakes end with a `Done` (`lib.rs:243-249`).
6. **LUNA tools are never callable from a subagent.**
   - Codex: the `luna_mcp::dispatch` fence requires the exact **root thread** and active turn (`crates/harness/src/codex/luna_mcp.rs:458-461`).
   - Claude: "a subagent's (tagged) tool use is refused like a Codex sub-thread" (`crates/harness/src/claude/luna_tools.rs:1-30`).

**OpenCode today.**
- v1: the `task` spawn decodes to `Agent: …` and direct children are bound by title prefix or FIFO.
- v2: the tool is `subagent`, so **no spawn is registered and every child transcript is dropped**.
- No `ChildStatus`/`ChildProgress`, no `stop_native_child`, no background rows, no `ProviderTurnStarted` for wakes. Grandchildren are dropped on both wires (C-AGT-01, G-6).

**OpenCode target (same contract, no OpenCode-specific UI):**

| Shared step | OpenCode backing |
| --- | --- |
| Spawn decode | v2 `subagent` (v1 `task`) → `ToolCall::Unknown{name: "Agent: <description>"}`, chip id = call id |
| Child binding | Tool `metadata.sessionID` when present, else `session.created{parentID}`; any depth (engine nested index) |
| Child traffic | Child-session `session.*` events wrapped in `Subagent{parent_tool_use_id}`; child tool ids prefixed so they can't collide with parent ids |
| Status | `ChildStatus` from child `session.execution.started/succeeded/failed/interrupted` |
| Progress | `ChildProgress{model}` from child `session.step.started.model`; tokens from child usage; `tool_uses`/`last_tool` from child tool events |
| Per-child Stop | Implement `stop_native_child` as `POST /api/session/{childSessionID}/interrupt`. Settle on the child's `execution.interrupted`, not on the ack (same rule as Claude). |
| Root Stop | Root interrupt; foreground children are interrupted natively. Background children are stopped explicitly via `stop_native_child`, matching how Claude settles background tasks. |
| Background subagent | Spawn chip stays running. Completion arrives as `session.synthetic` on the parent and starts a parent execution, emitted as `ProviderTurnStarted` (a wake), so `deterministic_turn_end` stays true |
| Background shells | `shell.created/exited/deleted` (shell jobs) → `NativeTaskChanged{kind: Shell}` like Claude `local_bash` |
| LUNA tools from children | Refused. The evidence fence accepts a `luna` MCP call only when `_meta["ai.opencode/sessionID"]` (sent by OpenCode, A-TOOL-18) equals the **root** session and a matching root `session.tool.called` was observed. Inline config also denies `luna_*` for subagent agents. |

**Not pursued:**
- OpenCode-specific subagent controls: `session.background`, continuing a child by `sessionID` from the UI, and a subagent model picker.
- The `opencode_models` tool gets an ordinary tool card only.

Delegated commands (`subagent: true`) need no special handling; they're ordinary spawns.

Work package: **OC-11**. Scenarios: OCE-01 (with a Claude comparison run), OCE-03 (b), OCE-15 (a), OCE-21.

## 3. Side chats: handle them like Claude and Codex

**How LUNA handles side chats.**
- A side chat is a separate chat beside its parent. It is created from an active parent conversation on the same device (`crates/engine/src/side_chats.rs:349-433`), with one of two context modes chosen by the workbench chooser (`crates/ui/src/shell/workbench/chooser.rs:390,753`):
  - **`nativeFork`**: the provider forks its own session at the origin message (`fork_session_staged`). Offered when the provider's history capability is not `None`.
  - **`contextSnapshot`**: a bounded transcript seed (≤32 entries, ≤24,000 chars, `side_chats.rs:479-500`) in a fresh session.
- Codex and Claude receive the snapshot seed through the **runtime-contract instruction section**. Other providers get a quoted prefix on the first user message ("legacy providers without an instruction channel", `side_chats.rs:772-801`).
- A **read-only** parent can only spawn side chats on Codex (or Mock), which has a real sandbox; Claude is refused (`side_chats.rs:426-433`).
- The side chat inherits the parent's provider config, with the account frozen (`crates/engine/src/sessions.rs:3854`).

**OpenCode today.**
- OpenCode's history capability is `Checkpoint` (`crates/engine/src/history_operations.rs:21-29`), so the chooser offers **native-fork side chats**. On v2 these hit the broken fork path: `messageID` instead of `before`, a probable `"usage"` checkpoint, v1 readback. The result is a failed side chat plus an orphaned native fork (G-7).
- Snapshot side chats work only through the legacy quoted-prefix path, because OpenCode has no runtime-contract channel (`Unsupported`).
- A read-only parent is refused, the same as Claude, which already matches.

**OpenCode target:**

| Shared step | OpenCode backing | WP |
| --- | --- | --- |
| Native-fork side chat | Fixed `fork_session_staged`: `{before: <first message after the kept prefix>}`, a real `msg_` checkpoint id, cursor-paged v2 readback, orphan cleanup | OC-12 |
| Snapshot seed in the instruction channel, like Codex and Claude | Runtime contract via `PUT /api/experimental/session/{id}/instructions/entries/luna` (the same channel as §5), so the seed is no longer a quoted user-message prefix | OC-13 |
| Read-only parent | Refused, like Claude (no change). OpenCode has no OS sandbox (F-PERM-11). | — |
| Account freeze | n/a: LUNA has no OpenCode account selection (§6) | — |

Scenario: **OCE-24** (new), comparing side chats from a Claude parent and an OpenCode parent.

## 4. Child chats: handle them like Claude and Codex

**How LUNA handles child chats.**
- A child chat is a persistent, separate LUNA chat that a **parent agent** creates with the `create_thread` LUNA tool. There's a one-use approval when access escalates, plus lineage, `reportResults`, `continueParent`, and provider/model selection through `list_child_models` (`crates/engine/src/cross_task_tools.rs`, `crates/engine/src/child_threads.rs`).
- Only the brief is copied; there is no history or fork lineage. Cross-provider children drop the parent's model, reasoning and account (`child_threads.rs:156-217`). Access is capped at the parent's ceiling and the provider's supported modes (`child_threads.rs:219-264`). A read-only parent can only create Codex children (`:256-262`).
- **Destinations:** `CHILD_PROVIDERS = [Codex, ClaudeCode, Opencode]`, filtered to installed and enabled providers (`cross_task_tools.rs:33-37`, `child_threads.rs:119-131`; narrowed in `2307d46f`, 2026-10-06 23:58, after the original research pin).
- **Parents:** only providers whose adapter delivers LUNA tools (`CodexMcpTools`, `ClaudeMcpTools`).

**OpenCode today.**
- **As a destination:** already offered (correction to the earlier research, which said destinations were gated). A Codex or Claude parent can create an OpenCode child. Its config gets saved provider defaults, no account, Ask/Full capped at the parent, and read-only refused like Claude. In practice the child's first turn fails on v2 because of G-0..G-3, and `list_child_models` for OpenCode boots a discovery server that hits G-0.
- **As a parent:** impossible. Delivery is `Unsupported`, so `create_thread` is never attached.

**OpenCode target:**

| Shared step | OpenCode backing | WP |
| --- | --- | --- |
| OpenCode as destination | Works once boot, contract and permissions are fixed (OC-02..OC-04). Model catalog from the child's execution scope (`models_for_execution`-style, run location, not home dir) | OC-02..04, OC-10 |
| OpenCode as parent | `RuntimeDelivery::OpenCodeMcpTools` once tool delivery is verified (§5); then `create_thread`, `read_thread`, `send_message_to_thread`, `wait_threads`, `get_workspace` and `manage_workspace` attach exactly as for Claude | OC-13 |
| Ceiling | OpenCode child rules compiled from the capped mode (OC-04). Read-only refused like Claude. | OC-04 |
| Results back to parent | Provider-agnostic queue delivery (no change) | — |

Scenario: **OCE-15**, both directions, Claude as the comparison provider.

## 5. LUNA runtime contract and tools: deliver them like Claude

Claude's mechanism (`crates/harness/src/claude/luna_tools.rs:1-30`):
- the **same per-process loopback `luna` MCP endpoint** as Codex
- attached with one **inline config entry**, so nothing is written to the user's config and the user's own MCP connections (Executor) are unaffected
- a bearer token supplied through the child environment
- an evidence fence that accepts only root tool uses seen on the wire in the current turn
- `ProviderTurnStarted` turn ids for the engine's claim fence

OpenCode equivalents, all v2.0.22 E2:

| Claude step | OpenCode backing |
| --- | --- |
| Inline `--mcp-config` naming only `luna` | `OPENCODE_CONFIG_CONTENT` on the spawned server: the **highest-priority config layer**, merged over the user's config, with `{env:…}` substitution and nothing written to disk (`oc2:core/src/config.ts:206-237`; `oc2:cli/src/server-process.ts:114`). Entry: `mcp.servers.luna = {type: remote, url, headers: {Authorization: "Bearer {env:LUNA_MCP_BEARER_TOKEN}"}}`. The experimental `PUT /api/experimental/mcp/{server}` is a fallback only. |
| `--allowedTools` for exactly the attached tools | Session permission rules: `luna_<tool>` allow for the attached tools; deny `luna_*` for subagent agents |
| Bearer token in the child environment | Same. Note that agent tool processes inherit the server env; the evidence fence, not secrecy, is what blocks direct calls. |
| Evidence: root `tool_use` id seen on stdout | Root `session.tool.called{name: "luna_<tool>", id}` seen on the bus plus `_meta["ai.opencode/sessionID"] == root session` |
| Runtime contract in a stable instruction section | `PUT /api/experimental/session/{id}/instructions/entries/luna {value}` (experimental; applied from the next step; copied on fork via `session.forked.instructionEntries`). The OpenCode `instructions` config key is ignored in v2, and agent `system` overrides would replace OpenCode's prompt, so neither is used. |
| `RuntimeDelivery::ClaudeMcpTools` | New `RuntimeDelivery::OpenCodeMcpTools`, returned only after a verified harmless call (PD-10) |

Work package **OC-13**; scenario **OCE-18** (including the child-session refusal step).

## 6. Everything else: parity disposition

"Target" means the same mechanism as the listed provider. "Not pursued" means an OpenCode-only feature with no LUNA counterpart (PD-14).

| Shared LUNA surface | Codex | Claude | OpenCode target | WP |
| --- | --- | --- | --- | --- |
| Start, resume, stop, turn end | ✓ | ✓ | Same (fix G-0..G-3 first) | OC-02, OC-03 |
| Executable and version truth | ✓ | ✓ | Same (resolver record, override validation) | OC-00, OC-17 |
| Mid-turn steering (StepBoundary) | `turn/steer` | warm mailbox | `delivery:"steer"` plus inbox delivered/cancelled evidence | OC-07 |
| Delivery receipts, client message ids, delivery-unknown guard, safe recovery | ✓ | — (Codex-only engine paths today, `crates/engine/src/sessions.rs:4689-4810` @`9b3e9a2e`) | Same as Codex where the engine path is provider-neutral, backed by caller `msg_` ids and the inbox id | OC-07 |
| Approvals (Ask/Full, shared card and details) | ✓ | ✓ | Same; Auto stays unavailable like any provider without a native reviewer | OC-04 |
| Questions (shared panel) | ✓ | ✓ | Forms → shared panel | OC-05 |
| MCP elicitation (shared form contract) | ✓ | ✓ (`crates/harness/src/claude/controls.rs:274`) | Forms → shared MCP form contract | OC-05 |
| Tool cards, full output, live output, diffs | ✓ | ✓ | Same (v2 tool names, no 4 KiB cap, `ToolActivity`, `ToolDiff`) | OC-06 |
| Images on every route | ✓ | ✓ | Same | OC-09 |
| Typed skill input | ✓ | ✓ (skills only) | Like Claude: `skills[]`. Plugin/app mentions stay Codex-only. | OC-09 |
| Per-project slash commands (`commands_in`) | ✓ | ✓ | Same | OC-14 |
| Model catalog, effort, model options | ✓ | ✓ | Same (`models_live`; variants as model options) | OC-10 |
| Live model/effort change | — | ✓ | Like Claude (`POST …/model` with readback) | OC-10 |
| Cost and context ring | context | ✓ | Same (`ProviderAccounting`, correct occupancy) | OC-10 |
| Usage-page history (tokens and cost over time) | ✓ | ✓ | Same, by T3's method: read-only scan of OpenCode's local database (F-MOD-15) | OC-10 |
| Plan limits on the Usage page | ✓ | ✓ | Already the same as T3 for OpenCode Go (endpoint, windows, key sources), plus OpenCode 2's sign-in; kept (PD-17) | — |
| Compaction lifecycle | ✓ | — | Like Codex (`ContextCompaction` from `session.compaction.*`) | OC-10 |
| Retry notices | ✓ | — | Like Codex (`ProviderNotice` from `session.retry.scheduled`) | OC-07 |
| Reconnect, replay, restart recovery | ✓ | ✓ | Same | OC-08 |
| Fork (user and assistant rows) | ✓ | ✓ | Same (today assistant-only and broken on v2) | OC-12 |
| Edit earlier message, same-chat rewind | ✓ | ✓ | Like Claude (`native_history_turns`, `rewind_native_conversation` via `revert.stage{files:false}` + commit) | OC-12 |
| Native file-checkpoint rewind | — | ✓ | Later (PD-16), like Claude (`rewind_native_files` via snapshot revert); normal rewind first | OC-12 |
| Native subagents | ✓ | ✓ | §2 | OC-11 |
| Background shells and tasks | — | ✓ | Like Claude (`NativeTaskChanged{Shell}`) | OC-11 |
| Side chats | ✓ | ✓ | §3 | OC-12, OC-13 |
| Child chats | ✓ | ✓ | §4 | OC-13 |
| LUNA tools and runtime contract | ✓ | ✓ | §5 | OC-13 |
| Run titles (`run_title`) | ✓ | ✓ | Like Claude: a tool-less one-shot title run | OC-14 |
| Integration inventory (MCP status, extensions) | catalog | ✓ | Like Claude: read-only `native_extensions` / `session_mcp_observation` from `GET /api/mcp` and `/api/skill` | OC-14 (P2) |
| Review action | native `review/start` | `/code-review` line | Like Claude: OpenCode's built-in `/review <target>` command line (a text review; no structured findings) | OC-15 |
| Goals | `thread/goal` | native `/goal` | **Unavailable**: no native goal engine (same rule as any provider without one) | — |
| Schedules and monitors | — | ✓ | Unavailable (no native counterpart) | — |
| Account selection | ✓ | ✓ | Not pursued: OpenCode credentials are per model-provider integration inside OpenCode, with no single provider login to select. The usage card stays. | — |
| Native session import and reconciliation | ✓ | — | Not pursued (Claude lacks it too; revisit if it becomes a shared surface) | — |
| Conversation deep link | ✓ | — | Not pursued | — |
| Read-only sandbox for side or child chats | ✓ | refused | Refused, like Claude (already matches) | — |
| Provider updates | ✓ | ✓ | Same, generation-aware | OC-17 |

## 7. OpenCode-only features: not pursued (PD-14)

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
| Code Mode-specific presentation | F-TOOL-13 | Generic tool card only; nested calls shown only if emitted as ordinary tool events (Q-12) |
| External server attach, managed background service, pairing, web UI | F-WS-01/02/03 | not pursued. LUNA keeps private `--stdio` servers and disables the embedded web UI. |
| Multi-account credential management | F-MOD-05 | not pursued (§6) |
| Warming, transport settings, policies, plugin RPC and management, formatters, LSP config | F-MOD-08/09/13, F-EXT-05/06/13 | native config; n/a |
| Desktop browser namespace, persistent PTYs, worktree management, SSH/WSL, ACP, SDKs, TUI and desktop features | F-TOOL-16, F-WS-05/08/10, F-CLIENT-* | n/a or excluded (PD-5) |

## 8. Corrections and new findings from this refinement

1. **OpenCode is already a child-chat destination** (`cross_task_tools.rs:33-37` @`36cd741a`). The earlier "gated" classification of F-AGT-13 came from an older QA doc (`docs/qa/child-chats.md:10`, which refers to *creating* providers). What blocks it in practice is G-0..G-3.
2. **Native-fork side chats are offered for OpenCode and will fail on v2.** The capability is `Checkpoint` and the fork path is broken (G-7). This is a user-visible failure in a shared surface, not only a fork-button issue.
3. **Snapshot side chats send their context as a quoted user-message prefix for OpenCode,** because there is no instruction channel. Codex and Claude use the runtime-contract section. Fixed by OC-13.
4. **LUNA tools from native subagents must be refused,** matching both Codex and Claude. This closes Q-28.
5. **Agent cards already show an OpenCode child model** as its raw id once `ChildProgress{model}` is emitted. No OpenCode-specific UI is needed (the earlier note "model label Codex-only" was imprecise).
6. **Claude's review is its own native command line** (`/code-review`, `crates/engine/src/sessions.rs:4280-4297`). OpenCode's `/review` is the direct counterpart, so Review moves from "gated" to "same as Claude".
7. **`OPENCODE_CONFIG_CONTENT` is the right attachment point** for LUNA tools (no experimental endpoint, nothing on disk), mirroring Claude's inline `--mcp-config`.
