# Evidence, live probes and repeatability

All observations below were made on 2026-10-04, Europe/Bucharest. Source facts, installed-app behavior, existing user examples, and proposed Luna work are kept separate.

## Versions and source boundaries

| App | Local source inspected | Installed app exercised |
| --- | --- | --- |
| T3 Code | 4ee6bfd50ef4a089440d5c3662db2298da9cc50e | 0.0.46-nightly.20261004.2648 |
| BB | a2bbfaadaa91084cde68caa06954c8927f0015a4 | 0.45.0 |
| Luna | 0800dfc6ec3ed05ef74a0e52f517eb44099508dd plus pre-existing work | Not rebuilt or runtime-tested by this research |

The two reference checkouts were updated with SSH fast-forward pulls. T3's pre-existing untracked docs/research directory was preserved; BB remained clean. Luna's existing source, tests and pending documentation were preserved. This package is a separate new directory.

[baselines.json](evidence/baselines.json) includes the before/after and both review baselines. [source-index.json](evidence/source-index.json) records SHA-256 fingerprints for selected source files; it is not a claim that every line in every repository was audited.

The substantive evidence is local source, commit diffs, installed UI, the BB CLI, and tightly scoped read-only T3 SQLite queries. Release terminology was initially cross-checked against the upstream migration announcement.

Luna's rpc.rs changed concurrently while the report was being written. Its handoff selection/limits, provider-switch path and automation-availability declaration were reread at the end and still support the findings. The original file fingerprint is retained; the verifier reports this working-tree drift rather than pretending the entire mutable checkout was frozen.

## Live results

| ID | Scenario | Outcome | Evidence |
| --- | --- | --- | --- |
| T3-OBS-1 | Open the user's completed Claude→Codex child card | Child transcript, original prompt/result 73, actual Codex model, parent navigation and lineage observed | Initial UI readback; this was existing work, not a newly spawned test child |
| T3-E2E-1 | Projectless Codex→Claude transfer | Claude recalled ORBIT-CEDAR-641 without it appearing in the new request | [AX](evidence/t3-provider-handoff.txt), [image](evidence/t3-provider-handoff.jpg) |
| T3-E2E-2 | Return Claude→Codex with a second marker | Codex recalled ORBIT-CEDAR-641 and MAPLE-208 | [AX](evidence/t3-roundtrip-and-merge.txt), [bindings](evidence/t3-provider-bindings.json), [handoffs](evidence/t3-handoffs.json) |
| T3-E2E-3 | Native Codex fork, then continue | Persistent new app thread, distinct native Codex ID, source-turn reference, both markers recalled | [bindings](evidence/t3-provider-bindings.json), [transfer records](evidence/t3-transfers.json) |
| T3-E2E-4 | Merge fork context back into parent | Parent recalled fork-only marker FORK-MERGE-517 | [AX](evidence/t3-roundtrip-and-merge.txt), [image](evidence/t3-roundtrip-and-merge.jpg), [handoffs](evidence/t3-handoffs.json) |
| T3-E2E-5 | Queue and edit during an active turn | Edited request persisted in queue and ran after WAIT_DONE; returned QUEUE_EDITED_OK | [queue before dispatch](evidence/t3-queue-edited.txt), [final transcript](evidence/t3-queue-result.txt), [final image](evidence/t3-queue-result.jpg) |
| BB-E2E-1 | Projectless eager fork before new prompt | Stable fork ID and source-derived title existed before a new user message | UI readback and [CLI metadata](evidence/bb-fork-readback.json) |
| BB-E2E-2 | Continue the BB fork | Codex recalled SILVER-BIRCH-902 | [AX](evidence/bb-eager-fork.txt), [image](evidence/bb-eager-fork.jpg) |

All eight recorded T3 test runs were completed at final readback: seven in the parent and one in the fork. See [t3-runs.json](evidence/t3-runs.json). BB's fork readback was idle. These are small functional probes, not performance measurements.

### Persistent test identities

- T3 parent: ecce5cc4-8416-4a84-aaf3-9e241fb13d91.
- T3 fork: 18fa694d-dcd2-46f2-865f-ff02ac99b309.
- T3 local environment: aec26b09-8c9c-4898-9d9d-e3be39a9f1c3.
- BB source: thr_sin3dht5et.
- BB fork: thr_pinm6gsbmv.

These chats are retained as inspectable evidence. A sidebar settle interaction in T3 did not produce a confirmed state change; no successful settlement or deletion is claimed. No test turn remains in progress. No ongoing automation was created.

## Repeating the probes

Use new projectless test conversations. Do not reuse a real work conversation. Replace markers with fresh values if repeating the test.

### T3 continuity and merge-back

1. Select Codex GPT-6.1-Sol / Low. Send: “LUNA harness research probe. Do not use tools, delegate, or modify files. Remember ORBIT-CEDAR-641. Reply exactly: BASELINE_OK ORBIT-CEDAR-641.”
2. After completion, change the provider to Claude Opus 5.5. Send: “Do not use tools, delegate, or modify files. What marker did I ask you to remember in the previous turn? Reply with only that marker.”
3. Confirm the visible Context handoff and exact answer. Send: “Do not use tools or modify files. Also remember MAPLE-208. Reply exactly: SECOND_OK.”
4. Return to Codex. Ask for both earlier markers without including them in the request.
5. Click Fork from the completed response. Confirm a distinct app thread exists before typing.
6. In the fork, ask it to recall the original markers and add FORK-MERGE-517, with no tools or file changes.
7. After completion, use Merge back in Lineage. In the parent, ask only for the new marker added in the merged fork.
8. Inspect provider bindings and transfer receipts, not just the transcript. The recorded example has a native fork reference, a delta handoff on the return trip and an injected fork delta.

### T3 queue edit

1. In the isolated test conversation, send: “Controlled queue UI test. Use exactly one shell tool call running /bin/sleep 30, then reply WAIT_DONE. Do not inspect or modify files, browse, or delegate.”
2. While the sleep is active, type and submit: “No tools. When this queued follow-up runs, reply exactly QUEUE_FIRST.”
3. Click Edit queued message, replace the sentinel with QUEUE_EDITED_OK, and click Update queued message before the active turn finishes.
4. Confirm the queue row contains the edited text.
5. Wait for completion. Expect WAIT_DONE followed by QUEUE_EDITED_OK, with the original sentinel absent from assistant output.

The request constrained the test command to sleep; the UI's full working duration was longer than the sleep itself. It includes provider/tool overhead and is not a latency benchmark.

### BB eager fork

1. Open BB and choose Don't work in a project. Select Codex 6-Sol / Low.
2. Send: “LUNA BB harness research probe. Do not use tools, delegate, or change files. Remember SILVER-BIRCH-902. Reply exactly: BB_BASELINE_OK SILVER-BIRCH-902.”
3. From the assistant message menu, select Fork into new thread.
4. Before sending another message, confirm a new stable URL/thread ID, copied transcript, source-derived title and Forked from link.
5. Ask only for the marker from the source conversation. Expect SILVER-BIRCH-902.
6. Independently read the created thread through BB CLI. In this run the CLI was invoked through the installed version's npm package because bb was not on PATH. The initial provider query failed while the app server was stopped; opening the installed app restored access, and the later thread-show call succeeded.

Example read-only command for the recorded fork:

~~~sh
npx --yes --package=bb-app@0.45.0 bb thread show thr_pinm6gsbmv --json
~~~

Do not treat projectless fork lineage as agent-child ownership: the CLI result has sourceThreadId but parentThreadId and lifecycleOwnerThreadId are null.

## Read-only persisted T3 verification

The installed app's database was discovered at /Users/iulian/.t3/userdata/statev2.sqlite. Only the two test thread IDs were queried. No authentication tables, settings secrets, unrelated conversation payloads or database writes were used.

[readback.sql](evidence/readback.sql) repeats the bounded metadata reads:

~~~sh
sqlite3 -readonly -json /Users/iulian/.t3/userdata/statev2.sqlite < docs/pending-tasks/orchestrator-v2-and-bb-2026-10-04/evidence/readback.sql
~~~

The query output contains provider IDs, native conversation references, run states and context-transfer metadata. It does not extract credentials or hidden reasoning. Separate JSON files in this package preserve the observed values.

## What was not verified live

No crash/restart recovery, lost-acknowledgement injection, concurrent duplicate dispatch, deep descendant cancellation, filesystem rollback, scheduled execution, PR watch, native question flow, rate-limit exhaustion, remote-host transition, large-history performance or provider upgrade was exercised.

Those paths were traced in source where described. Existing upstream test files were read as executable specifications; they were not run here. No claim of passing the upstream suites is made.

Luna runtime behavior was not tested. This research makes no claim that every current uncommitted change is complete, built or running in LUNA Dev.

The two tested providers do not certify Pi, ACP, Cursor, Grok, Antigravity or every OpenCode version. A capabilities declaration is evidence of intended adapter behavior; negotiated runtime support and end-to-end tests still matter.

## Artifact verification

Run the recorded-artifact verifier from the Luna root:

~~~sh
node docs/pending-tasks/orchestrator-v2-and-bb-2026-10-04/evidence/verify-evidence.mjs
~~~

It checks Markdown targets, source permalink paths/line bounds against the local pinned commits, source fingerprints, recorded JSON outcomes and sentinel evidence. It does not rerun model requests and must not be reported as a new live E2E pass.

SHA256SUMS protects the report and evidence bundle against accidental changes. validation.json records the verifier's result and coverage limits.
