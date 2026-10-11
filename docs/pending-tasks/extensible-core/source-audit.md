# Current-source audit

Source cut: `2838125e42646b5d5ba86a3f8dfbdd93922ec60d`, 11 October 2026.
The inventory covers tracked owned Rust, TypeScript/TSX, Swift, Python, MJS and
shell source under `crates`, the LUNA apps, iOS, edge and scripts. Vendor code is
excluded. It counts **989 files and 721,372 physical lines**, including inline
tests, fixtures and comments. These numbers measure concentration, not code
quality or execution cost. [Inventory summary](evidence/inventory.json) and
[per-file hashes](evidence/files.jsonl) make the source cut checkable.

This was a repository and selected-path architecture investigation, not a
line-by-line review of every file or a new whole-repository test sweep. The
installed Dev CLI's read-only status call to port 59777 returned connection
refused. [Readback](evidence/live-inspect.json) records that limit; no app was
launched/refreshed and no live plugin or performance result is claimed.

## Concentration and ownership

| File | Physical lines | Why its boundary matters |
| --- | ---: | --- |
| `crates/ui/src/shell.rs` | 21,594 | Routes, sidebar/workbench composition, panels, navigation and tests. |
| `crates/ui/src/composer.rs` | 18,132 | Draft/input presentation, submission, provider-specific affordances and tests. |
| `crates/engine/src/sessions.rs` | 16,591 | Run lifecycle, native provider behavior, journal/event processing, recovery and tests. |
| `crates/ui/src/transcript.rs` | 16,283 | Projection, virtualized rendering, interactions, media and tests. |
| `crates/engine/src/rpc.rs` | 13,640 | Transport dispatch, operation admission and cross-domain orchestration. |
| `crates/ui/src/pickers.rs` | 10,836 | Provider/model/default/account choices, direct engine knowledge and tests. |
| `crates/engine/src/doc_host.rs` | 9,896 | Durable projections, command drain, replay and storage coordination. |

For example, the main test modules begin at shell line 16,433, composer 14,330,
sessions 11,037 and transcript 11,534, with additional modules elsewhere.
Splitting tests out would improve navigation but would not by itself remove
runtime coupling. Extract use cases and lifecycle owners, then put each
contract's retained tests beside its strongest public boundary.

## Findings and required change

**F01 — Provider identity is closed at the durable boundary.**
[`HarnessId`](../../../crates/proto/src/agent.rs#L7) is a serde enum, including
retired wire values. The harness trait returns it
([trait](../../../crates/harness/src/lib.rs#L235)); the
[registry](../../../crates/engine/src/registry.rs#L637) constructs the three
built-in implementations at compile time. A separately installed provider
cannot merely register an arbitrary stable ID. The inventory finds 1,744
`HarnessId::` mentions across 255 source files, including tests; this is an
impact index, not 1,744 independent defects. Introduce stable namespaced IDs,
legacy aliases and unknown-provider read behavior before extracting adapters.
The text index omits type-only usages and persisted external records; CORE-04
must trace all readers and migration formats, not treat the count as a checklist.

**F02 — A provider is more than a run callback.**
The current `Harness` contract already covers models, steering, native history,
fork, rewind, compaction, native extension inventory and exact recovery behavior.
Preserve this investment. Remaining policy is spread into
[permissions](../../../crates/engine/src/permissions.rs#L14),
[account handling](../../../crates/engine/src/agent_accounts.rs#L382),
[model source signatures](../../../crates/engine/src/model_cache.rs#L36),
provider updates and runtime tool delivery. UI
[pickers](../../../crates/ui/src/pickers.rs#L334) call engine policy directly,
and at line 8,787 call a Claude-specific model fallback. A useful provider SDK
must move provider facts/operations behind capabilities while keeping permission
decisions and command admission in LUNA.

**F03 — Typed domain objects exist, but a public SDK is missing.**
There is no repository-owned distributable SDK package in the inspected workspace
manifests. [`RpcClient::call`](../../../crates/rpc/src/client.rs#L179) still takes
a method string and JSON params; `call_as` types only deserialization of the
result. Method constants and `ALL` are maintained in
[`luna-rpc`](../../../crates/rpc/src/lib.rs#L37), while dispatch, forwarding and
remote policy live elsewhere. The recent audit found actual inventory drift
for automation methods. A schema-backed operation catalog should generate
clients, capability metadata and policy coverage while handlers remain owned
domain code. Runtime validation remains necessary for non-Rust clients.

**F04 — CLI and app can share a real service boundary.**
The [inspect CLI](../../../apps/luna/src/inspect_cli.rs#L1) already reads the
running engine rather than opening stores, and memory/WebSocket RPC use the
same framing. Preserve that path. The current
[CLI command enum](../../../apps/luna/src/main.rs#L37) supplies inspect/admin
operations, not a full public extension authoring/control SDK. The
[UI manifest](../../../crates/ui/Cargo.toml#L230) directly depends on engine and
harness, and [`EngineHandle`](../../../crates/ui/src/state.rs#L408) mixes
bootstrap/attachment responsibilities with client state. Keep bootstrap in a
composition root and make ordinary views depend on domain projections/clients.

**F05 — Durable authority must survive the refactor.**
[`EngineCore`](../../../crates/engine/src/lib.rs#L303) assembles many concrete
services but already owns profile/instance-lock lifetime. The device-write
handle is wired into multiple services at assembly, despite the stale
“product writers are unwired” comment in
[`device_write.rs`](../../../crates/engine/src/device_write.rs#L6). Do not infer
an unwired system from that comment or delete it as test-only. Existing
[task command receipts](../../../crates/proto/src/task_command.rs),
[engine-owned intent checks](../../../crates/engine/src/rpc.rs#L10361), journal
ownership and write epochs are preservation contracts. Break up orchestration
around them, with one owner per resource and explicit shutdown/cancellation.

**F06 — Plugin authority needs its own principal.**
The [remote guard](../../../crates/rpc/src/remote_auth.rs#L432) distinguishes
paired device scopes and local-only methods. A plugin must not reuse a root
owner connection or select an actor in a JSON payload. Add a broker with
server-owned plugin/version/instance/target/grant context and intersect it with
existing command authority. Process separation alone does not limit a native
program's OS permissions; the runtime/trust design must say exactly what is
enforced.

**F07 — Shell composition is compiled in.**
[`SidebarPage` / `SidebarNavItem`](../../../crates/ui/src/shell.rs#L613) enumerate
Automations and Board, with a fixed `ALL`. The
[`RightSurface`](../../../crates/ui/src/shell.rs#L699) enum explicitly lists
Files, Browser, Diff, Terminal and other surfaces. These need stable contribution
IDs and lifecycle/view-model contracts to support add-on or alternate sidebar
presentations. Plugins should receive authorized projections/actions, not GPUI
entities or mutable access to the entire shell.

**F08 — Browser policy and native mechanics have different owners.**
[`BrowserSurface`](../../../crates/ui/src/browser/mod.rs#L82) owns chrome, page
state, subscriptions and native page handles. The
[macOS backend](../../../crates/ui/src/browser/macos.rs) owns WebKit/OS objects;
Linux uses a separate native helper. Extract browser product behavior and
contributions, retaining a narrow supported native-view/platform service.
The ordinary browser's cookie/navigation policy must remain separate from the
isolated HTML/MCP Apps renderer already planned in IC.

**F09 — Rich chat content needs a durable extension envelope.**
[`MessagePart`](../../../crates/doc/src/parts.rs#L381) has explicit variants for
text, image, reasoning, tools, questions and errors. It has useful backward-reader
privacy conventions and lazy output references. A video or arbitrary plugin
renderer needs a namespaced/versioned content reference, fallback and bounded
payload/asset lifecycle. Do not serialize a plugin's live DOM or raw local paths
into synced conversation state. Reuse the IC/SW asset contracts and the existing
[tool output privacy policy](../../reference/tool-output.md).

**F10 — Some extension-friendly work already exists.**
[`luna-theme`](../../../crates/theme/src/lib.rs#L1) separates imported formats
from complete resolved themes and supports a custom family registry. Keep it.
Current [`NativeExtensionsView`](../../../crates/proto/src/native_extensions.rs#L135)
describes provider-owned settings/hooks/commands; it is not an application
plugin loader. MCP servers, provider-native plugins, LUNA application extensions
and declarative themes need distinct owners and labels, with bridges where
explicitly supported.

**F11 — Test cleanup must build on the recent campaign.**
The [9 October audit](../../audits/2026-10-09-test-audit/README.md) records 3,428
reviewed units, 161 deletions, 79 consolidations and preserved contracts. Those
are historical results, not rerun totals from this planning pass. Existing
provider subprocess peers, engine command/timeline suites, RPC subscription
tests and native fixtures are valuable extraction gates. Outstanding audit
items include test-only production paths needing a product decision and examples
without CI routing. Assign those named items; do not launch another indiscriminate
deletion campaign or replace three different provider peers with one generic fake.

Fresh targeted caller reads distinguish three concrete cleanup decisions:

- [`integration_actions.rs`](../../../crates/engine/src/integration_actions.rs#L1)
  explicitly has no registered production owner driver; targeted engine callers
  show its module declaration and internal tests. CORE-02 decides whether a real
  integration owner adopts its receipt contract or retires it with remaining proof.
- [`persist_unwired_interrupt_with_id`](../../../crates/engine/src/doc_host.rs#L3954)
  has targeted call sites in the module's test section. CORE-02/03 must resolve
  this prospective path against the actual task-work/Stop owner before retaining
  or deleting it. Its existence does not prove a delivered interrupt operation.
- [`UiConfig::synthetic_core`](../../../crates/ui/src/lib.rs#L189) has an actual
  caller in [`native_fixture.rs`](../../../apps/luna/src/native_fixture.rs#L287).
  It supports packaged native QA; it is not safe to delete as an unused test seam.
  Rehome it behind the explicit bootstrap/dev-host boundary as that boundary moves.

These are planning findings, not completed full deletion ledgers. Any removal
still requires caller/history/keeper evidence and the test-audit gate.

**F12 — Optimization needs current measurements.**
The [macOS report](../../performance-macos.md) and
[idle-presence analysis](../../performance-idle-presence.md) already distinguish
row invalidation, native GPU work, process footprint and driver attribution.
File size and RSS alone cannot prove an improvement. Extension idle/startup,
IPC, rendering and resource lifetime must be measured against the same current
build/workloads. Keep existing lazy catalogs, bounded streams, virtualized rows,
build cache and process ownership unless a measured replacement improves them.

## Readiness conclusion

The desired product is feasible as a staged architecture program. The decisive
missing pieces are public/versioned identity and service contracts, a supervised
extension runtime, dynamic UI/content contributions and a complete lifecycle
for independently installed packages. Reformatting or splitting the large
files alone cannot supply these capabilities. Neither a native Rust `dylib`
interface nor an unrestricted JavaScript hook into shell state is a suitable
default public API. The proposed architecture and its executable proof gates
are in the following documents.
