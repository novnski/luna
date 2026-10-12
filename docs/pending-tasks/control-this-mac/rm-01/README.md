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

## Milestone B: the engine-owned direct gateway

Source before the change: `ce43e491`.

**Change:**
- **`luna-remote` gateway** (`crates/remote/src/gateway.rs`). It reuses the RM-00
  bind rules (one explicit interface, no wildcard or loopback, a port that is
  also free on loopback) and the profile's pinned identity. The identity must
  name the authority store's host and profile.
- **Routes:**
  - descriptor and health;
  - `POST /remote/v1/pair` (redeems a one-time credential);
  - `POST /remote/v1/session/check` and `POST /remote/v1/socket-ticket`
    (bearer token, target in the body);
  - `GET /remote/v1/ws` (`Authorization: Ticket …`).

  No secret travels in a URL.
- **WebSocket:** upgraded on the same TLS connection. Each socket runs the
  existing per-connection remote guard, bound to the ticket's session, over the
  engine's restricted remote service. Nothing reaches local-owner IPC.
- **Bounds:**
  - 32 connections;
  - 16 KiB headers and bodies (a declared oversize body is refused before it
    is read);
  - 1 MiB WebSocket messages;
  - 32 requests or streams in flight per socket.

  A browser `Origin` is refused.
- **Lifecycle:**
  - `stop` (disable and engine shutdown) refuses every route and every socket
    call first, then closes every socket and the listener.
  - Revoking a session refuses its calls at once (the guard re-checks the
    session per call), and a watcher closes its sockets.
  - Dropping the gateway also cancels it.
- **Store:** single-use socket tickets, held in memory as hashes, valid for
  30 s. Redeeming one doesn't check revocation (owner decision); the socket
  then refuses every call of a revoked session. The store also gains session
  lookups and a throttled last-use record.
- **Engine `RemoteAccess`** (`crates/engine/src/remote_access.rs):
  - The config lives in `remote-access.json` (private, atomic) and the
    identity in `remote-identity/`, both beside `remote-auth.json`.
  - Enable, disable and status go through local-only `GetRemoteAccess` and
    `SetRemoteAccess`, and `luna remote enable --bind`, `disable` and
    `status`.
  - Enabled access resumes with the engine's maintenance.
  - Engine shutdown stops the gateway first, before command admission closes.
- **Catalog:** two local-only rows, with the contract regenerated.
- **CI:** `remote_access` joins the Linux engine keepers.

**Proof environment.** The macOS application firewall here is on in stealth
mode. It silently drops inbound connections to any binary it hasn't approved,
even from this machine. The known-good RM-00 probe host times out in the same
way, both unsigned and signed with the Apple Development identity. So:
- **The keepers** probe the gateway once enabled. A timeout reports SKIPPED
  with this reason; a refused connection (nothing listening) still fails.
- **Locally,** the keepers ran over loopback through a temporary, uncommitted
  patch that allowed loopback binding. It was reverted, and its absence was
  checked before the commit.
- **Linux CI** runs them on the real LAN route.

## Milestone B evidence

| Check | Result |
| --- | --- |
| Authority keepers (A) | Pass locally |
| Gateway keepers (`gateway::…`, 3 tests) on Linux CI, branch run 38250744982 (`session-sync-regressions`) | Pass. The `remote_access` binary finished in 0.15 s, well under the 2 s skip probe, so the gateway was exercised on the LAN route |
| Gateway keepers locally, on this Mac | Reported SKIPPED (firewall). Passed over loopback with the temporary debug patch ([log](evidence/b-keepers-loopback.txt)) |
| Mutations B3 (Origin allowed), B5 (stop and Drop don't cancel), B6 (revoked socket stays), B7 (reusable ticket), RP19 (no resume) | All caught over loopback ([mutations](evidence/mutations-b.json)). B5 was first run without its `Drop` half and stayed green: `stop` consumes the gateway, and `Drop` also cancels. That's defence in depth, so the recorded mutation removes both. |
| `luna-engine`, full (local) | 971 passed, 1 failed, 12 ignored. The failure, `m5_repos_diffs_terminals::git_history_search_is_fuzzy_complete_and_accepts_sha_prefixes`, is an unrelated flake: a random commit SHA fuzzy-matched another commit's 8-character prefix. It passed 3 of 3 alone and is flagged as a separate task. |
| `luna-rpc` / contract / `luna-headless` / `luna` app | 34 (1 ignored) / 3 / 3 / 31 (1 ignored) passed |
| Clippy (`luna-remote`, `luna-rpc`, `luna-engine`, `luna`), rustfmt, `git diff --check` | Clean on changed lines |
| LUNA Dev refresh | 121 s, PID 84796, IPC 59777, signature verified. `luna remote status`: disabled, not listening (the default) |

**Pending (milestone C), landed untested at the owner's request:**
- **The rest of the branch CI run** (UI, Linux browser, macOS) was stopped
  before it finished.
- **The RP campaign extensions:**
  - RP-02: no receipt after a denied remote command;
  - RP-03: expired and wrong-target pairing;
  - RP-06: a ticket minted before a revoke;
  - RP-19: extend `backup_restore`'s exclusion keeper with `remote-access.json`
    and `remote-identity/`.
- **The native readback on LUNA Dev.** It opens a LAN port, and macOS will ask
  to allow incoming connections.

