# Contract owners and workflow set

Each acceptance contract has one primary owner at its strongest existing
boundary. "Gap" names only what current code and evidence lack, classed as
**implementation**, **verification**, **external dependency** or **optional**.
Suites are `crates/engine/tests/<name>.rs` unless a path is given. Native
runners read the environment shown and nothing else.

## Owner map

| Contract | Primary owner (exists today) | Gap at `83ede04b` | Class → ticket |
| --- | --- | --- | --- |
| E01 active-session inventory | `claude_mcp_inventory`; `GetIntegrationInventory`/`GetRuntimeContract`; `scripts/qa-integrations-picker.mjs` (`LUNA_PICKER_EVIDENCE_DIR`, `LUNA_IPC_PORT`, `--chat`/`--space`, optional `--account --target --path`) | Codex MCP has no health or attachment readback. Claude session attachment is memory-only and gone after a restart. All 16 stale contracts (`restartRequired`) look like current ones. No nonce call proves callability. | implementation + verification → SW-01 |
| E02 scoped external MCP permissions | `claude_permissions`, harness `claude_effective_config`/permission peers | Claude Ask/Auto don't load Executor (user scope). claude.ai connectors do load. No restricted route to the selected Executor connection. | implementation → SW-02 |
| E03 skill behaviour and migration | `scripts/verify-managed-skills.sh`, managed-skill lifecycle | Claude Ask/Auto load no user or project skills, so skills can only arrive as guidance. No real-mode run follows a skill. | implementation + verification → SW-03, SW-11 |
| E04 support/infra evidence to draft | none (new workflow runner contract only) | No LUNA chat has made an Executor, Intercom or Slack call. Live read targets need the user's approval. | verification → SW-04 (after SW-02/03) |
| E05 staged write, uncertainty, no resend | bridge call fences in `cross_task_tools`, `restart_resume`, `message_queue` | No disposable independently logged service across a transport loss. | verification → SW-04 |
| E06 chronological truth and viewport | `timeline_order_e2e` (5 pass) for fold and identity; native transcript QA for the viewport | Viewport and restart are unproven in a real native session. Add a fold case only if a reproduction fails on this baseline. | verification → SW-05, SW-06 |
| E07 late questions, queued images, steering | `message_queue` (34), `native_inputs` (4), `queued_attachments` (1); `scripts/qa-native-inputs.sh`, `qa-steering-status.sh` | Real provider input and receipt evidence with question interleaving. | verification → SW-06 |
| E08 restart, history, handoff | `restart_resume` (7 + 1 live-gated), `session_history`; `qa-context-transfer-live.mjs` (four `LUNA_DEV_*` overrides, `LUNA_AUDIT_NATIVE_BASE`), `qa-related-chats-live.mjs` (`LUNA_RELATED_QA_*`), `qa-same-chat-rewind.mjs` | The live Claude restart case is ignored by default. Resumed-session tool delivery is unproven live. | verification → SW-06 |
| E09 visual tool output and privacy | none. Media is scrubbed from computer-use results, and `FetchToolBlob` returns text only. | No typed image or resource reference, and no authorized retrieval of returned images. | implementation → SW-09, after IC-0 `shared` and CORE-14 |
| E10 exact browser/native target | none in LUNA. Codex `config.toml` `computer-use`; external owner through Executor. | Target binding and readback are unproven, and Codex MCP health is unknown. | implementation + external dependency → SW-08 |
| E11 inspect, revise, reopen a visual | IC H01–H09 (not started; IC-0 waits on CORE-01, which waits on SW-00) | The whole renderer and storage path. | external dependency → SW-07 consumes IC-1–IC-3 |
| E12 three helpers and exact stop | `cross_task_tools` (5); task-work stop receipts in `engine/src/rpc/task_work.rs`; `qa-child-chats.mjs` (`LUNA_CHILD_QA_BINARY`, `LUNA_CHILD_QA_EVIDENCE`), `qa-agent-workspace-controls.mjs` (four `LUNA_DEV_*`, `AGENT_WORKSPACE_NATIVE_DIR`) | `interrupt_thread` is omitted from every contract. No agent-side exact stop. | implementation → SW-10 ([contract](contracts.md#helper-control)) |
| E13 coordinated code delivery | existing helpers plus Git paths; no workflow owner | A disposable repo plus bare-remote delivery run. | verification → SW-10 |
| E14 past work and general scope | `local_import` (8), `session_history`, global history search; IC S01–S06 for Find | Find within a conversation is unbuilt (IC-5). Import has no real-history run. | verification + external dependency (IC-5) → SW-11 |
| E15 shipped Board/automation replay | `board_tools_e2e` (2), `board_store_e2e` (7), `automations_e2e`; [fix package](../../../design/board-automation-ticket-fixes/verification.md) | Current native replay only. | verification → SW-12 |
| E16 recurrence and host lifecycle | `automations_e2e` (30 + 1 live-gated); `qa-automations-live.mjs` (four `LUNA_DEV_*`, `LUNA_QA_AUTOMATION_PROVIDERS`, `LUNA_QA_AUTOMATION_TARGETS`) | Two real occurrences plus a sleep/restart campaign. | verification → SW-13 |
| E17 additional interactive app | IC-4/IC-6 A01–A07 | — | optional → SW-14 |
| E18 voice/phone/offline decision | [Control this Mac](../../control-this-mac/README.md) M0–M4 (M0 waits on CORE-01) | A decision record only. | optional → SW-16 |
| E19 switching campaign | SW-15, reusing E01–E18 | — | release gate |

Native runners that use a disposable QA app need all four `LUNA_DEV_APP_NAME`,
`LUNA_DEV_BUNDLE_ID`, `LUNA_DEV_IPC_PORT` and `LUNA_DEV_DATA_DIR` overrides
([QA guide](../../../reference/qa-infrastructure.md)). The picker runner and
`qa-workflow-baseline.mjs` attach read-only to an already running engine.

## Required and optional workflow set

A required journey blocks a complete-switch result until it passes on Codex and
Claude in the [selected modes](README.md#providers-accounts-and-modes).

| Journey | Required? | Contracts | Representative bounded case |
| --- | --- | --- | --- |
| Support/account investigation to draft | required | E01–E05 | One user-approved read-only Intercom conversation plus Moralis account and usage facts through Executor. Draft only. |
| Infrastructure/source investigation | required | E01, E04 | One user-approved Infra Monitor or Better Stack read through Executor, plus the owning source revision. |
| Requested parallel coding with delivery | required | E12, E13 | Three helpers on a disposable Git repo and local bare remote. |
| Interactive visual iteration and computer-use readback | required | E09–E11 | A self-contained mock or chart, plus one browser target with DOM and pixel readback. |
| Long-session recovery and steering | required | E06–E08 | A real session through steer, a late question, stop/restart, fork and rewind. |
| Past-chat handoff and skill migration | required | E03, E08, E14 | Sanitized imported Codex and Claude history, then a fresh handoff. |
| Local recurring follow-up | required | E16 | A current-chat heartbeat plus a new-chat follow-up across a restart. |
| Shipped Board/automation replay | required | E15 | The five-ticket fix package in a disposable project. |
| Additional connected app | optional | E17 | Chosen only if a real workflow needs it. |
| Voice, phone or offline execution | optional | E18 | Decision only. |
| OpenCode switching | optional for this plan | — | IC still requires it for HTML. |

Deterministic checks use sanitized, independent fixtures. Live reads use only
targets the user approves when the owning ticket runs. This baseline names no
customer, conversation or monitor. Production sends, snoozes and mutations stay
outside QA authority.
