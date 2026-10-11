# Failure contracts, test ownership and performance gates

Everything in this file is **planned acceptance**, not a passing result from
the source audit. Before changing code, follow the repository's
[test-audit gate](../../../.codex/skills/test-audit/SKILL.md): observable behavior,
credible regression, strongest single owner and no test-only production seam.
Do not add unit tests after implementation. Existing parser/wire tests that own
distinct contracts stay valuable; prefer real E2E for lifecycle and integration.

## Acceptance ownership

| ID | Behavior and credible failures | Primary boundary / retained artifact |
| --- | --- | --- |
| E01 | Current baseline/ADR: wrong source/version, unknown runtime assumption, current defect called a migration regression, untested isolation claim. | CORE-00; source/build/platform/provider inventory, failing/passing baseline ledger, prototype receipts and decision record. |
| E02 | Performance: main-thread plugin stall, startup discovery of disabled plugins, unbounded worker memory/logs/queues, hidden offscreen video, expired subscriptions, event-to-paint delay, shifting memory to child processes. | CORE-00 baseline → CORE-20 final; identical workload/data/hardware manifests, native traces, per-process/aggregate samples, raw measurements and reviewed budget deltas. |
| E03 | Contract/versioning: unknown provider/content breaks old readers; wrong schema/default/null semantics; future method bypasses policy; changed JSON order changes command identity; SDK decodes success incorrectly. | CORE-01/05/21; independently produced old/new wire fixtures and SDK requests, real service responses, denied-call receipts and retained resulting stores. |
| E04 | Test ownership: moved keeper lost, baseline bug dismissed, fake supplies the answer, test-only export retained without use, an example never executes in CI. | CORE-02/22; current R/F/C/D delta with remaining keeper/callers, before/after failures, selected independent mutation checks and actual target routing/run artifacts. |
| E05 | Core lifecycle: two writers, stale epoch after reopen, mismatched task/checkout/profile, cancellation leak, receipt before durable admission, duplicate side effect after crash. | CORE-03; existing public engine command/restart/identity scenarios with real temp stores/processes, sequence/receipt/journal/OS-lock readback. |
| E06 | Dynamic registry: install/enable does not refresh picker, late result uses wrong host/account, ID collision or rename corrupts persisted tasks, missing provider silently replaced. | CORE-04; real registry/picker readback before/after changes, migration fixtures, exact native owner/account/session records and original history hashes. |
| E07 | Client parity: CLI/SDK can do more than UI authority, target inferred from focus, leaked watch after drop, unknown outcome retried as new command. | CORE-05/21; app, CLI and independent SDK client exercise one same allowed/denied operation against a real engine and compare state/receipts. |
| E08 | Package/grants: archive traversal/symlink escape, hash tampering, install executes code, incompatible dependencies, grant copied to another profile, partial install after crash, expanded update grants silently accepted. | CORE-06/19; real package files, independent filesystem/process/grant readback, staged/active package hashes, restart and refusal logs. |
| E09 | Runtime broker: plugin forges caller/target, gets owner IPC, blocked host import evades timeout, event floods starve core, native trust mislabeled, crash takes UI down or replays work. | CORE-07/19; deliberately failing portable and native fixture packages, OS process/resource evidence, exact denied operation and unchanged unrelated task state. |
| E10 | New provider: a catalog row is mistaken for execution; core needs a new enum/compile; unsupported resume/approval claimed; Stop or lost reply duplicates work. | CORE-08; external SDK-only package and actual selected CLI, native picker/run/response/control evidence and unchanged installed app/bundle hashes. |
| E11 | Built-in parity: Codex, Claude or OpenCode loses its own wire semantics, credentials, mode, steer/child/native history or full-result policy during extraction. | CORE-09/10/11; each original adapter's retained subprocess peers plus native provider-specific acceptance through the new worker transport. |
| E12 | UI contributions: plugin replaces selected task or draft, stale callback targets new tab, contribution removal breaks saved layout, custom sidebar hides recovery/permissions, focus/AX fails, render work is unbounded. | CORE-12/13; real native custom/default/sidebar/panel activation, keyboard/AX/pixels, view-state identities, restart and safe-mode readback. |
| E13 | Rich content: unknown renderer loses data, copied asset grants another task access, offscreen media keeps resources, deleted/forked content lifetime wrong, old/mobile reader exposes secret payload. | CORE-14/15 plus IC owners; exact content/asset hashes, denied reads, old/current doc fixtures, native video seek/scroll/restart/disable and safe fallback. |
| E14 | Feature extraction: browser content becomes an app bridge; cookies cross isolation contexts; diff line/base/root mismatch; extension bypasses Git/dirty/unsaved checks. | CORE-16/17; existing browser/preview native fixtures and independent Git tree/index/ref/patch oracle, permissions, tab ownership and update/removal proof. |
| E15 | In-app creation: generated code self-enables, test success becomes permission, candidate changes after preview, missing toolchain goes unnoticed, example needs private LUNA source. | CORE-18; actual agent request, external project, public SDK build/test, exact reviewed artifact, declined/accepted enable receipts and final normal app behavior. |
| E16 | Updates/host/platform: active worker replaced, removed provider destroys history, irreversible schema rolled back silently, other host's package/grants used, core cannot recover with plugins disabled. | CORE-19/21/23; version-pinned runs, staged switch/rollback/crash/restart, independent two-host readback, actual platform results and stable core app hashes. |

## Existing proof to preserve and extend

These are current owners to inspect, not instructions to run every suite for
every ticket:

| Boundary | Current primary sources |
| --- | --- |
| Provider wires | `crates/harness/tests/codex.rs`, `claude.rs`, `opencode.rs`; permission suites and their distinct subprocess peers under `tests/fixtures`. |
| Admission and ownership | `crates/engine/tests/task_commands.rs`, `task_identity.rs`, `local_profiles.rs`, `restart_resume.rs`, `device_routing.rs`. |
| Timeline and private output | `crates/engine/tests/timeline_order_e2e.rs`, `tool_output.rs`; source contracts in `docs/reference/tool-output.md`. |
| Client watch lifetime | `crates/rpc/tests/subscription_lifecycle.rs`; transport/device-room tests. |
| Git and feature services | `crates/engine/tests/m5_repos_diffs_terminals.rs`, scoped diff/source-control suites; existing native browser/preview fixtures. |
| Board lifecycle | Current `board_*_e2e.rs`; preserve fixed chat parent, reviewer lifecycle, task identity and delivery receipts. |
| Native behavior | Existing UI fixture scripts and real-provider QA; `scripts/qa-task-work-live.mjs` demonstrates command/state/native/restart evidence. |
| Performance | `scripts/resource-profile.mjs`, `profile-composer.py`, native macOS resource/Metal probes and existing performance reports. |
| Audit context | `docs/audits/2026-10-09-test-audit/README.md` and relevant per-owner ledgers; their historical results are not fresh green baselines. |

The shared provider conformance runner adds the **external worker transport**
contract. It should consume retained provider scenarios instead of introducing
a duplicate copy of every parser or faking all providers with the same peer.
SDK tests exercise a published package in an external directory, not a test-only
adapter imported from inside the app crate.

## Before implementation

For the owning ticket, write the exact failures, current handler path, existing
keeper and independent readback. Record the baseline before changing it. When
moving a test or retiring a seam, identify its non-test callers, historical
purpose, remaining proof and affected CI targets. A large file or flaky name
alone is not evidence for deletion. Reproduce timing failures in isolation and
retain genuine unresolved defects.

Do not rewrite expectations to match a new implementation until the public
contract deliberately changes. Preserve seed data independent of the new code:
old snapshots, actual protocol recordings, known Git contents, denied grants and
source/bundle hashes. The implementation must not generate both sides of its own
expected-versus-actual comparison.

## Measurable optimization

CORE-00 records numerical acceptance budgets before broad refactoring. Use at
least a small ordinary workspace, a large repository, a long populated chat,
multiple open/working chats, a remote/offline client, and a low-end supported
hardware profile when available. Record power/display/foreground state, OS/GPU,
build flags, cache state and provider/extension versions. Historical M4/Metal
observations cannot stand in for current cross-platform results.

Measure cold and warm startup separately from first extension activation.
Report P50/P95/P99 event/input/paint latency with enough repetitions to show
variance; record peak and settled process/graphics footprint, CPU/wakeups,
worker/queue/subscription counts, frame stalls and cleanup after idle/disable.
Measure the total app plus workers, not only the UI PID. Plugin logic must not
run synchronously in a frame callback. Disabled unused extensions should have
no executing worker or active work subscription, while persisted bounded
metadata/cache is permitted and accounted for.

Use both absolute budgets and permitted relative deltas. If a security or
isolation boundary costs latency/memory, document the measured trade-off and
approved target instead of hiding it. Benchmark an alternative only when a
failed budget justifies it. Keep the existing Cargo/build-cache/disk policy;
do not introduce a new linker, remote cache or global build change as part of
routine refactoring.

## Repeatable artifact and delivery contract

Each owning E2E ends with: source/app/SDK/plugin hashes; exact commands and exit
codes; platform/mode/configuration; expected versus observed state; receipt/event
correlation; sanitized logs; required screenshots/video/AX; resource samples;
limitations; and a SHA-256 manifest. Evidence must identify whether it is a
deterministic peer, real provider, native view or physical-device observation.

Compile/check through `scripts/cargo.sh --locked` for changed packages, run the
relevant existing E2Es, configured format/Clippy/CI-equivalent targets, and verify
new public SDK packages independently. Run only one GUI app and respect the
shared build guard. After meaningful runtime changes, refresh LUNA Dev and
verify its exact process, IPC and strict signature. Do not force-quit unsaved work.

Hosted CI was blocked by GitHub billing in the queue's retained verification.
Recheck it when implementation begins; local green results and configured jobs
are distinct from an executed CI run. Lack of native/platform evidence blocks
that platform's certification, not the recording of a passing local contract.

The final CORE-23 report connects existing owned evidence and reruns the actual
user journeys. It cannot declare completion from interface inventories, package
installation receipts, model rows, generated screenshots or percentages alone.
