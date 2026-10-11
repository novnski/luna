# Direct host contract after RM-00 (M0)

This is the reviewed contract that RM-01 to RM-07 build on. It rebases the
[Control this Mac plan](../README.md) and the
[T3 remote/mobile audit](../../t3-remote-mobile/README.md) onto current source,
keeps the approved direction, and maps endpoints to scopes, operations,
receipts and failures. Proposed items are marked as proposals; everything else
exists in source after RM-00.

## Rebased baseline

| Input | Pinned evidence | Finding |
| --- | --- | --- |
| LUNA | `146c66b5` at the start of RM-00, after CORE-01 | The audit's LUNA SHA `4f3a6b0c` never reached `main`: it was a commit rewritten before landing. `7b269800`, the parent of the landed research commit `7f3e2a7e`, holds all 27 cited files byte-for-byte. Since then only three cited files changed: `remote_auth.rs` (CORE-01: method policy now comes from the catalog, no scope changed), `apps/ios/README.md` and `control-this-mac/README.md` (iOS parity links). |
| T3 | `pingdotgg/t3code` `a8bdfdb5` | All 192 cited T3 files still verify in the local checkout. |
| Audit verifier | `t3-remote-mobile/verify-evidence.py` | It failed: the rewritten SHA left 27 sources unavailable, and two package documents were edited after hashing. The manifest is now re-pinned to `7b269800` with a dated note, and the two document hashes are refreshed. It passes with 0 errors ([log](evidence/t3-verify.json)). |
| Zeron | Plan cited `b42fc2b8`; local checkout `40091d46` | RM-00 adopts its UniFFI version (0.32.2) and its iOS core build layout. Zeron runs networking inside Rust; LUNA deliberately does not (see below). |
| Toolchain | rustc 1.97.1, Xcode 27.0 (27A266a), iPhone 16 Pro on iOS 27.0, macOS on Darwin 27.0 | iOS Rust target `aarch64-apple-ios` installed; the simulator target is not. |

## Direction kept

No architecture change was needed or made:
- **Direct only.** No relay, Tailscale, cloud tunnel, hosted identity or
  push service in V1.
- **One authoritative engine and profile.** It runs in the headed app on a
  local-only profile.
- **The host is Rust.** `luna-remote` runs in the engine's Tokio runtime from
  RM-01 on. In M0 it runs as a standalone probe.
- **Native transport on the phone.** Swift owns `URLSession`, TLS trust,
  Keychain, permissions and lifecycle. It feeds an owned Rust core through
  UniFFI. RM-00 proved this split on a physical phone; Zeron's in-Rust
  networking is not adopted.
- **T3 is design evidence only.** No React Native, and no T3 server.

Two M0 implementation choices are recorded for RM-01:
- **HTTP layer.** M0 serves with `hyper` directly, because `axum` isn't in
  the lockfile. RM-01 chooses the gateway framework.
- **No stored certificate.** The certificate is re-derived from the key at
  every start, so renewal keeps the pin and a key/certificate mismatch can't
  exist.

## Identity model

**Host identity** is the pair:
- `RemoteTarget`: device id, workspace scope, environment id, protocol
  version and required capabilities. It now lives in
  `luna_proto::remote_control` and `luna-rpc` re-exports it, so the host,
  pairing, authentication and the phone core compare one type.
- **The TLS key pin**: lowercase hex SHA-256 of the key's
  SubjectPublicKeyInfo (DER). Only ECDSA P-256 keys are accepted.

**A route is never identity.** One host may be reachable by IP, by mDNS name
or later by a public name. A connection is trusted only when the presented
key matches the pin and the descriptor read over it names the expected
target. RM-00 proved this: the mDNS route verified, and the same key with
another target was refused.

| Holder | Key | Rule |
| --- | --- | --- |
| Mac identity directory | One per profile; RM-01 places it beside `remote-auth.json` | Key `0600` inside a `0700` directory. Damaged, permissive, symlinked or partial identities are refused and never silently regenerated. Rotation is a deliberate new key that requires re-pairing (RM-07 acceptance). |
| Phone saved host (M1) | `deviceId` + `environmentId`, with the pin | A changed pin is a different host until it is re-paired. |
| Phone session (M1) | Keychain, scoped by target and app profile, device-only, not synchronized | It's never a URL, query string or preference. |
| Phone cache (M2) | `deviceId` + `environmentId` + pin | A different key or profile never shows another host's cache (RP-09). |

**Host ticket** (`HostTicket`, the trust half of the future QR):
- Fields: `version` 1, `target`, `endpoints` (1–8 bare `https://host[:port]`
  origins) and `spkiSha256`.
- It carries no credential. M1 adds the one-time pairing secret and expiry.
- The phone core refuses a malformed, oversized (over 4 KiB), wrong-version
  or strangely shaped ticket, and any endpoint that isn't a bare HTTPS origin,
  before any network activity.

## Endpoint map

| Endpoint | Authentication | Authority | Operations (catalog v1) | Success | Failures |
| --- | --- | --- | --- | --- | --- |
| `GET /remote/v1/health` (exists) | None, over TLS | Public | None | `{protocol}` | TLS failure; 404, 405, 413 |
| `GET /remote/v1/descriptor` (exists) | None, over pinned TLS | Public | None. It exposes `catalogVersion` | `RemoteDescriptor`: protocol, target, catalog version, pin | Same. The phone refuses a wrong target, pin, protocol or catalog |
| `POST /remote/v1/pair` (proposal, RM-01/02) | One-time credential in the body, over pinned TLS | `remoteBootstrap` | `RedeemRemotePairing` | `IssuedSession` | Denied: target mismatch, expired, used, revoked, protocol or capability mismatch; uncertain durability (revoke and re-pair) |
| `POST /remote/v1/session/check` (proposal) | Device bearer header | `remoteBootstrap` | `AuthenticateRemote` | `SessionView` | Unknown, revoked or expired session |
| `POST /remote/v1/socket-ticket` (proposal) | Device bearer header | Session-bound | None (new) | Single-use, short-lived ticket | Same as session check |
| `GET /remote/v1/ws` (proposal) | Socket ticket, then a fresh `RemoteRpcService` per socket | Per call: the catalog row's `remote(scope)` | Every catalog row with remote authority | CORE-01 frames | `unknown method: <m>` (no row), remote denial for local-only or out-of-scope, `bad params` |
| `GET /remote/v1/snapshot/...` (proposal, RM-03) | Device session | `tasks-read` or `environment-read` | New bounded projections; broad local watches are not exposed as they are | Snapshot plus cursor | Repair or resnapshot on a gap or expired window |
| `POST /remote/v1/commands` (proposal, RM-04/05) | Device session | `tasks-operate` / `interactions-respond` (new scopes) | `ExecuteTaskCommand` with a nested-intent allowlist, never the method wholesale | `CommandReceipt` | Receipt statuses and reason codes below; transport errors |
| `POST /remote/v1/uploads/...` (proposal, RM-06) | Task-bound upload grant | `tasks-operate` | Upload staging | Staged attachment | Size, hash, ownership, cancellation |

**Current remote authority** (catalog v1, 251 operations, from
`crates/rpc/contract/v1/operations.json`):

| Authority | Operations |
| --- | --- |
| `environment-read` | 6: harness, model, composer and command catalogs; engine info and readiness |
| `tasks-read` | 10: doc messages, queue, task work, identity, runtime contract, command receipt, snapshot and stream |
| `files-read` | 15: repos, branches, refs, history, workspace files and images, checkout diffs, terminal list |
| `terminal-operate` | 5. Excluded from the default phone grant |
| `remoteBootstrap` | 2: `RedeemRemotePairing`, `AuthenticateRemote` |
| `localOnly` | 213, including every task mutation |

**Proposed nested-intent split for RM-04/05.** RM-04 owns the final
allowlist; every intent not listed stays local-only until reviewed.
- `tasks-operate`:
  - `createChat`;
  - `session.run`, `session.steer` and `session.interrupt`;
  - `queueMessage`, `editQueuedMessage`, `removeQueuedMessage`,
    `sendQueuedNow` and `steerQueuedNow`;
  - `rename`, `setPinned`, `setRead`, `setArchived` and `markSeen`.
- `interactions-respond`: `session.respondInput`,
  `session.respondAsyncQuestion` and `session.respondApproval`. Each must match
  the outstanding request id and revision.
- **Not remote without a separate decision**:
  - `delete`;
  - `switchProvider`, `setPermission`, `setProviderAccount`, `setHost`,
    `setCwd` and `setBranch`;
  - `repairWorkspaceIdentity`, the rewind, restore and edit-earlier family,
    `forkTask` and `moveProject`;
  - the child-chat intents and `applyContextTransfer`;
  - `compactContext`, `clearContext`, `goal`, `setConfig` and `setModel`.
- **Never remote**: `StopEngine`, backups, accounts and credentials, updater
  control, local-owner root claim and remote session administration.

**Receipts and recovery** (CORE-01 contract):
- `CommandReceipt.status` is `pending`, `applied`, `rejected`, `conflict`,
  `superseded` or `failed`, with a typed `reason.code`.
- The reason codes are:
  - identity: `commandIdConflict`;
  - staleness: `staleTaskRevision`, `staleBindingGeneration`, `staleTurn`;
  - queue: `queueRevisionConflict`, `queueClaimed`, `queueMissing`;
  - activity: `activeTurnConflict`, `foreignActive`;
  - compatibility: `invalidDestination`, `incompatibleWorkspace`,
    `incompatibleEnvironment`;
  - outcome: `providerAcceptanceUnknown`, `transactionFailed`,
    `unsupportedOperation`, `operationRejected`, `operationFailed`.
- A phone that times out reads the receipt back with `GetTaskCommandReceipt`
  (`tasks-read`). It never resends under a new id.
- Remote admission must bind the session principal, scope, device, command
  id, target and payload before execution. It produces the same receipt as a
  local action.

## Wire and version policy

- **Three independent versions:**
  - the direct protocol: `descriptor.protocol`, currently
    `remote-direct-v1`;
  - remote authority: `target.protocolVersion`, `remote-auth-v1` version 1;
  - the operation catalog: `catalogVersion` (CORE-01), plus per-operation
    versions.
- **A client accepts only versions it knows.** The M0 core supports protocol
  `remote-direct-v1` and catalog `{1}`. Anything else is
  `unsupportedVersion`; it never guesses.
- **Strictness splits along trust.**
  - Strict (`deny_unknown_fields`): `RemoteTarget` and `HostTicket`, because
    they are identity and trust inputs.
  - Lenient: `RemoteDescriptor`, whose additive fields readers ignore.
  - Unknown namespaced ids are kept verbatim, per the CORE-01 rule.
- **Capabilities are advertised only when the admission path exists.**
  `remote-direct-v1` is not added to the engine's current capability list
  until RM-01 serves it from the engine. `remote-task-control-v1` and
  `remote-interactions-v1` follow RM-04/05.
- **No secret in a URL.** Pairing credentials go in request bodies or a
  locally consumed QR. Socket tickets are single-use and redacted from logs.

## Hand-off to RM-01

- **What RM-01 starts from:**
  - `luna-remote` with `HostIdentity` and the bind rules;
  - `luna_proto::remote_control` for the wire types;
  - `luna-mobile` with the UniFFI build script;
  - the probe app as the native transport reference.
- **What RM-01 must decide:** the gateway framework, the per-profile identity
  location, and the engine-owned listener lifecycle (enable gate,
  cancellation and denial before teardown).
- **What M0 did not cover** is listed at the end of the
  [failure cases](failures.md).
