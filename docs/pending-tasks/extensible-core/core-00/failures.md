# Pre-implementation acceptance and failure ledger

Written before the CORE-00 prototypes. Test-audit authoring gate: these probes
protect transport/runtime/native extension boundaries that do not yet exist in
production; existing provider/engine keepers remain their own owners. No new
test-only production export, adapter or conditional branch is permitted. Use
standalone prototype binaries and an example using public GPUI APIs.

## Worker protocol (real child process)

1. Compatible major/minor negotiation succeeds with a correlated request ID;
   unknown major, missing capability and malformed or oversized frames fail.
2. A worker that never responds is terminated/reaped within a deadline; EOF and
   wrong IDs cannot be confused with a valid handshake. Output is bounded before
   parsing, and stderr must not be allowed to fill an unread pipe.
3. Child exit, rejection or timeout does not stop the host. Retain raw replies,
   exit status, elapsed time, expected refusal and child cleanup evidence.

## Wasm (real selected runtime, independently authored WAT guests)

1. An infinite guest loop cannot starve the host deadline. Fuel or epoch yielding
   must actually interrupt running guest code.
2. Epochs alone do not cancel a pending host call. An asynchronous host function
   must yield, its future must be dropped at the deadline, and its operation
   must release a tracked live resource without committing a late effect.
3. Linear-memory growth past the declared bound is refused; missing imports
   cannot obtain filesystem, network, environment or engine authority.
4. A later successful invocation after each refusal proves host recovery.
   Record runtime version/features, cold compile/instantiate, warm calls and
   actual cancellation distributions, including raw samples and maxima.

## Native contribution and persistence (public GPUI boundary)

1. Read an external data-only declaration after startup, add a native sidebar
   contribution, remove it, and render again without rebuilding the host.
2. The selected task and host-owned draft survive registration/removal. A
   replacement declaration cannot supply arbitrary Rust/GPUI callbacks.
3. Persist an unknown-plugin content envelope, close/reopen it, render only its
   approved fallback, and preserve opaque payload bytes. Never print the opaque
   payload into text, accessibility or logs when its renderer is absent.
4. Retain actual rendered images, input declaration and durable file hashes,
   receipts and per-transition timings. A headless native Metal/CoreText image
   proves rendering feasibility, not accessibility, compositor latency or a
   production integration. CORE-12/14 owns full native interaction/migration.

## Current performance (existing app/engine boundaries)

1. A stale profiling script calls a retired mutation and measures no work.
   Establish successful independent readback before interpreting timings.
2. Startup timing includes compilation, or warm launch is mislabeled cold.
   Separate first process launch, repeated process launch and fresh data; never
   claim a purged OS cache without actually controlling it.
3. Long transcript/multiple chats exist only in an input file, not the rendered
   app. Capture real store/watch output and render it through current UI.
4. RSS reduction merely moves work to children/graphics. Record process tree,
   aggregate samples and native physical footprint where available.
5. RPC response time is mislabeled event-to-paint latency. Keep boundaries
   explicit; unavailable metrics remain unmeasured, with follow-up owner.
6. A green E2E is used as proof that historical timeline/scroll defects vanished.
   Only the exact freshly executed contract is certified.

Initial design ceilings, set before probe execution: worker handshake deadline
1,000 ms; frame 64 KiB; Wasm call deadline 50 ms with maximum observed return
150 ms; one 16 MiB guest memory; host cleanup count zero at cancellation; native
contribution frame P95 16.7 ms after warmup. These are local feasibility targets,
not a cross-device SLA. Record misses instead of adjusting them to manufacture
a pass. Final migration budgets additionally compare identical baseline runs.
