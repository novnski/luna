# Providers idle-archive native follow-up

Tested source: **9bb805ac357bf71796e383daad37f5f5ede3e190**, including the
reviewed archive lifecycle fix `c1d425a0`. This is a report-only follow-up to
`provider-native-qa-2026-09-26.md`; it does not amend the original evidence or
add runtime changes.

## Native build and identity

The guarded refresh completed in **59 seconds**, using `CARGO_INCREMENTAL=0`
and the existing build profile/cache. The four assigned overrides were:

- `LUNA QA Providers`
- `com.novnski.luna.qa.providers`
- IPC `59781`
- `/Users/iulian/.codex/worktrees/e577/luna/target/luna-qa-data/providers`

PID **42775** owned `127.0.0.1:59781` and the exact executable
`/Users/iulian/.codex/worktrees/e577/luna/target/luna-macos-dev/LUNA QA Providers.app/Contents/MacOS/luna`.
Strict deep signature checks passed before and after QA. Binary SHA-256:
`9586a746e2711c7b431d851d95478b8e54b86d5ad40d48a54c48389c0d34623a`.
The app PID remained unchanged throughout every fresh task, completed turn,
archive, sibling continuation and cleanup operation below. No restart was
used between a completed turn and its archive.

Build evidence is under
`target/qa/provider-audit-2026-09-26/archive-followup-build/`.

## Targeted real-provider result

The app's refreshed catalog confirmed **Codex `gpt-6-luna` / High**. Two fresh
QA tasks shared one owned temporary workspace. Exactly three inference turns
were requested; every prompt prohibited tools, file reads and subagents.

| Step | Verified result |
| --- | --- |
| Warm sibling first turn | Exact `LUNA_ARCHIVE_SIBLING_BEFORE_a443d39e` |
| Idle target turn | Exact `LUNA_ARCHIVE_TARGET_a443d39e`; idle snapshot with no active turn |
| Archive idle target | Receipt `4f8ca575-cd8b-47e8-9107-544fcc540f9b` applied; native and LUNA archived; native task/binding unchanged |
| Sibling remains usable | Original warm server PID `43254` survived the target archive; exact `LUNA_ARCHIVE_SIBLING_AFTER_a443d39e` returned on the same native task/binding |
| Cleanup sibling | Receipt `661c52d5-e359-4261-9b85-f9088a2b7e8c` applied; native and LUNA archived without restarting the app |

The target is LUNA task `18fdd5a3-e139-4053-a212-b69319fbb945`, native task
`01a0dbba-44bf-7102-8d01-b09762a3a101`, binding
`729e867a-d716-4fec-b38e-c86204c9469c`, generation 1. These identities remained
stable after archive and after a fresh IPC connection read them again.

The sibling is LUNA task `c95e365d-e667-419a-961e-df3502308aec`, native task
`01a0dbba-23ff-7762-ad6f-b55d41ff94c0`, binding
`5685eb0b-ca63-4cc5-928d-7ea18109d23b`, generation 1. Its second-turn command
`b128a5ef-17c7-4a2e-b7ce-3d877a2797f6` applied. The process-survival claim is
specifically before that second send: the server process set changed during
the sibling's own follow-up lifecycle, while its native identity and successful
continuation remained intact.

## Driver assertion correction

The original driver sampled sibling-associated app-server candidates
`[43254, 43657]`, then incorrectly required both to survive archive. The saved
snapshot **immediately before archive** already contained
`[43254, 43937, 44488]`: PID 43657 had exited before the operation. After the
target archive, PID 43254 remained. The failed assertion therefore did not
show that archive retired the sibling runtime.

The failure was preserved, and the driver comparison was narrowed to the
established sibling candidate alive immediately before archive. A fresh,
explicit sibling continuation completed the originally planned test. The
target archive, earlier prompts and command IDs were not replayed. No new
target was created and no app restart occurred. This was a driver-only
correction; the tested application binary never changed.

## Evidence and cleanup

All live artifacts are under `target/qa/provider-archive-followup.RgQsJd/`:

- `result.json`, `marker-and-identity-readback.json`: final passing result,
  exact replies, stable native identities and workspace cleanup.
- `target-archive-idle-before.json`, `target-archive-after.json`,
  `target-archive-receipt.json`: idle state, successful archive and identity.
- `sibling-process-classification.json`, `sibling-after-target-archive.json`:
  causal PID evidence distinguishing the expired candidate from the survivor.
- `sibling-second-*`, `sibling-cleanup-*`,
  `target-persistent-identity.json`: independent sibling continuation,
  cleanup and fresh target archive readback.
- `failure.json`, `continuation-preflight.json`: original assertion failure
  and unchanged source/binary/app PID at continuation.

The preparation/driver files remain under
`target/qa/provider-audit-2026-09-26/archive-followup-prep/`. The driver uses
persistent transcript subscriptions and folds their deltas. It never restarts
the app or retries uncertain mutations. Existing restart-workaround artifacts
under `native/` remain separate and unchanged.

Both new task histories remain in the QA app and provider archives. The owned
`/private/tmp/luna-archive-followup-uTWVHB` directory was removed after verified
cleanup. Final storage readback: **9.8 GiB** `target/debug`, no release target,
**138.48 GiB** aggregate build/cache usage, **400.57 GiB** available space.

This closes the observed idle-writer archive symptom for this real macOS/Codex
sequence. The task owner's deterministic suite covers the queue-drain race;
that fault was not injected into a real inference here. Explicit native queued
steering remains the separate coverage gap recorded in the earlier report.
The prior 273-test audit batch was not rerun for this targeted native check.
