# CORE-03: what counts as a failure

Written before any CORE-03 code, on `d81a9d4d` (12 October 2026). These are
E05's lifecycle failures, made concrete for the current engine. Each row names
the public boundary that must prove it.

## Reproduced on the baseline

A temporary probe drove the real RPC boundary with a temp store and the
production assembly path:

1. **A command from a shut-down engine is still admitted.** After
   `EngineCore::shutdown()` returns, an RPC client the engine handed out
   earlier runs `createChat`, gets `applied`, and the chat persists.
2. **A stale engine is a second writer after the same profile reopens.** With
   engine B open on the profile, a client kept from engine A still runs
   `createChat` and `createSpace`, and both report success.
   - The receipt lands in the shared SQLite ledger, but the chat never reaches
     B's workspace document.
   - B and every later engine show no chat, yet `GetTaskCommandReceipt`
     reports `applied`.
   - A retry of the same command on the current engine is deduplicated to
     `applied` and still creates nothing.
   - The user's command is lost permanently and silently.

## Failure cases for the lifetime and admission owner (milestone 1)

| ID | Failure | Required behaviour | Owner proof |
| --- | --- | --- | --- |
| L1 | A command reaches a handler after its engine began shutting down | Refused with a clear "engine stopped" error. No receipt, document or file write. | `task_commands` E2E through `memory_client` |
| L2 | A stale engine's client writes after the same profile reopened | Refused. The reopened engine has no receipt for it, and the same command ID then applies normally there. | Same E2E, with a real reopen on the same root |
| L3 | Shutdown tears down stores while a command admitted just before it is still running | Shutdown first closes admission, then waits (bounded) for admitted commands before settling runs and flushing. | Shutdown order in the contract. Any timeout is logged with the count still running. |
| L4 | An admitted command outlives its engine and a new engine opens the same root | The admitted command holds the device lease (the InstanceLock) until it returns, so the new engine cannot open alongside it. | `InstanceLock` contention, via the device-write lease |
| L5 | A closed engine restarts maintenance workers because a caller asked for another RPC service | A closed engine never starts cleanup, child-thread, automation or Board maintenance again. | Contract plus review |
| L6 | Gating breaks normal work: reads, watches or quit-time saves are refused while the engine is open, or the UI loses a draft at quit | Only catalogued `Command` operations take admission. Queries and watches are unchanged. The UI's prepare-quit already waits for drafts before engine shutdown. | Existing suites (engine, UI fixtures) stay green |
| L7 | Internal engine work is refused | Admission sits only at the transport boundary (`EngineRpc`'s `RpcService::handle`). Internal callers and maintenance workers are owned and drained by shutdown. | Existing suites |

**What the keeper catches:** an `EngineRpc` that serves commands without
passing the engine's gate. A mutation that skips admission must turn it red.

**What it can't catch:** L3's ordering, admission closing at the start of
shutdown rather than the end, matters only during teardown. Proving it at the
public boundary would need a sleep standing in for an engine event, so it is
enforced by the code order and the [contract](contract.md) instead. L4's lease
is likewise held by construction (`AdmittedCommand` owns it); no timing test
was added.

## Milestone 2: composition root and maintenance supervisor

Written before the M2 code, on `431662b0`. Each `rpc_service*()` call built a
new `EngineRpc`, and every copy brought its own side effects:
- a session-history indexer (the activity bootstrap) writing the store;
- an hourly backup scheduler, when the root is a copy-isolated backup root;
- registration as the agent's cross-task backend, where the newest live copy
  wins;
- a Board authoring service.

The first copy built inside a runtime also owned the five maintenance workers,
and only the cleanup worker was joined at shutdown.

| ID | Failure | Required behaviour | Owner proof |
| --- | --- | --- | --- |
| S1 (reproduced) | The agent's tools depend on which transport was built last. A remote factory built after the local service, as `luna headless --remote-port` does, becomes the newest cross-task backend. It has no chat workbenches, so `get_workspace` and `manage_workspace` disappear from the agent's LUNA MCP tool list. | Agent tools, Board discussions and maintenance always use the engine's one full local service. | `pane_chat_mcp` Codex E2E with the headless build order. It failed on `431662b0`. |
| S2 | Maintenance runs on whichever copy was built first. In a synced runtime that is the host relay's restricted copy (no project commands or workbenches), so behaviour differs by profile. | Maintenance always runs on the local service. | Construction (one supervisor, started from the local service). Review. |
| S3 | Indexers and backup schedulers multiply with every `rpc_service()` call, and outlive shutdown until the last client drops. | Exactly one of each per engine. They stop at shutdown. | Construction and review; existing session-history suites stay green. |
| S4 | Child-thread, automation and Board maintenance are cancelled but not joined, so they can still be running after `shutdown()` returns. | Shutdown cancels every supervised task and waits for it, bounded at 10 s. Anything still running at the bound is logged and stops at its next cancellation point. | Construction (`TaskTracker`). Existing cleanup, automation and Board suites stay green. |
| S5 | The remote transports start running the user's saved project commands, for example when a phone starts a Board ticket in a new worktree. | Remote transports keep today's restricted service, without project commands, workbenches or backup. Every one of those methods is `LocalOnly` in the catalog. | Catalog check plus existing remote suites |
| S6 | The single service misses state attached after assembly (auth, links, updater) or a test's late field change. | Services are built lazily on first request, as each copy was. | Existing device-routing, inspect, SSH and remote CLI suites |

A timing test that proves a task is *absent* after shutdown would need a sleep
standing in for an event, so S3 and S4 are proven by construction instead.

## Remaining CORE-03 failure focus (later milestones)

- **Run coordination:** a split moves globals but leaves competing writers, for
  example per-`EngineRpc` state that must be engine-wide.
- **Projection:** replay after a lost reply duplicates work, or a receipt is
  recorded before durable admission.
- **Resources:**
  - Storage locks outlive or underlive their actual consumers.
  - Watch streams from a closed engine keep running.
  - Subprocesses or subscriptions leak after cancellation.
