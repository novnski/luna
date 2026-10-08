# Workspace and orchestration engine review

Review baseline: `d3819203`, changes since 2026-10-03. Scope: persistent child
and fork chats, agent workspace tools/defaults, owned work controls, automations,
and portable context. Builds, native app operation, and final validation are
centralized by the parent review task.

## Failure cases recorded before implementation

### W1: Surviving forks lose workspace tools when their source is deleted

`agent_workspace_root` follows both parent and source provenance and requires
every ancestor to retain a live local identity and chat row. Deleting a source
preserves its ordinary persistent fork, but `get_workspace` subsequently fails
with a target-scope error. Descendants of that fork also lose discovery and
background placement. A moved ancestor produces the same failure.

The root search must retain its caller validation, cycle/depth checks, and
same-project/host boundary. When an ancestor becomes unavailable, the last
valid local conversation becomes the root. It must not reconnect surviving
forks to unrelated conversations or allow deleted callers themselves.

Failure-first integration scenario: create a source and two nested portable
forks through RPC; discover their shared workspace; delete only the source;
verify both surviving forks discover the first fork as their root, retain each
other as related, and reject unrelated targets. Record before/after JSON.

### W2: Failed delegated children are displayed as successful work

The engine finalizes both successful and errored turns with
`MessageStatus::Complete`; errored turns retain an explicit `MessagePart::Error`.
The child row in `GetTaskWork` only checks the message status, so it reports
`completed` and `needsAttention: false` for a failed delegated turn.

Failure-first integration scenario: keep the parent provider turn running,
create a child whose provider reports an error, wait for normal settlement, then
read the shared work projection. Require `failed`, attention, terminal/nonblocking
status, retained ownership, and no Stop control. A failed child must not become a
successful completion simply because its transcript is finished.

### W3: Archive can hide or terminate live native background work

Workspace archive preflight uses `active_turn_id`, which correctly becomes empty
after the root response even when native children continue. The ordinary archive
command does not separately check live background observations. Codex idle-writer
retirement also treats that runtime as idle and can close it. The fix must be
archive/history-operation specific and must not redefine ordinary turn activity.

Failure-first integration scenario: a persistent Claude fork emits a native
child and completes its root response while the child remains live. Require both
agent archive preparation and direct `SetArchived` to refuse; the fork and runtime
must remain intact. Deliver the exact child's terminal event and require archive
preparation and the ordinary archive command to succeed afterward. Native Codex
archive retirement coverage remains in the existing `idle_archive` suite.

The input/lifecycle reviewer owns the shared sessions predicate and direct archive
admission. This reviewer owns workspace preflight and the integration scenario.

## Implemented changes and ownership

| Finding | Severity | Change | Failure-first integration |
| --- | --- | --- | --- |
| W1 | P2 | `rpc/agent_workspace.rs` stops ancestry traversal at an unavailable, tombstoned, foreign-host or foreign-project ancestor. The surviving local conversation becomes the root; caller validation, cycles, and depth limits remain. | `agent_workspace_controls::surviving_forks_keep_workspace_tools_after_source_deletion` |
| W2 | P2 | `rpc/task_work.rs` checks a completed child's explicit error part before declaring success and marks failed children as needing attention. | `task_work::failed_child_work_remains_failed_and_needs_attention` |
| W3 | P1 | Workspace archive preflight checks live background observations. The input/lifecycle reviewer added `SessionsEngine::has_live_background_work`, the shared idle-writer retirement guard, and direct `SetArchived` admission. `active_turn_id` retains its existing meaning. | `task_work::archive_refuses_background_native_work_until_its_terminal_event` |
| W4 | P2 | `side_chats.rs::stop` uses ordinary `sessions.interrupt` so its direct compatibility RPC retains the recovery hold; it no longer opts into the durable Stop command's separate-hold path. | `recovery::legacy_side_stop_holds_ordinary_fork_recovery_across_restart` |

This worker changed only `crates/engine/src/rpc/agent_workspace.rs`,
`crates/engine/src/rpc/task_work.rs`, their two existing integration-test files,
and this report in the first batch. The second batch adds only the one-line
`side_chats.rs::stop` correction and an appended integration scenario in
`tests/recovery.rs`. Shared `sessions.rs` / `rpc.rs` changes belong to the
input/lifecycle reviewer. No unit tests were added.

## Feature and edge-case coverage

The table records source inspection and existing executable coverage, **not a
claim that those suites were run during this worker's pass**.

| Feature / entry points | Reviewed edges and current guard | Existing or added verification |
| --- | --- | --- |
| Child creation (`child_threads.rs`) | Fresh brief versus context fork; default provider; explicit model, reasoning and model-option validation; installed/enabled provider filtering; account/auth-instance pinning before and after provisioning; four-level/cycle bounds; ordinary local project requirement. | `claude_child_chats`, `cross_task_tools`, `agent_workspace_controls`, `agent_delegation_background` |
| Child access and checkout | Parent effective permission ceiling; explicit escalation grant; hidden Ask refusal; inherited read-only restriction; alternate existing path canonicalization; new-worktree provisioning recorded separately from task commit; cleanup only for fresh uncommitted checkouts. | `claude_child_chats`, child creation cases in `cross_task_tools` |
| Child durability and reporting | Stable create/start/outbox/report IDs; startup receipt separated from provider completion; reports hold until parent turn ends; archived/missing/off-host parent defers delivery; provider-aware Claude peer envelope; bounded synced row budget and delivered-outcome compaction. | `agent_delegation_background`, `claude_child_chats`; no new defect established in these paths |
| Persistent forks (`side_chats.rs`, `rpc/side_chats.rs`) | Ordinary independent lifecycle; legacy lifecycle migration only for materialized rows; interrupted reservation does not rerun native fork; exact context creation hash; no expiry/view-unload teardown; separate source/task identity; durable tombstones; native binding verification; source deletion survival. | `side_chats`; W1 adds missing agent discovery coverage after source deletion |
| Fork context | Native idle-source readiness; explicit snapshot fallback; bounded frozen source text; anchored source message validation; clear-context/provider boundary suppresses reinjection; ordinary provider instruction channel preserves quoted-data attribution. | `side_chats`, `context_transfer`; native-provider history owned by history reviewer |
| Agent workspace (`rpc/agent_workspace.rs`) | One exact local project; caller/root protection; unrelated target rejection; bounded unique target set; revision required for pane/split destinations; optional latest-layout application for chat-ID actions; exact request/layout receipts; partial archive/placement outcomes surfaced. | `agent_workspace_controls`, `claude_luna_tools`, `pane_chat_mcp`; W1 and W3 add uncovered lifecycle edges |
| Tool discovery (`cross_task_tools.rs`) | Runtime-delivery capability and granted scopes; focused optional guide; bounded output; model catalog deadline; cancellation; unavailable provider diagnostics; mutation tools retain separate approved/fenced execution path. | `cross_task_tools`, `claude_luna_tools`, provider-specific LUNA tool suites |
| Provider defaults (`provider_defaults.rs`, RPC facade) | Profile-scoped file; symlink/size guard; exact model/reasoning/options; optimistic revision; atomic replacement and file sync; current settings not rewritten; explicit child selections override defaults. | `agent_workspace_controls::provider_defaults_persist_reject_stale_or_unsupported_choices_and_resolve_children` |
| Execution settings (`sessions_execution.rs`, permissions resolver) | Requested/admitted/observed values distinct; private options omitted; pending provider/account/workspace/model/reasoning/options/permission change blocks steering; changed child ceiling rechecked; no live runtime treated as historical execution. | Existing execution scenarios in `e2e`, `message_queue`, `claude_child_chats`; shared lifecycle implementation owned by input reviewer |
| Owned work (`rpc/task_work.rs`, `sessions_work.rs`) | Ancestry distinguished from launch ownership; initial child ownership versus later independent turn; recursive native child references; shell/native-ledger deduplication; services excluded; cold live claims become unknown; bounded traversal/pages; no implicit stop-all. | `task_work`; W2 fixes terminal failure classification |
| Exact stop controls | Private run/message/session/binding fence; live provider capability; stable sorted preview digest; receipt saved before effects; exact replay never repeats stop; unsupported work excluded; acknowledgement remains requested until terminal evidence. | `task_work`, including existing failed SQLite intent/completion writes and warm reuse cases |
| Automation definitions (`automation.rs`, `rpc/automations.rs`) | Name/prompt/timezone/interval/calendar/PR validation; exact preview hash and one-minute expiry; owner/revision checks; replayed mutation ID must have same payload; pause/delete do not stop accepted work; cancel only an exact unsent claim. | `automations_e2e` |
| Automation scheduling / store | Latest eligible missed occurrence; explicit lateness bound; DST gap skip and earliest fold; excessive calendar backlog pauses; deterministic task/message/command IDs; CAS claim; one active/unknown run per definition; queue/recovery/active target blocks delivery; previous boot becomes unknown rather than replay. | `automations_e2e`, including double consumer/restart, 100-row reconciliation pagination, pause at admission, busy queue, new-conversation permissions, and atomic write failure |
| PR watches (`source_control/pr_watch.rs`, scheduler) | `gh api` GET only; validated repo/PR identity; exact SHA check/status queries; bounded pages and duplicate/incomplete/changing-page refusal; self remarks ignored; first baseline quiet; stable change cursor; four concurrent polls and 45-second deadline; backoff; finite wake budget including skipped busy attempts; external content quoted as data. | Existing injected GitHub-boundary `automations_e2e` cases; live GitHub observation was **not** run in this pass |
| Portable context (`context_transfers.rs`, `doc_host_context_transfers.rs`, RPC facade) | Completed visible user/assistant text only; partial UTF-8 ranges; conservative prompt/window budget; explicit preview/apply/send boundary; matching target/source history and binding; pending/unknown dispatch blocks reinjection; exact correlated native acceptance; delta deduplication; new task/ledger/projection atomicity. | `context_transfer`, including deleted source after capture, stale preview, rollback, unknown restart, partial delta and provider roundtrip |
| Local dispatch holds | Read-only preflight versus post-provider unknown acceptance; ExecutionUnavailable hold survives automatic drain/edit/cancel-edit; explicit Send now repeats checks; native uncertainty not classified by error strings. | `docs/design/harness-evolution/slice-4.md`; source inspected read-only, implementation/repro coverage owned by input reviewer |

## W4: legacy StopSideChat during ordinary recovery (second batch)

Failure cases recorded before the second-batch implementation: `side_chats.rs:869` calls
`sessions.interrupt_for_stop`, which explicitly disables the interruption-owned
recovery hold because ordinary durable Stop commands supply that hold in their
own projection. `StopSideChat` does not produce that ordinary Stop projection.
Legacy `SendSideChat` requests cannot create recovery chains, so their existing
stop test correctly sees no recovery row. Ordinary queued fork requests can
recover (`recoverable_fork_run` accepts their frozen run command). A compatibility
RPC stop during an accepted recovery attempt leaves its row `Running` after the
runtime exits; both explicit retry and cancel refuse that state. On restart,
reconciliation retains the known accepted attempt and classifies it as an
interruption, allowing a safe-continuation provider to schedule automatic work
again. Codex supports that continuation contract.

No `StopSideChat`/`STOP_SIDE_CHAT` call site was found under `crates/ui`; the current
UI uses ordinary task controls. This defect is confined to the exposed
compatibility RPC. The failure-first public-RPC/restart integration scenario is:
create a persistent fork; queue an ordinary request; cause a retryable provider
failure; explicitly resume its recovery and hold the retry; call `StopSideChat`;
assert no live runtime and a **Held** recovery row, then restart and verify no
automatic retry. Preserve the provider call count across restart, then require
one new explicit retry to complete the same chain without repeating original
effects. Reuse `RecoveryHarness::AcceptedOverloadThenPending` and the existing
fork command/recovery helpers; no provider fixture or architecture change is
needed. The production repair is one line: use ordinary `sessions.interrupt`,
which supplies the interruption-owned hold for a direct stop.

## Validation status and central requests

Scoped `rustfmt --edition 2024` and `git diff --check` succeeded. This worker did
not run Cargo, start an application, create branches/worktrees, or commit/push.
The four added scenarios were written before their associated production fixes.
Both code batches are frozen for parent review and centralized compilation/E2E.

Required focused suites: `agent_workspace_controls`, `task_work`, `idle_archive`,
`side_chats`, and `recovery`. Broader unchanged-contract checks: `agent_delegation_background`,
`claude_child_chats`, `automations_e2e`, `context_transfer`, and the input reviewer's
queue/execution scenarios. Use the project's locked `scripts/cargo.sh` flow and
central build-budget admission rather than parallel worker builds.

Set `AGENT_WORKSPACE_EVIDENCE_DIR` for workspace-test JSON and
`LUNA_WORK_EVIDENCE` for work-test JSON. New named artifacts are
`surviving-fork-workspace.json`, `failed-child-work.json`, and
`background-archive.json`. The second-batch scenario writes
`legacy-side-stop-recovery.json` when `FORK_EVIDENCE_DIR` is set. Preserve command
logs, exit codes, and SHA-256 hashes
with the central validation artifacts. Live Claude/Codex/OpenCode verification
and the final single-app LUNA Dev refresh remain the parent task's responsibility.

Current known product limits remain explicit: automations accept Codex/Claude
(plus the mock verification harness); native child stop requires adapter support;
workflow/background-shell stop may remain unavailable; uncertain automation or
context delivery is never automatically replayed; workspace layouts/results are
bounded and can report partial placement separately from successful task creation.

## Native runner profile overrides: failure cases before editing

Scope: `qa-agent-workspace-controls.mjs` and `qa-workspace-native.mjs` only,
following the strict optional profile override pattern already implemented in
`qa-context-transfer-live.mjs`. Preserve the existing default identities and
existing evidence-directory environment variables.

1. Any subset of the four `LUNA_DEV_APP_NAME`, `LUNA_DEV_BUNDLE_ID`,
   `LUNA_DEV_IPC_PORT`, and `LUNA_DEV_DATA_DIR` values must fail before IPC or
   fixture creation. Empty, multiline, unsafe-name/ID and invalid-port values
   must also fail; the primary port `59777` is forbidden.
2. A full override must identify a signed QA bundle in this checkout, its exact
   listener PID/executable, matching display name/bundle ID/port/data plist
   fields, and `LUNA_QA_MCP_ISOLATION=1`.
3. QA data must be absolute, canonical, below this checkout's canonical
   `target/`, and outside the bundle staging/primary data tree. Symlinks and
   foreign-checkout paths cannot redirect a verified profile.
4. `qa-workspace-native` must validate uploads and cleanup against the verified
   selected QA data root, never its old hardcoded profile after an override.
   Cleanup remains restricted to exact returned upload files and its disposable
   fixture; validate the canonical paths again before removing returned files.
5. Existing output directory variables (`AGENT_WORKSPACE_NATIVE_DIR` and
   `LUNA_NATIVE_REPORT_DIR`), scenario modes, provider catalog selection and
   scenario scope must remain intact. No build, GUI start or provider execution
   belongs to this edit/verification pass.

Implemented the optional all-four profile override in both runners. Defaults
remain `LUNA QA Agent Workspace` / `59804` and `LUNA QA Workspace` / `59783`;
the latter still accepts its existing `LUNA_IPC_PORT=59783` invocation. All QA
paths now require the signed MCP isolation flag. Evidence directory environment
variables and the scenario bodies are unchanged; identity evidence includes the
selected profile and verified data root. Upload deletion rechecks the canonical
selected root and exact returned file path.

Verification: both `node --check` commands and scoped `git diff --check` passed.
Twenty-four subprocess preflight cases passed (each runner rejected partial,
empty, multiline, unsafe app name, unsafe bundle ID, primary port, nonnumeric
port, out-of-range port, relative data, external data, noncanonical data and
bundle-staging data). All rejected before signature commands, fixture creation
or IPC; no GUI/provider call, build, new checkout or worktree was performed.
The coordinator owns successful signed `LUNA QA Review` / `59841` execution.
