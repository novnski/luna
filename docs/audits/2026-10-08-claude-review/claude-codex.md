# Claude and Codex adapter audit — 2026-10-08

Scope: recent Claude/Codex adapter changes through `d3819203`, in the existing checkout. Builds, application QA and Git operations are coordinated by the lead. Claude native workflow pursuit remains cancelled, including workflow Stop and post-turn workflow approvals.

## Failure modes recorded before regression implementation

1. A Claude harness configured with `with_config_dir` launches turns in that home but lists sessions in the inherited/default home. A live writer in the configured home is missed, while a same-id writer in an unrelated home can block a valid resume. The settings listing is inconsistent with the launched provider instance too.
2. The external-session view truncates to 200 rows before the resume check searches it. A matching writer beyond the displayed rows can be missed. This is distinct from the explicitly documented fail-open behavior when listing itself is unavailable.
3. A control-only history process inherits a parent Claude process's messaging socket, token and bridge/session identity. Ordinary turns already remove these variables, but history fork validation and rewind controls do not.
4. History controls promise no SessionStart side effects, but `--setting-sources ""` alone does not express `disableAllHooks`; managed hooks remain a concern. The production discovery path already explicitly disables hooks for its initialize-only subprocess.
5. Conversation/file rewinds cold-resume the original session without running the concurrent-writer guard at all. The engine excludes its own parked runtime, but its history owner confirmed that it does not exclude an external Claude writer. Both rewind APIs must refuse before their control request can mutate transcript/files when the listing reports that writer.

Regression approach: exercise public harness methods against the existing subprocess peers; assert provider-home selection, pre-launch refusal, and the exact history child environment/settings. No new unit tests. Record assertions before production edits. Lead runs Cargo and captures repeatable test evidence.

## Repairs prepared

| Finding | Trigger and consequence | Change | Repeatable regression |
| --- | --- | --- | --- |
| P1: history controls bypassed the concurrent-writer check | An external terminal/IDE owns the native session when LUNA runs a conversation or file rewind. Engine-local parked-session exclusion does not cover that process; the control can edit a concurrently written transcript or restore files. | `claude/history.rs::run_control` checks the exact resumed session in its effective provider home before launching. Fork validation checks the newly created destination ID, preserving the ability to fork a live source. | `history_rewinds_refuse_an_external_session_writer_before_mutation` checks both public rewind APIs, the written-progress callback, unchanged transcript/file hashes and no history child launch. |
| P2: wrong provider home for listing/guard | `with_config_dir` points turns at an independent provider home, while `agents --json` previously inherited the default home. The guard can miss a real writer or reject an unrelated same-ID row. | A scoped listing helper receives the harness config home for Settings and resume, retaining the public inherited-home listing API. | `external_sessions_and_resume_use_the_configured_provider_home` checks refusal with a held row, then successful resume/readback after clearing only the configured home's row while the default-home row remains. |
| P2: display truncation hid a writer | More than 200 native sessions put the matching row beyond the UI page. | The guard filters the bounded raw response by session before applying the UI row bound. The Settings view still returns at most 200 rows with `truncated`. | `resume_checks_writers_beyond_the_external_session_display_limit` supplies 201 unrelated rows followed by the live writer and verifies refusal with no turn process. |
| P2: control-only history environment was not isolated | LUNA itself is launched from Claude, exporting peer/bridge/session identity, or managed SessionStart hooks exist. History subprocesses previously inherited the identity and did not explicitly disable hooks. | The history child uses the existing session-env scrubber and `--settings {"disableAllHooks":true}`. Its empty setting sources and strict empty MCP config remain. | `history_controls_disable_hooks_and_drop_inherited_session_identity` exercises fork validation, file preview and conversation rewind in an isolated test subprocess; the peer records only inherited variable names and argv, never values. |

The hook change follows the existing history contract (the comment explicitly excludes SessionStart side effects) and the discovery implementation in `claude/profile.rs::discovery_settings`. Managed settings are not removed by an empty `--setting-sources` value. The fixture verifies the production launch contract; it does not claim to reproduce Claude's managed-hook implementation.

## Feature coverage and edge cases

| Feature | Paths reviewed / existing verification surfaces | Assessment and remaining QA |
| --- | --- | --- |
| Native LUNA tool delivery | `claude/luna_tools.rs`, `claude/mod.rs`, shared `codex/luna_mcp.rs`; `claude_luna_tools` fence matrix | Claude's bridge records root tool-use identity and parks stdout/MCP ordering races, rejects foreign subagent evidence, and ends the turn fence on a result. Retain tests for same-name/different-ID calls, call replay, missing executor and Stop during approval. No change justified by this review. |
| Tool results and saved Bash output | `claude/tool_results.rs`, `saved_output.rs`; `claude_tool_results` | The result ledger scopes calls by parent and tool-use ID, distinguishes absent versus empty output, and attaches a frame-level native result only when one result block is present. Saved output admission checks root ownership/session identity. Existing artifact refusal/replay tests should be rerun; no new defect established. |
| Session resume and history | `claude/background_sessions.rs`, `history.rs`, launch guard in `mod.rs`; `claude_coordination`, `claude_history` | Repairs above. Keep existing stopped/owned/unrelated row behavior, unavailable-listing fail-open, exact UUID checkpoint boundaries, fork prefix/readback, stale rewind refusal, and file-preview immutability coverage. |
| Permissions and cancel | `claude/controls.rs`, `profile.rs`, cancellation branches in `mod.rs`; `claude_permissions`; `codex/approvals.rs`, `elicitation.rs` | Claude retires request IDs under a lock; cancel withdraws the token and a late answer cannot write a grant. Full still rejects unsupported plan/interactive paths. Shared elicitation remains typed and bounded. Keep native-cancel/replay, concurrent request IDs, native weaker-mode failure and Stop-with-pending-card cases. |
| Native children and Stop | `claude/native_tasks.rs`, `native_stop.rs`; `codex/subagents.rs`; native child tests in `claude` and `codex` | Stop is addressed only to a tracked unsettled spawn of the registered live session. A successful `stop_task` response is a request receipt, not a terminal child outcome. Codex child approvals are tied to a live observed child turn and cancelled when it ends. Retain live child stop/follow-up QA and ensure parent completion remains independent. |
| Models and live switches | `claude/catalog.rs`, `runtime_controls.rs`, `live_session.rs`; `claude_cc06` | Static fallbacks coexist with discovered effort capabilities. Live switch stages input until applied-settings readback; failure closes input rather than silently writing the pending prompt. Live catalog/model eligibility still needs provider evidence; cached metadata alone is not entitlement. |
| Images and typed inputs | `claude/input.rs`; `claude_inputs` | First/resumed/warm inputs share a builder; it rechecks bytes before writing, limits compressed and decoded sizes, suppresses empty image-only text blocks, and positions slash invocation in the last text block. Retain changed/missing/oversized/undecodable file and image+skill ordering cases. |
| Codex lifecycle / parity | `codex/mod.rs`, `subagents.rs`, `approvals.rs`, `elicitation.rs`; `codex` integration suite | Reviewed recent changes and request/child ownership boundaries; no concrete new Codex defect established. Preserve tests for resumed child ownership, stale terminal events, late approvals, exact steering receipts, typed MCP forms and unknown outcomes after disconnect. This is scoped source review, not a claim of full current-provider certification. |
| Claude native workflows | Certification decision at `d3819203` | Intentionally untouched. No workflow Stop, post-turn approval, or live workflow QA pursuit. Existing decode/display behavior remains. |

One investigated candidate was not changed: Claude's process-wide `any_done` can suppress a synthetic harness terminal after a later warm-turn crash. The engine reviewer confirmed that active-stream EOF is converted to `Done::Errored` in `sessions.rs`, so the visible turn still settles; the diagnostic may be generic. This was not escalated into unnecessary lifecycle surgery.

## Validation handoff

Completed here: selected-file Rust formatting and `git diff --check`. No Cargo command, live provider turn, app launch, GUI action, commit or push was run by this agent. Source/tests are frozen for central integration.

Lead's first focused command:

```sh
scripts/cargo.sh test --locked -p luna-harness --test claude_coordination --test claude_history
```

Then the coordinator should run the required harness check/lint and existing focused suites for tool results, permissions, input, CC-06 switches, LUNA tools and Codex lifecycle according to its shared-build budget. Save command exit statuses and logs in the audit evidence directory and hash the retained artifacts.

Live checks to retain in final evidence: an external Claude writer refuses a cold LUNA resume; after that exact external writer closes, the same draft resumes once; alternate provider-home inspection stays scoped; native child Stop reports the eventual native lifecycle result; current model discovery, image-plus-prose input, and root tool receipts survive native UI readback. A history hook-isolation probe should use an isolated provider config/hook marker and verify no marker appears. These live checks have not been performed by this agent.

Residual limits: session listing is an observation before launch, not an atomic native lock; documented unavailable-listing fail-open behavior remains, and existing LUNA-owned children are excluded. The guard is not a guarantee against a different process starting between listing and launch. Installed CLI/account support and managed-policy behavior require current live verification.

## Live guard probe: failure cases before implementation

1. A print process might not register, initialize may fail, or the installed CLI may gate `agents --json`. Require an exact live PID + native UUID row; never count an unavailable/empty listing as guard success.
2. Spawning the holder from the harness probe would classify it as LUNA-owned and exempt it. A Python controller owns the holder; a separate bounded launcher owns the Rust probe. The holder parent, probe parent and Rust guard-owner PID are checked independently.
3. A broken guard could start a paid turn. The executable wrapper passes only exact `agents --json` to the real CLI; every other invocation records a nonsecret marker and exits before reading a prompt or launching Claude.
4. Inherited Claude peer/session environment could suppress registration or attach foreign peers. Remove the seven production-scrubbed variables plus ambient session-kind/name and nested-launch markers for the owned subprocesses; keep auth environment untouched and never inspect credential files.
5. A timeout or early assertion might leave an idle process/pipe behind. The controller closes the holder stdin, waits a bounded grace period, then signals only its own unreaped `Popen` child if necessary. Never discover a cleanup PID by a broad process-name search.
6. Raw initialize/settings/listing data can contain private information. Evidence stores only selected metadata for the exact synthetic session and response shape/counts; no environment values or raw provider stdout/stderr.
7. A custom provider home is invisible to an already-running default-home app. The probe supports default/inherited home for Settings readback and explicit `--config-home` for scoped adapter verification; it never changes the app environment.

## Installed CLI and exact live execution handoff

Read-only inspection on 2026-10-08 found **Claude Code 2.1.293**, executable `/Users/iulian/.local/share/claude/versions/2.1.293`, SHA-256 `4e21122a227857da1178aca3299700c1fd7f2b77c93f12e73c2c76db796a105e`. This is newer than the 2.1.286 certification ledger. `claude agents --help` explicitly describes `--json` as listing active interactive and background sessions. The installed executable's bundled source (`printAgentsJson`, `QYt`/`vU`) defaults ordinary registered processes to kind `interactive`; the registration gates cover native subagent/ambient child identity and parked spare processes, not print mode. Therefore use an ordinary print holder, with **no `--bg` supervisor**. This source evidence is not a substitute for the exact live PID/UUID assertion.

No existing example/test exercises this exact live refusal: `claude_luna_mcp_probe` has no resume; `live_real_cli_single_turn` runs a model turn; coordination tests use synthetic peers. The added **live E2E example**, `claude_writer_guard_probe`, calls the public `external_sessions` and `run` APIs through a wrapper that allows only real listing. It is not a unit test. The controller owns all subprocesses and stores only sanitized metadata and output hashes.

Lead commands (not executed by this agent):

```sh
scripts/cargo.sh build --locked -p luna-harness --example claude_writer_guard_probe
python3 scripts/qa-claude-writer-guard.py \
  --out "$PWD/target/qa/claude-writer-live-$(date +%Y%m%dT%H%M%S)" \
  --hold-seconds 30
```

Use `--config-home /absolute/existing/disposable/config` for separate scoped-home coverage. Omit it for readback in a running default-home LUNA app. While the controller prints its successful refusal and holds the idle process for 30 seconds, call IPC `ListClaudeExternalSessions` with `{}` and inspect Settings → Providers. Verify the exact synthetic session UUID, live external status, and read-only row. That RPC currently uses the engine's inherited/default provider home; it cannot select an arbitrary config home.

The controller's exact holder arguments are:

```text
--print --session-id <fresh-uuid>
--input-format stream-json --output-format stream-json --verbose
--tools "" --permission-mode dontAsk --setting-sources ""
--settings {"disableAllHooks":true}
--strict-mcp-config --mcp-config {"mcpServers":{}}
--no-chrome --no-session-persistence
```

It writes exactly one line to stdin, retains the pipe, and sends **no user message**:

```json
{"type":"control_request","request_id":"luna-writer-guard-initialize","request":{"subtype":"initialize"}}
```

Removed identity variables: `CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_BRIDGE_SESSION_ID`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_JOB_DIR`, plus ambient `CLAUDE_CODE_SESSION_KIND`, `CLAUDE_CODE_SESSION_NAME`, and `CLAUDECODE`. Auth variables remain inherited; no credential store is inspected or changed. Auto-updates and nonessential telemetry are disabled for the owned children.

Bounds: 20 seconds for initialize; at most three five-second listing calls with one-second gaps; a 20-second probe deadline inside a 23-second launcher bound; optional hold at most 60 seconds. Cleanup closes stdin and waits five seconds; fallback terminate/kill addresses only the still-owned unreaped child, and any such escalation fails certification. The probe never uses `claude stop` or signals a PID found by process-name search. `evidence.json` plus `SHA256SUMS` retain the selected holder row, distinct process identities, expected pre-acceptance refusal, absence of an unexpected-launch marker, output hashes and clean exit. Raw provider output and unrelated session rows are discarded.

### Native history isolation readback

Use a separate disposable provider config and captured test transcript (or already-created disposable live QA transcript), never a user conversation. Read its native UUID/checkpoint using `native_history_turns` and record transcript/file hashes. For hook isolation, place only a harmless SessionStart marker hook in a disposable `managed-settings.json` directory selected by `CLAUDE_CODE_MANAGED_SETTINGS_PATH`; never edit the platform managed-settings folder. First initialize a control-only CLI without `disableAllHooks` to prove the marker hook loads, then remove only that marker. Exercise the history path with its production `--settings {"disableAllHooks":true}`, empty setting sources/strict empty MCP and injected synthetic peer variables. Require initialize/control acknowledgement, absent hook marker, and no inherited peer names in the subprocess observation. A missing positive control is inconclusive, not a pass.

File-preview readback sends only the native control request below after initialize, then closes stdin. Keep `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=1`, `dry_run:true`, and compare fixture file hashes before/after. A native `canRewind:false` is a valid readback of an unavailable checkpoint, not successful restore support. Conversation rewind is a mutation and belongs only on a separately reviewed disposable copy.

```json
{"type":"control_request","request_id":"luna-history-preview","request":{"subtype":"rewind_files","user_message_id":"<known-fixture-checkpoint>","dry_run":true}}
```

Tooling was formatted and Python syntax parsed without executing the controller. No holder, model turn, app or build was launched by this agent.

## Goal live-QA budget correction: failure cases before edit

The retained `codex-goals-live/idle-continuation-timeline.json` has 357 snapshots. At `2026-10-08T00:51:09.739Z`, the real goal changed from active to `budgetLimited` with 10,511 tokens used against the runner's 6,000-token cap; final accounting reached 13,208 at `00:51:15.797Z`. The runner nevertheless waited until `00:53:51` for completion, then wrote `failure-1791420831621.json`. This is evidence of a QA budget mismatch, not a product continuation failure.

Failure cases for the runner correction:

1. A fixed cap below one provider continuation's scaffold/input cost cannot exercise the required second continuation. Keep a finite default of 60,000 and allow explicit `LUNA_GOAL_QA_TOKEN_BUDGET` selection.
2. Empty, zero, negative, fractional, nonnumeric, infinite, or unsafe-integer values must fail before app discovery, IPC, artifact creation or provider work; do not silently coerce or remove the cap.
3. Create/update-preserves/restore assertions must all use the selected value; changing only the creation request would break the QA contract.
4. Budget exhaustion must immediately retain its timeline and a separate budget-limited diagnostic, rather than waiting for the three-minute completion timeout or classifying the goal as complete.
5. The selected finite value must be present in the run manifest/result. Preserve the existing explicit remove-budget/restore-budget check, transcript/history/usage assertions, and all previously captured fixture values.

Implemented only in `scripts/qa-goals-live.mjs`: positive decimal safe-integer validation before any app/file/IPC access; finite default **60,000**; configurable `LUNA_GOAL_QA_TOKEN_BUDGET`; shared creation/preservation/restoration assertions; manifest/result budget retention. Observing `budgetLimited` now saves the timeline and `<phase>-budget-limited.json`, then fails immediately with `LUNA_GOAL_QA_BUDGET_LIMITED`. No product or stored fixture state was changed.

Static verification passed: `node --check`, `git diff --check`, and nine CLI preflight cases using a deliberately nonexistent app path (default, 60,000 and 1 passed budget validation; empty, zero, negative, fractional, Infinity and unsafe integer failed before app lookup). Those preflight cases created no files and called no app IPC/provider. Live rerun remains coordinator-owned, using a fresh evidence directory and for example `LUNA_GOAL_QA_TOKEN_BUDGET=60000` alongside its existing model/app settings. All original transcript, checkpoint, accounting and goal-state checks remain.
