# Claude Code advanced lanes: certification ledger

This ledger covers parity packages CC-13 (workflows, teams, channels and Claude's external sessions) and CC-16 (hosted and device integrations). It is pinned to Claude Code **2.1.286**, binary SHA-256 `75e3016e9d2570767b08e43a7467d4817a4f149232c169ca295f2c95fef21433`.

The UI ledger (`crates/ui/src/claude_advanced.rs`, `CLAUDE_ADVANCED_LEDGER`) shows the same rows in Settings → Providers. Keep the two in step.

## Statuses

| Status | Meaning |
| --- | --- |
| Certified | Verified against the installed CLI with retained evidence. Any LUNA behaviour on top of it is covered by E2E tests. |
| Supported · live QA pending | Implemented and proven end to end with frame shapes taken from the installed binary. A real provider run is still pending. |
| Not enabled by LUNA | The native feature exists, but LUNA never turns it on. Imported or replayed data still decodes safely. |
| Unsupported in LUNA | LUNA cannot do this today. The UI says so. |
| Externally gated | Needs an account, plan, OS permission or hosted service that LUNA does not embed. |
| Native Claude Code only | Usable only in Claude Code itself. LUNA offers no embedded control and does not claim support. |

No row was certified by running a model turn. Agents had no isolated login, and the default configuration would write into `~/.claude*`. Live acceptance steps for the lead are in the cc13 evidence (`live-acceptance.md`).

## CC-13 — workflows, teams, channels, external sessions

| Lane | Status | LUNA behaviour | Evidence |
| --- | --- | --- | --- |
| Dynamic workflow runs (`task_type: local_workflow`) | Supported · live QA pending | The harness decodes `task_started`, `task_progress` (`workflow_progress` phases/agents), `task_updated` and `task_notification`. Each observation becomes `AgentEvent::NativeTaskChanged`, which the engine stores in the chat document (`meta.nativeTasks`) before the parked-session gate. The Agents pane shows each run. A run is never a subagent, and it never writes transcript text. | SDK schema and emitter excerpts in `cli/sdk-schema-and-emitters.txt`; harness `claude_coordination::native_workflow_graph_survives_the_parked_wake_shape`; engine `claude_native_tasks::*`; fixture PNG 01/04 |
| `workflow_progress` field | Supported · live QA pending | The workflow emitter sends this field, but the SDK schema does not list it. It is treated as an optional full snapshot (logs filtered). If it is absent, the last graph is kept and the UI says the graph was not reported. | Emitter call site in the schema excerpt |
| Terminal and lifecycle rules | Supported · live QA pending | Terminal states are sticky: a late progress frame or a second outcome never rewrites one. `killed` and `stopped` both map to Stopped. `worker_restart` is shown as such. When the provider process ends mid-run, the task is settled as Stopped · Claude process ended. A still-reporting workflow keeps a parked session past the 30-minute idle reaper. | Harness and engine tests above |
| Workflow availability | Certified | LUNA follows Claude's own gate. `CLAUDE_CODE_DISABLE_WORKFLOWS=1` removes `/deep-research` and `/workflow-authoring` from `initialize`. LUNA's Ultracode effort sets the `ultracode` setting, which is existing behaviour. | `probes/init/results.json` |
| Stop a workflow | Unsupported in LUNA | Claude 2.1.286 accepts `control_request {subtype: "stop_task", task_id}`. LUNA has no task-control channel in `RunControls` yet, and CC-08 owns native task cancellation. | `cli/sdk-schema-and-emitters.txt` (stop_task schema and handler) |
| Pause, resume, restart agent, save script | Native Claude Code only | Only the TUI `/workflows` view has these. The SDK has no control for them, and `/workflows` is absent from the print-mode command list. | `probes/init/results.json` (`all_commands`) |
| Workflow agent approvals after the launching turn | Unsupported in LUNA | The engine's parked gate auto-declines permission requests that arrive with no active turn. Workflow agents that need approval after the turn ends are denied. The workflow card says so. | `crates/engine/src/sessions.rs` parked gate |
| Agent teams (`in_process_teammate`) | Not enabled by LUNA | The opt-in (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1` or the hidden `--agent-teams`) is never set. Teammate tasks started by external configuration decode as an experimental kind, never as subagents or child chats. | Binary string evidence; `probes/init` teams case; harness test |
| Channels | Not enabled by LUNA | LUNA never passes the hidden `--channels` or `--dangerously-load-development-channels` flags. In stream-json without `--replay-user-messages`, inbound channel prompts are not echoed, so LUNA neither displays nor attributes them. | Binary flag list; live 2.1.228 capture (task-notification prompt not echoed) |
| Cross-session peer messages | Unsupported in LUNA | LUNA-launched Claude children drop the messaging and bridge identity variables a parent Claude session exports (`CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_BRIDGE_SESSION_ID`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_JOB_DIR`). A LUNA chat therefore never joins a foreign session's peer bridge. Non-human prompts never become human permission. | Harness `launched_claude_child_drops_inherited_session_identity` |
| Claude sessions outside LUNA (`claude agents --json`) | Certified (read-only) | Settings → Providers lists background and interactive sessions read-only, and marks LUNA's own print-mode processes. LUNA never attaches to, stops, removes or respawns them. An unreadable listing shows as unavailable, never as "no sessions". | `probes/bg/bg-lifecycle.json`, `probes/bg/print-session-listing.json`; harness `external_session_listing_maps_real_rows_and_explicit_unavailability`; fixture PNG 02/03 |
| Two-writer protection on resume | Supported · live QA pending | A cold `--resume` checks the listing first. If another live, non-LUNA process holds the same session, the run is refused before launch (`RejectedBeforeAcceptance`) with the native handoff (`claude stop <id>`). LUNA's own children and stopped rows never block. The check fails open when the listing is unavailable. | Harness `resume_*` tests |
| Remote agents (`remote_agent`) | Externally gated | These decode as a remote-agent kind when observed. They need Claude cloud access. | Binary task-type literals |

### Probe facts (no model turn)

- `claude agents --json` does not start Claude's supervisor. `claude --bg` starts a transient supervisor, which exits when idle. A real lifecycle ran in an isolated config: start (no prompt) → list → logs → stop → `--all` list → rm. The user's `~/.claude` daemon and job state were unchanged (mtime check).
- Background rows carry `id`, `state` and, while alive, `pid`/`status`. Stopped rows keep `state: "stopped"` and have no `pid`.
- Every live print-mode Claude process is listed as `kind: "interactive"`, including LUNA's own. That is why ownership is checked against the engine process.
- `initialize` reports `remote_control_available: true` even when unauthenticated. It is an availability hint, not proof of a working bridge.

## CC-16 — hosted and device integrations

| Service | Status | Supported interface | Identity and permissions | Results and assets | Stop or revoke | LUNA |
| --- | --- | --- | --- | --- | --- | --- |
| Claude in Chrome | Native Claude Code only | `--chrome` and `--no-chrome` flags. No embeddable API. | Chrome extension, claude.ai plan and per-site permissions | Browser tool results inside Claude | Extension and site permissions | Never passes `--chrome`. `initialize` is unchanged with `--chrome` (probe). |
| Computer use | Externally gated | Built-in MCP inside Claude | macOS Accessibility and Screen Recording, plus a plan gate | Screenshots and actions inside Claude | OS permission revocation | LUNA never grants OS access and does not enable it. |
| Remote Control | Native Claude Code only | `--remote-control` and `claude remote-control`, interactive only | claude.ai account. Bridge identity env is scrubbed for LUNA children. | Remote client drives an interactive session | End the session or revoke in claude.ai | Distinct from LUNA's own remote transport. Not embedded. |
| Cloud sessions and teleport | Native Claude Code only | `--cloud`, `--teleport`, `--environment` | claude.ai account and organisation | Cloud VM session | Cloud session controls | Not embedded. No import or link of cloud session ids. |
| Cloud Projects | Externally gated | No embeddable interface verified. `claude project` only purges local state and is unrelated. | Account | — | — | Not integrated. |
| Artifacts | Externally gated | Hosted publication inside Claude | Account and sharing scope | Published links | Unpublish in claude.ai | LUNA does not publish on Claude's behalf. |
| Routines and cloud schedules | Externally gated | Hosted (`RemoteTrigger`) | Account or organisation | Hosted runs | Disable in claude.ai | Separate from session cron (CC-14). Not integrated. |
| Ultrareview | Native Claude Code only | `claude ultrareview [--json] [--post]`. Billed. | Account | `bugs.json` findings | `--timeout` or cancel the CLI | LUNA does not start it. `/ultrareview` is absent from the print-mode command list. |
| Push notifications | Externally gated | `PushNotification` tool | Claude mobile app and account | Device notification | App settings | Not integrated. |
| Desktop handoff | Native Claude Code only | `--desktop` | Claude Desktop | — | — | Not integrated. |

No credential store was read, no hosted service was called, and nothing was published, posted or scheduled.
