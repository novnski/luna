# CORE-00 execution baseline

Source baseline: `ffdf7b835cdb87a8ff02a10c48f2e308aa329c36`, 11 October 2026.
The runtime tree is unchanged from the [source audit](../source-audit.md)
at `2838125e`; the intervening app change is iOS README guidance only.
Do not repeat that inventory or treat its line counts as performance evidence.

This ticket freezes contracts and exercises architecture risks. Its probes are
disposable design instruments, not the production plugin API. CORE-01 onward
owns the migration. Live status and ownership belong to `pending_tasks.py`.

## Invariants and existing owners

| Invariant | Production owner | Existing strongest proof / later owner |
| --- | --- | --- |
| One writer per local profile; shutdown releases actual ownership | `engine/src/lib.rs`, `engine/src/profile.rs`, `engine/src/device_write.rs` | `local_profiles`, `restart_resume`; CORE-03 |
| Commands carry stable task identity, principal, admission and durable receipt | `proto/src/task_command.rs`, `engine/src/rpc.rs`, `engine/src/task_identity.rs` | `task_commands`, `task_identity`; CORE-01/03 |
| Provider wire semantics survive a transport change | `harness/{codex,claude,opencode}` and `Harness` | Distinct adapter subprocess peers; CORE-09/10/11, broader live matrix SW-00 |
| Journal/replay and transcript ordering preserve user data | `engine/sessions.rs`, `doc_host.rs`, `doc/parts.rs` | `timeline_order_e2e`, `tool_output`, `restart_resume`; CORE-03/14 |
| A disconnected client releases watches | `rpc/client.rs`, transport server | `subscription_lifecycle`; CORE-05 |
| Local control is not a plugin's authority | `rpc/remote_auth.rs`, engine admission | Device routing and denied-call receipts; CORE-06/07 |
| Native focus, selection, drafts and recovery remain host-owned | `ui/shell.rs`, `composer.rs`, `transcript.rs` | Current native fixtures; CORE-12/13 |
| Unknown content remains durable and has a safe readable fallback | Future versioned envelope, current document migration boundary | CORE-00 feasibility only; production migration CORE-14/15 |
| Process separation does not remove native ambient OS authority | Worker launcher and platform trust policy | CORE-00 ADR; enforcement CORE-06/07/19 |

Source module names above identify their crate's `src/` tree; test names identify
that crate's `tests/` suite. Retain existing owners; do not duplicate
their parser tests in a generic plugin conformance suite. Current timeline
keepers include failures added after the historical 9 October review. Historical
passing or failing observations must not override a fresh result.

## Evidence and limitations

The authoring gate and pre-code failure cases are in [failures.md](failures.md).
The executed [results](results.md), [ADR](decision.md) and
[repeat commands](../../../../tools/core-baseline/README.md) are the handoff
for subsequent CORE tickets.
Raw local execution artifacts live under `target/qa/core-00/`; portable,
sanitized receipts and hashes will be recorded here. Build flags, exact source,
hardware and measurement boundary travel with each result. macOS evidence does
not certify Linux, Windows, low-end hardware, iOS or live provider performance.
No production task, provider credentials or user conversation is a benchmark
fixture. Baseline failures stay separate from prototype regressions.
