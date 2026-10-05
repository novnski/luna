# MCP startup and repeated Intercom OAuth prompts

Investigation and scoped implementation, 2026-10-05. Scope: LUNA provider
launches, local Executor logs, and the installed `mcp-remote` label. A
credential-free synthetic Codex/MCP probe was run after the initial read-only
review. No Intercom, OAuth, or GUI action was run. The primary LUNA Dev app and
user credentials were left alone.

## Pre-change path inventory

The Rust build has no path to an Intercom MCP connection. Running a refreshed
LUNA app, a provider discovery probe, or a provider chat does. The paths must
be distinguished. This table describes the paths before the scoped edits below:

| Path | LUNA behavior | MCP implication |
| --- | --- | --- |
| Codex model and skill discovery | Spawns short-lived `codex app-server` processes and calls `model/list` or `skills/list` after `initialize` (`crates/harness/src/codex/mod.rs:207-255,274-370`). | No chat turn, but the process inherits the selected Codex home. The adapter supplies no MCP exclusion before spawn. Whether this Codex build starts MCP during `initialize` or waits for a thread is not yet proven. |
| Codex Settings inventory refresh | Only explicit `probeProvider:true` starts native app-server reads of `skills/list` and `config/read`; ordinary Settings reads are local (`crates/engine/src/rpc.rs:9844-9929`, `crates/harness/src/codex/native.rs:176-212`). | Two bounded processes can be started concurrently for one refresh. `config/read` is configuration visibility, not a live upstream call. |
| Codex fresh/resumed chat | Spawns `app-server`; attaches a process-local `luna` MCP endpoint on both fresh and resumed ordinary sessions (`crates/harness/src/codex/mod.rs:856-944,1550-1620`). | User MCP configuration remains available. The local LUNA endpoint is additive, not an Executor replacement. Actual MCP tool calls are a separate later step. |
| Codex title | Starts app-server, then calls `config/read` and disables each configured MCP server in `thread/start` or `thread/resume` config (`crates/harness/src/codex/mod.rs:1531-1570`). | Servers are disabled for the title thread; this does not prove pre-`initialize` isolation, because the override is assembled after `initialize`. |
| Codex out-of-turn compaction | Uses a new native app-server connection, `thread/resume`, then `thread/compact/start` (`crates/harness/src/codex/native.rs:632-693,726-785`). | This maintenance process currently inherits the selected Codex home and does not attach the LUNA endpoint. It is another plausible provider-start trigger, independent of a tool invocation. |
| Claude model/command/extension discovery | Model and command lists come from short-lived `claude` processes sent `initialize`; command discovery is cached per scope. Hooks are disabled, but inherited MCP is not (`crates/harness/src/claude/mod.rs:326-351,465-491`, `crates/harness/src/claude/discovery.rs:155-235`, `crates/harness/src/claude/profile.rs:169-176`; extension readback uses the same launch shape at `crates/harness/src/claude/extensions.rs:1058-1126`). | The provider process can load configured MCPs without a user message or tool call. The source comments explicitly note that `initialize` otherwise runs `SessionStart` hooks. |
| Claude Settings provider check | Explicit Refresh requests `mcp_status` for Ask, Auto, and Full. Equivalent modes are grouped into at most two concurrent control processes (`crates/engine/src/claude_mcp.rs:313-357`, `crates/harness/src/claude/mcp.rs:618-704`, UI trigger at `crates/ui/src/settings/harnesses.rs:1330`). | This is an intentional live MCP status check; it can contact/authenticate unrelated saved servers. Passive Settings reads do not request it. |
| Claude fresh/resumed chat | Ordinary `--print` runs attach LUNA's local MCP endpoint and deliberately omit `--strict-mcp-config`, preserving user MCPs such as Executor (`crates/harness/src/claude/mod.rs:590-706`, `crates/harness/src/claude/luna_tools.rs:1-20,335-366`). | External MCP startup may happen as a provider startup effect even if the chat never invokes Intercom. A later `mcp__executor__execute` is a distinct tool invocation. |
| Claude title/history | Title launches use strict empty MCP config and disabled settings sources; history control operations do likewise (`crates/harness/src/claude/mod.rs:680-690`, `crates/harness/src/claude/history.rs:648-663`). | These paths already show how LUNA can isolate metadata-only work per process. Claude has no verified out-of-turn compact operation (`crates/engine/src/context_controls.rs:21-29`). |

The QA bundle settings in `docs/reference/qa-infrastructure.md:64-80`
isolate app name, bundle ID, IPC port, data, and worktrees. They do not isolate
`CODEX_HOME`, `CLAUDE_CONFIG_DIR`, or Executor's saved connections. A separate
QA app can therefore still launch a provider with the user's MCP configuration.

The installed global `mcp-remote` 0.1.38 sets its OAuth `clientName` to
`MCP CLI Proxy` (`/opt/homebrew/lib/node_modules/mcp-remote/dist/proxy.js:130`).
That title identifies the proxy implementation, not the initiating LUNA chat.
The supplied Executor connection metadata points to `mcp-remote` for Intercom
EU. I did not re-read saved connection credentials.

The current `/Users/iulian/.executor/logs/daemon.log` contains repeated
`intercom_eu_mcp` discovery timeouts of 15 seconds and transport failures,
with the existing catalog preserved. The last ten timeout timestamps in the
local log include 14:58, 15:18, 16:18, 17:18, 19:18, 19:38, 19:58, 20:18,
20:38, and 20:58. The log records local time without a date on those lines;
its file was last modified on 2026-10-05. This roughly 20-minute recurrence
is compatible with gateway refresh/retry activity, but the scheduler and
initiator have not been identified. Neither that log nor the current process
snapshot ties an OAuth prompt to a particular LUNA turn. The historical PIDs
in the earlier report are not live targets.

A credential-free Codex 0.160.0 experiment used a synthetic `CODEX_HOME`, a
single harmless stdio marker MCP, and app-server `initialize`, `config/read`,
then `thread/start`. The marker was absent after `initialize` and
`config/read`, then started on `thread/start` with baseline configuration.
`-c mcp_servers={}` still reported the marker enabled and started it;
`-c mcp_servers.marker.enabled=false` reported it disabled and prevented its
start. A quoted-key spelling did not initialize successfully, so the staged
helper accepts only simple bare-key server names and fails closed otherwise.
The repeatable script and redacted result are under
`target/qa/agent-delegation-ux/mcp-investigation/`.

## Scoped implementation and remaining proposals

Failure cases written before implementation:

| Case | Failure to prevent | Required observation |
| --- | --- | --- |
| F1 | Claude model, command, extension, or typed-skill metadata probe starts an inherited MCP server | The provider process receives strict empty MCP launch flags before `initialize`, while its requested catalog/skill result still works. |
| F2 | Codex metadata probe or title performs MCP startup before thread-scoped config disables it | A synthetic home with a marker MCP shows no marker process in isolated probes; ordinary thread startup still sees the marker. |
| F3 | The QA app inherits user Executor or direct MCP servers | Its signed environment carries only the QA isolation flag, and a native provider run omits inherited servers while retaining the local `luna` endpoint. |
| F4 | A normal app loses external MCP access after the fix | Fresh, resumed, and forked provider sessions still report and can invoke a harmless external fixture; explicit MCP status checks still inspect it. |
| F5 | Compaction loses LUNA tool discovery or loads an unrelated MCP during maintenance | Codex out-of-turn compaction starts without user MCP, and the next normal turn finds `get_workspace`; Claude automatic compaction is checked inside an ordinary session. |
| F6 | A repeated OAuth prompt is blamed on LUNA without provenance | Redacted provider launch and gateway timestamps show the exact parent process; adjacent build/app refresh is not treated as causal evidence. |

The first artifact is a synthetic local provider/MCP fixture; the source edit
is staged, and native acceptance remains for the coordinator. No test in this
subtask writes to user credential/config directories or calls Intercom.

The staged source implements these bounded changes:

1. For Claude metadata-only model/command/extension discovery, apply
   `--strict-mcp-config --mcp-config '{"mcpServers":{}}'` **before spawn**, as
   title/history already do. Keep ordinary chat and explicit Settings MCP
   checks using the user's effective configuration. Compare model/command
   catalogs before and after with a local harmless MCP fixture to detect
   unexpected discovery loss.
2. Codex 0.160.0 did not start the marker during metadata-only discovery, so
   those processes are unchanged. QA chat launches and out-of-turn compaction
   read effective server names through a metadata-only `config/read`, then use
   a per-spawn `-c mcp_servers.<name>.enabled=false` for each. An empty-table
   override would merge and is deliberately not used. Unsupported names or a
   failed config read stop the isolated launch before its chat/compact process
   starts. The LUNA endpoint is applied last and remains enabled.
3. The QA bundle's signed environment sets `LUNA_QA_MCP_ISOLATION=1`; primary
   LUNA Dev sets `0`. QA ordinary provider sessions request strict isolation
   while preserving LUNA's local server. Ordinary user sessions and explicit
   MCP checks outside QA continue to use provider-owned configuration. No
   global Executor policy or credential was changed.
4. Keep the compact `get_workspace` recovery instruction and the per-process
   LUNA MCP attachment on fresh/resumed/forked ordinary sessions
   (`crates/engine/src/runtime_contract.rs:115,363-387`). Confirm a post-compact
   provider turn can discover and invoke `get_workspace`; avoid putting a full
   tool manual into each user message.

A separate future diagnostic improvement would record provider, launch purpose,
local correlation ID, PID, start/end time, and whether inherited MCP was
allowed, without logging argv, URLs, headers, auth values, or full MCP
responses. Only that evidence, paired with gateway timestamps, could attribute
a specific OAuth prompt to LUNA rather than an adjacent build or GUI refresh.
It is not part of this staged source change or its acceptance gate.

## Verification and limits

The synthetic Codex override probe used an isolated home and harmless marker
MCP, without user credentials or connections. It proves the installed CLI's
override merge behavior. The `mcp_startup_isolation` harness E2E then passed
through LUNA's actual adapters and subprocess boundary, checking metadata,
isolated ordinary runs with the local LUNA endpoint, and ordinary launch
configuration. The existing Claude effective-config, MCP, and LUNA-tool
suites passed, as did all 76 enabled Codex adapter tests (six native tests
remain explicitly ignored).

The signed native QA bundle carries `LUNA_QA_MCP_ISOLATION=1` and successfully
ran real Codex and Claude parents and background helpers. Both parents called
the local LUNA tools. The primary app's bundle stamps `0`, preserving ordinary
provider configuration. This verifies launch configuration and LUNA tool use;
it does not claim a live Intercom call or prove all user-configured upstream
servers are healthy. No user credentials or global Executor settings changed.

Native compaction also exposed two existing lifecycle issues outside MCP
discovery: an idle warm writer still held the conversation, and the operation
used the ten-second metadata deadline. Compaction now reuses the verified
idle-writer release/dispatch guard and has a separate three-minute deadline.
An engine/RPC subprocess E2E checks delayed completion, binding/transcript
preservation, another warm writer remaining live, and idempotent replay.
Native acceptance and its recorded receipts are summarized in this directory's
verification report; a failed or unknown attempt is never replayed blindly.

Commands and evidence are under `target/qa/agent-delegation-ux/`. Provider
launch correlation logs and a real external-MCP invocation matrix remain
possible follow-up investigations, not results of this change. The precise
initiator of the observed Intercom OAuth prompts remains unproven.
