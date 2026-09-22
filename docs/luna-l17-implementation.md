# L17 backup/export/restore workstream

Base: `b33548978ca942e4dba06410ef101b7dff0b2bbc` on `codex/l17-backup-restore` in this task's isolated worktree.

## Implemented core

- `crates/engine/src/backup.rs` adds a deliberately isolated backup service. Both the source root and archive destination need exact synthetic/copy-isolated markers. No normal LUNA root is eligible through this entry point.
- Manual export and backup produce an encrypted versioned archive. The encrypted manifest records the app version, source environment ID, creation time, size and SHA-256 for each entry, authoritative database counts, exclusions, and optional previous-backup digest. AES-256-GCM authenticates bounded frames and the final frame. Archive publication follows complete decrypt/hash/reopen verification and an atomic no-clobber rename.
- The allowlist captures LUNA's local/account-scoped `docs.sqlite3` databases through SQLite `VACUUM INTO`, journals, completed profile uploads, and selected non-secret settings. It excludes credentials, provider-native stores, active grants, worktrees, arbitrary workspace files, pending uploads, preview/cache data, and device identity from the archive. SQLite activity/search envelope rows are removed from the snapshot as derived data.
- A daily-after-change policy entry point and seven-daily/four-older-weekly retention selection are available. They do not start a scheduler or delete old backups themselves.
- Restore preview decrypts and verifies the complete archive and reports counts, categories, exclusions, bytes, conflicts, and unsupported conditions. Staging extracts into a private sibling directory, checks all digests, runs supported SQLite migrations, checks integrity/counts and attachment references, and fingerprints both stage and target. Activation on an isolated same-location root uses an interruption journal, retains the old root as rollback, and verifies the activated root. Rollback and interrupted-swap recovery preserve the failed root for inspection.

## Required parent integration and current limits

- Recovery key authority is injected only. There is **no** Keychain read/write, passphrase provisioning, owner-held recovery flow, provider-store access, or credential copy. Production must supply a separately reviewed key lifecycle before connecting this service to UI/RPC.
- Backups currently require an offline isolated root. The live engine cannot call this service as-is because the service takes the engine instance lock and captures journals/files while quiesced. A live scheduler, consistent write barrier or online snapshot coordination, persisted backup receipts, destination controls, and UI/RPC are still required. The retention function returns candidates; it does not purge.
- Activation supports only the original absolute data-root location. Existing absolute attachment references, provider-native identities, workspace paths, optional appearance assets, and external dependencies are not migrated. Provider credentials and remote grants remain excluded. There is no live-profile restore path.
- Staging removes derived activity/search rows; if any existed, preview marks index rebuild unsupported and activation refuses until an engine-backed staged rebuild is integrated and verified. Usage index rebuild and post-start engine readback are likewise not implemented here. This branch therefore is **not** an accepted complete L17 feature.
- The manifest does not invent an RPC protocol version because the repository has no single exposed protocol-version constant. The parent should add the authoritative compatibility value when wiring the service.
- Preview reports archive bytes as required storage, but no filesystem free-space preflight exists. Interrupted archive snapshot temporary directories can remain after a process crash and need a bounded, owner-verified cleanup policy. This workstream does not implement L18 cleanup.

## Verification

- `cargo check --locked -p zeron-engine`: passed (`/tmp/luna-l17-check2.log`).
- `cargo build --locked -p zeron-engine`: passed after final code changes (`/tmp/luna-l17-build-final.log`).
- `cargo test --locked -p zeron-engine --test backup_restore`: three synthetic-root integration tests passed (`/tmp/luna-l17-test-final.log`). They cover encrypted/verified export, wrong key, corruption/truncation, excluded fake secrets, recovery of a deleted attachment, staged activation/change conflict, rollback, and refusal of forbidden target material.
- `cargo test --locked -p zeron-engine --lib backup::tests::`: three unit tests passed (`/tmp/luna-l17-unit-final.log`). They cover both interrupted-swap recovery points and retention selection.
- `rustfmt --edition 2024 --check crates/engine/src/backup.rs crates/engine/tests/backup_restore.rs`: passed (`/tmp/luna-l17-fmt-final.log`).
- `cargo clippy --locked -p zeron-engine --test backup_restore`: passed with warnings in existing engine/dependency files, none in the new backup module (`/tmp/luna-l17-clippy-final.log`).
- `cargo fmt --all -- --check` fails on pre-existing unrelated formatting differences in UI files; no unrelated files were changed.

## Shared file and protected-path readback

- Shared edit: `crates/engine/src/lib.rs` adds exactly `pub mod backup;`. No proto/RPC/UI entrypoint or existing feature module was edited.
- All runtime tests used freshly created temporary synthetic roots and `[17; 32]`/`[18; 32]` fake keys. No normal LUNA app was launched, no native QA run, no provider login or Keychain used, and no actual profile, credential, provider store, or workspace was read or mutated.
- No push, remote pull, merge, main-branch edit, or L18 cleanup was performed.
