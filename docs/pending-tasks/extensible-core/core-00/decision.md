# ADR CORE-00: bounded extension execution and host-owned native composition

Status: accepted for the implementation sequence; measurements and certification
limits are recorded in `results.md`. This settles the feasibility choices, not
the final public DTOs or a production plugin host. CORE-01 versions those DTOs;
CORE-06/07 implements and hardens the host. Retire these prototypes after their
contracts have a stronger production owner; do not ship their fixture adapters.

## Execution envelope

Keep authority, storage, command admission, identity, journals, permissions and
recovery in the core. Share that service boundary with UI, CLI and SDK clients.
An extension receives scoped broker operations, never the owner's IPC credential
or a pointer to `EngineCore`. Transport negotiation grants no permissions.

Choose a subprocess worker protocol with a major/minor handshake, explicit
required capabilities and correlated request IDs. The first framing profile is
UTF-8 JSON lines, bounded **before** decoding to 64 KiB per control frame; bulk
assets use host-issued references instead. Handshake timeout is 1 second, followed
by graceful child termination and bounded reap/escalation. Drain stderr while
retaining a bounded log window. CORE-06 must also bound concurrent requests,
output/event queues and child process trees; this handshake probe does not claim
to implement the entire supervisor. Unsupported majors fail closed; optional
minor additions must not silently satisfy a missing required capability.

Choose **Wasmtime 49.0.2**, pinned in the isolated probe lockfile, for the first
desktop portable worker. Host operations are asynchronous, cancellable futures;
the host owns each resource lease and any external effect's commit boundary.
Use fuel with periodic async yields (probe: 10,000 fuel interval), a 50 ms call
deadline and a 16 MiB guest linear-memory ceiling. Running guest code and blocked
host imports require different cancellation mechanisms. Dropping a future does
not undo an external operation already committed: CORE-06 must define idempotency
and commit semantics for each broker operation.

The initial portable profile is **core WebAssembly modules with explicitly
versioned LUNA imports; no WASI imports**. Component-model/WASI support is not
required for provider workers, which can use the native protocol. Do not commit
to a WASI version on the strength of a core-module probe. Introducing components
or WASI later requires a versioned profile and its own capability/cancellation
evidence. Wasmtime stays outside the UI process and outside today's production
dependency graph. The probe's small guests do not size a real provider SDK.

Wasmtime documents why epochs/fuel alone do not interrupt a blocking host call;
the host must use asynchronous calls and own cancellation. The current runtime
also exposes store resource limiters. References: [interruption](https://docs.wasmtime.dev/examples-interrupting-wasm.html),
[store limits](https://docs.rs/wasmtime/49.0.2/wasmtime/struct.Store.html),
[async imports](https://docs.rs/wasmtime/49.0.2/wasmtime/struct.Linker.html#method.func_wrap_async).

## Trust and platform policy

| Execution class | Initial policy |
| --- | --- |
| Data-only themes/layout declarations | Validate bounded schema and assets; no executable worker. |
| Wasm desktop workers | No ambient WASI filesystem/network/environment imports. Expose only reviewed broker capabilities. Runtime sandbox is not a substitute for scoped imports or an external-effect policy. |
| Native macOS/Linux/Windows workers | Explicitly trusted user-level code. Process supervision limits protocol/resource behavior but does **not** claim OS filesystem/network confinement. An OS sandbox label requires a separate platform implementation and evidence. |
| iOS client | Declarative supported contributions and safe fallback; execution stays on the authoritative host. This decision does not introduce third-party native/JIT code on iOS. |

Package the first external provider as a local `.luna-plugin` directory containing
a version-1 JSON manifest plus a pinned worker executable
(per supported OS/architecture) or Wasm module, assets and content hashes. The
manifest declares protocol/profile, provider ID, contributions and requested
capabilities. Keep install, review and enable distinct; generated code cannot
enable itself. CORE-07 owns filesystem-safe installation, integrity, rollback,
trust UI and staged updates. No marketplace, signature authority or cross-platform
binary compatibility is inferred from these probes.

## Native UI and durable content

Choose bounded data-only native contribution descriptions: stable namespaced ID,
kind, label/icon, location, host-issued action and required capability. The first
primitives are command, sidebar item, panel, settings group and theme references.
The host constructs native GPUI elements and owns selection, drafts, focus,
accessibility, lifecycle and protected permission/recovery surfaces. Never call
extension logic synchronously from paint. Arbitrary native widget code is outside
this envelope; CORE-12 adds the real registry and lifecycle, with interaction/AX
proof beyond this offscreen composition probe.

Version a content envelope with renderer ID/version, an approved safe fallback,
opaque payload and scoped asset references. Unknown/disabled renderers show only
the safe fallback; absence must not rewrite or delete opaque content. The probe
reads an independently authored durable file across fresh processes and renders
fallback without forwarding payload to the view. Production Loro migration,
asset permissions, old-reader privacy and rich media lifetime remain CORE-14/15.

## Budgets and how to compare

Pre-execution feasibility ceilings are in `failures.md`: 1,000 ms handshake,
64 KiB control frame, 50 ms Wasm deadline / 150 ms maximum observed return,
16 MiB guest memory, zero live host calls or late effects after cancellation,
and 16.7 ms P95 native contribution update-to-draw after warmup. Report misses;
do not tune ceilings after seeing a result. Guest memory excludes runtime/code
and native worker memory, which must be measured separately.

For migration, replay the retained workload on the same machine/build profile:
startup, RPC and native draw P95 must stay within **10% or 1 ms**, whichever is
larger, of a fresh matching baseline; settled aggregate footprint within **10%
or 5 MiB**. These are regression gates, not universal device performance claims.
Absolute local objectives: warm engine readiness P95 under 1,000 ms, ordinary
command receipt P95 under 50 ms, native contribution update-to-draw P95 under
16.7 ms, and idle CPU below 1% of one core after settling. Baseline misses are
optimization work, not grounds to redefine correctness or hide measurements.

Use nearest-rank P50/P95/P99 with sample count and maximum; small-N P99 is simply
an observed maximum and does not estimate the tail reliably. First fresh-profile
launch is not OS-cold startup: no caches were purged. RPC completion and offscreen
draw CPU time are separate from compositor event-to-paint latency. CORE-20 owns
that instrumentation, disabled-worker aggregate cost, low-end and other-platform
budgets; SW-00 owns live provider timing. Do not certify unmeasured paths.
