# Core and plugin implementation tickets

All CORE tickets start **Ready with unmet dependencies where applicable**.
Current status belongs to the shared queue. These are bounded deliverables,
not permission to combine all phases into one rewrite. Record failures before
code, extend the strongest existing E2E owner, preserve data and current behavior,
then deliver through the normal build/review/app/main workflow.

New package names mentioned below are proposals. Select module/crate boundaries
from actual dependencies and independently owned lifetimes. The detailed E01–E16
contracts and evidence requirements are in [verification.md](verification.md).

## CORE-00 — Freeze execution contracts and measured baselines

**P0; no dependencies. Owners:** architecture/QA, current performance scripts,
provider contract owners and the test-audit ledger. Use this source audit first.

Record the current runtime invariants, existing owners and exact scenarios used
to compare old/new paths. Measure cold/warm startup, idle/active work, long
transcripts, multiple chats and build/test cost on representative hardware.
Keep current regressions separate. Prototype only the high-risk decisions:
versioned subprocess handshake, bounded Wasm host call/cancellation, dynamic
native UI contribution and unavailable-plugin durable fallback. Pin the chosen
runtime/API envelope, platform trust policy and numerical budgets in an ADR.
SW-00 remains the owner of the broader actual provider/mode/workflow inventory.

**Failure focus:** a convenient demo becomes an unmeasured permanent architecture;
memory is attributed to the wrong process; a sandbox label has no enforcement.
**Gate:** E01/E02 baselines and prototype artifacts; no invented performance
numbers or untested runtime commitments. Hand off the selected interfaces and
specific unresolved constraints.

## CORE-01 — Publish canonical domain and operation contracts

**P0; CORE-00, SW-00. Owners:** `crates/proto`, RPC operation inventory, authority
and forwarding metadata; future schema/contract package.

Specify and implement versioned commands/queries/watches, target/principal
requirements, errors, receipts, subscriptions and limits. Separate stable public
DTOs from implementation state. Add namespaced extension/provider/contribution/
content identifiers and unknown-value compatibility rules; CORE-04 performs
the provider migration. Generate schema/operation metadata and coverage of
dispatch/policy declarations, without granting newly declared methods. Keep
legacy method spellings/receipts valid during migration.

**Failure focus:** different clients interpret IDs/errors differently; new
methods bypass policy; additive fields change historical payload hashes.
**Gate:** E03 cross-language/old-reader and method-authority evidence, independent
invalid inputs, unchanged existing command receipts. Publish the minimal
contract consumed by IC-0 and RM-00.

## CORE-02 — Reorganize test ownership around real boundaries

**P0; CORE-00. Owners:** affected engine/harness/UI/SDK/QA suites and CI routing.

Rebase the existing 9 October R/F/C/D ledgers. Assign every touched contract to
its strongest owner: domain command, adapter subprocess, client transport,
native UI or external extension conformance. Move retained tests/support only
when it clarifies that owner; remove or consolidate only with the full
test-audit evidence gate. Resolve named orphan/example and test-only production
path decisions. Separate deterministic fixture, live-provider and native-pixel
evidence; keep real provider-specific peers distinct. Publish runnable targets
and a baseline failure/quarantine ledger, with genuine regressions still blocking.

**Failure focus:** duplicate suites test themselves, a keeper disappears during
extraction, or timing/CI absence is mistaken for correctness.
**Gate:** E04 owned boundary scenarios and mutation/refusal proof for moved
contracts; retained evidence manifests and actual CI target mapping. This is
not a target to delete a percentage of tests.

## CORE-03 — Extract cohesive engine services and supervised lifetimes

**P0; CORE-01, CORE-02. Owners:** `EngineCore`, sessions, doc host, RPC dispatch,
write epochs, task identity, journals, recovery and existing domain services.

Move composition/bootstrap out of operation handlers. Extract bounded owners
for command admission, run coordination, event/journal projection and resource
lifetime. Use typed service ports and explicit start/stop/disposal/cancellation;
retain a single profile/store writer and the current durable authority. Keep
normal in-process operations efficient. A domain service should be testable and
headless without constructing the entire UI. Document shutdown order, backpressure,
lease/epoch behavior and cancellation of owned subprocesses/subscriptions.

**Failure focus:** a split moves globals but leaves competing writers; stale
handles mutate after shutdown; lost replies replay work; storage locks outlive
or underlive actual consumers.
**Gate:** E05 command/restart/race/ownership E2Es and focused existing keepers
pass through the public boundary before deleting replaced paths.

## CORE-04 — Make provider identity and discovery dynamic

**P0; CORE-01, CORE-03. Owners:** proto persisted provider IDs, harness registry,
model/account/capability catalogs, preferences and picker projections.

Replace the public closed provider enum with stable identifiers and legacy
aliases; preserve old task/account/session bindings and retired-provider reads.
Keep built-in adapters behind compatibility registrations initially. Advertise
typed capabilities/options, generation and availability from the authoritative
host. Move provider facts out of scattered UI name checks. Add registry change
subscriptions so install/enable/remove updates the existing picker. CORE-08
connects a real separately installed provider; SW-01 owns the effective-tool UI.

**Failure focus:** unknown providers make old history undecodable; provider
updates select another account; stale model responses enter the wrong chat.
**Gate:** E06 migration/unknown-provider/catalog-generation proof and unchanged
existing provider behavior, including missing/retired providers after restart.

## CORE-05 — Deliver typed SDKs and a unified CLI client

**P0; CORE-01, CORE-03. Owners:** `luna-rpc`, `apps/luna`, UI client state and
proposed public SDK packages.

Generate a typed Rust client, JSON schemas and TypeScript SDK from CORE-01.
Provide watch disposal/cursor repair, typed errors, cancellation, target
selection and durable receipt lookup. Expand CLI discovery/read/watch/command
operations through the same services. Separate app bootstrap from normal
view-model clients; migrate one complete user operation across app/CLI/SDK to
prove parity. Preserve existing inspect JSON output and scripts. Supply explicit
SDK/API versions and minimal external examples with no private engine imports.

**Failure focus:** CLI bypasses app permission checks; SDK success means only
queued; stale subscriptions leak; a generated client accepts invalid runtime JSON.
**Gate:** E03/E07 drive identical allowed/denied operations through all clients
and independently compare receipts, events and resulting state.

## CORE-06 — Add extension manifests, installation and explicit grants

**P1; CORE-01, CORE-03. Owners:** new extension manager using profile/configuration,
versioned package store and existing owned update/credential primitives.

Define/package IDs, hashes, API/platform/runtime compatibility, contributions,
dependencies, settings/data schemas and trust class. Implement local staged
validation/install, disabled-by-default candidates, explicit enable/disable and
grant persistence. Validate archive paths, dependency closure and namespace
conflicts. Expose exact package provenance/state through SDK/CLI and a minimal
protected management route. Do not require a marketplace. Built-in migration
preserves the user's existing enablement and permissions.

**Failure focus:** malformed packages escape their root, installation executes
code, settings sync implies grants, or a required incompatible dependency loads.
**Gate:** E08 valid/invalid package and grant lifecycle using actual package
bytes, independent file readback and crash/restart recovery.

## CORE-07 — Implement supervised extension execution and broker

**P1; CORE-06. Owners:** extension runtime, process supervision, capability broker
and platform enforcement chosen in CORE-00.

Implement the selected bounded Wasm path and native provider-worker transport.
Handshake API/version/hash/capabilities before activation. Bind plugin instance,
version, user grant and exact target on the server; expose no local-owner root
connection. Meter memory/CPU/queues/host calls/logs, cancel or quarantine runaway
instances, and preserve unrelated work after crash. State platform limitations
for trusted native workers accurately. Keep credentials in the owning host and
broker scoped handles only where the selected operation requires them.

**Failure focus:** fake capabilities authorize writes, blocking imports evade
timeouts, native code is mislabeled sandboxed, or a crashed extension crashes UI.
**Gate:** E09 denial, saturation, hang, crash, cancellation and restart at the
actual host boundary, with process/resource/grant readback.

## CORE-08 — Prove one independently installed provider end to end

**P1; CORE-04, CORE-05, CORE-07. Owners:** provider extension API, registry and
existing picker/run/input/projection path.

Implement the minimal and optional provider profiles from architecture.md.
Create an extension outside the app repository using only the SDK and a real
selected CLI protocol. Validate/install/enable it, discover models/options and
complete a real task through the ordinary picker. Prove Stop, failure and the
supported continuation semantics; explicitly omit unsupported optional profiles.
Retain exact native input/result identity. Verify the installed app hash never
changed while the new provider was added.

**Failure focus:** only a model row appears; execution still needs a compiled
provider ID; the generic bridge invents acceptance, authority or completion.
**Gate:** E10's full no-app-update journey plus conformance failures and native
evidence. This is the first plugin milestone, before extracting all providers.

## CORE-09 — Extract Codex as a first-party provider package

**P1; CORE-08, CORE-02. Owners:** Codex harness/native client, provider-specific
catalog/account/update/policy facts and retained Codex E2Es.

Move its implementation into an independently packaged worker using the public
provider/broker contract. Preserve app-server wire behavior, approvals, exact
steer delivery, history/fork/rewind, accounting, tool/media/native child identity
and existing permission limitations. Keep native credentials/settings ownership.
Port retained provider-specific peers to the real worker transport and prove
native fresh/resumed tasks. Remove the old registration only after parity passes.

**Gate:** E11 Codex matrix, old-chat migration and pinned-version/update failure
proof; no provider-only private shortcut remains in the core or picker.

## CORE-10 — Extract Claude Code as a first-party provider package

**P1; CORE-08, CORE-02. Owners:** Claude harness, native extensions, login/account
facts, MCP/tool delivery and retained Claude E2Es.

Preserve stream-json event correlation, Ask/Auto/Full ceilings, real account
eligibility, exact writer/resume ownership, questions, goals, history, images
and native extension scope. Consume SW-02/03 fixes/evidence when available;
otherwise retain their visible limitations as open work, never certify missing
capabilities by renaming them. Pin worker/CLI compatibility and external config
provenance. Resolve auxiliary process cwd/authority through its actual owner.

**Gate:** E11 Claude matrix and current native behaviors through the package,
with refusal modes and old conversations preserved. A failed/unsupported Auto
workflow remains explicit rather than weakening confinement.

## CORE-11 — Extract OpenCode as a first-party provider package

**P1; CORE-08, CORE-02. Owners:** OpenCode 2 HTTP/SSE adapter, child/permission/
delivery reconciliation and retained OpenCode peers.

Preserve actual OpenCode 2 protocol, account/model variant source, session
permission readback, SSE ordering, child ownership and uncertain delivery rules.
Do not bring back retired OpenCode 1 or ACP behavior. Native server lifecycle,
port ownership, restart and package update belong to the supervised worker;
engine command acceptance remains core-owned.

**Gate:** E11 OpenCode matrix, real native run/steer/stop/reconnect, malformed/
late events and unchanged old task identity with independently observed receipts.

## CORE-12 — Introduce native UI contribution and view-model contracts

**P1; CORE-01, CORE-03, CORE-05, CORE-06, CORE-07. Owners:** UI shell/pickers/
settings/workbench, native platform host and extension registry projections.

Extract shell state and domain view models from rendering. Register stable
command/menu/keybinding/settings/sidebar/panel contributions and host actions.
Migrate selected current built-ins to that interface before accepting external
ones. Expose authorized data and declarative native widgets without GPUI
entities or synchronous plugin work on the render thread. Preserve stable
selection, navigation, focus, drafts, AX and protected recovery/permission UI.

**Failure focus:** removing a plugin loses navigation state; callbacks run
against a new tab; extension UI blocks the frame loop or hides approval controls.
**Gate:** E12 real native composition and removal/restart/refusal evidence,
including default contributions and unknown IDs in saved layouts.

## CORE-13 — Ship custom appearance and alternate-sidebar examples

**P1; CORE-12. Owners:** public UI contribution SDK, `luna-theme`, settings and
sidebar projection/action owner.

Create independently installable examples for a theme/layout and an alternate
sidebar presentation. Exercise grouping/order/badges/actions from host-owned
data. Reuse theme import/tokens and existing preference scope; do not add a new
theme format or second task model. Preview and explicit enable/disable must
work without app update, retaining a reachable known-good sidebar.

**Gate:** E12 no-app-update, keyboard/AX/narrow/reduced-motion and selection/draft
proof; malformed themes/views, excessive update rates and removal fail safely.
AW-01/02/03/06/08/15 consume this contribution boundary for their own UX scope.

## CORE-14 — Add generic durable content and renderer registration

**P1; CORE-01, CORE-03, CORE-12, IC-0. Owners:** proto/doc content envelopes,
host assets, transcript projection and public renderer registry.

Implement namespaced/versioned typed content references with stable part IDs,
owner/asset scope, bounded metadata and privacy-safe fallback. Register/resolve
renderers without changing the core enum for each future media type. Define
unknown/disabled renderer, fork/delete/rollback/archive/restart/sync behavior.
Use IC-0's shared ownership contract. IC-1 owns the concrete immutable HTML
service and isolated renderer; SW-09 consumes these same references for tool media.

**Failure focus:** older readers lose a whole message, unknown content leaks
private payload, late rendering moves rows, or a plugin obtains a path grant.
**Gate:** E13 current/older-reader round trips and actual native projection,
cross-task denial, resource lifetime and unavailable-owner fallback.

## CORE-15 — Prove an independently installed video renderer

**P1; CORE-14, IC-1, CORE-07. Owners:** public content SDK and isolated rendering/
media primitives, with core asset authorization unchanged.

Build a plugin that recognizes declared video content, plays/seeks authorized
media and releases playback/decoding resources outside its visible lifecycle.
Reuse the IC isolated surface or the CORE-00-proven host media primitive; do not
introduce another general WebView or unrestricted host bridge. Preserve controls,
accessibility, bounded reads, offline state, export and fallback after disable.

**Gate:** E13 native playback/seek/scroll/restart/disable on unchanged app bytes,
with media hashes, independent frame/action readback and resource measurements.
Unsupported formats/platforms produce an explicit usable fallback.

## CORE-16 — Extract browser product behavior as a feature package

**P2; CORE-12, CORE-07. Owners:** browser chrome/model/session contribution,
native browser service and existing browser/preview fixtures.

Move browser commands, UI composition and product state behind the public
feature contract. Keep WebKit/WebKitGTK ownership, main-thread attachment,
native permissions and managed lifetimes in a narrow platform service. Preserve
host routing, tabs, keyboard, downloads/navigation and ordinary-browser storage.
Keep this authority/storage separate from offline HTML/MCP Apps surfaces.

**Gate:** E14 existing native browser gates plus disable/re-enable/version/focus/
host tests on the extracted path; no undocumented internal-shell imports.

## CORE-17 — Extract diff and source-control workbench behavior

**P2; CORE-12, CORE-03, ZI-01. Owners:** changes/diff/Git UI and domain operations,
snapshot lifetime, workspace and command-authority services.

Package diff/review presentation and Git workflow contributions around typed
host-owned checkout/snapshot operations. Keep actual file/process/Git mutations
behind core permission, target and freshness checks. Reuse ZI-01's bounded private
snapshot implementation. Preserve staged/unstaged/latest-turn/base/PR distinctions,
line coordinates, unsaved files and real remote readback.

**Gate:** E14 independent Git oracle plus native line navigation and disable/
update/restart tests. An extension cannot forge a checkout, bypass dirty-state
checks or mutate Git by pretending a review preview authorized it.

## CORE-18 — Build, validate and enable plugins from inside LUNA

**P1; CORE-05, CORE-08, CORE-12, CORE-14. Owners:** SDK developer tools, app
extension-development workflow and existing agent tool/runtime surface.

Provide public templates and a CLI/dev-host path for provider, command, sidebar
and renderer packages. A LUNA agent creates code in a scoped workspace, runs
conformance checks, previews a candidate and produces a hashed package. Show
required toolchains, declared grants, provenance and actual test limitations.
User enablement activates the exact reviewed candidate; neither a model reply
nor a passing test can self-authorize activation or publishing.

**Gate:** E15 actual in-app request → generated external project → CLI tests →
preview → explicit enable → normal-picker/provider or UI use, with no app rebuild.
Exercise declined enablement, changed package bytes and failing conformance.

## CORE-19 — Complete updates, rollback, removal and safe mode

**P1; CORE-06, CORE-07. Owners:** package manager, supervisor, settings/data
migrations, native recovery entry and SDK/CLI management.

Stage/verify updates; pin active runs; drain or deliberately stop before
switching. Reconcile unknown provider outcomes instead of replay. Handle
dependency/API conflicts and expanded grants. Keep explicit plugin data migration/
retention/removal and rollback compatibility. Quarantine repeated crashes and
offer a protected safe mode with optional extensions disabled. Preserve the
default shipped packages and offline operation without a marketplace.

**Gate:** E08/E09/E16 power-loss/failed-update/active-run/rollback/safe-mode
scenarios with independent package, process, grant and durable-state readback.

## CORE-20 — Certify performance and resource lifetime

**P1; CORE-22. Owners:** current performance harnesses, extension supervisor,
native UI, client transports, storage and build metrics.

Repeat CORE-00 workloads against the fully extracted app on the same hardware/
data/providers. Measure core plus workers: startup/idle/first-use, event-to-paint
latency, P95/P99 input/scroll, CPU, process/graphics footprint, wakeups, queues,
plugin activation, watchers, shutdown and incremental build/test time. Attribute
regressions and repair the owner. Optional disabled features must not keep active
workers/subscriptions or expensive discovery on startup. Publish exact deltas
against the recorded numerical budgets, including accepted trade-offs.

**Gate:** E02 reproducible logs/traces/hashes and actual native scenarios pass.
No “optimized” claim follows solely from fewer files, less Rust code or moving
memory into another process.

**Inherited from CORE-03** ([contract](core-03/contract.md#8-milestones)):
- Watch streams of a stopped engine keep serving its last state until the
  client drops them. Cancelling them on close needs a UI retry/backoff review
  first.
- Provider events into the engine are unbounded per run.

Both need measured limits before a bound is chosen.

## CORE-21 — Verify host, platform and client compatibility

**P1; CORE-05, CORE-08, CORE-12, CORE-14, CORE-19. Owners:** headless/client/RPC,
device routing, remote auth, native platform adapters and current mobile readers.

Prove that execution-host packages/accounts and viewing-client presentation
packages are distinct. Exercise two hosts with different extension versions,
missing renderers and different capabilities. Preserve existing transport scopes;
unsupported direct task operations stay refused until RM supplies them. Verify
macOS/Linux/Windows supported paths and explicit fallbacks, plus old/mobile
reader compatibility. No downloaded native iOS plugin runtime is promised.
RM-03 consumes this SDK; it retains ownership of the direct native phone client.

**Gate:** E03/E07/E16 compatibility and authority evidence, actual supported
platform builds/native tests, and honest unavailable paths. Missing CI/platform
access is recorded as a certification blocker, not inferred success.

## CORE-22 — Retire replaced internals and lock dependency boundaries

**P1; CORE-09/10/11/13/15/16/17/21. Owners:** application composition root,
engine/UI dependency graph, adapter packages and their test owners.

Remove superseded registration, compatibility branches and private imports only
after current/old-data acceptance proves they are no longer needed. Preserve
wire aliases/readers that still have supported consumers. Split remaining large
files by domain and lifetime, not arbitrary line count. Prove independent package
builds against published SDK artifacts without private source access; keep the
core free of dependencies on concrete provider/browser/diff product packages.
Update current architecture/reference docs and targeted build/test routing.

**Gate:** E04/E05/E11/E12/E14 keepers and dependency/artifact evidence. No orphan
test seam, hidden privileged built-in path or lost default feature is accepted.

## CORE-23 — Certify the integrated extensible application

**P1; CORE-18, CORE-19, CORE-20, CORE-21, CORE-22. Owner:** release acceptance;
reuse existing feature proof and execute the final user journeys.

Start from a delivered signed app and representative old data. Use built-in
provider packages, create/install/enable a new external provider, customize the
sidebar, play plugin video and use extracted browser/diff features. Update/disable/
rollback a package during controlled active/idle scenarios, restart, and inspect
history/default recovery. Compare app/bundle hashes before and after plugin-only
changes. Assemble exact compatibility/performance/permission evidence and remaining
limitations. Retain ordinary LUNA Dev handoff and verified main delivery.

**Gate:** all required E01–E16 owners pass; user journeys use public SDKs and real
native behavior, no special build or hidden private registration. This closes
the core/plugin program, not unrelated deferred marketplace or mobile products.
