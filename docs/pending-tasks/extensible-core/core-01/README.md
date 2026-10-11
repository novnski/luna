# CORE-01 canonical domain and operation contracts

Source before the change: `40800c6c` (11 October 2026). Live status belongs to
`pending_tasks.py`. The published contract for consumers is
[contract.md](contract.md). The failure cases, written before code, are in
[failures.md](failures.md).

## What changed

- **One reviewed operation catalog.** `crates/rpc/src/operations.rs` has one
  row for each of the 251 RPC methods. Each row holds the stable id and version
  (`luna.core/<operation>@1`), kind, reply shape, target, authority and
  forwarded deadline. The wire method names are unchanged.

  Four separate hand-kept lists now read the catalog instead of keeping their
  own copies:
  - remote scopes (`remote_auth::method_policy`);
  - device forwarding (`forwardable`);
  - forwarded streams (`is_stream_method`);
  - forward deadlines (`forward_deadline`).

  The versioned readiness frame in `server.rs` comes from the catalog too: it
  was hard-coded for `WatchCheckoutChangeRequest` and is now the
  `streamAfterReady` reply shape.
- **Fail-closed dispatch.** `EngineRpc::handle` refuses any method that has no
  catalog row before any handler runs. A new row is local-only until it names a
  remote scope.
- **Public contract types** in `luna-proto`:
  - `contract.rs`: `NamespacedId`, `ExtensionId`, `ContributionId`,
    `ContentTypeId` and `ProviderRef`, with legacy provider spellings kept
    byte-for-byte and mapped to `luna.provider/<x>`.
  - `TaskCommandEnvelope<I>`: the engine's private `TaskCommandParams` is now
    this public envelope over its private intent, with the same serde defaults.
- **Generated, CI-checked artifacts.** An optional `schema` feature, using
  schemars `=1.2.2` (already in the lockfile, so nothing was downloaded),
  generates `crates/rpc/contract/v1/`. That holds `operations.json` plus JSON
  Schemas for frames, the command envelope, receipts, command reads and ids.
  The frame schema is written next to the type, because a derived schema
  cannot express "`ok` present but null".

  The `contract` test fails on drift and runs in CI (`ui-tests.yml`).
- **No behavior change.** No method was renamed and no policy was widened. Your
  primary LUNA Dev runs this build.

## Evidence (E03)

| Case | Result | Evidence |
| --- | --- | --- |
| C1 uncatalogued method refused before dispatch | pass | Live app and headless engine: `FutureDangerousMethod` gets `unknown method`, while catalogued reads still work ([wire](evidence/wire-after/frames.json)) |
| C2 new method not remotely callable by default | pass | `engine/tests/remote_auth.rs` (real authenticated remote sessions through the shipped dispatch): writes and unknown methods refused, `local only` |
| C3 routing decisions unchanged | pass | 251 methods compared with the untouched pre-change sources: remote scope, forwarding, forwarded stream and all 11 non-default deadlines, 0 differences ([c3](evidence/c3-equivalence.json)) |
| C4 catalog and `methods::ALL` agree, ids unique and well-formed | pass | `contract` test |
| C5 schemas match the real wire | pass | Python `jsonschema` 4.26, independent of Rust, validates every request, reply, envelope, receipt and read from both captures ([report](evidence/reader-report.json)) |
| C6 receipts and payload hashes unchanged | pass | Each "after" reply equals its "before" reply apart from clocks and per-profile ids. The only hash change is create-chat, whose intent contains the capture's temporary project path |
| C6b payload hash is a pure function of the normalized intent | pass | Python reimplements the hash and reproduces the engine's value for 5 commands in each capture |
| C7 key order never changes identity | pass | The retry with every key reordered returned the original receipt and hash |
| C8 id rules agree across implementations | pass | One hand-written 19-row table, checked by the Rust parsers (`contract` test) and by the published schemas (Python) |
| C9 success vs error decoding | pass | All 15 recorded frames classify correctly from the frame schema alone, including `ok`, `item` and `err` |
| C10 artifacts current | pass | `contract` drift test, also in CI |

**Two defects caught during the work, both fixed before delivery:**
- The drift test caught a clock default (`issuedAtMs`) embedded in the
  generated schema.
- The existing `forward_deadlines_are_tiered_and_bounded` test caught four
  15-minute forward deadlines that my extraction had read as 30 s, because it
  missed the expression `15 * 60`. The equivalence check was redone against the
  pre-change sources.

**Suites run:**
- `luna-rpc` and `luna-proto`: 69 passed, 1 ignored.
- Engine lib, `remote_auth`, `device_routing` and `task_commands`: 455 passed.
- An earlier sweep that also ran `message_queue` and `task_identity` passed
  apart from the deadline test, which is now fixed.
- `contract`: 3 passed.
- Clippy is clean on the changed code.

All commands and exit codes are in [commands.json](evidence/commands.json).
Hashes for the committed evidence, the contract files, the sources and the local
raw run are in [sha256.json](evidence/sha256.json).

## Repeat

```sh
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
node scripts/qa-contract-wire.mjs "$PWD/target/qa/core-01/<run>/before"   # on the old build
node scripts/qa-contract-wire.mjs "$PWD/target/qa/core-01/<run>/after"    # on the new build
agent-python -I scripts/qa-contract-reader.py crates/rpc/contract/v1 \
  crates/rpc/tests/fixtures/contract-ids.json <before>/frames.json <after>/frames.json <report.json>
```

The wire capture now uses a fixed project path, so create-chat hashes can be
compared exactly in later runs.

## Limitations and hand-off

- **Kind classification:** query, command or watch is reviewed metadata from a
  documented naming rule. It doesn't change runtime behaviour.
  `ImportLocalWorkspace` is a command that streams progress.
- **Scope of the schemas:** they cover the minimal contract (frames, envelope,
  receipts, command reads, ids). Per-operation request and result schemas, the
  typed error envelope and the SDKs are CORE-05.
- **What the contract does not change yet:**
  - `HarnessId` stays an enum; CORE-04 adopts `ProviderRef`.
  - Size and concurrency limits are CORE-06/07.
  - The legacy `WatchAutomations` entry in the old stream list was unreachable,
    because that method isn't forwardable, and is not carried over.
- **Who consumes it:** IC-0 and RM-00 consume [contract.md](contract.md).
