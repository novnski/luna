# History, persistence and platform review

Baseline: `97343ad1` (last commit before 2026-10-03) through `d3819203`.
This review owns history/fork/rewind persistence, related document and RPC
contracts, CLI credential boundaries, backup, provider updates and URL scheme
registration. Provider adapters, input/turn loops, child/workspace/automation
logic and UI presentation belong to other reviewers.

## Failure cases recorded before implementation

1. A provider turn split around a system notice has several complete assistant
   entries but a checkpoint only on its final entry. Rewind must validate one
   native turn and keep/remove every segment together. Existing Claude,
   OpenCode and Codex rewind preflights reject the earlier segment as missing a
   checkpoint.
2. Claude file rewind selects the first assistant segment after the selected
   user row. A split turn therefore reports no checkpoint despite the final
   segment carrying the exact native prompt UUID. Preview and apply must use
   the same whole-turn boundary as conversation history.
3. Ignoring all entries without comparing native history would conceal an
   actually missing turn boundary. Validation must fail before native mutation
   when any native turn's checkpoint is absent or the transcript ends with an
   uncheckpointed assistant segment. Exact native ordered-history comparison
   must remain mandatory: a User row may itself be a native in-turn steer.
4. A missing earlier-turn checkpoint must not be replaced with a later turn's
   checkpoint. Failed preflight must preserve native transcript bytes and the
   complete LUNA projection and leave no pending rewind intent.
5. `DocsStore` now applies migration 18, while backup accepts only schema 17.
   Every current-profile export therefore fails as a future schema. Export,
   staged verification and activation must support 18 without loosening the
   refusal of unknown future schemas.
6. A schema-18 marker with any of `automations`, `automation_mutations`, or
   `automation_runs` missing must fail export. A restored deleted automation
   must remain deleted, and occurrence claims and mutation receipts must remain
   byte-identical so restore does not itself drop the no-replay evidence.
7. A steered User row can appear between an early assistant segment and the
   final checkpoint of the same native turn. Rewinding a later complete turn
   must retain that complete earlier turn without interpreting the steer as
   a missing root-turn checkpoint. The fixture for this case precedes the
   helper adjustment; selecting the steer itself remains ineligible.

## Verification plan

Extend the existing `luna-engine --test claude_history` engine/RPC/subprocess
scenario before implementation. Seed a chronological split in turn two, then
exercise native file preview/apply, same-chat rewind, uncertain readback and
reconciliation against the deterministic CLI fixture. Remove an earlier whole
turn's checkpoint to prove refusal leaves native bytes unchanged, then restore
the checkpoint before the successful scenario. Preserve the successful test
log as the repeatable artifact. Parent owns compilation and all test/app runs;
none are run by this reviewer.

Before the schema fix, extend `luna-engine --test backup_restore` with a full
encrypted export/stage/activation test for all three automation tables and
extend missing-table rejection coverage. Verify a deleted definition, mutation
receipt and blocked occurrence survive and that later local changes are
replaced by the exact saved values. Save a JSON readback when
`LUNA_HISTORY_PLATFORM_EVIDENCE_DIR` is provided.

## Findings and fixes

- **P1 — current profiles cannot be backed up.** Migration 18 was added in
  `824a0603`, but `backup.rs::CURRENT_SCHEMA` remained 17. Both export's
  `database_counts` and archive validation rejected schema 18. The supported
  version is now 18, and all three automation tables are required. SQLite
  backup already copies their rows in the same database snapshot; only the
  derived activity index is discarded. Existing migration-17 configuration
  validation remains in place. The integration scenario verifies encrypted
  export, staging, activation, a deleted definition, a mutation receipt and an
  Unknown occurrence whose `blocks_next` flag survives. This tests storage
  transport, with synthetic opaque record bytes; scheduler restart behavior is
  covered by the workspace review.
- **P2 — split turns cannot be rewound.** Chronological segmentation from
  `2bb76ee8`/`b81ef704` leaves a checkpoint on the final assistant segment.
  Both rewind preflights required one checkpoint per raw assistant row.
  `segment_checkpoints_for_native_comparison` now collects terminal segment
  checkpoints, rejects unfinished/uncheckpointed trailing segments and relies
  explicitly on each caller's unchanged exact native-history equality check.
  User rows within a turn may be steering and do not invent new boundaries.
  The existing engine/RPC/CLI integration case now has a split turn, missing
  earlier and tip checkpoint refusals, and an in-turn steer retained by a
  later rewind. The steered projection is seeded directly; this does not
  replace the separate live steering QA.
- **P2 — Claude file checkpoint preview fails on split turns.** It previously
  selected the first assistant entry. It now reuses `plan_history`'s exact
  before-user boundary, selecting the final segment before the next user row.
  Existing preview/apply/stale-digest checks now run against that split turn.

## Static coverage and boundaries

| Area | Evidence inspected | Result |
| --- | --- | --- |
| Fork/edit/history | `history_operations.rs`, history hunks of `rpc.rs`, `rpc/claude_history.rs`, raw entry and checkpoint APIs in `doc/schema.rs` | Fixed the segmentation issues above. Selecting a user row inside a steered turn remains ineligible; no provider boundary is inferred from its position. |
| Chronological projection/readback | `SegmentWriter`, segment-close and checkpoint-write paths in `sessions.rs`, `transcript_delta.rs`, checkpoint stream fields | Verified final-segment checkpoint ownership and additive checkpoint-ID readback contract. Input/turn-loop edits remain with their reviewer. |
| Recovery configuration | `store/workspace_repair.rs::record_execution_configuration`, migration 17, backup schema validation | Original configuration is frozen with `COALESCE`; backup retains the column. No additional defect found in these storage changes. Recovery dispatch is reviewed separately. |
| Canonical authority hashes | `doc_host.rs::executable_row_hash`, `9b3e9a2e` | Recursive JSON-key sorting then typed reconstruction preserves struct-field ordering and sorted map semantics across `preserve_order` builds. No additional defect identified. |
| RPC lifecycle | `rpc/client.rs`, `server.rs`, changed device-room supervisor branches in `bd603de1` | Connection close drains pending state, stream receiver drop cancels idle server work, aborted server futures own request cleanup, and broadcast lag retains supervisors. No additional defect found in inspected changes. |
| CLI credential boundary | `credentials.rs`, `agent_accounts.rs`, `7f86fc1f` | Native auth remains provider-owned; no Keychain fallback. New LUNA local keys enforce owner/permission/type/link checks, atomic no-replace publication and fail-closed unsupported platforms. No live credential files were accessed. |
| Provider updater | `provider_updates.rs`, `13c0fee9` | OpenCode uses `@opencode/cli`, Cellar formula/receipt provenance and same-major availability checks. No update/network probe executed; no further confirmed regression. |
| URL schemes | `ui/url_scheme.rs`, `ae7926a4` | macOS registration checks the running bundle's declared scheme case-insensitively, including malformed/unbundled fail-closed behavior. Native QA remains parent-owned. |
| Persistent placement and startup resume | Shared metadata/migration changes only | Full workspace/child/startup behavior belongs to the workspace reviewer, rather than duplicate review of those execution paths. |

Automation restore preserves all three authoritative tables and task receipts.
The workspace reviewer confirmed existing restart reconciliation marks prior
active claims Unknown through the boot identity; active/Unknown claims block
later occurrences. Pending future definitions may execute an eligible latest
slot under existing scheduler semantics. This patch makes no scheduler change.

## Central QA handoff

No Cargo build/test, GUI launch, live provider request, worktree, branch,
commit or push was performed by this reviewer. `git diff --check` passes;
the two edited integration test files were formatted with `rustfmt`.

Parent should preserve logs and run:

```sh
scripts/cargo.sh test --locked -p luna-engine --test claude_history
LUNA_HISTORY_PLATFORM_EVIDENCE_DIR=/tmp/luna-history-platform-evidence scripts/cargo.sh test --locked -p luna-engine --test backup_restore
scripts/cargo.sh test --locked -p luna-engine --lib history_operations
```

Required native QA: run a Claude or OpenCode turn that gets a child notice
between reply segments, then rewind a later whole user turn; verify exact
native retained turns, all earlier visible segments, no removed-turn return
after restart, and an unchanged source for a fork. For Claude additionally
preview/apply a file checkpoint on the split turn and verify a stale preview
digest refuses. For macOS scheme coverage, compare the default handler before
and after an isolated QA bundle that omits `CFBundleURLTypes` (parent owns the
single-GUI slot). Parent records actual runtime results separately.

### Central-test fixture correction

The parent's first backup run reported 26 passes and a failure in
`managed_offline_v9_archive_remains_restorable_after_current_migrations`.
That fixture manufactures a schema-9 database from the current schema by
dropping newer tables and deleting migration markers. Its downgrade list
omitted the three migration-18 tables, so a legitimate restore migration
failed because they already existed. Added those tables to the fixture's
drop list and updated its final current-schema assertion from 17 to 18.
Production migrations remain unchanged. Parent owns the focused rerun.

### Native side-fork regression exposed by central QA

The split-turn scenario also reached a per-assistant checkpoint assumption in
`rpc.rs::fork_hidden_turns` while creating a native side fork. Failure cases
recorded before its correction: valid notice segments must collapse to their
final native checkpoint; a missing complete earlier or tip checkpoint must
refuse the fork; inherited hidden turns must remain in exact native order;
and Before/Through boundaries must compare only their exact native prefix.
The existing missing-checkpoint integration loop now also asserts that native
side-fork creation fails while preserving source transcript bytes and rows.
The unchanged later CreateSideChat scenario must succeed on the split turn.

The correction collects checkpointed segments in order, refuses an unfinished
tip, and compares the entire requested retained prefix with native history
before fork creation. Claude/OpenCode use their existing native-history read;
Codex reuses `prepare_rewind`, which only reads `thread/turns/list` and computes
a prefix and never calls `thread/revert`. Provider boundaries still select
the exact Tip/Before/Through native prefix. Publication requires every retained
checkpoint remapping. Claude/Codex's exhaustive turn maps must have exactly
the expected count; OpenCode's map includes non-turn messages, so its newly
forked native turn list is read back and compared with the mapped prefix.
Codex's empty-history case has no readback anchor and remains guarded by the
required empty exhaustive result map. No `side_chats.rs` change was needed.

### OpenCode compaction compatibility correction

Failure case identified before correcting the new side-fork guard: an
OpenCode session containing a completed compaction still retains its native
history and supports forks, while `native_history_turns` reports
`compacted: true`. The newly added blanket check would reject both source
preflight and successful fork readback. Existing
`opencode_fork_plan` copies completed compaction items, and
`fork_readback_ignores_regenerated_ids_but_preserves_semantic_nested_fields`
already exercises their checkpoint mapping. Remove the compacted refusal for
OpenCode only; retain exact prefix and mapped-turn equality and Claude's
existing compacted-history restriction. Parent verification should include
the shared engine history fixture described below, existing harness coverage
and the native S5/S7 compaction/fork flow.

### Synthetic native-history fixture correction

Broader central integration reached
`side_chat_native_fork_preserves_binding_and_never_replays_unknown_creation`.
Its `HistoryHarness` identifies as OpenCode and implements fork but omitted
native-history readback, so the trait's unsupported default now stops before
the intended acknowledged/unknown fork. Failure cases before fixture changes:
source readback must expose `provider-message-1`; a minted fork must expose
its exact `fork-provider-message-N`; unknown synthetic sessions must not be
invented; successful compacted OpenCode history must pass the engine's source
and fork checks; both normal success and acknowledged failure must still call
native fork exactly once across repeated creation requests.

The fixture now implements native-history readback from its known synthetic
source and minted fork sequence. Added a `CompactedHistory` success branch to
the existing engine/RPC/SQLite side-fork scenario before implementing that
readback, alongside unchanged normal-success and fail-after-acknowledgement
branches. No production guard was changed for this fixture correction.
Parent should rerun the isolated `task_commands` test named above.

## Native audit runner portability

Failure cases recorded before changing the three existing runners
(`qa-context-transfer-live.mjs`, `qa-automations-live.mjs`, and
`qa-execution-summary-live.mjs`):

1. Partial or empty identity overrides must stop before connection or QA work;
   app name, bundle ID, IPC port and data directory travel together.
2. Primary identities, port 59777, malformed ports, a data directory outside
   this checkout's target, symlinked targets, or bundle-staging data must be
   refused. The override must still match signed plist and owning process.
3. An override whose signed plist lacks `LUNA_QA_MCP_ISOLATION=1` must refuse.
4. Evidence relocation must not silently change the default app's expected
   data directory. Each runner needs its own evidence subdirectory under the
   common audit base so restart/result files cannot collide.
5. An explicit unavailable model must fail catalog validation without falling
   back to another model. Claude's old 4.x preference must not prevent choosing
   current Sonnet 5.5 from the live catalog.
6. Default execution must keep each runner's existing app/bundle/port/data
   identity and its signature, plist, listener-owner and PID-executable checks.

All three runners were read in full. They only create local fixture Git
projects/chats, use native provider turns with bounded QA prompts, exercise
context/permission transitions or one-shot local automations, and save
readbacks. No external-message send or PR-creation path is present. Parent
owns serial native runs on the existing review app; this edit performs no app
launch, process control, provider call, or Cargo build.

Portability implementation is confined to those three drivers. Supply all of
`LUNA_DEV_APP_NAME`, `LUNA_DEV_BUNDLE_ID`, `LUNA_DEV_IPC_PORT`, and
`LUNA_DEV_DATA_DIR`, or leave all unset for the original default identity.
Overrides require a canonical separate target data path and a canonical QA
bundle; all existing signature/plist/port/PID checks remain, with signed
display-name and MCP-isolation checks added. The common
`LUNA_AUDIT_NATIVE_BASE` gets separate `context-transfer/`, `automations/`, and
`execution-summary/` evidence subdirectories; it never changes default data
identity. Existing command-line run/restart/readback modes are unchanged.

Optional `LUNA_QA_CODEX_MODEL` and `LUNA_QA_CLAUDE_MODEL` select exact IDs from
each runner's live `ListModels` result; unknown or empty explicit values fail
instead of falling back. Claude's preference list now tries
`claude-sonnet-5-5` before the existing 4.x fallbacks. Model choice itself is
still catalog-backed; the edit did not query providers. All three
`node --check` commands and `git diff --check` pass. Native invocation and
identity negative-case execution remain parent-owned.

### Automation transcript observation ordering

Failure case recorded before driver correction: a completed automation ledger
entry may be observed before the independent transcript read exposes its
completed assistant row. The preserved run
`native-expanded/automations/run-f9FbuN/failure-readback-1791421845771.json`
contains exactly the scheduled user row and a completed ledger occurrence;
`automation-codex-failure-fresh.json` later contains that same user row, an
assistant row marked complete with the exact marker (plus trailing newlines),
and an idle session. These observations confirm a transient cross-surface
readback ordering issue in this QA assertion, not a proven production root
cause or a product fix.

The runner must wait boundedly for that exact scheduled user ID and its later
completed assistant marker, retaining all exact user/occurrence counts and
ledger expectations. This wait may only read the transcript; it must never
resend the prompt or create another occurrence. The failed run and evidence
remain untouched; a fresh parent-owned run will exercise both providers.
