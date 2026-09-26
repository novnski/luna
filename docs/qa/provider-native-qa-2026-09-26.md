# Providers native QA — 26 September 2026

This follows `provider-audit-2026-09-26.md`. The signed app was built from clean
consolidated source **8169b69219f5d1b598f64189a8e2b1b937aa3d7c**. This report
adds no runtime changes and does not amend the verified audit commit.

## App handoff

- App: **LUNA QA Providers**, bundle ID `com.novnski.luna.qa.providers`.
- Bundle: `/Users/iulian/.codex/worktrees/e577/luna/target/luna-macos-dev/LUNA QA Providers.app`.
- Data: `/Users/iulian/.codex/worktrees/e577/luna/target/luna-qa-data/providers`.
- Guarded refresh: **126 seconds**, `CARGO_INCREMENTAL=0`, all four assigned
  identity/data/port overrides supplied. No guard bypass or primary-app refresh.
- Initial PID **91386** owned `127.0.0.1:59781`; after a graceful QA-only restart,
  final PID **22428** owns the same port and exact bundle executable.
- `codesign --verify --deep --strict` passed both before and after restart.
  Source SHA, binary SHA-256, plist, process, listener, signature and storage
  evidence are retained under `target/qa/provider-audit-2026-09-26/native/`.

## Real provider checks

Every inference used this app's IPC, its normal task command admission, a
disposable workspace, and **Codex `gpt-6-luna` at High**. `ListModels` with
`refresh=true` confirmed availability and supported reasoning levels. The
provider's task metadata reported CLI **0.157.0**. No credentials were inspected
or accounts switched. Claude/OpenCode coverage remains fixture-based.

| Check | Observed result | Evidence |
| --- | --- | --- |
| Normal reply | Exact `LUNA_PROVIDER_NORMAL_cb798d0d`; run receipt applied | `normal-transcript-latest.json`, `normal-run-receipt-final.json` |
| Tool plus direct active-turn steer | Exact final `LUNA_PROVIDER_STEER_DIRECT_cb798d0d`; steer receipt applied | `steer-transcript-latest.json`, `steer-deliver-receipt-final.json` |
| No replay in the successful direct-steer case | Tool proof and inline tool result contain `LUNA_PROVIDER_TOOL_DIRECT_cb798d0d` exactly once; one user steer entry; original tool resolved | `steer-no-replay.json`, `workspace-files.json` |
| Interruption | Stop receipt applied; assistant terminal status `aborted`; no requested post-sleep completion marker | `interrupt-stop-receipt-final.json`, `interrupt-transcript-final.json` |

The accepted direct steer is command `fa99ff1c-a1d8-43db-afe7-ecda42ae3a33`;
the accepted Stop is `1ae30476-4787-431c-948f-ce23cfd5797e`. The prompt, initial
reply, settled receipt, and visible transcript are retained separately. The
first driver's missing task-revision/turn-ID rejections are driver mistakes,
not application failures; their original artifacts remain available.

The real no-replay observation covers this successful steer. The lost-ACK
transport fault itself is covered by the deterministic subprocess regression,
not by fault injection against a real account.

## Queued steering classification

The exploratory queued case is task `9404536a-5586-44e1-8a4f-ff232cbfb9a0`,
row `089f48a3-4c31-45b5-873b-336fb29ed04c`. Its enqueue request supplied
`holdForTurnEnd=true` but omitted `followUpBehavior`; the observed row is
**`followUpBehavior=queue`**, revision 1. It was not a conformant Steer-intent
row. No rejected row or its text was retried.

| Boundary | Epoch milliseconds | Evidence |
| --- | --- | --- |
| Enqueue applied, task revision 4 | 1790389908231 | `queued-attempt/steer-queue-request.json`, `steer-queue-reply.json` |
| Fenced manual steering accepted for validation | 1790389908233 | `queued-attempt/steer-deliver-request.json` |
| Manual control returns `conflict / queueClaimed` | 1790389908235 | `queued-attempt/steer-deliver-reply.json` |
| Automatic next-turn dispatch accepted | 1790389918059 | `queued-case-classification.json` internal dispatch receipt |
| Automatic dispatch remains pending / `deliveryUnknown` | 1790390026300 | Same receipt and retained queue row |

The full queue row immediately between enqueue and the manual control was
**not read**. The preserved streaming transcript predates enqueue; the receipt
snapshots carry the same active turn ID. These are not an atomic observation
of provider completion.

Source at the tested revision explains why `queueClaimed` alone is not proof
of an early claim: `dispatch_queued_with_claim` blocks manual steering when
`follow_up_behavior != Steer`, and the RPC maps that `Blocked` outcome to
`queueClaimed`. The reply's row correlation is consistent with reaching that
projection branch. The automatic dispatch receipt was accepted **9.828 seconds
after enqueue**. `drain_queue` also checks `turn_in_flight` before next-turn
dispatch. This evidence supports a driver intent mismatch with a broad reason
label; it does not demonstrate a held-row early-dispatch defect.

The eventual automatic dispatch's unknown acceptance is preserved as an
unresolved observation for the task-audit owner. Its cause is not established
here. Full command events, receipts, queue, transcript and native binding are
in `queued-case-classification.json`. `post-restart-state.json` shows that the
unknown row survived restart without being replayed. The archived task and
its row/history remain inspectable in the QA app data.

## Archive cleanup and follow-up limitation

Initial IPC archive attempts exposed a separate lifecycle limitation: idle
tasks had no active turn, yet Codex rejected `thread/archive` with **“already
has an active writer (code -32600)”**. Other requests timed out or remained
pending. `archive-before-restart/` and `cleanup-fresh-readback.json` retain the
exact receipts. This was reported to the coordinator for task-lifecycle review;
this report does not claim a code fix.

The QA app was gracefully quit using the verified PID/executable and reopened
from the same existing bundle/data. No force quit, rebuild, or primary-app
operation was used. Once the idle writer leases were released, IPC archive
and readback confirmed **all five disposable tasks archived in both LUNA and
Codex**. One initial LUNA watch snapshot lagged its applied archive receipt;
a fresh read converged and is saved separately. `cleanup-verification.json`
is the final authoritative five-task readback.

The owned `/private/tmp/luna-provider-qa-jempUZ` workspace was removed only
after saving its proof files, file metadata and archive verification. Provider
history, queued-row evidence, app data, packaged app and build cache remain.
Final local storage: **9.8 GiB** `target/debug`, no `target/release`, about
**132.54 GiB** aggregate build/cache usage and **404.73 GiB** available space.

## Repeatable artifacts

The native artifact directory contains `ipc.mjs`, `normal.mjs`, `direct.mjs`,
and `cleanup.mjs`, plus every fixed prompt, command ID, initial reply, settled
receipt and final readback. The attempted queue driver and original conflicts
remain under `driver-attempt/` and `queued-attempt/`. Use a fresh artifact
directory and unique QA IDs/workspace for a new live run; do not replay the
saved row. Live runs require the coordinator's slot and consume account quota.

The earlier fixture batch remains **273 distinct passing tests** (288 test
executions including focused reruns), with compile/format/Clippy verification.
It was not rerun for this report-only follow-up. No additional inference runs
were made to retry the rejected queue row or diagnose its unknown delivery.
