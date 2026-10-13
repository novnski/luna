# CORE-08: an independently installed provider, end to end

Baseline: `6a0ad25c` (13 October 2026). Live status belongs to
`pending_tasks.py`. This file is the contract, the failure cases (written
before the code), the criteria mapping and the evidence plan.

## Rebased against current main

What already existed and is reused:
- **CORE-04:** `HarnessId` is an opaque namespaced value, and the
  `HarnessRegistry` has collision-safe `try_register`/`unregister`, a
  generation-tagged catalog and the `WatchProviderCatalog` subscription the
  existing picker already follows. Model discovery (`GetProviderModels`)
  checks host, registry, generation and executable provenance.
- **CORE-05:** `@luna/sdk` (`sdk/typescript`) is the public TypeScript SDK.
- **CORE-06:** packages, `provider` contributions, the
  `luna.capability/provider.register` capability and per-profile grants.
- **CORE-07:** `StartExtensionInstance {require}` (a worker must advertise
  every required operation), `CallExtensionInstance` (statuses `ok`,
  `failed`, `deadlineExceeded`, `workerExited`, `busy`; calls are never
  replayed), `StopExtensionInstance` (shutdown frame, `SIGTERM`, `SIGKILL`
  for the whole group), the fault quarantine and the native trust table.
  **Native workers are not OS-sandboxed**; nothing here changes that.
- **PM-01:** the permission contract. Full never asks; Auto lets the
  provider's own reviewer decide and never becomes a human question.

What was missing (the gaps this ticket closes):
1. Nothing connected an enabled `provider` contribution to the registry, so
   a package could never appear in the picker.
2. The permission resolver (`permissions::provider_capabilities`) knew only
   the compiled providers; every other id resolved to "no longer supported"
   and could not run.
3. No provider profile existed on the worker protocol: no operation names,
   run/event/Stop semantics, or rule for unsupported capabilities.
4. The public SDK had no way to write a provider worker.
5. No external package for a real CLI existed.

## Contract

### Where a provider is declared

A package contributes a provider with a CORE-06 contribution
`{"id": "<package id>[.<name>]", "kind": "provider"}` and the
`luna.capability/provider.register` capability. Its static facts live in a
listed, hash-pinned package file, **`luna-provider.json`**, read without
running any code:

```json
{
  "declarationVersion": 1,
  "providers": [{
    "id": "novnski.gemini/gemini-cli",
    "displayName": "Gemini CLI",
    "profile": {"major": 1, "minor": 0},
    "optional": ["resume"],
    "permissionModes": {"full": "gemini --approval-mode yolo: every tool call runs without approval"},
    "automaticReviewer": false,
    "reasoningLevels": [],
    "unsupported": [{"capability": "auto", "reason": "..."}],
    "limitations": ["..."]
  }]
}
```

Unknown fields are refused. Each provider contribution needs exactly one
declaration and each declaration must name a provider contribution. The
file's bytes must still match the manifest's SHA-256 when the engine reads
it. Static facts (identity, profile, optional profiles, permission modes)
come only from this reviewed file; the worker's runtime `describe` can report
availability and its executable, never widen them.

### Registration (the existing picker path)

`ExtensionManager` reconciles providers when the engine opens and after every
enable or disable. An enabled package hash whose grant includes
`provider.register` and whose declaration validates is registered in the
engine's `HarnessRegistry` under the contribution id, as an ordinary
`Harness`. Disabling (or a grant without `provider.register`) unregisters it.
The registry generation changes, so `WatchProviderCatalog` and the open
picker update with no restart and no app change. Removal changes
availability only: chats keep their provider id and history; the next send
is refused before anything starts, and re-enabling the same package makes it
runnable again. A collision with another registered id is refused and
reported. Nothing starts a worker at registration: workers start only for
model discovery (the picker's, and the existing background model refresh
that every enabled provider already gets) and runs.

As a safety net for lifecycle paths that do not reconcile yet, the provider's
`installed()` probe is "this exact package hash is still the enabled one in
this profile" (a grants-file read, never a spawn). A stale registration
therefore leaves the picker on the next catalog read even before it is
unregistered.

The existing picker needed no change: it already renders catalog rows
generically (name, the generic widget icon, `rail-other` QA key), fetches
models through `GetProviderModels` with provenance checks, and resolves
permission availability through the same resolver the engine uses.

`ListExtensionProviders` (query, local, `localOnly`) reports every provider
contribution of an enabled package: `registered` or `refused` with the
reason, the validated declaration, every omitted capability with its reason,
the trust (`native`, `osSandbox: false`) and the last `describe` result.

### Permission modes

The declared modes feed `permissions::provider_capabilities` through a
process-wide table of registered declarations (a registered extension id is
never a built-in spelling: contribution ids are namespaced and `luna.*` is
reserved). Resolution is otherwise the existing resolver.

| Declared | Offered | Approval request from the worker |
| --- | --- | --- |
| `full` | Full access | Answered `allow` once by the bridge. No card, nothing saved. |
| `auto` with `automaticReviewer: true` and the `approvals` profile | Auto | The reviewer escalated: answered `deny`, and the reason shown as an error part. Never a card. |
| `auto` without both | Omitted and reported | — |
| `ask` | Never offered (legacy) | A legacy Ask run is refused before the worker starts. |

A declaration with no runnable mode is refused. Content questions (the
`questions` profile) reach the user in every mode, as for built-in providers.

### Provider profile 1.0 (operations over worker protocol 1.0)

Mandatory, required by name at every `StartExtensionInstance`:

| Operation | Params → result |
| --- | --- |
| `provider.describe` | `{}` → `{profile: {major: 1, minor}, providerId, available, unavailableReason?, executable?: {path, version?}}` |
| `provider.models` | `{}` → `{models: [Model]}` (1–256, unique ids) |
| `provider.run.start` | `{prompt, cwd, model?, reasoning?, modelOptions, permissionMode, resume?, attachments}` → `{runId}` |
| `provider.run.events` | `{runId, after, waitMs}` → `{events: [{seq, type, ...}], done}` |
| `provider.run.stop` | `{runId}` → `{stopping}` |

Optional profiles, declared in `luna-provider.json`:

| Profile | Adds | Without it |
| --- | --- | --- |
| `resume` | `resume` in `run.start`; the run's `session` event must echo that exact id | A run with a stored provider session is refused before the worker starts |
| `questions` | `inputRequest {requestId, questions}` events; `provider.run.answer {runId, requestId, answers \| cancelled}` | Such an event fails the run as a conformance error |
| `approvals` | `approvalRequest {requestId, kind, subject}` events; `provider.run.answer {runId, requestId, decision, reason?}` | Such an event fails the run as a conformance error |
| `attachments` | staged image paths in `run.start.attachments` | A run with attachments is refused before the worker starts |

`provider.run.answer` is required at start when `questions` or `approvals` is
declared. Never part of profile 1.0, always omitted and reported: mid-turn
steering (`steering: false`, turn-boundary only), fork, rewind, compaction,
native child control, native goals, title generation, provider accounts and
LUNA runtime tool delivery. Composer native selections (`input_items`),
native review/goal requests and the engine's no-file-edits option are
refused before the worker starts.

### Run semantics (the bridge)

One supervised instance per run (and one per model discovery), stopped when
the run ends, so a crash, a Stop or a hang affects only that run.

1. **Admission** (no process yet): mode, resume, attachments and inputs are
   checked against the declaration. Refusals are
   `RejectedBeforeAcceptance`: nothing reached the CLI.
2. **Start**: `StartExtensionInstance {require}` re-verifies the package and
   handshakes; `provider.describe` must name this provider and profile major
   1 and report `available`. Any failure here is also
   `RejectedBeforeAcceptance`, and the instance is stopped.

   What the user sees for steps 1–3: the same owner as every compiled
   provider. `run()` returns the typed error, and the engine's run task
   records it as the chat's task recovery (`RejectedBeforeAcceptance`,
   `Recovery needed`, never automatic). The transcript's `turn-recovery`
   notice shows its detail, which carries the exact reason. A message sent
   through the task command API also gets a `rejected` receipt with that
   reason, and its draft stays recoverable. No turn started, so there is no
   transcript error part. Failures after acceptance (step 4 onwards) are
   error parts in the turn.
3. **`run.start`**: `ok` with a `runId` means the worker launched the CLI.
   An error with code `rejected` means it did not (`RejectedBeforeAcceptance`).
   Any other error, a deadline or a worker exit is **delivery unknown**: the
   instance group is stopped and the request is never resent.
4. **Events**: long-polled with a cursor. Events carry `seq` 1, 2, 3... per
   run. `after` acknowledges every event at or below it and the worker frees
   them, so a reader keeps one cursor per run and never moves it back; a
   repeated poll with the same `after` (a lost reply) returns the same events
   again. A `seq` at or below the cursor is a duplicate and is dropped, so a
   lost reply never duplicates transcript content; a gap is a conformance
   failure.
   `session`, `text`, `reasoning`, `toolCall`, `toolResult`, `usage`,
   `notice`, `error`, `inputRequest`, `approvalRequest` and `done` map to the
   existing `AgentEvent`s; an unknown type is recorded as provider evidence
   and never alters the transcript.
5. **Completion**: only the worker's terminal `done {status}` completes a
   run. `completed` is `Done{Completed}`; `failed` is an error part and
   `Done{Errored}`; `interrupted` is accepted only after LUNA asked to stop
   (otherwise `Errored`). A worker exit before `done` is `Done{Errored}` with
   the exit, never `Completed`, and the turn is not replayed.
6. **Stop**: `provider.run.stop`, then events until `done{interrupted}` for
   at most 2 s, then `StopExtensionInstance` regardless, which ends every
   process in the worker's group (including the CLI). The turn ends
   `Interrupted`; an unconfirmed stop says so in its error.
7. **Continuity**: a resumed run must report the requested session id
   before any content, or it is stopped and fails; the chat's binding is not
   moved to another session.

Every failure path ends with exactly one `Done` and a stopped instance.

### SDK and the external package

- `@luna/sdk/provider` (`sdk/typescript/src/provider.ts`) implements the
  worker side: hello, call dispatch, one answer per call, cancel within the
  grace period, deadlines, bounded frames and pages, the run event queue and
  `serveProvider()`. It imports nothing from LUNA's engine.
- `examples/extensions/gemini-cli/` is the external package for Gemini CLI
  0.29.5 (`/opt/homebrew/bin/gemini`), which LUNA ships no provider for. It is
  authored like a third party's: TypeScript on Node.js, `#luna-sdk/provider`
  as its only LUNA import (vendored from the SDK by `scripts/pack.mjs`, which
  also writes the manifest and hashes). It sits outside every Cargo workspace
  member and is installed through the ordinary `InstallExtension` path from a
  directory outside the repository.

Gemini mapping: `gemini --output-format stream-json --approval-mode yolo
[--model M] [--resume S]`, the prompt on stdin (never in `argv`). Models come
from the installed CLI's own catalog module
(`@google/gemini-cli-core/dist/src/config/models.js`), read at runtime: the
two auto aliases (the CLI's default first) and its active concrete models.
Resume uses Gemini's exact session UUID (an unknown id exits with an error,
never a new session). Stop terminates the CLI's process tree. A run completes
only after the CLI's `result` frame **and** its exit, so its session file is
written before the next turn resumes it. Declared omissions: Auto (no
reviewer), questions (`ask_user` is excluded in headless mode), approvals (in
yolo none reach LUNA), attachments, steering.

## Failure contract (written before the code)

| ID | Failure | Defence |
| --- | --- | --- |
| P1 | A catalog row is mistaken for execution: the provider lists models it cannot run, or a run never reaches the CLI | Models come only from a live `provider.models`; a run goes through `provider.run.start` and ends only on the worker's terminal |
| P2 | Execution needs a compiled provider id or an app change | Registration from the enabled package's contribution id; dynamic declared permissions; no enum, kind or UI branch |
| P3 | A package registers without `provider.register`, or keeps its picker row after disable | Reconcile on open and after every enable/disable; grant checked; unregister on disable |
| P4 | A missing, invalid, tampered or mismatched `luna-provider.json` registers anyway | Listed, hash-checked at read, unknown fields refused, one-to-one with contributions; refused and reported |
| P5 | A bad handshake (wrong major, silence, early exit) is treated as a working provider | Discovery and runs fail with the handshake error; the group is gone; no `Done{Completed}` |
| P6 | A worker without a mandatory operation runs | `require` at every start refuses it before any call |
| P7 | `describe` names another provider or profile, or the CLI is unavailable, and discovery returns empty success | Refused with the reason; unavailable is an explicit error |
| P8 | Unsupported optional capabilities are claimed or used: Auto without a reviewer, legacy Ask, resume, attachments, inputs, questions or approvals without their profile, steering | Dropped and reported at registration; refused before start; a conformance failure mid-run; steering rejected |
| P9 | The bridge invents acceptance: an unconfirmed start is resent | Only `rejected` is `RejectedBeforeAcceptance`; deadline/exit is delivery unknown; the group is stopped |
| P10 | The bridge invents completion: crash, gap or unrequested interruption reported as success | Only `done{completed}` completes; exits and conformance failures are `Errored` |
| P11 | A lost events reply duplicates content | Cursor and `seq`; duplicates dropped |
| P12 | Stop is cosmetic: the CLI keeps running | `run.stop`, bounded confirmation, then the supervisor stops the whole group |
| P13 | A crash mid-run is replayed or leaves the CLI running | Never replayed; the instance is stopped; the group is cleared |
| P14 | An Auto or Full run parks a human approval card | The bridge settles approvals by mode and never calls `request_input` for them |
| P15 | A resume silently continues another session | The `session` event must echo the requested id first; otherwise stopped and failed |
| P16 | Removing the provider breaks history or silently substitutes another provider | Unregister only; chats keep their id; the next send is refused |
| P17 | A native provider is presented as sandboxed | `osSandbox: false` and the trust line in the provider's known limitations |

| Owner test (`crates/engine/tests/extension_providers.rs`) | Cases |
| --- | --- |
| `enabled_provider_package_joins_the_picker_runs_and_leaves_on_disable` | P1–P3, P11 (contiguous seqs), P14, P15 (positive), P16, P17 |
| `conformance_failures_are_explicit_and_leave_nothing_running` | P4–P7 |
| `stop_and_crash_end_the_turn_honestly_and_end_the_cli` | P9 (no resend), P10, P12, P13 |
| `unsupported_optional_capabilities_are_omitted_and_refused` | P8, P15 (mismatch) |

Every test installs real package bytes through `InstallExtension` and
`SetExtensionEnabled`, reads the registry through `GetProviderCatalog`,
`GetProviderModels` and `ListExtensionProviders`, sends through the ordinary
doc command queue (`Run`, `Interrupt`) and reads what the chat shows: the
transcript (error parts after acceptance) and the chat's recovery notice
(refusals before acceptance, with the exact reason). The worker is
an independently written Python peer of profile 1.0
(`tests/support/extension_provider_peer.rs`) that starts a real "CLI" child
process and records what it was asked to do in the run's working directory.
Readback the engine does not produce: process existence from the OS
(`kill(pid, 0)`) for the worker and the CLI child, the peer's own call log,
and the package hash recomputed from the published rule.

Not tested here:
- A lost `events` reply (needs transport loss between engine and worker);
  the cursor/seq rule is exercised only by the positive path.
- Disabling a package during a running turn (CORE-07 revocation stops the
  worker; the bridge reports the exit).
- The admission refusals for attachments, composer selections, native
  review/goal and the no-file-edits option: the engine validates staged
  attachments and selections before dispatch, so a deterministic case needs
  real uploads. The bridge code path is the same as the tested Ask/resume
  refusals.
- The `questions` profile and Auto with a declared reviewer: no peer
  scenario declares them; the Gemini package declares neither.
- The SDK and the Gemini adapter inside the engine suite (it would need
  Node.js in that job): `scripts/smoke.mjs` drives the packed worker over
  the same protocol with a fake `gemini`, and native QA runs the real one.
- The real Gemini CLI: native QA below.

## Criteria mapping

- **contract:** this file; `crates/engine/src/extensions/providers.rs` and
  `providers/{declaration,bridge,declared}.rs`; `sdk/typescript/src/provider.ts`.
- **delivery:** `examples/extensions/gemini-cli/` (worker, launcher,
  declaration, pack script); registry reconciliation; dynamic permission
  declarations; `ListExtensionProviders`.
- **verification:** the four E2E tests above and the native QA (E10) below.
- **handoff:** the orchestrator's build, E2E, lint, review, native QA, LUNA
  Dev refresh and delivery.

## Commands (orchestrator)

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-rpc -p luna-engine
scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract   # +1 row: ListExtensionProviders
scripts/cargo.sh test --locked -p luna-rpc --features schema --test contract
scripts/cargo.sh test --locked -p luna-rpc --lib remote_auth::tests::policy_inventory_is_complete_and_unknown_fails_closed
EVIDENCE="$PWD/target/qa/core08/$(date -u +%Y%m%dT%H%M%SZ)"
LUNA_CORE08_EVIDENCE_DIR="$EVIDENCE" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test extension_providers -- --nocapture
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test extension_runtime --test extension_packages --test provider_scope_e2e
scripts/cargo.sh clippy --locked -p luna-rpc -p luna-engine --all-targets
scripts/cargo.sh check --locked -p luna-ui
npm run typecheck --prefix sdk/typescript
```

The E2E needs `python3` on `PATH` (the peer's shebang is
`/usr/bin/env python3`, like CORE-07's). CI runs it in `ui-tests.yml`
("Provider extensions (CORE-08)").

Pack and typecheck the external package outside the repository:

```sh
OUT=/tmp/core08-external
rm -rf "$OUT" && mkdir -p "$OUT"
cp -R examples/extensions/gemini-cli "$OUT/source"
node "$OUT/source/scripts/pack.mjs" --sdk "$PWD/sdk/typescript/src/provider.ts" --out "$OUT/package"
sdk/typescript/node_modules/.bin/tsc -p "$OUT/package/tsconfig.json" --typeRoots "$PWD/sdk/typescript/node_modules/@types"
node "$OUT/source/scripts/smoke.mjs" "$OUT/package"     # protocol smoke with a fake gemini on PATH; no model call
```

## Native QA (E10, LUNA Dev, one real Gemini run)

`scripts/qa-core08-provider.mjs` talks to LUNA Dev's owner IPC
(`ws://127.0.0.1:59777`) with Node's built-in WebSocket and writes JSON
evidence; it never sends a prompt.

1. **Before:** quit nothing; record the app identity:
   `node scripts/qa-core08-provider.mjs hash-app "target/luna-macos-dev/LUNA Dev.app" "$EVIDENCE/app-before.json"`.
2. Pack as above. `node scripts/qa-core08-provider.mjs install "$OUT/package" "$EVIDENCE"`
   installs with the printed hash as `expectedPackageHash` and enables with
   exactly the requested grants. Readback: `install.json`, `providers.json`
   (`registered`, omitted capabilities, `osSandbox: false`), `catalog.json`
   (the provider row), `models.json` (Gemini's own model ids, with
   provenance `executable` = `extension:novnski.gemini/gemini-cli@1.0.0#<hash>`).
3. In LUNA Dev, without restarting: open the composer's model picker. A
   Gemini CLI rail appears with the models from `models.json`. Capture a
   screenshot and the AX labels.
4. Choose Full access (Auto is shown unavailable with its reason) and a
   model, in a disposable project. Send: `Reply with exactly the word
   PONG and nothing else.` Expect a streamed `PONG` and a completed turn.
   Capture.
5. **Stop:** send `Count slowly from 1 to 200, one number per line.` and
   press Stop after the first lines. The turn ends interrupted;
   `node scripts/qa-core08-provider.mjs readback "$EVIDENCE/after-stop"`
   shows the run's instance `stopped` with `groupCleared: true`; `pgrep -f
   'gemini.*stream-json'` finds nothing.
6. **Continuity:** send `What word did you reply with first?` in the same
   chat; it resumes the same Gemini session (the answer names PONG).
7. **Failure (and no silent new session):** right after step 6, the newest
   Gemini session file is this chat's:
   `S=$(ls -t ~/.gemini/tmp/*/chats/session-*.json | head -1); grep -c PONG "$S"`
   (non-zero). `mv "$S" "$S.qa-aside"` and send a message: Gemini exits
   before any model call and the turn ends with its own `Error resuming
   session` text; `ls -t ~/.gemini/tmp/*/chats/ | head` shows no new
   session. `mv "$S.qa-aside" "$S"` and send again: it resumes.
8. **Removal:** `node scripts/qa-core08-provider.mjs disable "$EVIDENCE"`:
   the open picker drops the rail; the chat stays readable; a send is
   refused. `enable` brings it back.
9. **After:** `hash-app` again to `app-after.json`; the bundle digest and
   every file hash are identical to `app-before.json`, and
   `codesign --verify --deep --strict` still passes.
10. `node scripts/qa-core08-provider.mjs manifest "$EVIDENCE"` writes
    `SHA256SUMS`.

## Limitations and hand-offs

- **Native trust:** the Gemini worker is user-level native code with the
  account's access, as CORE-07 states. Only LUNA's broker is scoped.
- **Environment:** workers get CORE-07's allow-listed environment (`PATH`,
  `HOME`, ...). A `GEMINI_API_KEY` exported in a shell profile does not
  reach Gemini; Gemini's own `~/.gemini/.env` and OAuth login do.
- **Remote pickers:** declared permission modes are known in the process
  that hosts the engine. A picker previewing another device's provider uses
  that host's resolution at dispatch, as before.
- **Process-wide declarations:** two engines in one process that register
  the same provider id share the latest declaration (one engine per process
  in the app).
- **Lifecycle (CORE-19):** a new package version is a different hash. The
  update, rollback, removal and safe-mode paths should call
  `ExtensionManager::providers_changed()` after their grants write (enable
  and disable already do). Until they do, the `installed()` safety net hides
  a stale registration, but a newly switched version joins the picker only
  at the next enable/disable or engine open. Safe mode refuses starts, so a
  registered provider then fails at send with the safe-mode reason.
- **Gemini:** no Auto reviewer, no mid-turn steering, no questions; model
  availability depends on the user's Gemini account; token usage is the
  CLI's own `result.stats`.
- **Closed without a real Gemini run (user decision, 2026-10-14):** LUNA
  will not add Gemini. Native QA steps 1–3 and 8–10 passed on unchanged app
  bytes (`target/qa/core08/20261013/native/results.md`). Step 4 surfaced a
  real failure: Gemini refused the account (`IneligibleTierError`) before
  any model call. Steps 5–7 (Stop, continuity, resume failure) were not run
  against a real CLI; the conformance E2E covers them with a scripted CLI.
  The first real provider turn, Stop and continuation through a package
  path are proven by CORE-09–11, which move Codex, Claude Code and OpenCode
  onto this contract. `examples/extensions/gemini-cli/` stays as the
  reference third-party package; it is not shipped.
