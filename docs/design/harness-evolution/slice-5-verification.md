# Host-owned automation acceptance

Verified on 2026-10-05. Evidence is retained under
`target/qa/harness-evolution/slice-5/`, with failed attempts kept separately from
the successful clock, native workbench and real-provider runs.

## Engine and provider boundaries

Thirteen clock-controlled automation E2Es pass through the actual RPC, SQLite,
task-command and provider-peer paths. They cover one durable occurrence with two
consumers, restart and uncertain delivery without replay, sleep coalescing, pause
and task-lane races, held user queues, new-chat permission ceilings, transactional
receipt failure, DST gaps/folds, expiring previews, invalid timezones, archived
targets, fair reconciliation and an excessive calendar backlog. The backlog case
records a failure and pauses that definition instead of blocking the sweep.

PR scenarios verify head changes, unchanged state, own-comment filtering,
same-second remark IDs, bounded wake budgets, failed reads and pagination beyond
100 rows. Forty stalled PR watches cannot prevent an ordinary due prompt. A
separate live, read-only GitHub check passed against `BurntSushi/jiff#635`; no
GitHub comment, change or other external write was made.

The native agent-tool protocol scenario proves that an exact local **Allow once**
can save one definition. Forged thread identity, a repeated provider call, changed
payload under the same key, ordinary approval responses and task-wide permission
cannot grant persistent work.

The broader focused suite passed 142 tests in total, including these automation
cases and Activity, child chat, context transfer, queue, recovery, restart,
cross-task tool and task-command regressions. Engine/sync and UI Clippy passed;
existing unrelated warnings remain. The existing 1,185 UI library tests passed.
No new unit tests were added after implementation.

## Native workbench

The final complete `chat-workbench-fixture` run passed and captured 79 PNGs plus
`readback.json`. The coordinator reviewed the automation editor, remote-host
restriction, completed history and paged Settings view. The workflow verifies
review invalidation, unconfirmed mutation and exact receipt recovery, IANA-zone
preview, PR validation, a real two-second scheduler-worker dispatch, pause,
selection changes, reconnect and 52 rows across two pages.

The final one-shot row offers **Edit time** after its time passes. Clicking it
opens the exact automation and revision for editing, without a Resume request.
The editor explains that applying a newly reviewed future time resumes it.
Readback also proves the scheduled message appears once in its intended chat and
creates no new send command in either the original Main or Child A.

## Real Codex and Claude runs

`scripts/qa-automations-live.mjs` ran against the signed disposable
`LUNA QA Automations` bundle on IPC 59813, with its own data directory. Live model
catalogs supplied Codex `gpt-6-luna` and Claude `claude-fable-5-1`, as recorded in
the manifest. Each provider received one short scheduled marker prompt under Ask and
workspace-write permissions. Both occurrences reached Completed, with exactly
one user message and the expected assistant marker.

Each provider also had a separate one-shot paused before its due time; neither
produced an occurrence. Restart changed the app PID from 90291 to 94158. Readback
preserved both completed occurrence IDs, one user message per target and zero
paused occurrences. Completed definitions were paused too.

The first disposable setup approved a schedule before Git checkout discovery
finished. Its identity fence correctly produced Skipped and no message. That
record was preserved. The successful fixture waits for the actual checkout
identity before review and uses fresh schedules; it never retries unknown work.

## Cleanup and repeatability

Successful live evidence is in `native/run-O3X32J/`; the earlier skipped setup is
in `native/run-r3ALvW/`. Their data remains available. The stopped QA bundle was
signature-verified, hashed and unregistered before scoped removal. Workbench
temporary profiles were checked for open files, archived and hashed before
removal. The shared build cache and the primary application's data were retained.

Repeat engine cases with an absolute `LUNA_AUTOMATION_EVIDENCE` path and
`scripts/cargo.sh test --locked -p luna-engine --test automations_e2e`. The live
script documents run, inspect and restart modes; inspect uncertain results before
performing any new run. Use the four QA app identity overrides from the QA guide.
Full-output logs, screenshots, receipts, manifests and cleanup records are
covered by the evidence directory's `SHA256SUMS`.

The primary **LUNA Dev** app was refreshed in 72 seconds. Its exact bundle
process (PID 10272), IPC listener on 59777 and strict deep code signature all
passed readback; `handoff.json` records the executable hash. The 101 GiB dev cache
was reviewed against the aggregate budget and retained for the continuing final
slice; no other checkout or build owner was present. Final scoped cleanup follows
the performance work, preserving app data and evidence.

Host schedules run only while LUNA is open on their owning host. A failed or
uncertain delivery is surfaced for review; it is never automatically replayed.
