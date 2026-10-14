# CORE-11: OpenCode as a first-party provider package

Baseline: `11b97743` (14 October 2026). Live status belongs to
`pending_tasks.py`. This file is the contract, the failure cases (written
before the code), the criteria mapping and the evidence plan.

User decision (2026-10-14): Codex and Claude Code stay built-in default
providers (CORE-09/10 deferred). OpenCode becomes a provider package, and this
ticket supplies the real-provider proof CORE-08 could not get from Gemini: a
real OpenCode model listed in the ordinary picker completes a real task,
steers, stops and fails honestly, all through the package path.
`examples/extensions/gemini-cli/` stays the reference third-party package.

## Rebased against current main

Reused, unchanged in meaning:
- **CORE-08:** provider contributions → `HarnessRegistry`, `luna-provider.json`,
  declared permission modes, `ListExtensionProviders`, the profile 1.0 bridge.
- **CORE-07:** `StartExtensionInstance {require}`, `CallExtensionInstance`
  (never replayed), `StopExtensionInstance` (whole group), fault quarantine,
  per-group memory/process sampling, safe mode. **Native workers are not
  OS-sandboxed.**
- **CORE-06/19:** packages, grants, update/rollback/removal, safe mode,
  `providers_changed()` after every enablement change.
- **The OpenCode 2 adapter** (`crates/harness/src/opencode`, ~28 files) and its
  retained peers (`crates/harness/tests/opencode.rs`, `src/opencode/tests.rs`,
  `engine/tests/board_tools_opencode_e2e.rs`).

What was missing:
1. Profile 1.0 is a thin normalized vocabulary (text, tool call, done). It has
   no steering, native children, LUNA tools, approval cards, fork, history,
   rewind, compaction, titles, commands, MCP status or native extensions, so
   OpenCode could not move onto it without losing behavior.
2. No first-party package path: the reserved `luna` namespace could not be
   installed or even read back, and nothing bundled, installed or enabled a
   package by default.
3. A package could not take over a built-in provider id, so old chats bound to
   `opencode` could not continue through a package.
4. CORE-07's environment allow-list strips what OpenCode reads in process
   (provider keys, XDG/OpenCode config and data paths, the QA
   `OPENCODE_EXECUTABLE` wrapper).

## OpenCode 2 today (the `contract` criterion)

Read from the adapter and the installed CLI on this machine (`~/.opencode/bin/
opencode` 2.0.24; a Homebrew 1.18.34 earlier on `PATH` is skipped by the
resolver). The adapter's documented wire was verified against 2.0.22; every
boot checks the live `/openapi.json` against the operations it uses.

| Fact | Behavior (kept exactly) |
| --- | --- |
| Generation | OpenCode 2 only: the resolver accepts a binary whose `--version` is 2.x (`OPENCODE_EXECUTABLE`, then `PATH`, the login shell's `PATH`, known install dirs). A 1.x-only machine keeps the provider visible and fails with "OpenCode 2 is required". No 1.x, ACP, SDK or `opencode run` path exists or is added. |
| Server | `opencode serve --stdio --port 0`: the OS picks a loopback port, the `{"url"}` stdout line is readiness, `GET /api/info` proves 2.x, a per-server Basic password rides `OPENCODE_PASSWORD` (dropped from the server's own env in `--stdio`), stdin is the ownership lease. Startup bound 300 s. |
| Ownership before | Each run spawned its own server as a child of LUNA; discovery, titles, fork, history, rewind, commands, extensions and MCP status spawned short-lived servers (home or the chat's location); compaction of a live chat reused that run's server. |
| Ownership now | Each turn's server is a child of **that turn's package worker** (its own CORE-07 instance and process group); out-of-turn servers are children of the provider's **service worker**. Ports and passwords never leave the worker's adapter. |
| Events | One global SSE bus (`/api/event`), every session token-level; turn = `POST …/prompt {id, delivery}`, starts on `inbox.delivered{id}`, ends on the next `execution.succeeded/failed/interrupted` of that session; reconnect resyncs by readback; malformed and unknown frames are logged, never transcript. |
| Steering | Native, step-bounded: `delivery: "steer"`, accepted on HTTP 200, `Steered` on its own `inbox.delivered`. Ordinary warm routing may continue the run with a follow-up prompt. |
| Models | `GET /api/model` (enabled models of configured providers), id `<providerID>/<modelID>`, labels from `GET /api/provider`. No bundled list; a failed probe keeps the engine cache's last good snapshot. |
| Variants | The effort ladder picks the first advertised variant id per level (`low`→low/minimal … `max`→max/xhigh/high); variants it cannot express become a `variant` model option; an unoffered variant is dropped at run setup. |
| Accounts | OpenCode's own: `opencode auth login`, credentials in OpenCode's data directory; provider keys from the environment. LUNA has no OpenCode account settings. |
| Permissions | Legacy Ask: session rules (reads allow; shell, edits, web, external paths ask), approval cards with LUNA-owned "Allow for session" grants. Full: allow all except configured denies, stray asks answered `once`, never a card. Auto: unavailable (no reviewer; the engine refuses it as a hidden Ask). Rules are read back before the first prompt; a run whose rules did not apply never starts. |
| Children | Native child sessions at any depth; per-child Stop through the live run. |
| LUNA tools | Inline `luna` MCP layer via `OPENCODE_CONFIG_CONTENT`, verified through `/api/mcp` before any tool is allowed; per-version delivery verdict. |
| History | Checkpoint fork (staged progress), turn readback, same-session rewind (`revert.stage{files:false}` + `commit`), provider compaction. |

## Contract

### Provider profile 2.0: the hosted harness profile

`crates/harness/src/hosted.rs` (contract and codecs) and
`hosted/worker.rs` (worker side) define a second provider profile for a
worker that hosts a whole `Harness`. It carries LUNA's own public types, so
nothing the adapter emits is re-modelled:

| Operation | Params → result |
| --- | --- |
| `provider.describe` | `{}` → `{profile:{major:2}, providerId, available, unavailableReason?, executable?, runtimeDelivery}` |
| `provider.job.start` | `{job, op, params}` → `{}`; ops `models run title commands extensions mcp fork history rewind compact` |
| `provider.events` | `{after, waitMs}` → `{events:[{seq, type, job, …}]}`, one queue per worker |
| `provider.job.reply` | `{job, kind: input\|tool\|progress, id, …}` |
| `provider.run.steer` / `run.stop` / `job.cancel` / `child.stop` / `upload` | steer (acceptance via `steerAck`), the run's own interrupt, abort, native child Stop, chunked parameters |

Queue items: `started`, `agent {event: AgentEvent}`, `streamError {kind,
message}`, `input {questions: [UserInputQuestion], launchUrls}`, `tool {name,
arguments, context}`, `steerAck`, `progress {ack?}`, `observation`, `done
{ok, result|error}` and `fragment`. Each has the next `seq`; `after`
acknowledges; a repeated `after` re-reads; a gap fails the worker. Items over
20 KiB are split into consecutive fragments; parameters over 40 KiB are
uploaded in 16 KiB chunks. Nothing is dropped or truncated: above 8 MiB
unacknowledged, the run's event stream waits. Fork and rewind progress block
the adapter until the bridge confirms it, as the in-process reporter does.

The declaration's `hosted` section (`luna-provider.json`, profile 2.0) states
the adapter's static facts (steering mode, deterministic turn end, safe
recovery, fork capability, native child stop, runtime delivery, the job
operations) and its `ProviderPermissionCapabilities` verbatim; legacy Ask may
be declared for chats that already use it. Profile 1.0 is read as before.

### The bridge (`crates/engine/src/extensions/providers/hosted.rs`)

- **One CORE-07 instance per turn**, stopped when the turn ends. It owns that
  turn's `opencode serve`, so Stop, a crash or a memory/process limit ends only
  that turn. A native child Stop and the compaction of a chat with a live turn
  go to that turn's worker (live root-session map).
- **One service instance** for the out-of-turn operations, kept while the
  provider is registered (the single in-process adapter's lifetime); replaced
  when its worker is gone.
- **Acceptance.** Admission is the engine's (mode resolution, input
  validation, receipts). A failure before `job.start` reached the worker, or
  the worker's `rejected`, is `RejectedBeforeAcceptance`. The adapter's own
  `run` decides the rest: `started`, or its exact typed error. A worker that
  ends before either is delivery-unknown: "the prompt may have reached
  OpenCode, so LUNA does not send it again".
- **Completion.** Only the adapter's `Done` ends a turn. The bridge adds one
  only when the worker ended without it: `Errored` with "… during the run; the
  turn was not replayed", or `Interrupted` after a Stop.
- **Stop.** `provider.run.stop` cancels the adapter's own interrupt token
  (OpenCode `POST …/interrupt`, then its 2 s/3 s escalation); without the
  run's end within 8 s, the bridge stops the instance (every process in its
  group).
- **Permission modes.** The adapter settles approvals itself. An approval card
  arriving in Full or Auto is declined and shown as an error; it never reaches
  a person. Content questions reach the user in every mode.
- **State between turns.** LUNA's "Allow for session" grants are exported by
  the turn's worker when its run ends and imported by the next, merged by
  session key (a session whose grants the adapter dropped is removed).

### The first-party package (`providers/first_party.rs`)

`luna.provider/opencode`, version = LUNA's version, files: `bin/luna-opencode-
worker` (a `luna-engine` binary that hosts `OpencodeHarness`) and the reviewed
`luna-provider.json` (`providers/opencode/`), pinned by a manifest assembled
at engine open. `runtime.limits`: 4096 MiB, 86 400 CPU s.

1. **Bundling.** `scripts/run-macos-dev.sh` builds and signs
   `luna-opencode-worker` next to `luna` in `Contents/MacOS`. The engine finds
   it next to its own executable, or at `LUNA_OPENCODE_WORKER` (absolute
   path; `off` installs and updates nothing).
2. **Install.** Through the ordinary staging and verification; the reserved
   `luna` namespace is accepted only on this path and when reading the store.
   An unchanged bundle is recognized from metadata (no re-hash); a rebuilt
   worker with the same version replaces the old bytes; unused versions are
   removed.
3. **Enable by default, once per profile**, with exactly native code +
   `provider.register` (`SetExtensionEnabled`). A profile record
   (`first-party-extensions.json`) remembers it: a user's disable survives
   restarts and updates. An enabled older first-party hash moves to the
   bundled one with the same grants.
4. **One path.** While the package is installed, the compiled `opencode`
   registration is removed from the registry. The package's contribution
   `luna.provider/opencode` registers under the legacy spelling `opencode`
   (byte for byte), so existing chats, their provider sessions and their
   permission modes are untouched. Disabled → no OpenCode in the picker, a
   send is refused before anything starts, history stays; enabled → back;
   removed (Extensions) → absent until the next launch reinstalls it,
   disabled; safe mode → OpenCode sends are refused with the safe-mode reason
   (Codex and Claude remain).
5. **Environment.** A first-party native worker inherits the engine's
   environment, as the in-process adapter did; its instance reports
   `environmentAllowList` as not enforced. Still no OS sandbox.

Without a bundled worker and without an installed package (other builds,
`luna-headless`, `off`), the compiled adapter keeps serving OpenCode.
`ListExtensionProviders.firstParty` reports bundle state, installed and
enabled hash, `enabledByDefault` and `replacesBuiltin`.

## Failure contract (written before the code)

| ID | Failure | Defence |
| --- | --- | --- |
| H1 | OpenCode still runs through the compiled slot (or a provider-id shortcut) while its package is installed | The installed package displaces the compiled registration; registration only from an enabled package; catalog provenance is the package |
| H2 | Old chats lose identity: a new spelling, session, mode or second registration | Legacy spelling `opencode`; registry key equality; the stored session resumes; one catalog row |
| H3 | A fresh profile lacks OpenCode, or a user's disable is undone at the next launch/update | Profile record; enable once; never again; updates move only an enabled hash |
| H4 | A build without the worker loses OpenCode, or installs something that cannot run | No bundle → no install; the compiled adapter stays unless a package is installed; status says why |
| H5 | A rebuilt worker keeps running old bytes, or same-version installation fails | Content-addressed replacement at open; enabled hash follows the bundle |
| H6 | Events reordered, coalesced, truncated, lost or duplicated across the worker | One queue, contiguous `seq`, cursor acknowledgement, duplicates dropped, gaps fail, lossless fragments, backpressure |
| H7 | A prompt, tool result or reply larger than a frame breaks the run | Uploads and fragments, bounded at 64 MiB |
| H8 | A start whose answer was lost is treated as accepted or resent | `rejected`/not sent → RejectedBeforeAcceptance; otherwise delivery-unknown, instance stopped, never resent |
| H9 | A worker exit or abort reported as completion | Only the adapter's `Done`; the bridge's own ending is `Errored`/`Interrupted` with the reason |
| H10 | Stop is cosmetic: `opencode serve` keeps running | Adapter interrupt, bounded wait, then the instance group is stopped |
| H11 | A crash replays the turn or leaves the server behind | Never replayed; the group is killed; error part |
| H12 | Permission drift: Full parks a card, Auto runs, stray asks unanswered, legacy Ask cards lost, session grants forgotten between turns, rules readback skipped | Same adapter in the worker; bridge declines cards in Full/Auto; declared facts equal the built-in; grants carried between turns |
| H13 | A native child Stop or a live compaction hits a server that does not own the session | Live root-session → turn worker map |
| H14 | LUNA tools claimed but not delivered, or their context forged | Tool calls relayed with the adapter-matched context; results returned verbatim; delivery narrowed when a turn reports it withdrawn |
| H15 | Fork/history/rewind/titles/commands/extensions/MCP status lost, or a mutation runs before its progress is recorded | Service worker jobs; progress acknowledged before the adapter continues |
| H16 | Account/model drift: the worker misses keys, config dirs or the QA wrapper | Inherited environment (first-party only), reported |
| H17 | Disable/removal/safe mode leaves OpenCode runnable, loses history or silently brings back the compiled adapter | Unregister only; chats keep their binding; next send refused; no fallback while installed; safe mode refuses starts |
| H18 | The worker is shown as sandboxed | `osSandbox: false`, native trust, allow-list reported not enforced |
| H19 | OpenCode 1.x or ACP comes back | The worker hosts only the OpenCode 2 adapter; resolver unchanged |
| H20 | One heavy turn or crash takes down other OpenCode turns | One instance (process group, memory and process budget) per turn |

| Owner test | Cases |
| --- | --- |
| `engine/tests/opencode_package_parity.rs` `the_package_keeps_the_builtin_behavior_and_the_chat_identity` | H1, H2, H4 (off → compiled adapter), H6 (late, malformed, reconnect), H7 (60 KB prompt, 100 KB reply), H12 (Full stray ask, rules not applied), H15 (commands, native extensions), H16, H19 |
| `engine/tests/opencode_package_lifecycle.rs` `turns_steer_stop_and_fail_honestly_through_the_package` | models with variants and provenance, H8, H9, H10, H11, H12 (Full and legacy Ask), H18, H20, steering |
| `…lifecycle.rs` `disable_enable_safe_mode_and_restarts_keep_one_opencode_path` | H3, H5 (no reinstall), H17, continuity after re-enable |
| `engine/tests/board_tools_opencode_package_e2e.rs` | H14: the retained A4 board scenario, unchanged, through the worker |
| Retained, unchanged: `harness/tests/opencode.rs`, `harness/src/opencode/tests.rs`, `engine/tests/board_tools_opencode_e2e.rs`, `engine/tests/extension_providers.rs` | Adapter wire semantics (the same code the worker hosts); CORE-08 profile 1.0 |

The parity test's expected side is the retained in-process adapter on the
same data root and scripted server, never the new path. The scripted server
(`engine/tests/support/fake-opencode-package.py`) only records what it was
asked (`<checkout>/.fake-opencode/*.jsonl`). Independent readback: the
server's records, OS process existence and parent pids (a turn's server is a
child of a worker, not of the engine), the installed package files re-hashed,
and the worker binary's bytes.

Not tested deterministically (stated, not claimed):
- H13 (no scripted native children), fork/history/rewind/titles/MCP status
  through the service worker (the codecs are shared, the retained harness
  peers own the adapter behavior), and the 8 MiB backpressure path.
- A worker crash during a fork or rewind: the outcome is reported unknown,
  never retried.

## Criteria mapping

- **contract:** this file ("OpenCode 2 today", "Contract");
  `crates/harness/src/hosted.rs`; `providers/opencode/luna-provider.json`.
- **delivery:** `crates/harness/src/hosted/worker.rs`,
  `crates/engine/src/bin/luna-opencode-worker.rs`,
  `extensions/providers/{hosted,first_party}.rs`, profile 2.0 in
  `providers/declaration.rs`, registration in `providers.rs`, grant
  continuity in `harness/src/opencode/permissions.rs`, bundling in
  `scripts/run-macos-dev.sh`.
- **verification:** the four E2E files and the retained peers above, CI step
  "OpenCode provider package (CORE-11)", and the native QA below.
- **handoff:** the orchestrator's build, tests, lint, review, native QA, LUNA
  Dev refresh and delivery.

## Commands (orchestrator)

```sh
python3 scripts/build-budget.py --status
scripts/cargo.sh check --locked -p luna-harness -p luna-engine
scripts/cargo.sh test --locked -p luna-harness --test opencode
scripts/cargo.sh test --locked -p luna-harness --lib opencode::
EVIDENCE="$PWD/target/qa/core11/$(date -u +%Y%m%dT%H%M%SZ)"
LUNA_CORE11_EVIDENCE_DIR="$EVIDENCE/e2e" CARGO_INCREMENTAL=0 \
  scripts/cargo.sh test --locked -p luna-engine --test opencode_package_parity \
  --test opencode_package_lifecycle --test board_tools_opencode_package_e2e -- --nocapture
CARGO_INCREMENTAL=0 scripts/cargo.sh test --locked -p luna-engine --test extension_providers \
  --test extension_runtime --test extension_packages --test extension_lifecycle \
  --test board_tools_opencode_e2e
scripts/cargo.sh clippy --locked -p luna-harness -p luna-engine --all-targets
scripts/cargo.sh check --locked -p luna-ui -p luna
scripts/run-macos-dev.sh --refresh
```

The E2Es need `python3` on `PATH`. Cargo builds `luna-opencode-worker` for
the engine's test targets (`CARGO_BIN_EXE_luna-opencode-worker`); no extra
build step. Each parity/board file is one test because it sets process
environment before its engine starts.

## Native QA (E11, a QA app, real OpenCode)

Use a QA identity with all four `LUNA_DEV_*` overrides
(`docs/reference/qa-infrastructure.md`) and a fresh data dir; real OpenCode 2
signed in (`opencode auth login`). `PORT` is the QA app's IPC port.
`Q="node scripts/qa-core11-opencode.mjs"`; `APP` the QA bundle.

1. **Before:** `$Q hash-app "$APP" "$EVIDENCE/app-before.json"` (records the
   bundled `luna-opencode-worker` hash too).
2. **Built-in era (old chat):** start the QA app once with the package off:
   `open --env LUNA_OPENCODE_WORKER=off "$APP"`. `$Q readback "$EVIDENCE/builtin"
   --port $PORT`: `providers.json` `firstParty[0].bundle == "off"`,
   `installed == false`; `processes.json` after a turn shows its `opencode
   serve` owned by `luna`. In a disposable project, an OpenCode chat (Full
   access): `Reply with exactly the word PONG and nothing else.` → PONG. Quit
   LUNA normally.
3. **Package era:** open the QA app normally. `$Q readback "$EVIDENCE/package"
   --port $PORT`: `firstParty[0]` installed, enabled, `enabledByDefault`,
   `replacesBuiltin`; `package.json` origin = the bundled worker path;
   `catalog.json` exactly one `opencode` row, `executable` =
   `extension:luna.provider/opencode@<version>#<hash>`; `models.json` your
   real OpenCode models; `instances.json` the service worker, `osSandbox:
   false`, `environmentAllowList` under `notEnforced`.
4. **Picker:** open the composer's model picker: OpenCode lists the models of
   `models.json` (effort levels where variants exist). Screenshot + AX labels.
5. **Old chat continues:** in the step-2 chat send `What word did you reply
   with first?` → it names PONG (same OpenCode session). During the turn,
   `$Q processes "$EVIDENCE/package"`: its `opencode serve` has
   `ownedByWorker: true`.
6. **Real task:** new OpenCode chat, Full access: `Create a file named
   core11.txt containing the word ready, then reply DONE.` → tool card, file on
   disk, DONE, no approval card.
7. **Steer:** `Count slowly from 1 to 60, one number per line.` While it
   counts, Steer: `Stop counting now and reply STEERED.` → the steer appears
   mid-turn and the reply ends with STEERED in the same turn.
8. **Stop:** `Count slowly from 1 to 200, one number per line.` → Stop after
   a few lines: the turn ends interrupted; `$Q readback "$EVIDENCE/after-stop"`:
   that turn's instance `stopped`, `groupCleared: true`; `processes.json` has
   no `opencode serve` of that turn.
9. **Forced failure:** start the step-8 prompt again, find its worker in
   `processes.json` (the `ppid` of its `opencode serve`) and `kill -9` it: the
   turn shows `OpenCode: the worker exited (…) during the run; the turn was
   not replayed`, no `opencode serve` remains, nothing is resent. Optionally
   also pick a model of a provider you are not signed in to: OpenCode's own
   error appears in the turn.
10. **Disable / enable:** `$Q disable "$EVIDENCE"`: the open picker drops
    OpenCode, chats stay readable, a send is refused. Quit and reopen: still
    disabled, and no compiled OpenCode reappears. `$Q enable "$EVIDENCE"`: back;
    the step-2 chat answers again in its session.
11. **After:** `$Q hash-app "$APP" "$EVIDENCE/app-after.json"`: bundle digest
    and every file hash identical to step 1; `codesign --verify --deep
    --strict "$APP"` passes. `$Q manifest "$EVIDENCE"` writes `SHA256SUMS`.

## Limitations and what remains on the compiled path

- **Trust:** the worker is user-level native code with the account's access
  and, as LUNA's own, the engine's full environment. Only LUNA's broker is
  scoped; nothing is OS-sandboxed.
- **Budgets:** a turn's worker group may use 4 GiB and 64 processes (CORE-07
  ceilings); past that LUNA ends that turn, which the in-process adapter never
  did. Each turn also pays a worker start, the package re-hash at start and
  one `opencode --version` probe.
- **Still compiled:** the lazy `opencode` slot in `default_registry()` is the
  fallback when no package is installed. Engine readers that never run a turn
  keep calling the adapter crate directly: input admission
  (`provider_inputs.rs`), OpenCode storage for usage/accounts/skills
  (`usage_opencode*.rs`, `agent_accounts.rs`), version/compat readback
  (`provider_updates.rs`). `HarnessId::Opencode` branches in the engine and UI
  (steering ledger, title support list, settings) apply unchanged because the
  package registers the same id; their removal belongs to CORE-23/CORE-12.
- **Approximations:** the per-version LUNA tool delivery verdict lives in each
  turn's worker; the bridge withdraws delivery for 10 minutes after a turn
  reports it (the in-process adapter kept it per version). The service
  worker's command cache is no longer invalidated by a turn's catalog-change
  event; it ages out (TTL). The adapter's `tracing` output stays in the worker
  (stderr reaches the instance log window; `LUNA_OPENCODE_WIRE_LOG` still
  works).
- **Packaging:** only `scripts/run-macos-dev.sh` bundles the worker today;
  release packaging bundles no helper yet, so those builds keep the compiled
  adapter until it does.
- **Text:** the run policy's `providerMapping` for OpenCode now reads
  "extension provider: session rules: …" (CORE-08's declared-mapping prefix);
  the modes and their enforcement are unchanged.
- **Updates:** the package tracks LUNA's version; an independently updated
  OpenCode package cannot be installed (reserved namespace). A package
  version switch while a turn runs follows CORE-19 (drain or stop).
- **Platforms:** macOS native QA only; Linux runs the E2Es in CI; Windows
  (`.exe` suffix handled) untested. Hosted CI may still be blocked by billing.
