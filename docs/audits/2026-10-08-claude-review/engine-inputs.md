# Engine input and turn lifecycle review

Scope: changes since 2026-10-03 through `d3819203`, especially `4b21e811`,
`e29ad36e`, `8e131b34`, and the later short `ask_user` IDs.

## Failure cases recorded before fixes

1. A provider question with a recommended answer is raised by background work
   while the root turn runs. The root completes before its countdown expires.
   `take_provider_inputs` moves the live resolver out of the pending map and
   `reregister_late_ask` gives it a new request ID. The existing timer still
   addresses the old ID, and no timer/countdown is registered for the new ID.
   Result: the unattended answer never arrives, and the child waits forever.
   The repair must preserve the original absolute deadline, not reset it on
   each root/wake boundary, and must preserve a user's prior Hold choice.
2. `ask_user` queues `AsyncQuestions` and acknowledges acceptance immediately.
   If it arrives after root Done, the parked-session gate drops it through its
   default branch. Result: the acknowledged question never reaches the user.
   The repair must retain a finished card without reopening a provider turn,
   changing waiting status, or making a replayed event create a second card.
3. Coordinated with the workspace reviewer: archive promises to refuse
   running work, but foreground `active_turn_id` does not include a parked
   root's live native children/tasks. Native archive can close their provider
   writer; non-native archive can hide their still-running chat. Preserve
   foreground turn semantics and add an explicit live-background-work guard.
   The workspace reviewer recorded the failure case and wrote its shared
   lifecycle E2E before this guard was implemented.

## Verification plan

Full-engine integration cases are added before implementation to the existing
`crates/engine/tests/e2e.rs` suite. They exercise the input bridge, event loop,
pending resolver, document writer, and session state. Their compact document
evidence is written under `LUNA_AUDIT_EVIDENCE_DIR`, or a process-specific
directory in the system temporary directory when that variable is unset.
No Cargo/build/app commands are run by this reviewer; the parent coordinates
execution and the required final handoff.

## Changes made

- `PendingInput.unattended` retains both the answers and their absolute
  deadline. Carried cards rearm the timer against their fresh request ID and
  receive the same visible countdown. A held question has no saved plan, so
  carrying it cannot silently re-enable automatic selection.
- The parked event gate accepts `AsyncQuestions`, journals it, and writes a
  finished standalone card. Existing question IDs are ignored on replay.
  This path neither reopens the root turn nor changes session waiting state.
- Added the two full-engine regressions
  `parked_async_question_is_visible_once_without_reopening_turn` and
  `carried_question_preserves_unattended_deadline_and_hold`. The latter also
  checks that Hold survives card replacement and that a manual answer still
  reaches the original native resolver.
- Added `SessionsEngine::has_live_background_work` based on the run's existing
  native work observations. Direct `SetArchived` refuses to archive while
  those observations remain live. The shared native idle-writer release
  guard also refuses to retire that provider, protecting archive, rewind and
  compaction. The workspace reviewer added the archive tool's preflight
  guard and the `task_work` full-engine regression
  `archive_refuses_background_native_work_until_its_terminal_event`.

## Reviewed coverage

| Feature | Paths and result |
| --- | --- |
| Provider-neutral `ask_user` contract | `runtime_contract.rs` schema, bounded question/option validation, provider runtime guidance; `sessions.rs::invoke_ask_user` short stable IDs and event bridge. Fixed post-Done event loss. |
| Answer, answer later, amendment, duplicate | `doc_host.rs` RespondAsyncQuestion and `async_answer_prompt`; answers use ordinary durable prompt/steer delivery, blank selections keep cards open, equal latest answers are deduplicated, differing answers append amendment history. Existing full-engine cases cover these paths. |
| Answer after restart | `doc_host.rs` recovered RespondInput/RespondApproval and `deliver_prompt_with_inputs`; ordinary recovered input requires a settled open request ID and prior/current config. Recovered secrets and approvals are never converted into a user prompt; MCP acceptance cannot recreate an expired native resolver. |
| Provider input lifecycle | `sessions.rs` request_input bridge, respond_input, respond_secret_input, respond_approval, pending-input cleanup and expiry; `proto/agent.rs::unattended_answers` excludes approvals, MCP and secrets. Fixed lost automatic-answer schedule when a live child request is carried. |
| Hold/manual/automatic answer competition | Shared pending map removes one resolver. Hold removes the plan. Automatic resolution marks the card as selected by LUNA. New carried-question regression covers both unattended and held paths. |
| Cancelled turns | Local Stop drains blocking resolvers before cancelling the provider; terminal provider interruption fences queued follow-up work. An approval Cancel turn that Claude reports as errored is normalized to Interrupted. Existing scenario coverage remains necessary for real provider wording. |
| Stalled turns | Informational timer ignores pending inputs and provider retry notices, uses a longer window for live tools, and clears on real stream progress. It does not terminate or restart the run. |
| Background/wake lifecycle | `sessions.rs` parked event gate, carried provider inputs, separate late cards, fresh wake segments, end-of-stream synthesis; `sessions_work.rs` live work observations was read only. Active EOF produces Errored Done; EOF after a finished parked turn exits Idle. |
| Archive with live background work | `sessions.rs` active-turn versus live-native-work accounting and native idle-writer release; `rpc.rs` SetArchived. Added guards without making parked sessions look like active foreground turns. |
| Streaming card history | `doc/schema.rs::SegmentWriter::sync` only refreshes changed folded parts; answer history written directly to an unchanged async card is retained. No change needed. |

The Claude adapter review raised a missing follow-up Done candidate. The engine
synthesizes Errored Done on active stream EOF, so this was not counted as a
separate confirmed engine defect. A process that retains an open stream is a
different condition and was not reproduced here.

## Requested validation

Run the two new cases in `luna-engine --test e2e` with `--exact --nocapture`
using `scripts/cargo.sh test --locked`; set `LUNA_AUDIT_EVIDENCE_DIR` to the
parent's retained evidence directory. Also run the existing
`async_question_skip_and_late_answer_use_distinct_durable_paths`,
`async_answer_steer_waits_for_in_flight_tool_boundary`,
`interrupt_unblocks_a_run_awaiting_input`, and
`mcp_form_url_validation_concurrency_and_secret_boundaries_e2e` cases.
The workspace reviewer additionally requests the new `task_work` archive
regression above, which verifies both refusal during native child work and
successful archive after its terminal event.

For real-provider validation, the existing `qa-opencode-scenarios.mjs` S9Q,
S9U and S2X scenarios cover nonblocking cards, unattended answers and late
background input/wake turns. Add a root-completion-before-countdown checkpoint
and inspect that the carried card keeps its original deadline. These checks
have not been run by this reviewer.

Local verification performed: `rustfmt --edition 2024` on the two changed Rust
files, followed by `git diff --check`; both succeeded. Formatting log:
`/tmp/luna-engine-inputs-rustfmt.log`. No unit tests, build, Cargo test, app
control, branch, worktree, commit or Git transfer was run.

Central compilation found `E0433` in the new `SetArchived` guard because
`EngineError` is not imported in `rpc.rs`. Corrected the narrow reference to
`crate::EngineError::Other`; the parent owns the compile rerun. No additional
build or test was run by this reviewer for that correction.

The parent's concurrent engine E2E run passed the new regressions but exposed
a timing flaw in the existing late-amendment fixture; its isolated rerun
passed in 0.19s. `ScriptedHarness` advertises steering but never consumes its
live receiver. After preserving the duplicate-answer check's timing, the
late-amendment case now waits for `!core.sessions.has_live_run(CHAT)` before
submitting its amendment and reports an explicit rejected receipt immediately.
Production behavior and the separate live-steering boundary case are unchanged.
The parent owns the full four-thread E2E rerun.

## Goal-resume stack correction

The central extended suite and an isolated default-stack run both aborted
with stack overflow in
`cold_goal_resume_accepts_completion_before_readiness_is_observed`. A diagnostic
run with an 8 MiB stack passed in 0.43s. The crash report shows an ordinary RPC
goal-resume path through `apply_goal_operation`, `attach_existing_goal`,
`dispatch_inner` and runtime-tool catalog construction, with no recursive
command loop.

Read-only disassembly of `codex_goals-c23e5a2f2dca334f` found these debug poll
frame reservations, including the 32-byte saved-register prologue:

- `RpcService::handle`: `0x119cb0 + 0x20` bytes (about 1.10 MiB).
- `apply_task_command_intent`: `0x830c0 + 0x20` bytes (about 524 KiB).
- `execute_task_command_with_fences`: `0x145f0 + 0x20` bytes (about 81 KiB).
- New native-prefix preflight: `0x2290 + 0x20` bytes (about 8.7 KiB).

The fix boxes `apply_task_command_intent` at its single caller so its large
future storage does not inflate the surrounding RPC futures. This is a
production-path stack fix, not a larger test-stack setting. It does not spawn
another task, reorder provider operations, release the command lane, or detach
cancellation: the same cancellation guard surrounds the same awaited work,
and receipt/error handling remains immediately after that await. Allocation
is one owned future per task command and is released on completion or drop.

The existing failing full-RPC test is the before-fix regression. Parent-owned
validation must rerun all `codex_goals` cases with the default stack, followed
by the relevant `task_commands` suite. No test/build was run by this reviewer.
