# Project-command storage prerequisite — 26 September 2026

The self-contained prerequisite adds schema13 to the existing `docs.sqlite3`:
separate device-owned project-command configuration and attempt-fenced run rows.
The existing project/task retirement paths remove their owned records. Checkout
setup receipts survive task deletion as replay guards until project deletion.
Backup schema support and existing version assertions advance to13. No new
standalone database or runnable hook discovery is introduced.

Unknown future schema versions fail before WAL PRAGMAs or migration writes.
The file-backed future-schema999 test verifies an explicit error and byte-for-byte
preservation of the database. This does not retrofit old released binaries that
lack the guard.

Verified on the commands worktree, based on audited runtime8169b692:

- `CARGO_INCREMENTAL=0 scripts/cargo.sh check --locked -p luna-proto -p luna-sync -p luna-engine`: passed,16.97s reported compile time.
- `CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-sync --test project_command_store -- --nocapture --test-threads=1`:2/2 passed,23.95s reported build time,0.01s execution.
- Tests cover schema13, device ownership, duplicate admission, stale attempt
  rejection, consistent VACUUM snapshot inclusion, cleanup and newer-version rejection.
- Direct rustfmt and `git diff --check` passed.

Evidence: `/Users/iulian/.codex/worktrees/0ace/luna/target/qa/commands-recheck/check.log`
and `store.log`. The earlier check failure is retained in
`target/qa/commands-initial/check.log`; its two feature-level compiler errors were
corrected before the successful check. Existing repository warnings remain.

The prerequisite does not claim full project-command execution, backup archive
restore or native UI acceptance. Those tests belong to the subsequent feature
batch. No UI/native builds ran in this bounded slot.
