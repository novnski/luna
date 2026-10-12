# CORE-03 engine services and supervised lifetimes

Source before milestone 1: `d81a9d4d`; before milestone 2: `431662b0` (both
12 October 2026). Live status belongs to `pending_tasks.py`.
- The failure cases were written first: [failures.md](failures.md).
- The service, ownership, ordering, cancellation, backpressure and stale-epoch
  rules are in [contract.md](contract.md), with the milestone plan.

## Milestone 1: one lifetime and admission owner

**Defect reproduced on the baseline.** Each transport gets its own `EngineRpc`,
and none of them checked whether its engine was still running:
- After `shutdown()`, a client the engine had handed out still ran
  `createChat`, and the chat persisted.
- After the same profile reopened, that stale client was a second writer. Its
  `applied` receipt landed in the shared ledger while the chat was lost from
  the workspace document. Every retry then deduplicated to `applied` and still
  created nothing: a permanent, silent loss of the user's command.

**Change** (`crates/engine`):
- **New `engine_lifetime.rs`:** `EngineLifetime`, owned by `EngineCore`, and
  `CommandAdmission`, shared by every `EngineRpc` the engine builds.
  - `EngineRpc`'s `RpcService::handle` admits each catalogued `Command` before
    its handler runs. The admission is held for the handler, and for the
    stream of a streamed command.
  - Queries and watches are unchanged.
- **`shutdown()` closes admission first.** It then waits up to 5 s for admitted
  commands before settling runs and flushing. `Drop` closes it too.
- **An admitted command holds the device lease:** a new `DeviceLease` keeps the
  InstanceLock owner alive, so the same root can't be reopened while the
  command runs.
- **A stopped engine never restarts its maintenance workers,** even if another
  RPC service is requested.
- **`device_write.rs` drops its blanket `dead_code` allow.** The stale "product
  writers are unwired" comment the audit flagged (F05) goes with it; every item
  in the file now has a production caller.

The refusal reads "This LUNA engine has stopped and accepts no new commands.
Reconnect to the running engine."

## Milestone 1 evidence

| Check | Result |
| --- | --- |
| New keeper `task_commands::stopped_engine_refuses_commands_and_cannot_poison_the_reopened_ledger` | Failed on the baseline (the stopped engine applied `createChat`, [log](evidence/keeper-before.txt)); passes after |
| Mutation M1: `EngineRpc` skips admission | Keeper red |
| Mutation M3: no close and a lease that ignores retirement | Keeper red |
| Mutation M2: close alone disabled | Keeper green, as expected: the retired lease still refuses. This is defence in depth, not a gap ([mutations](evidence/mutations.json), [script](evidence/mutations.py.txt)). |
| `luna-engine`, full | First run: 964 passed, 2 failed, 12 ignored ([counts](evidence/engine-full-first.json)). Both failures are explained below. |
| Focused engine suites after the final edits (`task_commands`, `chat_workbench`, `local_profiles`, `restart_resume`, `recovery`) | 91 passed, 0 failed, 1 ignored |
| `luna-ui` lib, run as CI runs it | 1,095 passed |
| `luna` app tests / `luna-headless` | 31 passed (1 ignored) / 3 passed |
| Clippy (`luna-engine`, all targets), rustfmt, `git diff --check` | Exit 0; no finding on a changed line |
| LUNA Dev refresh | 95 s, PID 61353, IPC 59777, signature verified, `EngineInfo` answered |

Every command and exit code is in [commands.json](evidence/commands.json);
artifact hashes are in [sha256.json](evidence/sha256.json).

**The two failures in the first full run:**
- `chat_workbench::command_receipts_revision_conflicts_and_account_profiles_are_isolated`
  already expected a stopped engine's client to be refused. It asserted the
  workbench guard's own message, but the engine's gate now refuses first. The
  assertion now expects the engine-level message; the refusal itself is
  unchanged.
- `board_tools_codex_e2e::codex_board_tools_follow_role_authority_and_context_through_luna_mcp`
  failed once under full-suite load: a side chat's first turn was missing its
  Board context. It passed 3 of 3 in isolation and never stops an engine, so it
  is unrelated to this change. It is flagged as a separate task to find
  whether the race is in the product or the test.

## Milestone 2: engine-owned services and one maintenance supervisor

**Defect reproduced on `431662b0`.** Every `rpc_service*()` call built a new
`EngineRpc`, and the newest live one became the agent's cross-task backend.
`luna headless --remote-port` builds the remote listener's restricted service
after the local one. That restricted copy has no chat workbenches, so the
agent's LUNA MCP tool list silently lost `get_workspace` and
`manage_workspace`.

Other effects of the same per-copy composition (failure cases S2–S6):
- Maintenance ran on whichever copy was built first. In a synced runtime that
  was the host relay's restricted copy.
- Every copy started its own session-history indexer and, on a copy-isolated
  root, its own hourly backup schedule. These outlived shutdown until the last
  client dropped.
- Child-thread, automation and Board maintenance were cancelled at shutdown
  but never joined.

**Change** (`crates/engine`):
- **`EngineCore` owns exactly two services,** each built once, lazily, on
  first request:
  - **The local service** (in-process, IPC, headless). It alone is the agent's
    cross-task backend and the Board authoring service, and it runs
    maintenance.
  - **The remote service,** shared by the host relay, the direct listener and
    each per-connection remote guard. It stays restricted (no project commands,
    workbenches or backup), so a remote action never starts the user's saved
    shell commands.
- **New `maintenance.rs`:** one supervisor with one cancellation token and one
  `TaskTracker`. It supervises:
  - cleanup;
  - child threads;
  - automations (both producers);
  - Board review;
  - Board lifecycle, with its runtime watch;
  - the session-history indexer;
  - the backup schedule.

  Shutdown cancels them all and waits up to 10 s. `Drop` cancels.
- **`EngineRpc::new` has no side effects;** `start_maintenance` starts the
  work instead. `EngineRpc` loses its per-copy cancel token and its `Drop`.
- **Unrelated Windows fix:** `apps/luna/tests/ssh_remote_script.rs` drives a
  POSIX shell script and is now `#![cfg(unix)]`. Windows CI had failed to
  compile it on every recent commit.

## Milestone 2 evidence

| Check | Result |
| --- | --- |
| Control: `pane_chat_mcp` Codex E2E with the new workspace-tool assertion and no remote factory, on `431662b0` | Passed |
| Keeper: the same test with the headless build order (remote factory after the local service) | Failed on `431662b0` ("get_workspace is not offered", [log](evidence/m2-keeper-before.txt)); passes after |
| Mutation M2-S1: the remote service registers as the agent's backend again | Keeper red, file restored ([mutations](evidence/mutations-m2.json)) |
| `luna-engine`, full | 966 passed, 0 failed, 12 ignored (102 binaries) |
| Focused (`pane_chat_mcp`, `task_commands`, `chat_workbench`) after the final edits | 52 passed |
| `luna-ui` lib, run as CI runs it | 1,095 passed |
| `luna` app tests / `luna-headless` | 31 passed (1 ignored) / 3 passed |
| Clippy (`luna-engine`, all targets), rustfmt, `git diff --check` | Exit 0; no finding on a changed line |
| LUNA Dev refresh | 103 s, PID 30561, IPC 59777, signature verified, `EngineInfo` answered |

## Limitations and next milestones

- **Not proven by a test:** admission closing at the *start* of shutdown (L3)
  and the lease being held for the whole handler (L4). Proving either would
  need a sleep standing in for an engine event. Both are enforced by
  construction and documented in the contract.
- **Not gated:** queries and watches on a stopped engine are still served from
  its last state. Watch streams are not cancelled on close; that needs a UI
  retry review first.
- **`DocsStore` stays unguarded** for internal engine writers. Closing
  admission and draining before teardown is the guarantee for now.
- **M2 by construction:** S2–S4 (one maintenance owner, one indexer and
  schedule, a joined shutdown) are proven by construction and review. Proving
  that a task is *absent* after shutdown would need a sleep.
- **The five public `assemble_*` entry points remain.** Merging them is API
  churn with no behaviour change; see the contract.
- **Still to come:**
  - M3: run coordinator and projection host.
  - Close-out work in the [contract](contract.md#8-milestones).
