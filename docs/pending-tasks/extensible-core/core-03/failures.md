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

## Remaining CORE-03 failure focus (later milestones)

- **Run coordination:** a split moves globals but leaves competing writers, for
  example per-`EngineRpc` state that must be engine-wide.
- **Projection:** replay after a lost reply duplicates work, or a receipt is
  recorded before durable admission.
- **Resources:**
  - Storage locks outlive or underlive their actual consumers.
  - Watch streams from a closed engine keep running.
  - Subprocesses or subscriptions leak after cancellation.
