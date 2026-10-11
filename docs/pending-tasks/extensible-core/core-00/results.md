# CORE-00 measured results

11 October 2026, M3 Max / 36 GiB / macOS 27.0.1, AC power. Root workspace dev
profile; existing Cargo/OS caches retained. Source baseline `ffdf7b83`; prototype
source and executable hashes are in [sha256.json](evidence/sha256.json).
These are local feasibility and comparison baselines, not platform certification
or a shipped plugin system. See [the ADR](decision.md) and
[baseline failures](baseline-failures.md).

## Contracts and feasibility

- **65 existing boundary tests passed:** 58 engine ownership/command/restart/
  timeline tests and 7 RPC subscription-lifecycle tests. One separate real-Claude
  restart test remained explicitly ignored. [Engine](evidence/keeper-results.txt),
  [RPC](evidence/rpc-results.txt).
- **39 subprocess handshakes passed:** compatible minor negotiation, unsupported
  major/capability, malformed/oversized frames, wrong request ID, EOF, timeout,
  bounded stderr draining and 30 recovery launches. Every child was reaped.
  [Raw receipts](evidence/worker.json).
- **80 Wasmtime calls passed**, including 20 infinite loops and 20 asynchronous
  host waits stopped by the host deadline. All 20 waits actually entered; zero
  live host calls or late effects remained. Memory growth beyond 16 MiB and an
  unavailable WASI import were refused, and a later guest returned 42.
  [Raw samples](evidence/wasm.json).
- **Dynamic native composition and fallback passed in two fresh processes.**
  The host read a declaration after startup, added/removed its sidebar item,
  preserved selected task/draft and restored the original pixels on removal.
  The same opaque durable envelope hash survived both processes; only its
  fallback was sent to the view. An arbitrary callback declaration was refused.
  [Enabled](evidence/native-enabled.png), [removed](evidence/native-disabled.png),
  [receipts](evidence/native.json), [restart](evidence/native-restart.json).

## Timing and memory

Nearest-rank percentiles, milliseconds. Raw measurements and derived statistics
are retained in [metrics.json](evidence/metrics.json), with no outlier deletion.
For small N, P99 is merely the observed maximum; it is not a reliable tail SLA.

| Boundary | N | P50 | P95 | P99 / max |
| --- | ---: | ---: | ---: | ---: |
| Repeated empty-profile engine process readiness | 9 | 31.75 | 32.40 | 32.40 |
| Canonical task-command RPC response | 67 | 3.99 | 6.89 | 44.42 |
| Transcript reset RPC response | 285 | 3.42 | 11.89 | 40.29 / 65.03 |
| New worker process + handshake + reap | 30 | 27.00 | 31.73 | 34.86 |
| Wasm infinite-loop deadline return | 20 | 50.98 | 51.00 | 51.01 |
| Wasm pending-host-call deadline return | 20 | 57.98 | 61.27 | 61.82 |
| Native contribution update-to-draw, after three transitions | 40 | 0.074 | 0.183 | 0.206 |
| Current Shell active scroll draw-loop CPU wall time | 240 | 8.94 | 9.83 | 10.77 / 14.46 |
| Current Shell idle loop CPU wall time | 806 | 0.014 | 0.052 | 1.62 / 8.59 |

First fresh-profile engine readiness was **74.85 ms**, with OS caches uncontrolled.
The Wasmtime probe's first module compile took **25.96 ms**; its process peak RSS
was **16.91 MiB**, peak charged footprint **3.91 MiB**. These tiny guests do not
predict a real provider's compiled module or SDK footprint. [Process receipt](evidence/wasm-process.txt).

The real engine workload created 40 small-project chats, requested five
concurrent mock runs and independently observed **two simultaneously Working**,
then accumulated 20 long-text turns. Its selected transcript contained **42
entries / 171,734 serialized bytes** and compared equal after engine restart.
Another task registered and ran in the current large source checkout without
executing a shell command. This last scenario measures registration/run behavior,
not Git diff/search performance. [Receipts, process samples and clean shutdowns](evidence/engine.json),
[overlap](evidence/engine-concurrency.json), [replay input](evidence/frames.json).

Charged aggregate footprint peaked at **397.58 MiB** during the initial idle
window, which still included provider discovery children; it ended at 41.08 MiB.
The long-transcript phase peaked at 55.89 MiB. After ten seconds settling, the
aggregate was **51.95 MiB**; main-engine CPU averaged **2.72% of one core** over
that settled interval. The 1% idle objective is **not met**. CORE-03/20 must trace
the remaining idle work and CORE-04/20 must preserve honest accounting of startup
discovery. Do not treat the current excess as introduced by a future migration.
All 11 measured engine lifetimes exited 0 through `StopEngine`, with no remaining
child process group. Sampling at 500 ms can miss very short-lived children;
summed process footprints are charged totals, not physical-exclusive memory.

The current native Shell replay consumed the actual stored transcript with 40
additional synthetic, recently active chat records. The normal sidebar page cap
remained in effect. Its five-second scroll phase and idle phase are separate;
the [final pixels](evidence/replay.png) show a populated chat and active sidebar
after the startup overlay disappeared. Native process peak RSS was **201.44 MiB**
and peak charged footprint **504.30 MiB**, reflecting why RSS alone is inadequate
for this Metal path. [Raw draw samples](evidence/replay-draws.json),
[process counters](evidence/replay-process.txt). Startup had a 66.48 ms maximum
draw sample; this is retained separately from warm scroll timing.

## Verification cost and limits

The first selected engine test command took **90.83 s**, including 75 s reported
compilation against an existing cache. The signed QA app build took **103 s**.
Subsequent prototype builds took 2.49 s (runtime) and 3.25 s (native examples);
the selected RPC command took 27.09 s including compilation. These are observed
cache states, not clean-build benchmarks. [Build/check commands](evidence/build-checks.json),
[E2E commands](evidence/e2e-commands.json). Format checks, Node syntax checks,
standalone runtime Clippy with `-D warnings`, scoped UI Clippy and the selected
E2Es passed. Existing workspace/vendor warnings remain outside this ticket.

Current limitations: no compositor event-to-paint/input percentile, native AX/
keyboard contribution interaction, live provider latency, remote-network latency,
low-end hardware, Linux/Windows/iOS certification, production plugin supervisor,
Loro envelope migration or asset access proof. Offscreen native rendering is
not a full-app foreground measurement; its replay uses the existing synthetic
Shell scaffold/model display. CORE-12/14/20/21 and SW-00 own those respective
acceptances. The failed foreground attempt and superseded profiler attempts are
explicitly excluded from the comparison baseline.

Hosted CI run [38099968701](https://github.com/novnski/luna/actions/runs/38099968701)
still had zero executed steps because GitHub reported an account payment/spending
limit blocker. [Readback](evidence/hosted-ci.json). The new workflow routes the
probes, but its configuration is not a passing hosted run.

The primary **LUNA Dev** was refreshed in **2 s**; exact bundle process PID 12606,
IPC 59777, `EngineReady` and strict signature checks passed.
[Handoff readback](evidence/dev-handoff.json). The stopped disposable QA bundle
was unregistered and removed after its signature/hash were retained; its data
and shared build cache were preserved. These are point-in-time checks.
