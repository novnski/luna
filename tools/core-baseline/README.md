# CORE-00 feasibility instruments

These are architecture probes, not a shipped plugin host or SDK. Their pre-code
failure contracts, boundaries and decision are in
[`core-00`](../../docs/pending-tasks/extensible-core/core-00/README.md).
The standalone workspace pins Wasmtime without adding it to LUNA's dependency
graph. Do not load arbitrary native packages to run these fixtures.

Use the normal build guard and shared target. Keep one build at a time, and run
performance measurements after builds finish. Use new artifact directories for
each attempt; retain failures as well as passes. Commands run from repo root:

```sh
python3 scripts/build-budget.py --status
CARGO_TARGET_DIR="$PWD/target" scripts/cargo.sh build --locked --manifest-path tools/core-baseline/Cargo.toml
scripts/cargo.sh build --locked -p luna-ui --features resource-profile --example core-contribution-probe --example macos-resource-profile
mkdir -p target/qa/core-00-repeat
node scripts/qa-core-worker.mjs target/qa/core-00-repeat/worker
target/debug/luna-core-runtime-probe target/qa/core-00-repeat/wasm.json
target/debug/examples/core-contribution-probe tools/core-baseline/contribution.json tools/core-baseline/unknown-content.json target/qa/core-00-repeat/native
target/debug/examples/core-contribution-probe tools/core-baseline/contribution.json tools/core-baseline/unknown-content.json target/qa/core-00-repeat/native-restart
```

Build a current LUNA binary via `scripts/run-macos-dev.sh --build-only` (use all
four identity overrides from the QA guide for a QA bundle). Pass its executable
to the headless benchmark. It copies and ad-hoc signs that copy, uses a fresh
offline profile and free loopback port, invokes only the existing mock provider,
then stops its own processes. It never launches a GUI or edits the supplied app.
The copied binary's seal differs from the input; both hashes are retained.

```sh
node scripts/qa-core-baseline.mjs 'target/luna-macos-dev/LUNA Dev.app/Contents/MacOS/luna' target/qa/core-00-repeat/engine
LUNA_PROFILE_BACKGROUND_CHATS=40 LUNA_PROFILE_SCROLL=1 target/debug/examples/macos-resource-profile target/qa/core-00-repeat/engine/frames.json target/qa/core-00-repeat/replay
```

The engine run measures first fresh-profile and repeated process readiness,
idle and streaming resources, five requested concurrent chats (independently
requires at least two overlapping Working sessions), 40 small-project chats,
20 long-text turns and one read-only current-repository registration/run. It
compares the real transcript after a new engine process starts. The final reset
feeds the current native Shell replay with 40 background chat records (the
normal sidebar page cap still applies), a five-second scroll phase and idle
settling. The final capture is after the startup overlay. The replay uses
CoreText/Metal offscreen: do not call its CPU draw timings compositor latency or
a whole-app resource measure. It uses its own temporary settings under output.

The controlled mock lacks a native provider task correlation, so a run command
can return `pending/providerAcceptanceUnknown`; the runner accepts that specific
admission state only and then requires an independently read complete transcript.
It does not claim a completed real-provider command receipt. SW-00 owns that
broader live-provider proof. Large-repository presence is not a diff/search test.

On macOS, keep the desktop awake. Never run another LUNA GUI during a native
probe/replay. The headless engine may coexist with no GUI or the user's idle app,
but measurements should use a quiet machine with no concurrent builds. Do not
purge caches to label a launch cold. `core-baseline.yml` provides explicit CI
routing; hosted execution and local receipts remain distinct.

Required local checks:

```sh
node --check scripts/qa-core-worker.mjs
node --check scripts/qa-core-baseline.mjs
scripts/cargo.sh fmt --all --check
scripts/cargo.sh fmt --manifest-path tools/core-baseline/Cargo.toml --check
CARGO_TARGET_DIR="$PWD/target" scripts/cargo.sh clippy --locked --manifest-path tools/core-baseline/Cargo.toml -- -D warnings
scripts/cargo.sh clippy --locked -p luna-ui --features resource-profile --example core-contribution-probe --example macos-resource-profile
```

Keep raw commands, exit codes, platform/source/binary hashes, receipts, timings,
resource samples, native images and a SHA-256 manifest. Remove only the copied
benchmark binaries and compiled helper executables after evidence is sealed;
preserve fixture data, logs and shared build caches for repeatability.
