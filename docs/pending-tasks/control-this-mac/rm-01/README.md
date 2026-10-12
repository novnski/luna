# RM-01 headed direct gateway and scoped host authority

Live status belongs to `pending_tasks.py`. The failure cases were written
first: [failures.md](failures.md). Owner decisions (12 October 2026):
- **Order:** authority (A), then the gateway (B), then the RP campaign (C),
  each landed separately.
- **Issued capabilities** are short tokens, valid until expiry (60 s at most)
  even after a revoke. Every call over a socket is still checked against the
  live session.
- **Remote access** is off by default, persisted per profile, and enabled
  through a local-only engine operation and `luna remote enable`.

## Milestone A: authority store lifecycle

Source before the change: `f8d31c9b`.

**Catalog finding (A3, A4).** No remote-scoped operation carries nested
intents or caller actor fields. The remote commands are only pairing
bootstrap and the four `terminal-operate` operations. A per-intent allowlist
and principal binding would have no consumer in RM-01. They belong to RM-04,
which opens task commands, and the failure cases record the rule for it.

**Change:**
- **Store v2** (`crates/rpc/src/remote_auth.rs`): sessions gain an optional
  `lastUsedAtMs`, which RM-02's "last connected" needs and the gateway will
  record.
  - A v1 store migrates on open in one atomic, private publish; a failed
    publish leaves v1 intact.
  - The store version is now separate from the remote-auth wire protocol
    version, which stays 1. A version this build doesn't know is refused.
- **Corrupt-store recovery** (A2):
  - The engine keeps the reason the store failed to open and reports it on
    every pairing and session operation, instead of a bare "unavailable".
  - The new local-only `ResetRemoteAuthority` operation (`luna remote
    reset-authority`) moves an unreadable store aside, keeping its bytes, so
    the next start begins empty.
  - It refuses a healthy store. Nothing regenerates the store implicitly.
- **Catalog:** one new local-only row, `luna.core/reset-remote-authority`,
  with the contract regenerated.

## Milestone A evidence

| Check | Result |
| --- | --- |
| `remote_access::a_v1_authority_store_migrates_once_and_keeps_its_sessions` | Failed on `f8d31c9b` (the store stayed version 1); passes. The session paired under v1 authenticates after the migration, and the file stays `0600`. |
| `remote_access::a_corrupt_authority_store_stays_refused_until_an_explicit_reset` | Failed on `f8d31c9b` (no reason was reported); passes. Local work continues, there is no remote transport, the bytes are untouched until the reset, then quarantined intact, and the next start pairs. A healthy store is refused. ([baseline log](evidence/a-keepers-before.txt)) |
| Mutations A1 (no migration), A2b (no reason), A2c (engine resets a healthy store) | Keepers red ([mutations](evidence/mutations-a.json)) |
| Mutation A2 (the store's own healthy check) | Keepers green, as expected: the engine refuses first, so this is defence in depth |
| `luna-rpc` (with the device-room relay), contract, `luna-headless`, the app's remote and inspect CLI tests, the `luna-mobile` and `luna-remote` builds | 34, 3, 3 and 4 passed; both crates build |
| Clippy (`luna-rpc`, `luna-engine`), rustfmt, `git diff --check` | Clean on changed lines |
| LUNA Dev refresh | 92 s, PID 8105, IPC 59777, signature verified. `luna remote sessions` answered, and `luna remote reset-authority` refused the healthy store. The dev profile has no authority store yet, so no live migration happened. |
