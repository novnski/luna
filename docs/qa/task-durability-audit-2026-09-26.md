# Task lifecycle and durability audit — 26 September 2026

Base: `7b141044c10650cc2ec476128763384cf5084cfe`.
Worktree: `c906/luna`; branch: `codex/audit-task-durability`.

## Failure modes recorded before implementation

1. A late `UpdateQueuedMessage` edits the text of a Claimed,
   SteeringAccepted, or DeliveryUnknown row. Its provider request has already
   been frozen; editing recovery evidence can misrepresent what was delivered.
   A late `MoveQueuedMessage` similarly changes the claimed revision/order.
   The legacy RPCs must return `changed=false` and retain the exact row, while
   an ordinary Held row remains editable and movable. Restart must retain the
   original uncertain text without dispatching it.
2. `BeginDraftSubmission` reuses an existing command ID with byte-identical
   draft content but a different destination task (or no destination). The
   store currently checks only the serialized snapshot, silently acknowledging
   the different binding. Exact retries must remain idempotent across restart;
   a changed destination must conflict and leave only the original submission.
3. A restored queue claim references a pending command from another task,
   another queue message, or an unrelated intent. Startup currently marks that
   unrelated receipt delivery-unknown. Only the exact task/message/delivery
   intent may supply reconciliation evidence or have its receipt updated.
4. Coordinator review identified the stale-replica counterpart of failure 1:
   a viewer still sees Held after the owner claims the row. Its local edit/move
   passes the document guard and chat2 imports change the owner's frozen
   evidence. Add a two-engine RPC/actual-import regression before extending
   the implementation. Route these methods to the owner and reject non-owner
   mutation locally; remote authorization still applies and offline edits
   must not fall back to mutating a stale local replica.

Regression coverage is added at the real local RPC → engine → document/SQLite
boundary before changing production code. Synthetic profiles and provider homes
are disposable. No unit tests were added. The reviewed build guard and isolated
app support come from `d1c71c3a31d6ef7c895adc9084c27293940fce39`.

## Reproductions and corrections

| Defect | Original runtime evidence | Correction |
| --- | --- | --- |
| Claimed queue evidence remained mutable | All six RPC attempts (edit/move × claimed/accepted/unknown) returned `changed=true`. | `SessionDoc` rejects legacy text edits and moves after claim; held rows retain both operations. |
| Draft retry omitted its destination | Retrying the same frozen canvas snapshot for `task-b` after restarting returned `stored=true`, although the submission still belonged to `task-a`. | Include nullable destination task identity in the immutable retry comparison. |
| Recovered claims changed unrelated receipts | Reopening the persisted queue stamped `ProviderAcceptanceUnknown` onto receipts for another task, another message, and a rename operation. | Match task, message correlation and delivery intent before using or updating a receipt. Live correlation settlement also checks task identity. |
| Stale viewer edits bypass the local status fence | Both viewer RPCs returned `changed=true`; successful `EngineChatSink` import rewrote the owner's Claimed row from `frozen provider prompt`, revision 1, first position to `stale viewer replacement`, revision 3, second position. | Route edit/move to the owner and reject mutation at a non-owner engine before publishing a CRDT update. |

The queue claim/restart contract originates in `noviulian` commit `d00559edb`;
the legacy text/move methods predate it. The draft envelope omission originates
in `noviulian` commit `138fadec0`. These are additional defects beyond the seven
already repaired at the audit base. No provider adapter or visual style changes
are included.

## Repeatable verification

Artifacts: `target/qa/task-durability-2026-09-26/`. `production.patch` and
`regressions.patch` preserve the exact code separation. Baseline runs used the
new regressions with only the production patch reversed. Each baseline reached
its behavioral assertion and failed (exit 101): 51 seconds for the first cold
compile/run, then 4 and 2 seconds for the queue checks. Full logs, exit codes,
commands and build-budget snapshots are retained under `baseline/`.

```sh
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine \
  --test drafts --test message_queue --test remote_doc_authority --no-fail-fast
```

The three added regressions are:

- `submission_retry_preserves_the_exact_destination_across_restart`
- `legacy_queue_edits_preserve_claimed_delivery_evidence_across_restart`
- `restored_queue_claim_cannot_change_an_unrelated_command_receipt`

The queue checks preserve the original text/revision over shutdown and reopen,
assert zero provider dispatches, and exercise the real local RPC where relevant.
The draft check accepts exact retries before/after restart and rejects changed
destinations without moving the original submission.

First fixed batch: **40 passed, zero failed** (drafts 3, message queue 33,
remote document authority 4). Compilation took 31.96 seconds; test execution
took approximately 7.3 seconds. Existing warning output is retained unchanged.
The worktree debug target was 2.6 GiB after this batch. Additional lifecycle
checks are recorded below; the native app handoff uses a separate build slot.

## Remote-edit boundary

The owner-admission ledger prevents execution of a remotely modified row; it
does not undo edits imported by `EngineChatSink`. The new two-engine regression
`stale_viewer_queue_mutations_cannot_rewrite_the_owners_claimed_evidence` freezes
the viewer at Held, claims on the owner, attempts both legacy RPCs, and reconnects
the actual chat2 sink before comparing the complete queue projection.

The narrow routing correction intentionally removes offline/local-replica
edit/reorder fallback. Remote actions require the owner and its existing
authorization policy; no remote capability is added by this patch. Existing
legacy clients or arbitrary raw CRDT updates may still alter replicated display
data, but owner admission continues to prohibit their execution. Protecting
all replicated content from arbitrary foreign edits would require a separate
sync trust/projection change and is not claimed by this audit.

The first stale-viewer fixture run stopped at `PendingDependencies`, before
host-row comparison, because its delta omitted the viewer's initial operations.
It is preserved as `baseline/stale-viewer-fixture-missing-deps.log` (exit 101),
and is not product-failure evidence. Transferring the full replica snapshot
fixed the causal bridge without changing production code. The corrected run
completed import and failed the exact before/after queue comparison (exit 101);
see `baseline/stale-viewer.log`, including both complete rows.

UI verification includes a composer → memory RPC → queue subscription flow
that checks false acknowledgement and owner-error rollback, owner targeting,
and one mutation attempt. The existing revisioned edit/remove suites also cover
lease conflicts/expiry, attachment commit/cancel, revision fencing, and failed
projection recovery.

The corrected two-engine fixed run passed. The complete second engine batch
passed **41 tests** (drafts 3, queue 33, remote authority 5), followed by
**70 lifecycle tests** (recovery 24, restart 8, task commands 33, identity 5).
The opt-in real Claude restart test remains ignored. Document and sync library
checks passed **194 tests** (119 + 75). These runs include the existing
revisioned edit/remove behavior. Full results are under `remaining/`; that
runner places disposable fixtures in a task-specific temporary root and removes
them when the batch exits.

Final focused verification: **319 passed, zero failed**, with one opt-in live
Claude restart test ignored. This includes **13 queue UI tests** and the
existing settings round-trip test in addition to the 305 backend/persistence
checks. The queue UI regression confirms both false-ACK and owner-error rollback
through a fresh WatchQueue opening frame, exact owner routing, and no replay.
Locked compile and Clippy checks passed for engine, doc, sync, and UI; pre-existing
warning output remains in the full logs. Formatting and diff whitespace pass.

The first UI compile exposed test-fixture errors, not a runtime product defect:
the new GPUI flow needed `read_with(TestAppContext)`, and the existing settings
round-trip initializer lacked `diff_color_scheme`. The failed log is retained
at `remaining/ui-queue.log`; corrected UI/check/Clippy logs and commands are in
`ui-final/`. The settings initializer repair is a separate one-line commit,
`65c3800d78ede049ddba7caaa6169dca15876ad8`, verified by its exact round-trip test.

The debug target is approximately 10 GiB after checks; no additional Cargo
target was created. `budget-after-checks.json` records aggregate accounting.
The subsequent native refresh/readback is recorded below.

## Native combined-code verification

The clean branch fast-forwarded to reviewed consolidated source
`8169b69219f5d1b598f64189a8e2b1b937aa3d7c` before building. With the native
slot granted, the guarded refresh completed in **90 seconds** using
`CARGO_INCREMENTAL=0` and exactly these overrides:

```sh
LUNA_DEV_APP_NAME='LUNA QA Tasks'
LUNA_DEV_BUNDLE_ID='com.novnski.luna.qa.tasks'
LUNA_DEV_IPC_PORT=59782
LUNA_DEV_DATA_DIR="$PWD/target/luna-qa-data/tasks"
```

The app runs from this worktree's
`target/luna-macos-dev/LUNA QA Tasks.app/Contents/MacOS/luna`. Its initial PID
was **95051**, with that same process listening on **127.0.0.1:59782**.
Strict deep codesign verification passed. The plist identity and data path
matched the assigned values. The build slot was released immediately after
launch, before the IPC checks. The main application and data were not touched.

The repeatable IPC artifact is `native/persistence.mjs` under the audit artifact
directory. It creates one disposable unconfigured task, persists a draft and
recoverable submission, verifies same-target idempotence and changed-target
rejection, and inserts two **Steer-intent** rows. Those rows cannot automatically
become new turns. Local held-row editing and reordering both succeeded.
Readback verified the draft/submission and exact queue contents, revisions,
status and order, with an empty transcript and no started task session.

An exact-PID graceful termination followed by LaunchServices reopening the same
bundle (without Cargo) changed PID **95051 → 636**. All draft/submission and
queue values survived unchanged. The repeated draft ID remained idempotent for
the original target and rejected a null destination. The script then discarded
the recoverable draft, removed both rows, archived the disposable task, and
reread the empty queue, empty draft, absent submissions, archived task and empty
transcript. Both phases passed with **17 recorded RPC responses each**, one
expected draft-conflict response per phase and no other RPC errors. No provider
turn was started; the provider audit owns real Codex turn/steer/interrupt coverage.

Disposable task: `qa-tasks-ad4dc442-afac-495b-9109-3b62644574a0`.
Final exact process/port and strict signature were reverified after restart.
Signed executable SHA-256:
`d249f92ac0659b97098eb23f69e51dbaf62f276984529986c6009672deba6978`.
The source SHA, signed and pre-signing binary hashes, refresh output, plist,
process/port/signature readbacks, before/after RPC records and restart log are
retained in `native/`. The first seed attempt used an overly specific error-word
matcher; the engine correctly rejected the changed destination. That script-only
failure is retained as `seed-error-wording.log` and its RPC record; the corrected
attempt reused the same disposable task rather than leaving another task behind.

Final worktree debug storage is approximately **13 GiB**, with no release target.
The post-readback budget snapshot was **123.74 GiB aggregate** and **409.54 GiB
free**, below the shared guard limits. The QA app/data and all review artifacts
are retained. Native verification changed no production source; this report is
a new follow-up commit rather than an amendment to the verified fixes.
