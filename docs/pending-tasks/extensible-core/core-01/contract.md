# LUNA core contract v1 (minimal)

This is the public domain and operation contract that IC-0, RM-00 and later
SDKs build on. The authoritative, generated forms live in
[`crates/rpc/contract/v1/`](../../../../crates/rpc/contract/v1/): `operations.json`
and the JSON Schemas (draft 2020-12) under `schemas/`. CI fails if they drift
from the code (`cargo test -p luna-rpc --features schema --test contract`).
Regenerate them with
`scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract`.

## Operations

Every RPC method has exactly one row in the operation catalog
(`crates/rpc/src/operations.rs`, published as `operations.json`):

| Field | Meaning |
| --- | --- |
| `method` | The wire name. Existing names never change. |
| `id`, `version` | Stable namespaced id `luna.core/<operation>` and contract version (1). |
| `kind` | `query` (reads), `command` (changes state or spends provider capacity), or `watch` (subscription). |
| `reply` | `unary`, `stream`, or `streamAfterReady` (an `ok: {"stream": true}` readiness frame, then a stream). |
| `target` | `local`, or `deviceAddressable`: honours `targetDeviceId` and is forwarded to that device's engine. |
| `authority` | `localOnly` (owner IPC only), `remoteBootstrap` (pairing and authentication handshake), or `remote` with a `scope` (`environment-read`, `tasks-read`, `files-read`, `terminal-operate`). |
| `forwardDeadlineSecs` | The reply deadline when a unary call is forwarded to another device. |

Authority rules:
- The engine refuses any method without a row (`unknown method: <m>`), so a
  handler can't be reached without a reviewed entry.
- A remote session is refused unless the row names a scope the session holds.
- A row grants nothing on its own. New rows default to `localOnly`, and a
  catalog file is never an authorization.

## Identifiers

- **`NamespacedId`**, the shape `namespace/name`:
  - Namespace: one or more `.`-separated segments matching `[a-z][a-z0-9-]*`.
  - Name: matches `[a-z0-9][a-z0-9._-]*`.
  - At most 128 bytes.
  - Typed forms: `ExtensionId`, `ContributionId`, `ContentTypeId`.
- **`ProviderRef`**: a legacy provider spelling (`codex`, `claude-code`,
  `opencode`, `mock`, and the retired `cursor`, `devin`, `grok`, `hermes`, `pi`,
  `antigravity`) or a namespaced id.
  - It is kept byte-for-byte. Legacy `x` maps canonically to `luna.provider/x`.
  - Existing payloads keep their spellings, so their hashes don't change.
  - CORE-04 moves providers onto it.
- **Unknown-value rule:** a reader keeps a well-formed id it does not recognise
  exactly as written, shows a readable fallback, and never fails, drops or
  rewrites it. An id never carries a version or package hash; those are
  separate execution identities.

## Commands, receipts and identity

- **The envelope (`ExecuteTaskCommand`):**
  - Fields: `commandId`, `taskId` and `intent` (`{kind, ...fields}`).
  - Optional fences: `expectedTaskRevision`, `expectedBindingGeneration`,
    `expectedTurnId`.
  - Defaults: `actorId` `local-user`, `clientId` `local-ui`,
    `clientSessionId` `legacy`, and `issuedAtMs` set to the receive time.
- **Identity:** a command is identified by `commandId` plus its `payloadHash`.
  - The hash is SHA-256 over the engine-normalized intent: keys sorted
    recursively, compact JSON, and optional fields the caller omitted present as
    `null`.
  - Key order never changes identity. An identical retry returns the original
    receipt.
  - A different payload under the same id is a `commandIdConflict` receipt.
- **Receipts (`CommandReceipt`):**
  - `status` is `pending`, `applied`, `rejected`, `conflict`, `superseded` or
    `failed`.
  - On refusal there is a typed `reason.code` plus a human `reason.message`
    and a `recoveryAction`.
  - `pending` with `providerAcceptanceUnknown` means accepted by the engine and
    awaiting the provider.
  - A timeout is an unknown outcome. Read it back with `GetTaskCommandReceipt`;
    never resend it under a new id.
- **Reads (`TaskCommandRead`):** bounded pages of task-command events. If
  `reloadRequired` is set, replace local state with `snapshot`.

## Frames, errors, cancellation and limits

- **Requests** are `{id, method, params}`. Cancelling a stream is
  `{id, cancel: true}`.
- **Replies:** exactly one of these appears:
  - `ok`, which is success. `ok: null` is a valid result, distinct from a
    missing `ok`.
  - `err`, a failure string.
  - `item`, one stream element.
  - `done: true`, the end of a stream.
- **Transport errors** are strings with stable prefixes: `unknown method: <m>`
  and `bad params: <reason>`. Domain refusals are receipts, not transport
  errors. A typed error envelope is CORE-05.
- **Limits:** forwarded unary calls use the catalog deadline (30 s by
  default). Streams are unbounded until cancelled or done. Per-operation size
  and concurrency limits are CORE-06/07 broker work and are not promised here.
